//! 创建应用状态、启动渲染线程并运行 Win32 消息循环。

#[cfg(test)]
#[path = "app/gif_tests.rs"]
mod gif_tests;

use std::{
    collections::VecDeque,
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_EVENT, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{CreateEventW, GetCurrentThreadId, INFINITE, SetEvent},
    UI::Controls::Dialogs::{
        CommDlgExtendedError, GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR,
        OFN_PATHMUSTEXIST, OPENFILENAMEW,
    },
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
    UI::WindowsAndMessaging::{
        DestroyWindow, DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW,
        QS_ALLINPUT, TranslateMessage, WM_QUIT,
    },
    UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW, SetWindowTextW},
};
use windows::core::w;

const IMAGE_WORKER_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

use crate::{
    diagnostics::{Counter, ImageTaskTrace, Metric, Metrics},
    image::{
        DecodedImage, ImageBudget, SourceIdentity,
        navigation::{ImageDirectory, absolute_path},
        worker::{ImageLoader, LoaderMessage},
    },
    renderer, window,
};

#[derive(Clone, Copy)]
pub(crate) struct RenderSnapshot {
    pub(crate) sequence: u64,
    pub(crate) texture_index: usize,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) requested_at: Instant,
    pub(crate) request_origin: RequestOrigin,
    pub(crate) view_mode: ViewMode,
    pub(crate) zoom: f32,
    pub(crate) pan: (f32, f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewMode {
    Fit,
    NativePixels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestOrigin {
    InitialWindowSize,
    KeyInput,
    Resize,
    Minimize,
    Restore,
    Paint,
    ImageReady,
}

struct PendingState {
    snapshot: RenderSnapshot,
    image_generation: u64,
    // UI 设置同方向长按的首个选择代际；松键、反向、失焦或打开文件时清除。
    progress_floor: Option<u64>,
    // 已接受图片的最大选择代际，是同一个 Generation 的水位，不独立递增。
    accepted_image_generation: u64,
    redraw: bool,
    shutdown: bool,
    shutdown_started: Option<Instant>,
    // 选择代际、共享像素所有权、CPU 命中标记；最后一项只用于诊断，不是另一套版本号。
    image_update: Option<(u64, Arc<DecodedImage>, bool)>,
    large_request: Option<crate::large::worker::Request>,
    #[cfg(test)]
    clear_gpu_cache: bool,
    gpu_probe: Option<ResolvedSelection>,
    preload_uploads: VecDeque<Arc<DecodedImage>>,
    cancel_preloads: bool,
}

/// 一次从 UI 状态中取出的有界渲染工作批次。
pub(crate) struct PendingRenderWork {
    pub(crate) snapshot: RenderSnapshot,
    pub(crate) redraw: bool,
    pub(crate) shutdown: bool,
    pub(crate) image: Option<(u64, Arc<DecodedImage>, bool)>,
    pub(crate) large_request: Option<crate::large::worker::Request>,
    #[cfg(test)]
    pub(crate) clear_gpu_cache: bool,
    pub(crate) gpu_probe: Option<ResolvedSelection>,
    pub(crate) preloads: Vec<Arc<DecodedImage>>,
    pub(crate) cancel_preloads: bool,
}

/// UI 与 Render Thread 共享的有界状态。
///
/// 互斥锁中只保存最新快照和几个标志，不保存逐条输入消息。Windows auto-reset event 仅负责唤醒；
/// 多次输入在渲染线程读取前会自然合并成最新状态。`wake_event` 以整数保存，避免把 COM 或 HWND
/// 包装器跨线程传递；事件句柄在渲染线程退出并 join 后才由 UI 线程关闭。
pub(crate) struct Shared {
    pending: Mutex<PendingState>,
    wake_event: isize,
    metrics: Mutex<Metrics>,
    fatal_error: Mutex<Option<String>>,
    gpu_probe_reply: Mutex<Option<GpuProbeReply>>,
    ui_ready_event: Mutex<isize>,
}

/// 后台已校验的选择请求；只携带源修订和路径，不跨线程传递任何 COM 对象。
pub(crate) struct ResolvedSelection {
    pub(crate) generation: u64,
    pub(crate) path: PathBuf,
    pub(crate) identity: SourceIdentity,
    pub(crate) requested_at: Instant,
    pub(crate) preloads: Vec<PathBuf>,
}

/// 渲染线程只返回缓存判断，不把 GPU 接口交给 UI；单槽邮箱允许过期答复被较新的答复覆盖。
pub(crate) enum GpuProbeReply {
    Hit { generation: u64, path: PathBuf },
    Miss(ResolvedSelection),
    LargeError { generation: u64, message: String },
}

impl Shared {
    fn pending(&self) -> MutexGuard<'_, PendingState> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn metrics_report(&self) -> String {
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .report()
    }

    pub(crate) fn take_pending(&self) -> PendingRenderWork {
        let mut pending = self.pending();
        let redraw = std::mem::take(&mut pending.redraw);
        PendingRenderWork {
            snapshot: pending.snapshot,
            redraw,
            shutdown: pending.shutdown,
            image: pending.image_update.take(),
            large_request: pending.large_request.take(),
            #[cfg(test)]
            clear_gpu_cache: std::mem::take(&mut pending.clear_gpu_cache),
            gpu_probe: pending.gpu_probe.take(),
            preloads: pending.preload_uploads.drain(..).collect(),
            cancel_preloads: std::mem::take(&mut pending.cancel_preloads),
        }
    }

    pub(crate) fn current_snapshot(&self) -> RenderSnapshot {
        self.pending().snapshot
    }

    /// 发布 UI 的最新选择代际，并清除尚未消费的旧像素和缓存探测。
    pub(crate) fn set_image_generation(&self, generation: u64) {
        self.set_navigation_generation(generation, None);
    }

    /// 长按进度使用同一选择代际的下界，不增加另一套版本计数；最新目标仍由 UI 独占。
    pub(crate) fn set_navigation_generation(&self, generation: u64, progress_floor: Option<u64>) {
        let mut pending = self.pending();
        pending.image_generation = generation;
        pending.progress_floor = progress_floor;
        if pending
            .image_update
            .as_ref()
            .is_some_and(|(candidate, _, _)| !candidate_allowed(&pending, *candidate))
        {
            pending.image_update = None;
        }
        pending.gpu_probe = None;
        pending.large_request = None;
        drop(pending);
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_target_generation(generation);
    }

    /// 松键、反向或失焦时立即停止旧进度；当前最新目标继续加载。
    pub(crate) fn end_navigation_progress(&self) {
        let mut pending = self.pending();
        let changed = pending.progress_floor.take().is_some();
        if pending
            .image_update
            .as_ref()
            .is_some_and(|(generation, _, _)| *generation != pending.image_generation)
        {
            pending.image_update = None;
        }
        drop(pending);
        if changed {
            self.wake_render_thread();
        }
    }

    /// 新候选不能覆盖已接受的更新代际，长按进度也必须落在当前方向的有效区间。
    pub(crate) fn can_present_image_generation(&self, generation: u64) -> bool {
        let pending = self.pending();
        candidate_allowed(&pending, generation) && generation >= pending.accepted_image_generation
    }

    /// GPU 命中激活前原子认领代际，防止稍后到达的旧解码覆盖已接受的新纹理。
    pub(crate) fn claim_image_generation(&self, generation: u64) -> bool {
        let mut pending = self.pending();
        if !candidate_allowed(&pending, generation)
            || generation < pending.accepted_image_generation
        {
            return false;
        }
        pending.accepted_image_generation = generation;
        true
    }

    /// 记录成功返回的图片 Present 所对应的选择，不代表该帧已经显示。
    pub(crate) fn record_presented_generation(&self, generation: u64) {
        let progress = self.pending().image_generation != generation;
        let mut metrics = self
            .metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        metrics.record_presented_generation(generation);
        if progress {
            metrics.increment(Counter::NavigationProgressPresented);
        }
    }

    /// 在 UI 线程收到真实 WM_KEYUP 时记录该时刻的目标，关联松键后的最终更新。
    pub(crate) fn record_navigation_release_at(&self, at: Instant) {
        let generation = self.pending().image_generation;
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_navigation_release(generation, at);
    }

    /// 短暂持锁检查候选结果是否仍属于 UI 的最新选择。
    pub(crate) fn is_image_generation_current(&self, generation: u64) -> bool {
        let p = self.pending();
        !p.shutdown && p.image_generation == generation
    }

    /// 复用已有选择版本；后台大图构建取消不另建代际体系。
    pub(crate) fn image_generation(&self) -> u64 {
        self.pending().image_generation
    }

    /// 发布当前源的大图请求，清除上一源的缩放偏移；不在 UI 线程读取 PNG 或缓存。
    pub(crate) fn request_large(&self, request: crate::large::worker::Request) {
        {
            let mut p = self.pending();
            if p.shutdown || p.image_generation != request.generation {
                return;
            }
            // 新源不能继承上一张图的任意平移位置；保留用户选择的 Fit/100% 模式。
            p.snapshot.zoom = 1.0;
            p.snapshot.pan = (0.0, 0.0);
            p.snapshot.sequence = p.snapshot.sequence.wrapping_add(1);
            p.large_request = Some(request);
        }
        self.wake_render_thread();
    }

    /// 缩放以窗口中心为锚点；只更新快照，不在 UI 回调读取 Tile。
    pub(crate) fn zoom_by(&self, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.publish(RequestOrigin::Paint, |p| {
                let next = (p.snapshot.zoom * factor).clamp(0.01, 64.0);
                let ratio = next / p.snapshot.zoom;
                p.snapshot.pan.0 = (p.snapshot.pan.0 * ratio).clamp(-1e9, 1e9);
                p.snapshot.pan.1 = (p.snapshot.pan.1 * ratio).clamp(-1e9, 1e9);
                p.snapshot.zoom = next;
            });
        }
    }
    pub(crate) fn pan_by(&self, x: f32, y: f32) {
        self.publish(RequestOrigin::Paint, |p| {
            p.snapshot.pan.0 = (p.snapshot.pan.0 + x).clamp(-1e9, 1e9);
            p.snapshot.pan.1 = (p.snapshot.pan.1 + y).clamp(-1e9, 1e9);
        });
    }

    /// 向渲染线程发布最新一次已校验的选择；此处不读文件、不访问 GPU。
    pub(crate) fn request_gpu_probe(&self, selection: ResolvedSelection) {
        {
            let mut pending = self.pending();
            if pending.shutdown || pending.image_generation != selection.generation {
                return;
            }
            pending.gpu_probe = Some(selection);
        }
        self.wake_render_thread();
    }

    /// 安装 ImageLoader 借出的通知事件；UI 保证先 join 渲染线程，再释放该事件。
    pub(crate) fn set_ui_ready_event(&self, event: HANDLE) {
        *self
            .ui_ready_event
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = event.0 as isize;
    }

    /// 覆盖单槽答复并唤醒 UI，后续仍由 UI 按 Generation 拒收过期答复。
    pub(crate) fn publish_gpu_probe_reply(&self, reply: GpuProbeReply) {
        *self
            .gpu_probe_reply
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reply);
        let event = *self
            .ui_ready_event
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if event != 0 {
            // SAFETY: 该事件由 ImageLoader 持有；它在 UI 消息循环结束后才关闭，渲染线程只用它唤醒 UI 处理探测结果。
            let _ = unsafe { SetEvent(HANDLE(event as _)) };
        }
    }

    /// 消费最近一次 GPU 缓存探测答复；没有答复时返回 None。
    pub(crate) fn take_gpu_probe_reply(&self) -> Option<GpuProbeReply> {
        self.gpu_probe_reply
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    pub(crate) fn wake_event(&self) -> isize {
        self.wake_event
    }

    fn publish(
        &self,
        origin: RequestOrigin,
        update: impl FnOnce(&mut PendingState),
    ) -> std::time::Duration {
        let started = Instant::now();
        {
            let mut pending = self.pending();
            if pending.shutdown {
                return started.elapsed();
            }
            update(&mut pending);
            pending.snapshot.sequence = pending.snapshot.sequence.wrapping_add(1);
            pending.snapshot.requested_at = Instant::now();
            pending.snapshot.request_origin = origin;
            pending.redraw = true;
        }

        // SAFETY: `wake_event` 是窗口创建前生成的 auto-reset event。UI 作用域会在渲染线程 join 后才关闭它。
        // SAFETY: SetEvent 可跨线程调用；当前 UI 线程只传入仍由 WakeEvent 持有的有效句柄。
        unsafe {
            let _ = windows::Win32::System::Threading::SetEvent(HANDLE(self.wake_event as _));
        }
        started.elapsed()
    }

    pub(crate) fn resize(&self, width: u32, height: u32) {
        let origin = {
            let pending = self.pending();
            if width == 0 || height == 0 {
                RequestOrigin::Minimize
            } else if pending.snapshot.width == 0 || pending.snapshot.height == 0 {
                if pending.snapshot.sequence == 0 {
                    RequestOrigin::InitialWindowSize
                } else {
                    RequestOrigin::Restore
                }
            } else {
                RequestOrigin::Resize
            }
        };
        self.publish(origin, |pending| {
            pending.snapshot.width = width;
            pending.snapshot.height = height;
        });
    }

    pub(crate) fn select_texture(&self, texture_index: usize) -> std::time::Duration {
        self.publish(RequestOrigin::KeyInput, |pending| {
            pending.snapshot.texture_index = texture_index
        })
    }

    pub(crate) fn redraw(&self) {
        self.publish(RequestOrigin::Paint, |_| {});
    }

    /// 仅唤醒 Render Thread 继续处理有界的后台 GPU 上传，不请求重绘。
    pub(crate) fn wake_render_thread(&self) {
        // SAFETY: 事件由 UI 生命周期持有，且一定在 Render Thread join 后才关闭。
        unsafe {
            let _ = SetEvent(HANDLE(self.wake_event as _));
        }
    }

    /// 发布解码结果；`cpu_cache_hit` 仅用于区分到 Present 返回的测量路径。
    pub(crate) fn set_image(
        &self,
        generation: u64,
        image: Arc<DecodedImage>,
        requested_at: Instant,
        cpu_cache_hit: bool,
    ) {
        {
            let mut pending = self.pending();
            if !candidate_allowed(&pending, generation)
                || generation < pending.accepted_image_generation
            {
                return;
            }
            pending.accepted_image_generation = generation;
            pending.snapshot.sequence = pending.snapshot.sequence.wrapping_add(1);
            pending.snapshot.requested_at = requested_at;
            pending.snapshot.request_origin = RequestOrigin::ImageReady;
            pending.image_update = Some((generation, image, cpu_cache_hit));
            pending.redraw = true;
        }
        // SAFETY: 事件句柄由 UI 生命周期持有，并且只会在渲染线程 join 后关闭。
        unsafe {
            let _ = SetEvent(HANDLE(self.wake_event as _));
        }
    }

    pub(crate) fn preload_image(&self, image: Arc<DecodedImage>) {
        {
            let mut pending = self.pending();
            if pending.shutdown {
                return;
            }
            if !pending
                .preload_uploads
                .iter()
                .any(|existing| existing.source == image.source)
            {
                if pending.preload_uploads.len() == 2 {
                    pending.preload_uploads.pop_front();
                }
                pending.preload_uploads.push_back(image);
            }
        }
        // 预加载只唤醒渲染线程处理资源，不要求提交一帧。
        // SAFETY: 与工作事件的其他调用相同，句柄由 UI 外层持有到渲染线程 join 后。
        unsafe {
            let _ = SetEvent(HANDLE(self.wake_event as _));
        }
    }

    /// 新选择到来时丢弃渲染线程尚未上传的预加载项，并立即唤醒它处理取消标记。
    pub(crate) fn cancel_pending_preloads(&self) {
        {
            let mut pending = self.pending();
            if pending.shutdown {
                return;
            }
            pending.preload_uploads.clear();
            pending.cancel_preloads = true;
        }
        // SAFETY: wake_event 在渲染线程 join 前保持有效；此通知只用于让线程读取取消标记。
        unsafe {
            let _ = SetEvent(HANDLE(self.wake_event as _));
        }
    }

    pub(crate) fn set_view_mode(&self, mode: ViewMode) {
        self.publish(RequestOrigin::Paint, |pending| {
            pending.snapshot.view_mode = mode;
            pending.snapshot.zoom = 1.0;
            pending.snapshot.pan = (0.0, 0.0);
        });
    }

    pub(crate) fn request_shutdown(&self) {
        {
            let mut pending = self.pending();
            if pending.shutdown {
                return;
            }
            pending.shutdown = true;
            pending.shutdown_started = Some(Instant::now());
        }

        // SAFETY: 与 publish 相同，事件由外层 UI 生命周期持有，join 前不会关闭。
        unsafe {
            let _ = windows::Win32::System::Threading::SetEvent(HANDLE(self.wake_event as _));
        }
    }

    pub(crate) fn record(&self, metric: Metric, elapsed: std::time::Duration) {
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(metric, elapsed);
    }

    /// 保存一条有界请求诊断，退出时再输出，避免 Worker 在关键路径写控制台。
    pub(crate) fn record_image_task(&self, trace: ImageTaskTrace) {
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_image_task(trace);
    }

    /// 大图长尾信息只进入有界诊断缓冲，不在绘制路径同步写控制台。
    pub(crate) fn record_tile_view(&self, trace: String) {
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_tile_view(trace);
    }

    pub(crate) fn increment(&self, counter: Counter) {
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .increment(counter);
    }

    pub(crate) fn set_cpu_cache_bytes(&self, cpu: u64) {
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_cpu_cache_bytes(cpu);
    }

    pub(crate) fn set_gpu_cache_bytes(&self, gpu: u64) {
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_gpu_cache_bytes(gpu);
    }

    pub(crate) fn record_shutdown(&self) {
        let started = self.pending().shutdown_started;
        if let Some(started) = started {
            self.record(Metric::Shutdown, started.elapsed());
        }
    }

    pub(crate) fn set_fatal_error(&self, error: String) {
        *self
            .fatal_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
    }

    fn fatal_error(&self) -> Option<String> {
        self.fatal_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn candidate_allowed(pending: &PendingState, generation: u64) -> bool {
    !pending.shutdown
        && generation <= pending.image_generation
        && (generation == pending.image_generation
            || pending
                .progress_floor
                .is_some_and(|floor| generation >= floor))
}

pub(crate) struct WindowContext {
    pub(crate) shared: Arc<Shared>,
    pub(crate) close_started: bool,
    pub(crate) drag_position: Option<(i32, i32)>,
    pub(crate) action: Option<WindowAction>,
    navigation: UiNavigation,
}

#[derive(Clone, Copy)]
pub(crate) enum WindowAction {
    Open,
    Previous,
    Next,
    Fit,
    NativePixels,
    NavigationReleased { direction: isize, at: Instant },
    StopNavigation,
}

impl WindowContext {
    /// UI 在 WM_KEYUP 回调内立即关闭进度窗口，再交给消息循环更新 Worker 与松键诊断。
    pub(crate) fn navigation_key_released(&mut self, direction: isize, at: Instant) {
        if self.navigation.direction == Some(direction) {
            self.shared.end_navigation_progress();
            self.action = Some(WindowAction::NavigationReleased { direction, at });
        }
    }
}

#[derive(Default)]
struct UiNavigation {
    generation: u64,
    current_path: Option<PathBuf>,
    directory: Option<ImageDirectory>,
    pending_steps: i32,
    requested_at: Option<Instant>,
    fallback: Option<(Option<PathBuf>, Option<ImageDirectory>)>,
    direction: Option<isize>,
    progress_floor: Option<u64>,
}

/// UI 独占的渲染唤醒事件；所有发布者停止且 Render Thread join 后才释放句柄。
pub(crate) struct WakeEvent(HANDLE);

impl Drop for WakeEvent {
    fn drop(&mut self) {
        // SAFETY: WakeEvent 独占 CreateEventW 返回的句柄。它只会在渲染线程 join 后析构，届时已无等待者使用句柄。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct CompletionEvent(HANDLE);

impl CompletionEvent {
    fn new() -> Result<Self> {
        // SAFETY: 创建无名称 manual-reset event；UI 线程持有句柄，并在渲染线程 join 后才关闭。
        let event =
            unsafe { CreateEventW(None, true, false, None) }.context("创建渲染线程完成事件失败")?;
        Ok(Self(event))
    }
}

impl Drop for CompletionEvent {
    fn drop(&mut self) {
        // SAFETY: 完成事件由 CompletionEvent 独占；析构前渲染线程已 join，不再访问该句柄。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct CompletionSignal {
    event_value: isize,
    ui_thread_id: u32,
}

impl Drop for CompletionSignal {
    fn drop(&mut self) {
        // SAFETY: UI 线程持有完成事件，且 join 返回前不会关闭；此 guard 只在渲染线程退出时置位。
        let signaled = unsafe { SetEvent(HANDLE(self.event_value as _)) }.is_ok();
        if !signaled {
            // SAFETY: UI 线程 ID 在创建窗口后取得，线程队列已存在；WM_QUIT 是完成事件失效时的退出兜底。
            let _ = unsafe {
                windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    self.ui_thread_id,
                    WM_QUIT,
                    Default::default(),
                    Default::default(),
                )
            };
        }
    }
}

fn run_render_worker(shared: &Shared, render: impl FnOnce() -> Result<()>) {
    match catch_unwind(AssertUnwindSafe(render)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            shared.set_fatal_error(format!("渲染线程失败：{error:#}"));
            shared.request_shutdown();
        }
        Err(_) => {
            shared.set_fatal_error("渲染线程发生 panic；资源已在栈展开时释放".to_owned());
            shared.request_shutdown();
        }
    }
    shared.record_shutdown();
}

pub(crate) fn run() -> Result<()> {
    let process_started = Instant::now();
    // SAFETY: 本进程在创建任何 HWND 前设置 Per-Monitor DPI Awareness，后续不会再次更改进程级 DPI 模式。
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            .context("设置 Per-Monitor DPI Awareness V2 失败")?;
    }

    let wake_handle =
        // SAFETY: 空安全属性使用系统默认 ACL；无名称 auto-reset event 由 WakeEvent 在渲染线程 join 后关闭。
        unsafe { CreateEventW(None, false, false, None) }.context("创建渲染唤醒事件失败")?;
    let wake_event = WakeEvent(wake_handle);
    let completion_event = CompletionEvent::new()?;

    let initial_snapshot = RenderSnapshot {
        sequence: 0,
        texture_index: 0,
        width: 0,
        height: 0,
        requested_at: process_started,
        request_origin: RequestOrigin::InitialWindowSize,
        view_mode: ViewMode::Fit,
        zoom: 1.0,
        pan: (0.0, 0.0),
    };
    let shared = Arc::new(Shared {
        pending: Mutex::new(PendingState {
            snapshot: initial_snapshot,
            image_generation: 0,
            progress_floor: None,
            accepted_image_generation: 0,
            redraw: true,
            shutdown: false,
            shutdown_started: None,
            image_update: None,
            large_request: None,
            #[cfg(test)]
            clear_gpu_cache: false,
            gpu_probe: None,
            preload_uploads: VecDeque::new(),
            cancel_preloads: false,
        }),
        wake_event: wake_event.0.0 as isize,
        metrics: Mutex::new(Metrics::default()),
        fatal_error: Mutex::new(None),
        gpu_probe_reply: Mutex::new(None),
        ui_ready_event: Mutex::new(0),
    });

    let mut loader = ImageLoader::start(ImageBudget::default(), Arc::clone(&shared))?;
    shared.set_ui_ready_event(loader.ready_handle());
    let mut context = Box::new(WindowContext {
        shared: Arc::clone(&shared),
        close_started: false,
        drag_position: None,
        action: None,
        navigation: UiNavigation::default(),
    });
    let hwnd = window::create_main_window(&mut context).context("创建 Win32 主窗口失败")?;
    let startup_elapsed = process_started.elapsed();
    shared.record(Metric::Startup, startup_elapsed);
    println!(
        "Startup to CreateWindowExW return: {} us",
        startup_elapsed.as_micros()
    );

    let render_shared = Arc::clone(&shared);
    let raw_hwnd = hwnd.0 as isize;
    // SAFETY: 此 ID 属于当前 UI 线程；创建窗口后其消息队列已建立，并在 render thread 退出前保持存活。
    let ui_thread_id = unsafe { GetCurrentThreadId() };
    let completion_event_value = completion_event.0.0 as isize;
    let mut render_thread = Some(
        match thread::Builder::new()
            .name("pixstride-render".to_owned())
            .spawn(move || {
                let _completion_signal = CompletionSignal {
                    event_value: completion_event_value,
                    ui_thread_id,
                };
                run_render_worker(&render_shared, || {
                    renderer::render_thread(raw_hwnd, Arc::clone(&render_shared))
                });
            }) {
            Ok(thread) => thread,
            Err(error) => {
                context.close_started = true;
                // SAFETY: HWND 由当前 UI 线程创建，渲染线程尚未启动；Box 在销毁消息完成前仍然存活。
                if let Err(destroy_error) = unsafe { DestroyWindow(hwnd) } {
                    // 销毁失败时窗口过程仍可能读取 GWLP_USERDATA，故保留 Box 直到进程退出，避免悬空指针。
                    let _ = Box::into_raw(context);
                    return Err(error).context(format!(
                        "创建渲染线程失败，且销毁 HWND 也失败：{destroy_error}"
                    ));
                }
                return Err(error).context("创建渲染线程失败");
            }
        },
    );

    window::show_main_window(hwnd);

    let initial_argument = std::env::args_os().nth(1).map(PathBuf::from);
    let mut startup_argument_error = None;
    let has_extra_arguments = std::env::args_os().nth(2).is_some();
    if has_extra_arguments {
        startup_argument_error = Some("Pixstride 一次只接受一个图片路径参数。".to_owned());
    }
    if let Some(path) = initial_argument.filter(|_| !has_extra_arguments) {
        if let Err(error) = request_open(&mut context, &loader, hwnd, path) {
            startup_argument_error = Some(format!("无法打开命令行图片：{error:#}"));
        }
    }
    if let Some(error) = startup_argument_error {
        show_error(hwnd, &error);
    }

    let mut message = MSG::default();
    let mut loop_error = None;
    let mut render_done = false;
    let mut loader_done = false;
    let mut identity_done = false;
    let mut loader_stop_sent = false;
    let mut loader_shutdown_started = None;
    let mut render_join_error = None;

    while !render_done || !loader_done || !identity_done {
        #[derive(Clone, Copy)]
        enum WaitTarget {
            Render,
            LoaderReady,
            LoaderDone,
            IdentityDone,
        }
        let mut handles = Vec::with_capacity(4);
        let mut targets = Vec::with_capacity(4);
        if !render_done {
            handles.push(completion_event.0);
            targets.push(WaitTarget::Render);
        }
        handles.push(loader.ready_handle());
        targets.push(WaitTarget::LoaderReady);
        if !loader_done {
            handles.push(loader.completed_handle());
            targets.push(WaitTarget::LoaderDone);
        }
        if !identity_done {
            handles.push(loader.identity_completed_handle());
            targets.push(WaitTarget::IdentityDone);
        }
        let timeout = loader_shutdown_started
            .map(remaining_worker_shutdown_ms)
            .unwrap_or(INFINITE);
        // SAFETY: 事件句柄在所属线程停止前保持有效；运行时等待事件，关闭期只等待剩余宽限时间。
        let result =
            unsafe { MsgWaitForMultipleObjects(Some(&handles), false, timeout, QS_ALLINPUT) };
        let signaled_target = if result.0 < WAIT_OBJECT_0.0 + handles.len() as u32 {
            Some((result.0 - WAIT_OBJECT_0.0) as usize)
        } else {
            None
        };
        match signaled_target.and_then(|index| targets.get(index).copied()) {
            Some(WaitTarget::Render) => {
                if let Some(thread) = render_thread.take() {
                    render_join_error = thread.join().err();
                } else {
                    loop_error = Some(anyhow!("渲染线程完成事件触发时找不到线程句柄"));
                }
                render_done = true;
                context.close_started = true;
            }
            Some(WaitTarget::LoaderReady) => {
                process_loader_messages(&mut context, &loader, hwnd);
                process_gpu_probe_reply(&mut context, &loader, hwnd);
            }
            Some(WaitTarget::LoaderDone) => {
                loader_done = true;
                if !context.close_started {
                    loop_error = Some(anyhow!("解码线程提前退出"));
                    context.close_started = true;
                    shared.request_shutdown();
                }
            }
            Some(WaitTarget::IdentityDone) => {
                identity_done = true;
                if !context.close_started {
                    loop_error = Some(anyhow!("文件身份线程提前退出"));
                    context.close_started = true;
                    shared.request_shutdown();
                }
            }
            None if result == WAIT_EVENT(WAIT_OBJECT_0.0 + handles.len() as u32) => {
                // SAFETY: UI 线程独占消息队列；取出的消息仅在本线程翻译和分发。
                while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                    if message.message == WM_QUIT {
                        context.close_started = true;
                        shared.request_shutdown();
                        continue;
                    }
                    // SAFETY: 消息已从当前线程队列取出；Translate/Dispatch 与窗口线程归属一致。
                    unsafe {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                    process_window_action(&mut context, &loader, hwnd);
                }
            }
            None if result == WAIT_TIMEOUT
                && loader_shutdown_started.is_some()
                && (!loader_done || !identity_done) =>
            {
                eprintln!(
                    "Image workers did not stop within {} ms; detaching them until process exit",
                    IMAGE_WORKER_SHUTDOWN_GRACE.as_millis()
                );
                loader.detach_for_process_exit();
                loader_done = true;
                identity_done = true;
            }
            None if result == WAIT_FAILED => {
                loop_error = Some(
                    anyhow!(windows::core::Error::from_thread())
                        .context("等待窗口、解码结果或工作线程完成失败"),
                );
                context.close_started = true;
                shared.request_shutdown();
            }
            _ => {}
        }

        if context.close_started && !loader_stop_sent {
            loader.request_stop();
            loader_stop_sent = true;
            loader_shutdown_started = Some(Instant::now());
        }
    }

    if !context.close_started {
        context.close_started = true;
    }
    if !loader_stop_sent {
        loader.request_stop();
    }
    if let Err(error) = loader.join() {
        loop_error.get_or_insert_with(|| error.context("等待图片后台线程退出失败"));
    }
    // SAFETY: Render Thread 已 join；WIC Worker 已 join，或已分离且从未持有 HWND/WindowContext；HWND 仍由 UI 线程拥有。
    let destroy_result = unsafe { DestroyWindow(hwnd) };

    let report = shared.metrics_report();
    print!("{report}");

    if let Err(destroy_error) = destroy_result {
        let _ = Box::into_raw(context);
        return Err(destroy_error).context("工作线程退出后销毁 Win32 主窗口失败");
    }
    if render_join_error.is_some() {
        return Err(anyhow!("渲染线程发生未处理的 panic；窗口资源已完成清理"));
    }
    if let Some(error) = loop_error {
        return Err(error);
    }
    if let Some(error) = shared.fatal_error() {
        return Err(anyhow!(error));
    }
    Ok(())
}

fn remaining_worker_shutdown_ms(started: Instant) -> u32 {
    IMAGE_WORKER_SHUTDOWN_GRACE
        .saturating_sub(started.elapsed())
        .as_millis()
        .min(u32::MAX as u128) as u32
}

fn request_open(
    context: &mut WindowContext,
    loader: &ImageLoader,
    hwnd: windows::Win32::Foundation::HWND,
    path: PathBuf,
) -> Result<()> {
    if !crate::image::is_supported_path(&path) {
        bail!("只支持 JPEG、PNG 和 BMP 图片：{}", path.display());
    }
    let path = absolute_path(&path)?;
    stop_navigation(context, loader);
    context.navigation.generation = context.navigation.generation.wrapping_add(1).max(1);
    if context.navigation.fallback.is_none() {
        context.navigation.fallback = Some((
            context.navigation.current_path.take(),
            context.navigation.directory.take(),
        ));
    }
    context.navigation.current_path = Some(path.clone());
    context.navigation.pending_steps = 0;
    context.navigation.requested_at = Some(Instant::now());
    context
        .shared
        .set_image_generation(context.navigation.generation);
    context.shared.cancel_pending_preloads();
    loader.request_open(
        context.navigation.generation,
        path.clone(),
        context.navigation.requested_at.unwrap_or_else(Instant::now),
    );
    set_window_title(hwnd, &format!("正在打开：{}", path.display()));
    Ok(())
}

fn process_window_action(
    context: &mut WindowContext,
    loader: &ImageLoader,
    hwnd: windows::Win32::Foundation::HWND,
) {
    let Some(action) = context.action.take() else {
        return;
    };
    match action {
        WindowAction::Open => {
            stop_navigation(context, loader);
            match choose_image(hwnd) {
                Ok(Some(path)) => {
                    if let Err(error) = request_open(context, loader, hwnd, path) {
                        show_error(hwnd, &format!("无法打开图片：{error:#}"));
                    }
                }
                Ok(None) => {}
                Err(error) => show_error(hwnd, &format!("文件选择失败：{error:#}")),
            }
        }
        WindowAction::Previous => navigate(context, loader, -1),
        WindowAction::Next => navigate(context, loader, 1),
        WindowAction::Fit => context.shared.set_view_mode(ViewMode::Fit),
        WindowAction::NativePixels => context.shared.set_view_mode(ViewMode::NativePixels),
        WindowAction::NavigationReleased { direction, at } => {
            if context.navigation.direction == Some(direction) {
                stop_navigation(context, loader);
                context.shared.record_navigation_release_at(at);
            }
        }
        WindowAction::StopNavigation => stop_navigation(context, loader),
    }
}

fn navigate(context: &mut WindowContext, loader: &ImageLoader, delta: isize) {
    if context.navigation.direction != Some(delta) {
        stop_navigation(context, loader);
        context.navigation.direction = Some(delta);
        context.navigation.progress_floor =
            Some(context.navigation.generation.wrapping_add(1).max(1));
    }
    if let Some(directory) = context.navigation.directory.as_mut() {
        if directory.step(delta) {
            let path = directory.current().to_owned();
            schedule_selection(context, loader, path);
        }
    } else if context.navigation.current_path.is_some() {
        context.navigation.pending_steps = context
            .navigation
            .pending_steps
            .saturating_add(delta as i32)
            .clamp(-10_000, 10_000);
    } else {
        let current = context.shared.current_snapshot().texture_index;
        context
            .shared
            .select_texture(if delta < 0 { 0 } else { 1 - current });
    }
}

fn stop_navigation(context: &mut WindowContext, loader: &ImageLoader) {
    context.navigation.direction = None;
    context.navigation.progress_floor = None;
    context.shared.end_navigation_progress();
    loader.end_navigation_progress();
}

fn schedule_selection(context: &mut WindowContext, loader: &ImageLoader, path: PathBuf) {
    if context.navigation.current_path.as_ref() != Some(&path) {
        context.shared.increment(Counter::NavigationTargetChanged);
    }
    context.navigation.generation = context.navigation.generation.wrapping_add(1).max(1);
    context.shared.cancel_pending_preloads();
    context.navigation.current_path = Some(path.clone());
    context.navigation.requested_at = Some(Instant::now());
    context.shared.set_navigation_generation(
        context.navigation.generation,
        context.navigation.progress_floor,
    );
    let preloads = context
        .navigation
        .directory
        .as_ref()
        .map(ImageDirectory::neighbors)
        .unwrap_or_default();
    loader.request_identity(
        context.navigation.generation,
        path,
        context.navigation.requested_at.unwrap_or_else(Instant::now),
        preloads,
        context.navigation.progress_floor,
    );
}

fn process_gpu_probe_reply(
    context: &mut WindowContext,
    loader: &ImageLoader,
    hwnd: windows::Win32::Foundation::HWND,
) {
    let Some(reply) = context.shared.take_gpu_probe_reply() else {
        return;
    };
    match reply {
        GpuProbeReply::Hit { generation, path } => {
            if generation == context.navigation.generation
                && context.navigation.current_path.as_deref() == Some(path.as_path())
            {
                context.navigation.fallback = None;
                set_window_title(hwnd, &path.display().to_string());
                loader.request_preloads(
                    generation,
                    context
                        .navigation
                        .directory
                        .as_ref()
                        .map(ImageDirectory::neighbors)
                        .unwrap_or_default(),
                );
            }
        }
        GpuProbeReply::LargeError {
            generation,
            message,
        } => {
            if context.shared.is_image_generation_current(generation) {
                context.shared.increment(Counter::ImageLoadFailed);
                show_non_modal_error(hwnd, &format!("大图加载失败：{message}"));
            }
        }
        GpuProbeReply::Miss(selection) => {
            if selection.generation == context.navigation.generation
                && context.navigation.current_path.as_deref() == Some(selection.path.as_path())
            {
                loader.request_decode(selection);
            }
        }
    }
}

fn process_loader_messages(
    context: &mut WindowContext,
    loader: &ImageLoader,
    hwnd: windows::Win32::Foundation::HWND,
) {
    while let Some(message) = loader.try_receive() {
        match message {
            LoaderMessage::IdentityResolved {
                generation,
                path,
                requested_at,
                preloads,
                result,
            } => {
                if generation != context.navigation.generation
                    || context.navigation.current_path.as_deref() != Some(path.as_path())
                {
                    continue;
                }
                match result {
                    Ok(identity) => context.shared.request_gpu_probe(ResolvedSelection {
                        generation,
                        path,
                        identity,
                        requested_at,
                        preloads,
                    }),
                    Err(error) => {
                        context.shared.increment(Counter::ImageLoadFailed);
                        restore_navigation(context, hwnd);
                        show_non_modal_error(hwnd, &format!("图片读取失败：{error}"));
                    }
                }
            }
            LoaderMessage::Current {
                generation,
                path,
                requested_at,
                result,
            } => {
                let target = generation == context.navigation.generation
                    && context.navigation.current_path.as_deref() == Some(path.as_path());
                let progress = !target
                    && context.navigation.direction.is_some()
                    && context.shared.can_present_image_generation(generation);
                if !target && !progress {
                    continue;
                }
                match result {
                    Ok((image, cpu_cache_hit)) => {
                        if context.navigation.pending_steps != 0 {
                            continue;
                        }
                        if target {
                            context.navigation.fallback = None;
                        }
                        context
                            .shared
                            .set_image(generation, image, requested_at, cpu_cache_hit);
                        if target {
                            set_window_title(hwnd, &path.display().to_string());
                        } else if let Some(target_path) = context.navigation.current_path.as_ref() {
                            set_window_title(
                                hwnd,
                                &format!(
                                    "{} | 正在加载：{}",
                                    path.display(),
                                    target_path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                ),
                            );
                        }
                    }
                    Err(error) => {
                        if !target {
                            continue;
                        }
                        if context.navigation.pending_steps != 0 {
                            continue;
                        }
                        context.shared.increment(Counter::ImageLoadFailed);
                        restore_navigation(context, hwnd);
                        show_non_modal_error(hwnd, &format!("图片加载失败：{error}"));
                    }
                }
            }
            LoaderMessage::Large {
                generation,
                path,
                requested_at,
            } => {
                if context.shared.is_image_generation_current(generation) {
                    context.navigation.fallback = None;
                    set_window_title(hwnd, "正在构建大图缓存；可切换图片或关闭窗口");
                    context.shared.request_large(crate::large::worker::Request {
                        generation,
                        path,
                        started: requested_at,
                    });
                }
            }
            LoaderMessage::Directory { generation, result } => {
                if generation != context.navigation.generation {
                    continue;
                }
                match result {
                    Ok(directory) => {
                        context.navigation.directory = Some(directory);
                        let steps = std::mem::take(&mut context.navigation.pending_steps);
                        if steps != 0 {
                            let Some(directory) = context.navigation.directory.as_mut() else {
                                continue;
                            };
                            for _ in 0..steps.unsigned_abs() {
                                if !directory.step(steps.signum() as isize) {
                                    break;
                                }
                            }
                            let path = directory.current().to_owned();
                            if context.navigation.current_path.as_deref() != Some(path.as_path()) {
                                schedule_selection(context, loader, path);
                                continue;
                            }
                            schedule_selection(context, loader, path);
                            continue;
                        }
                        if let Some(directory) = context.navigation.directory.as_ref() {
                            loader.request_preloads(generation, directory.neighbors());
                        }
                    }
                    Err(error) => show_non_modal_error(hwnd, &format!("图片目录扫描失败：{error}")),
                }
            }
            LoaderMessage::Preloaded { generation, image } => {
                if generation == context.navigation.generation {
                    context.shared.preload_image(image);
                }
            }
            LoaderMessage::Fatal(error) => {
                show_error(hwnd, &error);
                context.close_started = true;
                context.shared.request_shutdown();
            }
        }
    }
}

fn show_non_modal_error(hwnd: windows::Win32::Foundation::HWND, message: &str) {
    eprintln!("{message}");
    set_window_title(hwnd, message);
}

fn restore_navigation(context: &mut WindowContext, hwnd: windows::Win32::Foundation::HWND) {
    if let Some((path, directory)) = context.navigation.fallback.take() {
        context.navigation.generation = context.navigation.generation.wrapping_add(1).max(1);
        context
            .shared
            .set_image_generation(context.navigation.generation);
        context.navigation.current_path = path.clone();
        context.navigation.directory = directory;
        context.navigation.pending_steps = 0;
        context.navigation.requested_at = None;
        if let Some(path) = path {
            set_window_title(hwnd, &path.display().to_string());
        } else {
            set_window_title(hwnd, "");
        }
    }
}

fn choose_image(hwnd: windows::Win32::Foundation::HWND) -> Result<Option<PathBuf>> {
    let filter: Vec<u16> = "图片文件\0*.jpg;*.jpeg;*.png;*.bmp;*.gif\0所有文件\0*.*\0\0"
        .encode_utf16()
        .collect();
    let mut file = vec![0u16; 32_768];
    let title = w!("打开图片");
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: windows::core::PCWSTR(filter.as_ptr()),
        lpstrFile: windows::core::PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: title,
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: `dialog`、filter 和 file 均在模态对话框调用期间保持可写且有效；调用发生于 HWND 所属 UI 线程。
    if unsafe { GetOpenFileNameW(&mut dialog) }.as_bool() {
        let length = file
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(file.len());
        return Ok(Some(PathBuf::from(OsString::from_wide(&file[..length]))));
    }
    // SAFETY: 上一次 GetOpenFileNameW 已返回；该无参数 API 只读取当前线程的 Common Dialog 错误状态。
    let error = unsafe { CommDlgExtendedError() };
    if error.0 == 0 {
        Ok(None)
    } else {
        bail!("Windows Common Dialog 错误码：0x{:08X}", error.0)
    }
}

fn show_error(hwnd: windows::Win32::Foundation::HWND, message: &str) {
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    // SAFETY: 两个 UTF-16 字符串在同步 MessageBoxW 调用期间保持有效，owner HWND 仍由 UI 线程拥有。
    unsafe {
        let _ = MessageBoxW(
            Some(hwnd),
            windows::core::PCWSTR(text.as_ptr()),
            w!("Pixstride"),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn set_window_title(hwnd: windows::Win32::Foundation::HWND, title: &str) {
    let text: Vec<u16> = format!("Pixstride — {title}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: UTF-16 标题在同步调用期间保持有效，HWND 属于当前 UI 线程。
    unsafe {
        let _ = SetWindowTextW(hwnd, windows::core::PCWSTR(text.as_ptr()));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{
        CompletionEvent, CompletionSignal, Metrics, PendingState, RenderSnapshot, Shared,
        WakeEvent, run_render_worker,
    };
    use anyhow::{Context, Result};
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };
    use windows::Win32::{
        Foundation::WAIT_OBJECT_0,
        System::Threading::{CreateEventW, GetCurrentThreadId, WaitForSingleObject},
    };

    pub(crate) fn test_shared() -> Result<(Arc<Shared>, WakeEvent)> {
        // SAFETY: 测试独占此无名称 auto-reset event，并在同步状态断言结束后关闭；没有工作线程等待它。
        let event = WakeEvent(unsafe { CreateEventW(None, false, false, None)? });
        let shared = Arc::new(Shared {
            pending: Mutex::new(PendingState {
                snapshot: RenderSnapshot {
                    sequence: 0,
                    texture_index: 0,
                    width: 0,
                    height: 0,
                    requested_at: Instant::now(),
                    request_origin: super::RequestOrigin::InitialWindowSize,
                    view_mode: super::ViewMode::Fit,
                    zoom: 1.0,
                    pan: (0.0, 0.0),
                },
                image_generation: 0,
                progress_floor: None,
                accepted_image_generation: 0,
                redraw: false,
                shutdown: false,
                shutdown_started: None,
                image_update: None,
                large_request: None,
                clear_gpu_cache: false,
                gpu_probe: None,
                preload_uploads: VecDeque::new(),
                cancel_preloads: false,
            }),
            wake_event: event.0.0 as isize,
            metrics: Mutex::new(Metrics::default()),
            fatal_error: Mutex::new(None),
            gpu_probe_reply: Mutex::new(None),
            ui_ready_event: Mutex::new(0),
        });
        Ok((shared, event))
    }

    #[test]
    fn large_source_reset_and_center_zoom_do_not_reuse_an_old_pan() -> Result<()> {
        let (shared, _event) = test_shared()?;
        shared.set_image_generation(4);
        shared.pan_by(10.0, 20.0);
        shared.zoom_by(2.0);
        assert_eq!(shared.current_snapshot().pan, (20.0, 40.0));
        shared.request_large(crate::large::worker::Request {
            generation: 3,
            path: "old.png".into(),
            started: Instant::now(),
        });
        assert!(shared.take_pending().large_request.is_none());
        shared.request_large(crate::large::worker::Request {
            generation: 4,
            path: "new.png".into(),
            started: Instant::now(),
        });
        assert_eq!(shared.current_snapshot().pan, (0.0, 0.0));
        assert_eq!(shared.current_snapshot().zoom, 1.0);
        shared.set_image_generation(5);
        assert!(shared.take_pending().large_request.is_none());
        Ok(())
    }

    #[test]
    fn render_worker_panic_sets_shutdown_and_signals_ui_completion() -> Result<()> {
        let (shared, _wake_event) = test_shared()?;
        let completion = CompletionEvent::new()?;
        let signal = CompletionSignal {
            event_value: completion.0.0 as isize,
            // SAFETY: 测试当前线程同时模拟消息循环所有者，且其消息队列在测试期间存活。
            ui_thread_id: unsafe { GetCurrentThreadId() },
        };

        run_render_worker(&shared, || panic!("模拟渲染线程 panic"));
        drop(signal);

        // SAFETY: 测试独占完成事件，WaitForSingleObject 只等待已由 guard 置位的有效句柄，超时为零不会阻塞。
        assert_eq!(
            unsafe { WaitForSingleObject(completion.0, 0) },
            WAIT_OBJECT_0
        );
        let work = shared.take_pending();
        assert!(work.shutdown);
        assert_eq!(
            shared.fatal_error().as_deref(),
            Some("渲染线程发生 panic；资源已在栈展开时释放")
        );
        assert!(
            shared
                .metrics_report()
                .contains("关闭请求到渲染线程退出: n=1")
        );
        Ok(())
    }

    #[test]
    fn rapid_updates_keep_latest_snapshot_instead_of_queueing_old_states() -> Result<()> {
        let (shared, _event) = test_shared()?;
        shared.select_texture(1);
        shared.resize(1200, 800);
        shared.select_texture(0);

        let work = shared.take_pending();
        assert_eq!(work.snapshot.sequence, 3);
        assert_eq!(work.snapshot.texture_index, 0);
        assert_eq!(work.snapshot.request_origin, super::RequestOrigin::KeyInput);
        assert_eq!((work.snapshot.width, work.snapshot.height), (1200, 800));
        assert!(work.redraw);
        assert!(!work.shutdown);
        Ok(())
    }

    #[test]
    fn resize_origin_distinguishes_initial_size_minimize_and_restore() -> Result<()> {
        let (shared, _event) = test_shared()?;
        shared.resize(800, 600);
        assert_eq!(
            shared.current_snapshot().request_origin,
            super::RequestOrigin::InitialWindowSize
        );

        shared.resize(900, 650);
        assert_eq!(
            shared.current_snapshot().request_origin,
            super::RequestOrigin::Resize
        );

        shared.resize(0, 0);
        assert_eq!(
            shared.current_snapshot().request_origin,
            super::RequestOrigin::Minimize
        );

        shared.resize(900, 650);
        assert_eq!(
            shared.current_snapshot().request_origin,
            super::RequestOrigin::Restore
        );
        Ok(())
    }

    #[test]
    fn shutdown_is_terminal_for_later_state_updates() -> Result<()> {
        let (shared, _event) = test_shared()?;
        shared.request_shutdown();
        shared.select_texture(1);

        let work = shared.take_pending();
        assert_eq!(work.snapshot.sequence, 0);
        assert_eq!(work.snapshot.texture_index, 0);
        assert!(!work.redraw);
        assert!(work.shutdown);
        Ok(())
    }

    #[test]
    fn cancel_preloads_wakes_renderer_without_requesting_redraw() -> Result<()> {
        let (shared, _event) = test_shared()?;
        shared.cancel_pending_preloads();

        let work = shared.take_pending();
        assert!(!work.redraw);
        assert!(!work.shutdown);
        assert!(work.preloads.is_empty());
        assert!(work.cancel_preloads);
        assert!(!shared.take_pending().cancel_preloads);
        Ok(())
    }

    #[test]
    fn hold_progress_is_monotonic_and_does_not_change_the_target() -> Result<()> {
        let (shared, _event) = test_shared()?;
        shared.set_navigation_generation(8, Some(3));
        assert!(shared.claim_image_generation(4));
        assert_eq!(shared.pending().image_generation, 8);
        assert!(!shared.can_present_image_generation(3));
        assert!(shared.claim_image_generation(7));
        assert!(!shared.claim_image_generation(6));
        assert!(!shared.claim_image_generation(9));
        assert!(shared.can_present_image_generation(8));
        Ok(())
    }

    #[test]
    fn release_reverse_and_newer_gpu_hit_reject_old_progress() -> Result<()> {
        let (shared, _event) = test_shared()?;
        shared.set_navigation_generation(8, Some(3));
        shared.end_navigation_progress();
        assert!(!shared.can_present_image_generation(7));
        assert!(shared.can_present_image_generation(8));
        shared.set_navigation_generation(10, Some(9));
        assert!(!shared.can_present_image_generation(8));
        assert!(shared.claim_image_generation(10));
        assert!(!shared.claim_image_generation(9));
        shared.request_shutdown();
        assert!(!shared.can_present_image_generation(10));
        Ok(())
    }

    #[test]
    fn release_discards_pending_progress_but_keeps_the_final_target() -> Result<()> {
        let (shared, _event) = test_shared()?;
        let image = Arc::new(crate::image::DecodedImage {
            source: crate::image::SourceIdentity::read(std::path::Path::new("Cargo.toml"))?,
            width: 1,
            height: 1,
            format: crate::image::PixelFormat::Bgra8Premultiplied,
            stride: 4,
            pixels: vec![0; 4],
        });
        shared.set_navigation_generation(8, Some(3));
        shared.set_image(5, Arc::clone(&image), Instant::now(), false);
        assert!(shared.pending().image_update.is_some());
        shared.end_navigation_progress();
        assert!(shared.take_pending().image.is_none());
        shared.set_image(8, image, Instant::now(), false);
        shared.end_navigation_progress();
        assert_eq!(shared.take_pending().image.unwrap().0, 8);
        Ok(())
    }

    #[test]
    fn navigation_reversal_and_opposite_key_release_keep_the_latest_direction() -> Result<()> {
        use crate::image::{ImageBudget, navigation::ImageDirectory, worker::ImageLoader};
        let (shared, _event) = test_shared()?;
        let mut loader = ImageLoader::start(ImageBudget::default(), Arc::clone(&shared))?;
        // 只验证状态转换；不消费身份线程返回的不存在文件错误，不需要创建 HWND 或 GPU。
        let files: Vec<_> = (0..3)
            .map(|index| std::path::PathBuf::from(format!("navigation-{index}.bmp")))
            .collect();
        let mut context = super::WindowContext {
            shared: Arc::clone(&shared),
            close_started: false,
            drag_position: None,
            action: None,
            navigation: super::UiNavigation {
                generation: 10,
                current_path: Some(files[0].clone()),
                directory: Some(ImageDirectory { files, selected: 0 }),
                ..Default::default()
            },
        };
        super::navigate(&mut context, &loader, 1);
        super::navigate(&mut context, &loader, 1);
        assert_eq!(context.navigation.generation, 12);
        assert_eq!(context.navigation.progress_floor, Some(11));
        assert!(shared.can_present_image_generation(11));
        super::navigate(&mut context, &loader, -1);
        assert_eq!(context.navigation.generation, 13);
        assert_eq!(context.navigation.progress_floor, Some(13));
        assert!(!shared.can_present_image_generation(12));
        context.navigation_key_released(1, Instant::now());
        assert!(
            context.action.is_none(),
            "先前方向的松键不得结束新的反向导航"
        );
        assert_eq!(context.navigation.direction, Some(-1));
        context.navigation_key_released(-1, Instant::now());
        assert!(shared.pending().progress_floor.is_none());
        super::process_window_action(&mut context, &loader, Default::default());
        assert!(context.navigation.direction.is_none());
        assert!(shared.can_present_image_generation(13));
        loader.request_stop();
        loader.join()?;
        Ok(())
    }

    #[test]
    fn budget_rejection_skips_repeated_wic_work_but_rechecks_a_changed_file() -> Result<()> {
        use crate::image::{
            ImageBudget, SourceIdentity,
            worker::{ImageLoader, LoaderMessage},
        };
        use std::io::Write;
        let root = std::env::var_os("PIXSTRIDE_TEST_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("pixstride-rejection-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let path = root.join("small.bmp");
        let mut bmp = vec![0u8; 62];
        bmp[..2].copy_from_slice(b"BM");
        bmp[2..6].copy_from_slice(&62u32.to_le_bytes());
        bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&2i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&1i32.to_le_bytes());
        bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
        bmp[28..30].copy_from_slice(&32u16.to_le_bytes());
        std::fs::write(&path, bmp)?;
        let (shared, _event) = test_shared()?;
        let mut loader = ImageLoader::start(
            ImageBudget {
                max_decoded_bytes: 4,
                ..ImageBudget::default()
            },
            Arc::clone(&shared),
        )?;
        let result = (|| -> Result<()> {
            for generation in 1..=3 {
                if generation == 3 {
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(&path)?
                        .write_all(&[0])?;
                }
                shared.set_image_generation(generation);
                loader.request_identity(generation, path.clone(), Instant::now(), Vec::new(), None);
                loader.request_decode(super::ResolvedSelection {
                    generation,
                    path: path.clone(),
                    identity: SourceIdentity::read(&path)?,
                    requested_at: Instant::now(),
                    preloads: Vec::new(),
                });
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    if Instant::now() > deadline {
                        anyhow::bail!("预算拒绝测试超时");
                    }
                    // SAFETY: loader 在循环及其后台线程结束前独占事件，等待不会关闭或转移句柄。
                    unsafe {
                        let _ = WaitForSingleObject(loader.ready_handle(), 100);
                    }
                    let mut finished = false;
                    while let Some(message) = loader.try_receive() {
                        if let LoaderMessage::Current {
                            generation: received,
                            result,
                            ..
                        } = message
                        {
                            assert_eq!(received, generation);
                            assert!(result.unwrap_err().contains("超过当前单图 CPU 预算"));
                            finished = true;
                        }
                    }
                    if finished {
                        break;
                    }
                }
            }
            assert_eq!(
                shared
                    .metrics
                    .lock()
                    .unwrap()
                    .counter(crate::diagnostics::Counter::BudgetRejectionHit),
                1
            );
            assert!(shared.metrics_report().contains("WIC GetFrame: n=2"));
            Ok(())
        })();
        loader.request_stop();
        loader.join()?;
        std::fs::remove_dir_all(root)?;
        result
    }

    #[test]
    fn worker_shutdown_timeout_reaches_zero_after_grace_period() {
        let started = Instant::now() - Duration::from_secs(3);
        assert_eq!(super::remaining_worker_shutdown_ms(started), 0);
    }

    #[test]
    fn stale_gpu_probe_is_replaced_and_cannot_publish_a_later_image() -> Result<()> {
        let (shared, _event) = test_shared()?;
        let identity = crate::image::SourceIdentity::read(std::path::Path::new("Cargo.toml"))?;
        let make_selection = |generation| super::ResolvedSelection {
            generation,
            path: identity.path.clone(),
            identity: identity.clone(),
            requested_at: Instant::now(),
            preloads: Vec::new(),
        };
        shared.set_image_generation(1);
        shared.request_gpu_probe(make_selection(1));
        shared.set_image_generation(2);
        shared.request_gpu_probe(make_selection(1));
        assert!(shared.take_pending().gpu_probe.is_none());
        shared.request_gpu_probe(make_selection(2));
        assert_eq!(shared.take_pending().gpu_probe.unwrap().generation, 2);
        let image = Arc::new(crate::image::DecodedImage {
            source: identity,
            width: 1,
            height: 1,
            format: crate::image::PixelFormat::Bgra8Premultiplied,
            stride: 4,
            pixels: vec![0; 4],
        });
        shared.set_image(1, Arc::clone(&image), Instant::now(), false);
        assert!(shared.take_pending().image.is_none());
        shared.set_image(2, image, Instant::now(), false);
        assert_eq!(shared.take_pending().image.unwrap().0, 2);
        Ok(())
    }

    pub(super) fn pump(
        context: &mut super::WindowContext,
        loader: &crate::image::worker::ImageLoader,
        hwnd: windows::Win32::Foundation::HWND,
        until: impl Fn() -> bool,
    ) -> Result<()> {
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !until() {
            if Instant::now() >= deadline {
                anyhow::bail!("诊断等待超时");
            }
            // SAFETY: 当前测试持有 ImageLoader，等待的事件在测试收尾 join 后才关闭；20 ms 超时只用于检查诊断期限。
            unsafe {
                let _ = WaitForSingleObject(loader.ready_handle(), 20);
            }
            let mut message = MSG::default();
            // SAFETY: 测试线程拥有 HWND 并独占其消息队列；只分发当前线程取出的有效消息。
            unsafe {
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            super::process_loader_messages(context, loader, hwnd);
            super::process_gpu_probe_reply(context, loader, hwnd);
        }
        Ok(())
    }

    /// 通过注入慢 CopyPixels 工作模拟长按，验证持续进度、反向和松键；不声称完成实际键盘或视觉验收。
    #[test]
    #[ignore = "需要 HWND 和 D3D11，显式运行 Release 长按诊断"]
    fn release_hold_navigation_probe() -> Result<()> {
        hold_navigation_probe(false)
    }

    #[test]
    #[ignore = "Release GIF 长按进度、反向与松键诊断，需要 HWND 和 D3D11"]
    fn release_gif_hold_navigation_probe() -> Result<()> {
        hold_navigation_probe(true)
    }

    fn hold_navigation_probe(gif: bool) -> Result<()> {
        use crate::{
            diagnostics::Counter,
            image::{ImageBudget, worker::ImageLoader},
            window,
        };
        // 仅诊断构建可禁用进度区间，复现此前“所有过期结果丢弃”的策略作为同负载参照。
        let strict_reference = std::env::var_os("PIXSTRIDE_TEST_STRICT_GENERATION").is_some();
        let root = std::env::var_os("PIXSTRIDE_TEST_ROOT")
            .map(std::path::PathBuf::from)
            .context("设置 PIXSTRIDE_TEST_ROOT 指向仓库外诊断目录")?
            .join(format!("hold-probe-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let extension = if gif { "gif" } else { "bmp" };
        for index in 0..40u8 {
            if gif {
                std::fs::write(
                    root.join(format!("{index:02}.gif")),
                    crate::image::gif_tests::generated([64, 64], [0, 0, 64, 64], true, true, false),
                )?;
                continue;
            }
            let size = 54 + 64 * 64 * 4;
            let mut bmp = vec![index.saturating_mul(5); size];
            bmp[..54].fill(0);
            bmp[..2].copy_from_slice(b"BM");
            bmp[2..6].copy_from_slice(&(size as u32).to_le_bytes());
            bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
            bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
            bmp[18..22].copy_from_slice(&64i32.to_le_bytes());
            bmp[22..26].copy_from_slice(&64i32.to_le_bytes());
            bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
            bmp[28..30].copy_from_slice(&32u16.to_le_bytes());
            std::fs::write(root.join(format!("{index:02}.bmp")), bmp)?;
        }
        let (shared, _event) = test_shared()?;
        let mut loader = ImageLoader::start(ImageBudget::default(), Arc::clone(&shared))?;
        shared.set_ui_ready_event(loader.ready_handle());
        let mut context = Box::new(super::WindowContext {
            shared: Arc::clone(&shared),
            close_started: false,
            drag_position: None,
            action: None,
            navigation: super::UiNavigation::default(),
        });
        let hwnd = window::create_main_window(&mut context)?;
        window::show_main_window(hwnd);
        let render_shared = Arc::clone(&shared);
        let raw_hwnd = hwnd.0 as isize;
        let render_thread =
            std::thread::spawn(move || crate::renderer::render_thread(raw_hwnd, render_shared));
        let count = |counter| shared.metrics.lock().unwrap().counter(counter);
        let result = (|| -> Result<()> {
            super::request_open(
                &mut context,
                &loader,
                hwnd,
                root.join(format!("00.{extension}")),
            )?;
            pump(&mut context, &loader, hwnd, || {
                count(Counter::ImageSelectionPresented) > 0
            })?;
            loader.set_copy_delay_for_test(Duration::from_millis(80));
            for direction in [1, -1] {
                context.navigation.direction = Some(direction);
                context.navigation.progress_floor =
                    (!strict_reference).then_some(context.navigation.generation + 1);
                let progress_before = count(Counter::NavigationProgressPresented);
                for index in 1..=30 {
                    let file_index = if direction == 1 { index } else { 39 - index };
                    let target = root.join(format!("{file_index:02}.{extension}"));
                    super::schedule_selection(&mut context, &loader, target.clone());
                    let deadline = Instant::now() + Duration::from_millis(20);
                    pump(&mut context, &loader, hwnd, || Instant::now() >= deadline)?;
                    assert_eq!(
                        context.navigation.current_path.as_ref(),
                        Some(&target),
                        "进度帧不得修改目标路径"
                    );
                }
                if strict_reference {
                    assert_eq!(count(Counter::NavigationProgressPresented), progress_before);
                } else {
                    assert!(
                        count(Counter::NavigationProgressPresented) > progress_before,
                        "慢解码期间必须显示同方向进度，不能把所有已完成画面丢弃"
                    );
                }
                let target_generation = context.navigation.generation;
                context.navigation_key_released(direction, Instant::now());
                super::process_window_action(&mut context, &loader, hwnd);
                assert!(shared.pending().progress_floor.is_none());
                assert!(!shared.can_present_image_generation(target_generation - 1));
                pump(&mut context, &loader, hwnd, || {
                    shared.metrics.lock().unwrap().presented_generation() == target_generation
                })?;
                assert_eq!(context.navigation.generation, target_generation);
            }
            Ok(())
        })();
        shared.request_shutdown();
        loader.request_stop();
        let render_result = render_thread
            .join()
            .map_err(|_| anyhow::anyhow!("诊断渲染线程 panic"))?;
        loader.join()?;
        // SAFETY: 两个后台文件线程与渲染线程均已退出，当前 UI 线程仍拥有 HWND 和 WindowContext。
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd)?;
        }
        println!(
            "strict_generation_reference={strict_reference}\n{}",
            shared.metrics_report()
        );
        std::fs::remove_dir_all(root)?;
        result?;
        render_result
    }

    /// 使用自行生成的 BMP 和真实 D3D11 验证命中路径；不替代桌面人工验收或显示延迟测量。
    #[test]
    #[ignore = "需要可创建 HWND 的 Windows 会话和 D3D11；手动运行 Release 诊断"]
    fn release_navigation_cache_probe() -> Result<()> {
        use crate::{
            diagnostics::Counter,
            image::{ImageBudget, worker::ImageLoader},
            window,
        };
        use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

        let root = std::env::var_os("PIXSTRIDE_TEST_ROOT")
            .map(std::path::PathBuf::from)
            .context("设置 PIXSTRIDE_TEST_ROOT 指向仓库外诊断目录")?
            .join(format!("cache-probe-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        for index in 0..3u8 {
            let size = 54 + 256 * 256 * 4;
            let mut bmp = vec![index.saturating_mul(80); size];
            bmp[..54].fill(0);
            bmp[..2].copy_from_slice(b"BM");
            bmp[2..6].copy_from_slice(&(size as u32).to_le_bytes());
            bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
            bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
            bmp[18..22].copy_from_slice(&256i32.to_le_bytes());
            bmp[22..26].copy_from_slice(&256i32.to_le_bytes());
            bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
            bmp[28..30].copy_from_slice(&32u16.to_le_bytes());
            std::fs::write(root.join(format!("{index}.bmp")), bmp)?;
        }
        let (shared, _event) = test_shared()?;
        let mut loader = ImageLoader::start(ImageBudget::default(), Arc::clone(&shared))?;
        shared.set_ui_ready_event(loader.ready_handle());
        let mut context = Box::new(super::WindowContext {
            shared: Arc::clone(&shared),
            close_started: false,
            drag_position: None,
            action: None,
            navigation: super::UiNavigation::default(),
        });
        let hwnd = window::create_main_window(&mut context)?;
        window::show_main_window(hwnd);
        let render_shared = Arc::clone(&shared);
        let raw_hwnd = hwnd.0 as isize;
        let render_thread =
            std::thread::spawn(move || crate::renderer::render_thread(raw_hwnd, render_shared));
        let count = |counter| shared.metrics.lock().unwrap().counter(counter);
        let result = (|| -> Result<()> {
            super::request_open(&mut context, &loader, hwnd, root.join("0.bmp"))?;
            pump(&mut context, &loader, hwnd, || {
                count(Counter::ImageSelectionPresented) >= 1
            })?;
            loader.request_preloads(
                context.navigation.generation,
                vec![root.join("1.bmp"), root.join("2.bmp")],
            );
            pump(&mut context, &loader, hwnd, || {
                count(Counter::GpuPreloadMiss) >= 2
            })?;
            let decodes_before = count(Counter::CpuCacheMiss);
            let presents_before = count(Counter::ImageSelectionPresented);
            let (gate_started, gate_release) = loader.hold_decode_for_test();
            gate_started.recv_timeout(Duration::from_secs(2))?;
            for index in 0..100 {
                let target = root.join(format!("{}.bmp", 1 + index % 2));
                super::schedule_selection(&mut context, &loader, target);
                pump(&mut context, &loader, hwnd, || {
                    count(Counter::ImageSelectionPresented) > presents_before + index
                })?;
                if index == 4 {
                    gate_release.send(())?;
                }
            }
            assert_eq!(
                count(Counter::CpuCacheMiss),
                decodes_before,
                "GPU 命中不应进入 WIC"
            );
            assert!(count(Counter::GpuCurrentHit) >= 100);
            // 不等待中间画面，模拟快速往返后停在最后一张；只能最终提交最新 Generation。
            for index in 0..200 {
                super::schedule_selection(
                    &mut context,
                    &loader,
                    root.join(format!("{}.bmp", index % 3)),
                );
            }
            let target_generation = context.navigation.generation;
            pump(&mut context, &loader, hwnd, || {
                shared.metrics.lock().unwrap().presented_generation() == target_generation
            })?;
            assert!(shared.is_image_generation_current(target_generation));
            // 修改实际像素并追加一个尾字节；长度变化使修订确定更新，不依赖文件时间戳的精度。
            use std::io::{Seek, SeekFrom, Write};
            let misses_before = count(Counter::CpuCacheMiss);
            let mut changed_file = std::fs::OpenOptions::new()
                .write(true)
                .open(root.join("1.bmp"))?;
            changed_file.seek(SeekFrom::Start(54))?;
            changed_file.write_all(&[123])?;
            drop(changed_file);
            std::fs::OpenOptions::new()
                .append(true)
                .open(root.join("1.bmp"))?
                .write_all(&[0])?;
            super::schedule_selection(&mut context, &loader, root.join("1.bmp"));
            let changed_generation = context.navigation.generation;
            pump(&mut context, &loader, hwnd, || {
                shared.metrics.lock().unwrap().presented_generation() == changed_generation
            })?;
            assert_eq!(count(Counter::CpuCacheMiss), misses_before + 1);
            // 失败必须只是非模态状态；删除文件后仍能继续处理下一次有效选择。
            std::fs::remove_file(root.join("1.bmp"))?;
            let failures_before = count(Counter::ImageLoadFailed);
            super::schedule_selection(&mut context, &loader, root.join("1.bmp"));
            pump(&mut context, &loader, hwnd, || {
                count(Counter::ImageLoadFailed) > failures_before
            })?;
            super::schedule_selection(&mut context, &loader, root.join("0.bmp"));
            let final_generation = context.navigation.generation;
            pump(&mut context, &loader, hwnd, || {
                shared.metrics.lock().unwrap().presented_generation() == final_generation
            })?;
            // 只在测试构建中主动淘汰非当前 GPU 条目，验证 CPU 命中后重传纹理的完整路径。
            let cpu_hits_before = count(Counter::CpuCurrentHit);
            for index in 0..100 {
                shared.pending().clear_gpu_cache = true;
                super::schedule_selection(
                    &mut context,
                    &loader,
                    root.join(format!("{}.bmp", if index % 2 == 0 { 2 } else { 0 })),
                );
                let generation = context.navigation.generation;
                pump(&mut context, &loader, hwnd, || {
                    shared.metrics.lock().unwrap().presented_generation() == generation
                })?;
            }
            assert_eq!(count(Counter::CpuCurrentHit), cpu_hits_before + 100);
            // 真实 WIC 失败也必须留下关联分项，且不能弹框阻止后续有效请求。
            let failures_before = count(Counter::ImageLoadFailed);
            std::fs::write(root.join("broken.jpg"), [0xff, 0xd8])?;
            super::schedule_selection(&mut context, &loader, root.join("broken.jpg"));
            pump(&mut context, &loader, hwnd, || {
                count(Counter::ImageLoadFailed) > failures_before
            })?;
            let report = shared.metrics_report();
            assert!(report.contains("file=\"broken.jpg\""));
            assert!(report.contains("outcome=error:"));
            super::schedule_selection(&mut context, &loader, root.join("0.bmp"));
            let recovered_generation = context.navigation.generation;
            pump(&mut context, &loader, hwnd, || {
                shared.metrics.lock().unwrap().presented_generation() == recovered_generation
            })?;
            // 先保证两个目标均已上传，随后用确定的暂停点检验忙 Worker 不能阻挡新 GPU 命中。
            for name in ["2.bmp", "0.bmp"] {
                super::schedule_selection(&mut context, &loader, root.join(name));
                let generation = context.navigation.generation;
                pump(&mut context, &loader, hwnd, || {
                    shared.metrics.lock().unwrap().presented_generation() == generation
                })?;
            }
            for action in ["gpu-hit", "release", "reverse", "focus-loss", "open-file"] {
                let slow_path = root.join(format!("slow-{action}.bmp"));
                std::fs::copy(root.join("0.bmp"), &slow_path)?;
                context.navigation.directory = Some(crate::image::navigation::ImageDirectory {
                    files: vec![root.join("0.bmp"), slow_path.clone(), root.join("2.bmp")],
                    selected: 1,
                });
                context.navigation.direction = Some(1);
                context.navigation.progress_floor = Some(context.navigation.generation + 1);
                let cancelled_before = count(Counter::DecodeCancelled);
                let hits_before = count(Counter::GpuCurrentHit);
                let (copy_started, copy_release) = loader.hold_copy_for_test();
                super::schedule_selection(&mut context, &loader, slow_path);
                let old_generation = context.navigation.generation;
                pump(&mut context, &loader, hwnd, || {
                    copy_started.try_recv().is_ok()
                })?;
                if action == "reverse" {
                    super::navigate(&mut context, &loader, -1);
                } else if action == "open-file" {
                    super::request_open(&mut context, &loader, hwnd, root.join("2.bmp"))?;
                    assert!(context.navigation.direction.is_none());
                } else {
                    super::schedule_selection(&mut context, &loader, root.join("0.bmp"));
                    if action == "release" || action == "focus-loss" {
                        use windows::Win32::UI::{
                            Input::KeyboardAndMouse::VK_RIGHT,
                            WindowsAndMessaging::{SendMessageW, WM_KEYUP, WM_KILLFOCUS},
                        };
                        let (message, key) = if action == "release" {
                            (WM_KEYUP, usize::from(VK_RIGHT.0))
                        } else {
                            (WM_KILLFOCUS, 0)
                        };
                        // SAFETY: 在 HWND 所属测试线程同步注入无指针消息，WindowContext 活到 DestroyWindow 之后。
                        unsafe {
                            SendMessageW(
                                hwnd,
                                message,
                                Some(windows::Win32::Foundation::WPARAM(key)),
                                Some(Default::default()),
                            );
                        }
                        assert!(!shared.can_present_image_generation(old_generation));
                        super::process_window_action(&mut context, &loader, hwnd);
                        assert!(context.navigation.direction.is_none());
                    }
                }
                let final_generation = context.navigation.generation;
                let final_path = context.navigation.current_path.clone();
                pump(&mut context, &loader, hwnd, || {
                    shared.metrics.lock().unwrap().presented_generation() == final_generation
                })?;
                assert!(
                    count(Counter::GpuCurrentHit) > hits_before,
                    "{action}: WIC 尚未放行，新 GPU 目标应已完成"
                );
                assert!(!shared.can_present_image_generation(old_generation));
                copy_release.send(())?;
                pump(&mut context, &loader, hwnd, || {
                    count(Counter::DecodeCancelled) > cancelled_before
                })?;
                assert_eq!(context.navigation.current_path, final_path);
                assert_eq!(
                    shared.metrics.lock().unwrap().presented_generation(),
                    final_generation,
                    "{action}: 旧解码结束后不得覆盖新图片"
                );
                println!(
                    "in-flight boundary passed: {action}, old={old_generation}, final={final_generation}"
                );
                super::stop_navigation(&mut context, &loader);
            }
            Ok(())
        })();
        shared.request_shutdown();
        loader.request_stop();
        let render_result = render_thread
            .join()
            .map_err(|_| anyhow::anyhow!("诊断渲染线程 panic"))?;
        loader.join()?;
        // SAFETY: 渲染线程和文件线程已 join，HWND 所属测试线程仍持有 WindowContext。
        unsafe {
            DestroyWindow(hwnd)?;
        }
        println!("{}", shared.metrics_report());
        result?;
        render_result?;
        Ok(())
    }
    /// 在真实 HWND/D3D11 上验证合成宽图、层级、普通图片切换和关闭，不代替人工像素观感验收。
    #[test]
    #[ignore = "C3 Release 集成诊断，需要 HWND 和独立 PIXSTRIDE_TILE_CACHE"]
    fn release_large_viewer_probe() -> Result<()> {
        use crate::{
            diagnostics::Counter,
            image::{ImageBudget, worker::ImageLoader},
        };
        use crate::{
            diagnostics::Metric,
            large::stream::tests::{generate, root},
        };
        use std::path::PathBuf;
        let root = root("viewer");
        let cache_root = std::env::var_os("PIXSTRIDE_TILE_CACHE")
            .map(PathBuf::from)
            .context("设置专用 PIXSTRIDE_TILE_CACHE")?;
        let wide = root.join("wide.png");
        generate(&wide, 20000, 65)?;
        let small = root.join("ordinary.png");
        generate(&small, 37, 19)?;
        let slow = root.join("cancel.png");
        generate(&slow, 20000, 1025)?;
        let (shared, _event) = test_shared()?;
        let mut loader = ImageLoader::start(ImageBudget::default(), Arc::clone(&shared))?;
        shared.set_ui_ready_event(loader.ready_handle());
        let mut context = Box::new(super::WindowContext {
            shared: Arc::clone(&shared),
            close_started: false,
            drag_position: None,
            action: None,
            navigation: super::UiNavigation::default(),
        });
        let hwnd = crate::window::create_main_window(&mut context)?;
        crate::window::show_main_window(hwnd);
        let rs = Arc::clone(&shared);
        let raw = hwnd.0 as isize;
        let thread = std::thread::spawn(move || crate::renderer::render_thread(raw, rs));
        let n = |m| shared.metrics.lock().unwrap().sample_count(m);
        let result = (|| -> Result<()> {
            super::request_open(&mut context, &loader, hwnd, small.clone())?;
            let generation = context.navigation.generation;
            pump(&mut context, &loader, hwnd, || {
                shared.metrics.lock().unwrap().presented_generation() == generation
            })?;
            let misses = shared
                .metrics
                .lock()
                .unwrap()
                .counter(Counter::CpuCurrentMiss);
            super::request_open(&mut context, &loader, hwnd, wide.clone())?;
            pump(&mut context, &loader, hwnd, || n(Metric::LargePreview) == 1)?;
            pump(&mut context, &loader, hwnd, || n(Metric::TileViewport) >= 1)?;
            let before = n(Metric::TileViewport);
            shared.set_view_mode(super::ViewMode::NativePixels);
            pump(&mut context, &loader, hwnd, || {
                n(Metric::TileViewport) > before
            })?;
            for pan in [9300.0, -18600.0, 9300.0] {
                let before = n(Metric::TileViewport);
                shared.pan_by(pan, 0.0);
                pump(&mut context, &loader, hwnd, || {
                    n(Metric::TileViewport) > before
                })?;
            }
            let before = n(Metric::TileViewport);
            shared.zoom_by(0.25);
            pump(&mut context, &loader, hwnd, || {
                n(Metric::TileViewport) > before
            })?;
            shared.resize(0, 0);
            shared.resize(900, 600);
            let before = n(Metric::LargePreview);
            super::request_open(&mut context, &loader, hwnd, small.clone())?;
            let generation = context.navigation.generation;
            pump(&mut context, &loader, hwnd, || {
                shared.metrics.lock().unwrap().presented_generation() == generation
            })?;
            super::request_open(&mut context, &loader, hwnd, wide.clone())?;
            pump(&mut context, &loader, hwnd, || {
                n(Metric::LargePreview) > before
            })?;
            // 在新源构建已有半成品时切回普通 GPU 缓存；大图 Builder 不得占据 WIC Worker。
            super::request_open(&mut context, &loader, hwnd, slow.clone())?;
            pump(&mut context, &loader, hwnd, || {
                std::fs::read_dir(&cache_root).is_ok_and(|r| {
                    r.filter_map(|e| e.ok())
                        .any(|e| e.file_name().to_string_lossy().starts_with(".build-"))
                })
            })?;
            super::request_open(&mut context, &loader, hwnd, small.clone())?;
            let generation = context.navigation.generation;
            pump(&mut context, &loader, hwnd, || {
                shared.metrics.lock().unwrap().presented_generation() == generation
            })?;
            assert!(
                shared
                    .metrics
                    .lock()
                    .unwrap()
                    .counter(Counter::GpuCurrentHit)
                    > 0
            );
            assert!(
                shared
                    .metrics
                    .lock()
                    .unwrap()
                    .counter(Counter::CpuCurrentMiss)
                    <= misses + 3,
                "大图不应运行完整 WIC 解码"
            );
            // 再次启动构建并在其开始后关闭，验证取消与渲染线程收尾。
            super::request_open(&mut context, &loader, hwnd, slow)?;
            pump(&mut context, &loader, hwnd, || {
                std::fs::read_dir(&cache_root).is_ok_and(|r| {
                    r.filter_map(|e| e.ok())
                        .any(|e| e.file_name().to_string_lossy().starts_with(".build-"))
                })
            })?;
            Ok(())
        })();
        shared.request_shutdown();
        loader.request_stop();
        let render_result = thread
            .join()
            .map_err(|_| anyhow::anyhow!("渲染线程 panic"))?;
        loader.join()?;
        // SAFETY: 所有使用 HWND 的线程已 join；当前线程仍持有窗口上下文。
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd)?;
        }
        println!("{}", shared.metrics_report());
        result?;
        render_result?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    /// 只用合成 PNG，关联多 Tile 读取与视口完成；可在测试进程中注入慢读。
    #[test]
    #[ignore = "Release 合成视口分项诊断，需要 HWND 和独立缓存根目录"]
    fn release_tile_viewport_profile() -> Result<()> {
        use crate::{
            diagnostics::Metric,
            image::{ImageBudget, worker::ImageLoader},
            large::{
                stream::tests::{generate, root},
                worker::Request,
            },
        };
        let root = root("viewport-profile");
        let image = root.join("synthetic.png");
        generate(&image, 8193, 4097)?;
        let (shared, _event) = test_shared()?;
        let mut loader = ImageLoader::start(ImageBudget::default(), Arc::clone(&shared))?;
        shared.set_ui_ready_event(loader.ready_handle());
        let mut context = Box::new(super::WindowContext {
            shared: Arc::clone(&shared),
            close_started: false,
            drag_position: None,
            action: None,
            navigation: super::UiNavigation::default(),
        });
        let hwnd = crate::window::create_main_window(&mut context)?;
        crate::window::show_main_window(hwnd);
        let rs = Arc::clone(&shared);
        let raw = hwnd.0 as isize;
        let thread = std::thread::spawn(move || crate::renderer::render_thread(raw, rs));
        let n = |m| shared.metrics.lock().unwrap().sample_count(m);
        let result = (|| -> Result<()> {
            shared.set_image_generation(1);
            shared.request_large(Request {
                generation: 1,
                path: image,
                started: Instant::now(),
            });
            pump(&mut context, &loader, hwnd, || n(Metric::TileViewport) > 0)?;
            shared.set_view_mode(super::ViewMode::NativePixels);
            for round in 0..2 {
                let reads = n(Metric::TileRead);
                let uploads = n(Metric::TileUpload);
                for pan in [
                    (-3000.0, -1000.0),
                    (6000.0, 0.0),
                    (0.0, 2000.0),
                    (-6000.0, 0.0),
                    (3000.0, -1000.0),
                ] {
                    let before = n(Metric::TileViewport);
                    shared.pan_by(pan.0, pan.1);
                    pump(&mut context, &loader, hwnd, || {
                        n(Metric::TileViewport) > before
                    })?;
                    println!(
                        "VIEWSTEP round={round} pan={pan:?} total_us={:?}",
                        shared
                            .metrics
                            .lock()
                            .unwrap()
                            .last_sample_us(Metric::TileViewport)
                    );
                }
                if round == 1 {
                    assert_eq!(n(Metric::TileRead), reads, "GPU 复用不应再次读取 Tile");
                    assert_eq!(n(Metric::TileUpload), uploads, "GPU 复用不应再次上传 Tile");
                }
            }
            Ok(())
        })();
        shared.request_shutdown();
        loader.request_stop();
        let render = thread
            .join()
            .map_err(|_| anyhow::anyhow!("渲染线程 panic"))?;
        loader.join()?;
        // SAFETY: Render/Loader 已退出，HWND 与 WindowContext 仍由当前 UI 测试线程持有。
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd)?;
        }
        println!("{}", shared.metrics_report());
        result?;
        render?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    /// C4 已发布缓存的真实 Viewer 路径；路径由本地环境传入，不触碰源图片内容。
    #[test]
    #[ignore = "C4 真实缓存 Viewer 验证，需要明确批准"]
    fn real_large_viewer_probe() -> Result<()> {
        use crate::{
            diagnostics::Metric,
            image::{ImageBudget, worker::ImageLoader},
        };
        anyhow::ensure!(
            std::env::var("PIXSTRIDE_APPROVED_LARGE_CACHE").as_deref() == Ok("1"),
            "缺少真实大图测试批准"
        );
        let source = std::env::var_os("PIXSTRIDE_STRESS_IMAGE")
            .map(std::path::PathBuf::from)
            .context("设置 PIXSTRIDE_STRESS_IMAGE")?;
        crate::large::cache::ensure_local(&source)?;
        let (shared, _event) = test_shared()?;
        let mut loader = ImageLoader::start(ImageBudget::default(), Arc::clone(&shared))?;
        shared.set_ui_ready_event(loader.ready_handle());
        let mut context = Box::new(super::WindowContext {
            shared: Arc::clone(&shared),
            close_started: false,
            drag_position: None,
            action: None,
            navigation: super::UiNavigation::default(),
        });
        let hwnd = crate::window::create_main_window(&mut context)?;
        crate::window::show_main_window(hwnd);
        let rs = Arc::clone(&shared);
        let raw = hwnd.0 as isize;
        let thread = std::thread::spawn(move || crate::renderer::render_thread(raw, rs));
        let n = |m| shared.metrics.lock().unwrap().sample_count(m);
        let result = (|| -> Result<()> {
            // C3 已验证路由；真实样本诊断直接发布大图请求，避免扫描私人目录或预加载其他私人图片。
            context.navigation.generation = 1;
            context.navigation.current_path = Some(source.clone());
            shared.set_image_generation(1);
            shared.request_large(crate::large::worker::Request {
                generation: 1,
                path: source,
                started: Instant::now(),
            });
            pump(&mut context, &loader, hwnd, || n(Metric::LargePreview) > 0)?;
            pump(&mut context, &loader, hwnd, || n(Metric::TileViewport) > 0)?;
            let before = n(Metric::TileViewport);
            shared.set_view_mode(super::ViewMode::NativePixels);
            pump(&mut context, &loader, hwnd, || {
                n(Metric::TileViewport) > before
            })?;
            let mut previous = (0.0, 0.0);
            for _ in 0..2 {
                for y in [-15500.0, -7750.0, 0.0, 7750.0, 15500.0] {
                    for x in [-23300.0, -11650.0, 0.0, 11650.0, 23300.0] {
                        let before = n(Metric::TileViewport);
                        shared.pan_by(x - previous.0, y - previous.1);
                        previous = (x, y);
                        pump(&mut context, &loader, hwnd, || {
                            n(Metric::TileViewport) > before
                        })?;
                    }
                }
            }
            let before = n(Metric::TileViewport);
            shared.set_view_mode(super::ViewMode::Fit);
            pump(&mut context, &loader, hwnd, || {
                n(Metric::TileViewport) > before
            })?;
            // GPU 完成且窗口不变时，不得由 Tile Worker 不断请求重绘。
            let presents = n(Metric::Present);
            let deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < deadline {
                // SAFETY: 测试持有 ready event；短等待仅观察空闲，不替代正式事件驱动循环。
                unsafe {
                    WaitForSingleObject(loader.ready_handle(), 25);
                }
            }
            assert_eq!(n(Metric::Present), presents, "静止后不应持续 Present");
            Ok(())
        })();
        let shutdown = Instant::now();
        shared.request_shutdown();
        loader.request_stop();
        let render_result = thread.join().map_err(|_| anyhow::anyhow!("Render panic"))?;
        loader.join()?;
        // SAFETY: 当前测试线程拥有 HWND，所有渲染资源使用者已退出，上下文仍然有效。
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd)?;
        }
        println!("REAL viewer shutdown_us={}", shutdown.elapsed().as_micros());
        println!("{}", shared.metrics_report());
        result?;
        render_result?;
        Ok(())
    }
}
