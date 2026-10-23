//! Render Thread 独占播放消费与 GPU 帧资源；Worker 的输出只在上传期间借用。
#[cfg(test)]
#[path = "animation_tests.rs"]
mod tests;
use super::*;
use crate::image::animation::{
    AnimationWorker, FrameOutput, Phase, Request, RestoreBackground, SessionKey,
};
use crate::renderer::{
    animation_timer::Timer,
    playback_clock::{Clock, passes},
};
use std::time::Duration;
use windows::{
    Win32::Graphics::Direct3D11::{
        D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_QUERY_DESC, D3D11_QUERY_EVENT, ID3D11Query,
    },
    core::BOOL,
};

/// Render Thread 唯一播放所有者；下一帧仍持有原 Credit，状态不跨线程共享。
pub(super) struct Player {
    pub(super) worker: AnimationWorker,
    session: Option<Session>,
    attempted: Option<(u64, SourceIdentity)>,
    pub(super) timer: Option<Timer>,
}
struct Session {
    key: SessionKey,
    source: SourceIdentity,
    clock: Clock,
    next: Option<Arc<FrameOutput>>,
    last_fetched: Option<u64>,
    limit: Option<u32>,
    paused: bool,
    awaiting: Option<(u16, bool, u64)>,
    selected_at: Instant,
    first_present: bool,
    first_output_at: Option<Instant>,
}
/// 帧顺序属于 Session；零帧元数据和序号溢出不能进入取模或普通加法。
fn valid_frame_order(last: Option<u64>, ordinal: u64, index: u32, pass: u32, frames: u32) -> bool {
    frames != 0
        && last.map_or(Some(0), |n| n.checked_add(1)) == Some(ordinal)
        && u64::from(index) == ordinal % u64::from(frames)
        && u64::from(pass) == ordinal / u64::from(frames)
}
impl Player {
    pub(super) fn new(worker: AnimationWorker) -> Self {
        Self {
            worker,
            session: None,
            attempted: None,
            timer: None,
        }
    }
    pub(super) fn cancel(&mut self) {
        self.worker.cancel();
        self.session = None;
    }
    pub(super) fn ready(&self) -> Option<HANDLE> {
        self.session
            .as_ref()
            .filter(|s| {
                !s.paused && !s.clock.finished() && s.next.is_none() && s.awaiting.is_none()
            })
            .and_then(|_| self.worker.ready_handle())
    }
    pub(super) fn deadline(&self) -> Option<Duration> {
        self.session
            .as_ref()
            .filter(|s| s.awaiting.is_none())
            .and_then(|s| s.clock.deadline())
    }
    pub(super) fn presented(&mut self, epoch: Instant, shared: &Shared) {
        if let Some(s) = &mut self.session {
            if let Some((delay, final_frame, _ordinal)) = s.awaiting.take() {
                s.clock.presented(epoch.elapsed(), delay, final_frame);
                if !s.first_present {
                    shared.record(Metric::AnimationFirstPresent, s.selected_at.elapsed());
                    if let Some(at) = s.first_output_at {
                        shared.record(Metric::AnimationOutputToPresent, at.elapsed());
                    }
                    s.first_present = true;
                }
                shared.increment(crate::diagnostics::Counter::AnimationPresented);
                if s.paused {
                    shared.increment(crate::diagnostics::Counter::AnimationPaused);
                }
                if final_frame {
                    self.worker.cancel();
                }
            }
        }
    }
    pub(super) fn pending_valid(&self, shared: &Shared) -> bool {
        self.session.as_ref().is_none_or(|s| {
            s.awaiting.is_none() || shared.is_image_generation_current(s.key.selection_generation)
        })
    }
}

struct Slot {
    texture: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
    source: SourceIdentity,
    session: SessionKey,
    size: (u32, u32),
    bytes: u64,
}
struct Retired {
    slot: Slot,
    query: ID3D11Query,
}
/// 最多两个纹理对象，退役资源在 Query 完成前继续计费；空闲时不轮询 Query。
#[derive(Default)]
pub(super) struct GpuFrames {
    active: Option<Slot>,
    retired: Vec<Retired>,
    pub(super) peak: u64,
}
impl GpuFrames {
    pub(super) fn bytes(&self) -> u64 {
        self.active.as_ref().map_or(0, |s| s.bytes)
            + self.retired.iter().map(|r| r.slot.bytes).sum::<u64>()
    }
    pub(super) fn view(
        &self,
        source: &SourceIdentity,
        generation: Option<u64>,
    ) -> Option<ID3D11ShaderResourceView> {
        self.active
            .as_ref()
            .filter(|s| &s.source == source && generation == Some(s.session.selection_generation))
            .map(|s| s.view.clone())
    }
    pub(super) fn collect(&mut self, context: &ID3D11DeviceContext) -> Result<()> {
        let mut i = 0;
        while i < self.retired.len() {
            let mut done = BOOL(0);
            // SAFETY: BOOL 输出与 EVENT Query 相符；S_FALSE 仍可能映射 Ok，必须检查 done 而非只看 Result。
            unsafe {
                context.GetData(
                    &self.retired[i].query,
                    Some((&mut done as *mut BOOL).cast()),
                    std::mem::size_of::<BOOL>() as u32,
                    D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                )
            }?;
            if done.as_bool() {
                self.retired.swap_remove(i);
            } else {
                i += 1;
            }
        }
        Ok(())
    }
    pub(super) fn retire(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
    ) -> Result<()> {
        if self.active.is_none() {
            return Ok(());
        }
        let mut query = None;
        let desc = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        };
        // SAFETY: Query 在本线程创建，输出槽有效；失败时 active 尚未移除。
        unsafe { device.CreateQuery(&desc, Some(&mut query)) }?;
        let query = query.context("未返回 GPU 退役 Query")?;
        // SAFETY: 解除 Context 的 SRV 引用；Query 排在最后一次使用之后。Flush 只在退役时提交，不等待 GPU。
        unsafe {
            context.PSSetShaderResources(0, Some(&[None]));
            context.End(&query);
            context.Flush();
        }
        if let Some(slot) = self.active.take() {
            self.retired.push(Retired { slot, query });
        }
        Ok(())
    }
    /// 调用者先校验 Session/Generation；在 Render Thread 上传完整画布并复用现有纹理。
    pub(super) fn upload(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        frame: &FrameOutput,
        source: &SourceIdentity,
        static_bytes: u64,
        total_budget: u64,
    ) -> Result<()> {
        let size = (frame.metadata.width, frame.metadata.height);
        anyhow::ensure!(
            size.0 > 0 && size.1 > 0 && size.0 <= 16384 && size.1 <= 16384,
            "动画纹理尺寸无效"
        );
        let stride = size.0.checked_mul(4).context("动画 Stride 溢出")?;
        let cost = u64::from(size.0)
            .checked_mul(u64::from(size.1))
            .and_then(|n| n.checked_mul(4))
            .context("动画像素大小溢出")?;
        anyhow::ensure!(frame.pixels().len() as u64 == cost, "动画完整帧布局不符");
        self.collect(context)?;
        if self.active.as_ref().is_some_and(|s| s.size != size) {
            self.retire(device, context)?;
            self.collect(context)?;
        }
        if self.active.is_none() {
            anyhow::ensure!(
                self.retired.len() < 2
                    && self
                        .bytes()
                        .checked_add(cost)
                        .is_some_and(|n| n <= 128 * 1024 * 1024
                            && n.saturating_add(static_bytes) <= total_budget),
                "动画 GPU 子额度或共享总额度不足"
            );
            let desc = D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                ..Default::default()
            };
            let mut texture = None;
            // SAFETY: 已核对布局和额度，无初始裸数据；Texture 只由本 Render Thread 使用。
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }?;
            let texture = texture.context("未返回动画 Texture")?;
            let mut view = None;
            // SAFETY: texture 存活，输出 SRV 与其共同保存在同一 Slot。
            unsafe { device.CreateShaderResourceView(&texture, None, Some(&mut view)) }?;
            self.active = Some(Slot {
                texture,
                view: view.context("未返回动画 SRV")?,
                source: source.clone(),
                session: frame.session,
                size,
                bytes: cost,
            });
            self.peak = self.peak.max(self.bytes());
        }
        let slot = self.active.as_mut().context("缺少动画纹理")?;
        // SAFETY: PBGRA8 连续数据长为 width*height*4；同一 Immediate Context 保证更新与 Draw 排序。
        // UpdateSubresource 返回前复制 CPU 源数据，调用返回后可释放 Frame/Credit；这不表示 GPU 已执行完成。
        unsafe {
            context.UpdateSubresource(
                &slot.texture,
                0,
                None,
                frame.pixels().as_ptr().cast(),
                stride,
                0,
            );
        }
        slot.source = source.clone();
        slot.session = frame.session;
        Ok(())
    }
}

impl Renderer {
    /// 每次最多取得两份输出（首帧复用加一份未来帧）；绝不建立第二个上传队列。
    pub(super) fn pump_animation(
        &mut self,
        shared: &Shared,
        snapshot: &RenderSnapshot,
    ) -> Result<bool> {
        let Some(mut player) = self.playback.take() else {
            return Ok(false);
        };
        let result = self.pump_player(&mut player, shared, snapshot);
        self.playback = Some(player);
        result
    }
    fn pump_player(
        &mut self,
        player: &mut Player,
        shared: &Shared,
        snapshot: &RenderSnapshot,
    ) -> Result<bool> {
        self.animation_gpu.collect(&self.context)?;
        shared.set_gpu_cache_bytes(self.gpu_cache_bytes + self.animation_gpu.bytes());
        let (generation, stable, control) = shared.animation_control();
        let eligible = stable && self.selection_generation == Some(generation);
        if player
            .session
            .as_ref()
            .is_some_and(|s| !eligible || self.current_image.as_ref() != Some(&s.source))
        {
            // 同一 GIF 重新选择也要先退役旧 Session，不能在新 Poster 命中时借用旧动画帧。
            self.animation_gpu.retire(&self.device, &self.context)?;
            player.cancel();
            shared.animation_available(generation, false);
        }
        if self.animation_gpu.active.as_ref().is_some_and(|s| {
            self.current_image.as_ref() != Some(&s.source)
                || self.selection_generation != Some(s.session.selection_generation)
                || self.tiles.as_ref().is_some_and(|t| t.active())
        }) {
            self.animation_gpu.retire(&self.device, &self.context)?;
        }
        if eligible
            && self.presented_generation == self.selection_generation
            && player.session.is_none()
        {
            if let Some((source, poster)) = self.current_image.as_ref().and_then(|key| {
                self.gif_posters
                    .get(key)
                    .filter(|p| p.frames > 1)
                    .map(|p| (key.clone(), *p))
            }) {
                if player.attempted.as_ref() != Some(&(generation, source.clone())) {
                    player.attempted = Some((generation, source.clone()));
                    if player.timer.is_none() {
                        match Timer::new() {
                            Ok(timer) => player.timer = Some(timer),
                            Err(error) => {
                                self.animation_error(
                                    player,
                                    shared,
                                    generation,
                                    &format!("创建动画计时器失败：{error:#}"),
                                );
                                return Ok(false);
                            }
                        }
                    }
                    let started = player.worker.start(Request {
                        path: source.path.clone(),
                        selection_generation: generation,
                        passes: None,
                        background: RestoreBackground::Transparent,
                    });
                    if let Some(completion) = player.worker.completion() {
                        shared.set_animation_completion(completion);
                    }
                    match started {
                        Ok(key) => {
                            let at = self
                                .poster_presented_at
                                .unwrap_or_else(Instant::now)
                                .saturating_duration_since(self.render_thread_started);
                            player.session = Some(Session {
                                key,
                                source,
                                clock: Clock::new(at, poster.delay_cs),
                                next: None,
                                last_fetched: None,
                                limit: None,
                                paused: false,
                                awaiting: None,
                                selected_at: self.selection_started,
                                first_present: false,
                                first_output_at: None,
                            });
                            shared.animation_available(generation, true);
                            shared.increment(crate::diagnostics::Counter::AnimationStarted);
                        }
                        Err(error) => {
                            self.animation_error(player, shared, generation, &format!("{error:#}"))
                        }
                    }
                }
            }
        }
        let Some(s) = &mut player.session else {
            return Ok(false);
        };
        let paused =
            control.user_paused || control.dialog || snapshot.width == 0 || snapshot.height == 0;
        if s.paused != paused {
            s.clock.pause(self.render_thread_started.elapsed(), paused);
            player.worker.pause(s.key, paused);
            s.paused = paused;
            if paused && (s.awaiting.is_none() || snapshot.width == 0 || snapshot.height == 0) {
                shared.increment(crate::diagnostics::Counter::AnimationPaused);
            }
        }
        if paused || s.clock.finished() || s.awaiting.is_some() {
            return Ok(false);
        }
        if s.clock.final_due(self.render_thread_started.elapsed()) {
            shared.animation_available(generation, false);
            shared.increment(crate::diagnostics::Counter::AnimationCompleted);
            return Ok(false);
        }
        if s.next.is_none() {
            for _ in 0..2 {
                let (status, frame) = player.worker.poll(s.key);
                if matches!(status.phase, Phase::Failed | Phase::Stopped) {
                    let error = status.error.unwrap_or_else(|| "动画线程已停止".to_owned());
                    self.animation_error(player, shared, generation, &error);
                    return Ok(false);
                }
                let Some(frame) = frame else {
                    break;
                };
                if !player.worker.is_current(&frame)
                    || frame.metadata.source.content != crate::image::ImageContent::GifAnimationV1
                    || !s.source.same_revision(&frame.metadata.source)
                    || frame.session != s.key
                    || !shared.is_image_generation_current(generation)
                    || frame.metadata.frames != self.gif_posters[&s.source].frames
                    || !valid_frame_order(
                        s.last_fetched,
                        frame.ordinal,
                        frame.frame_index,
                        frame.pass,
                        frame.metadata.frames,
                    )
                {
                    shared.increment(crate::diagnostics::Counter::AnimationRejected);
                    self.animation_error(
                        player,
                        shared,
                        generation,
                        "动画输出身份、修订或序号不符",
                    );
                    return Ok(false);
                }
                s.last_fetched = Some(frame.ordinal);
                if frame.ordinal == 0 {
                    println!(
                        "GIF animation metadata: frames={} raw_loop={:?} worst_owned_pixels={} bytes; screen timing not measured",
                        frame.metadata.frames, frame.metadata.raw_loop, frame.metadata.worst_pixels
                    );
                    s.limit = passes(frame.metadata.raw_loop);
                    s.first_output_at = Some(Instant::now());
                    shared.record(Metric::AnimationFirstOutput, s.selected_at.elapsed());
                    shared.increment(crate::diagnostics::Counter::AnimationPosterReused);
                    continue;
                }
                s.next = Some(frame);
                break;
            }
        }
        if !s.clock.due(self.render_thread_started.elapsed()) {
            return Ok(false);
        }
        let Some(frame) = s.next.take() else {
            if s.clock.underflow() {
                shared.increment(crate::diagnostics::Counter::AnimationUnderflow);
            }
            return Ok(false);
        };
        if !shared.is_image_generation_current(generation) || !player.worker.is_current(&frame) {
            return Ok(false);
        }
        if let Some(deadline) = s.clock.expected_deadline() {
            shared.record(
                Metric::AnimationLateness,
                self.render_thread_started
                    .elapsed()
                    .saturating_sub(deadline),
            );
        }
        let additional = if self
            .animation_gpu
            .active
            .as_ref()
            .is_some_and(|t| t.size == (frame.metadata.width, frame.metadata.height))
        {
            0
        } else {
            frame.pixels().len() as u64
        };
        self.evict_gpu_cache_for(additional);
        let at = Instant::now();
        let uploaded = self.animation_gpu.upload(
            &self.device,
            &self.context,
            &frame,
            &s.source,
            self.gpu_cache_bytes,
            self.gpu_cache_budget,
        );
        shared.record(Metric::AnimationUpload, at.elapsed());
        if let Err(error) = uploaded {
            self.animation_error(
                player,
                shared,
                generation,
                &format!("动画上传失败：{error:#}"),
            );
            return Ok(false);
        }
        let final_frame = frame.frame_index + 1 == frame.metadata.frames
            && s.limit.is_some_and(|limit| frame.pass + 1 >= limit);
        s.awaiting = Some((frame.delay_cs, final_frame, frame.ordinal));
        // frame 在本函数末释放；UpdateSubresource 已复制源数据，Credit 不随 GPU 排队延长。
        shared.set_gpu_cache_bytes(self.gpu_cache_bytes + self.animation_gpu.bytes());
        Ok(true)
    }
    fn animation_error(
        &self,
        player: &mut Player,
        shared: &Shared,
        generation: u64,
        message: &str,
    ) {
        player.cancel();
        shared.animation_available(generation, false);
        shared.increment(crate::diagnostics::Counter::AnimationFailed);
        shared.publish_gpu_probe_reply(GpuProbeReply::AnimationError {
            generation,
            message: message.to_owned(),
        });
    }
}
