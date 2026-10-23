//! Render Thread 独占 Tile SRV、Shader 与常量缓冲；文件读取全部交给独立 Worker。

use crate::{
    app::{GpuProbeReply, RenderSnapshot, Shared},
    diagnostics::Metric,
    large::{
        cache::{DiskCache, TilePixels},
        view::{self, Piece},
        worker::{Reply, Request, Worker},
    },
};
use anyhow::{Context, Result};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3, IDXGIDevice,
};
use windows::Win32::Graphics::{
    Direct3D11::*,
    Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
};
use windows::core::Interface;

const GPU_BUDGET: u64 = 128 * 1024 * 1024;
const TILE_VS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tile_vs.cso"));
struct Scene {
    generation: u64,
    cache: Arc<DiskCache>,
    preview: usize,
    textures: HashMap<usize, (ID3D11ShaderResourceView, u64)>,
    lru: VecDeque<usize>,
    bytes: u64,
    pieces: Vec<Piece>,
    sequence: Option<u64>,
    view_started: Instant,
    view_reported: bool,
    first: Option<Instant>,
    upload_error_reported: bool,
    trace: ViewTrace,
}

/// 同一 Snapshot 的阶段汇总；总和不是关键路径长度，因为 Worker 与 Render Thread 可重叠。
#[derive(Default)]
struct ViewTrace {
    missing: usize,
    cpu_hits: usize,
    uploads: usize,
    file_read: Duration,
    crc: Duration,
    allocation: Duration,
    upload: Duration,
    reply_max: Duration,
    queue_max: Duration,
    backpressure: Duration,
    dispatch: Duration,
}
/// 延迟到首次大图才创建，不给普通图片启动路径增加线程或 GPU 对象。
pub(super) struct Tiles {
    worker: Worker,
    scene: Option<Scene>,
    shader: ID3D11VertexShader,
    constants: ID3D11Buffer,
    peak: u64,
    adapter: Option<IDXGIAdapter3>,
    video_peak: Option<u64>,
    video_budget: Option<u64>,
}
impl Tiles {
    pub(super) fn new(device: &ID3D11Device, shared: &Arc<Shared>) -> Result<Self> {
        let mut shader = None;
        // SAFETY: 构建时编译的静态 DXBC 在调用期间有效，返回 COM 由 Render Thread 持有。
        unsafe { device.CreateVertexShader(TILE_VS, None, Some(&mut shader)) }?;
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: 32,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let mut constants = None;
        // SAFETY: 常量缓冲长度为 16 字节倍数，输出槽由当前栈帧持有；无初始数据指针。
        unsafe { device.CreateBuffer(&desc, None, Some(&mut constants)) }?;
        let adapter = device
            .cast::<IDXGIDevice>()
            .ok()
            .and_then(|d| {
                // SAFETY: Device 是当前线程持有的有效 COM 接口；返回 Adapter 引用独立计数。
                unsafe { d.GetAdapter() }.ok()
            })
            .and_then(|a| a.cast::<IDXGIAdapter3>().ok());
        Ok(Self {
            worker: Worker::start(shared)?,
            scene: None,
            shader: shader.context("缺少 Tile Shader")?,
            constants: constants.context("缺少常量缓冲")?,
            peak: 0,
            adapter,
            video_peak: None,
            video_budget: None,
        })
    }
    pub(super) fn request(&self, request: Request) -> Result<()> {
        self.worker.build(request)
    }
    pub(super) fn clear(&mut self) {
        self.scene = None;
    }
    pub(super) fn active(&self) -> bool {
        self.scene.is_some()
    }

    #[cfg(test)]
    pub(super) fn complete(&self) -> bool {
        self.scene.as_ref().is_some_and(|s| {
            !s.pieces.is_empty() && s.pieces.iter().all(|p| s.textures.contains_key(&p.id))
        })
    }
    pub(super) fn pump(
        &mut self,
        device: &ID3D11Device,
        shared: &Shared,
        snapshot: &RenderSnapshot,
    ) -> Result<Option<u64>> {
        let mut snapshot = *snapshot;
        let mut changed = None;
        for reply in self.worker.drain() {
            match reply {
                Reply::Ready {
                    generation,
                    cache,
                    preview,
                    started,
                } if shared.is_image_generation_current(generation) => {
                    let srv = match upload(device, &preview, shared) {
                        Ok(srv) => srv,
                        Err(error) => {
                            report_upload_error(device, shared, generation, error)?;
                            continue;
                        }
                    };
                    let bytes = preview.bytes.len() as u64;
                    let size = (cache.plan.size.width, cache.plan.size.height);
                    let Some(view) = shared.activate_view(generation, size) else {
                        continue;
                    };
                    snapshot = view;
                    let mut textures = HashMap::new();
                    textures.insert(preview.id, (srv, bytes));
                    self.scene = Some(Scene {
                        generation,
                        cache,
                        preview: preview.id,
                        textures,
                        lru: VecDeque::new(),
                        bytes,
                        pieces: Vec::new(),
                        sequence: None,
                        view_started: Instant::now(),
                        view_reported: false,
                        first: Some(started),
                        upload_error_reported: false,
                        trace: ViewTrace::default(),
                    });
                    shared.claim_image_generation(generation);
                    changed = Some(generation);
                }
                Reply::Tile {
                    generation,
                    pixels,
                    sequence,
                    ready_at,
                    read,
                    cpu_hit,
                    queue,
                    backpressure,
                } if shared.is_image_generation_current(generation) => {
                    shared.record(Metric::TileReplyToRender, ready_at.elapsed());
                    if let Some(scene) = self.scene.as_mut().filter(|s| s.generation == generation)
                    {
                        // 旧视口分块仍可进入同一图片的缓存；当前视口只统计其自身所需数据。
                        if scene.sequence == Some(sequence) {
                            scene.trace.cpu_hits += usize::from(cpu_hit);
                            scene.trace.file_read += read.file_read;
                            scene.trace.crc += read.crc;
                            scene.trace.allocation += read.allocation;
                            scene.trace.reply_max = scene.trace.reply_max.max(ready_at.elapsed());
                            scene.trace.queue_max = scene.trace.queue_max.max(queue);
                            scene.trace.backpressure += backpressure;
                        }
                        if !scene.textures.contains_key(&pixels.id) {
                            let bytes = pixels.bytes.len() as u64;
                            let mut candidates = scene.lru.len();
                            while scene.bytes + bytes > GPU_BUDGET && candidates > 0 {
                                candidates -= 1;
                                let Some(id) = scene.lru.pop_front() else {
                                    break;
                                };
                                if id != scene.preview && !scene.pieces.iter().any(|p| p.id == id) {
                                    if let Some((_, cost)) = scene.textures.remove(&id) {
                                        scene.bytes -= cost;
                                    }
                                } else {
                                    scene.lru.push_back(id);
                                }
                            }
                            if scene.bytes + bytes > GPU_BUDGET {
                                if !scene.upload_error_reported {
                                    shared.publish_gpu_probe_reply(GpuProbeReply::LargeError {
                                        generation,
                                        message: "Tile GPU 预算不足，保留低分辨率预览".into(),
                                    });
                                    scene.upload_error_reported = true;
                                }
                                continue;
                            }
                            let at = Instant::now();
                            let srv = match upload(device, &pixels, shared) {
                                Ok(srv) => srv,
                                Err(error) => {
                                    if !scene.upload_error_reported {
                                        report_upload_error(device, shared, generation, error)?;
                                        scene.upload_error_reported = true;
                                    }
                                    continue;
                                }
                            };
                            if scene.sequence == Some(sequence) {
                                scene.trace.upload += at.elapsed();
                                scene.trace.uploads += 1;
                            }
                            scene.textures.insert(pixels.id, (srv, bytes));
                            scene.bytes += bytes;
                            scene.lru.push_back(pixels.id);
                        }
                        changed = Some(generation);
                    }
                }
                Reply::Error {
                    generation,
                    message,
                } if shared.is_image_generation_current(generation) => shared
                    .publish_gpu_probe_reply(GpuProbeReply::LargeError {
                        generation,
                        message,
                    }),
                _ => {}
            }
        }
        if let Some(scene) = self.scene.as_mut() {
            if scene.bytes > self.peak {
                self.peak = scene.bytes;
                if let Some(adapter) = &self.adapter {
                    let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                    // SAFETY: Adapter3 和输出结构在调用期间有效；查询当前进程本地显存，不改变资源预算。
                    if unsafe {
                        adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info)
                    }
                    .is_ok()
                    {
                        self.video_peak = Some(self.video_peak.unwrap_or(0).max(info.CurrentUsage));
                        self.video_budget = Some(info.Budget);
                    }
                }
            }
            if scene.sequence != Some(snapshot.sequence)
                && shared.is_image_generation_current(scene.generation)
            {
                scene.pieces = view::pieces(&scene.cache.plan, &snapshot, scene.preview);
                scene.sequence = Some(snapshot.sequence);
                scene.view_started = snapshot.requested_at;
                scene.view_reported = false;
                let ids: Vec<_> = scene
                    .pieces
                    .iter()
                    .filter(|p| !scene.textures.contains_key(&p.id))
                    .map(|p| p.id)
                    .collect();
                scene.trace = ViewTrace {
                    missing: ids.len(),
                    dispatch: snapshot.requested_at.elapsed(),
                    ..Default::default()
                };
                self.worker.view(
                    scene.generation,
                    snapshot.sequence,
                    Arc::clone(&scene.cache),
                    ids,
                );
                shared.record(Metric::TileViewDispatch, snapshot.requested_at.elapsed());
            }
        }
        Ok(changed)
    }
    pub(super) fn draw(&self, context: &ID3D11DeviceContext, snapshot: &RenderSnapshot) {
        let Some(scene) = &self.scene else {
            return;
        };
        let viewport = D3D11_VIEWPORT {
            Width: snapshot.width as f32,
            Height: snapshot.height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
            ..Default::default()
        };
        // SAFETY: 所有 COM 接口均由当前 Render Thread 持有；常量缓冲绑定切片在调用期间有效。
        unsafe {
            context.RSSetViewports(Some(&[viewport]));
            context.VSSetShader(&self.shader, None);
            context.VSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
        }
        for piece in &scene.pieces {
            let (srv, uv) = if let Some((srv, _)) = scene.textures.get(&piece.id) {
                let t = &scene.cache.plan.tiles[piece.id];
                (
                    srv,
                    [
                        1.0 / (t.width + 2) as f32,
                        1.0 / (t.height + 2) as f32,
                        (t.width + 1) as f32 / (t.width + 2) as f32,
                        (t.height + 1) as f32 / (t.height + 2) as f32,
                    ],
                )
            } else {
                let Some((srv, _)) = scene.textures.get(&scene.preview) else {
                    continue;
                };
                (srv, piece.preview_uv)
            };
            let constants = [piece.rect, uv];
            // SAFETY: constants 是恰好 32 字节连续 f32 数组，UpdateSubresource 在返回前复制；缓冲同为 32 字节。
            // 每块逻辑区域只画一次，预览只填补缺块，不在透明像素后重复混合另一幅图片。
            unsafe {
                context.UpdateSubresource(
                    &self.constants,
                    0,
                    None,
                    constants.as_ptr().cast(),
                    0,
                    0,
                );
                context.PSSetShaderResources(0, Some(&[Some(srv.clone())]));
                context.Draw(6, 0);
            }
        }
    }
    pub(super) fn presented(&mut self, shared: &Shared) {
        if let Some(s) = self.scene.as_mut() {
            if let Some(at) = s.first.take() {
                shared.record(Metric::LargePreview, at.elapsed());
            }
            if !s.view_reported && s.pieces.iter().all(|p| s.textures.contains_key(&p.id)) {
                let elapsed = s.view_started.elapsed();
                shared.record(Metric::TileViewport, elapsed);
                if elapsed >= Duration::from_millis(100) {
                    shared.record_tile_view(format!(
                        concat!(
                            "generation={} sequence={:?} total={} visible={} missing={} cpu_hits={} ",
                            "uploads={} dispatch={} file_read_sum={} crc_sum={} alloc_sum={} ",
                            "upload_sum={} reply_max={} queue_max={} backpressure_sum={}"
                        ),
                        s.generation, s.sequence, elapsed.as_micros(), s.pieces.len(),
                        s.trace.missing, s.trace.cpu_hits, s.trace.uploads,
                        s.trace.dispatch.as_micros(), s.trace.file_read.as_micros(),
                        s.trace.crc.as_micros(), s.trace.allocation.as_micros(),
                        s.trace.upload.as_micros(), s.trace.reply_max.as_micros(),
                        s.trace.queue_max.as_micros(), s.trace.backpressure.as_micros(),
                    ));
                }
                s.view_reported = true;
            }
        }
    }
}
impl Drop for Tiles {
    fn drop(&mut self) {
        println!(
            "Tile GPU cache peak={} bytes; budget={} bytes; CPU resident budget=67108864 bytes plus bounded staging",
            self.peak, GPU_BUDGET
        );
        println!(
            "DXGI process-local video usage sampled_peak={:?} budget={:?} bytes; includes ordinary textures, swap chain and driver allocations; not Tile-only",
            self.video_peak, self.video_budget
        );
    }
}
/// 正常设备的分配失败保留粗预览；设备确实移除时沿用 Renderer 的错误退出流程。
fn report_upload_error(
    device: &ID3D11Device,
    shared: &Shared,
    generation: u64,
    error: anyhow::Error,
) -> Result<()> {
    // SAFETY: 当前 Render Thread 仍持有 Device；查询移除状态不接收裸缓冲，也不改变资源。
    unsafe { device.GetDeviceRemovedReason() }
        .with_context(|| format!("Tile 上传失败且设备已移除：{error:#}"))?;
    shared.publish_gpu_probe_reply(GpuProbeReply::LargeError {
        generation,
        message: format!("Tile 上传失败，保留已显示内容：{error:#}"),
    });
    Ok(())
}

fn upload(
    device: &ID3D11Device,
    pixels: &TilePixels,
    shared: &Shared,
) -> Result<ID3D11ShaderResourceView> {
    let at = Instant::now();
    let desc = D3D11_TEXTURE2D_DESC {
        Width: pixels.width,
        Height: pixels.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        ..Default::default()
    };
    let data = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.bytes.as_ptr().cast(),
        SysMemPitch: pixels.width * 4,
        SysMemSlicePitch: 0,
    };
    let mut texture = None;
    // SAFETY: TilePixels 由经过长度/CRC 校验的 DiskCache 创建，拥有完整 width*height*4 数据；调用同步复制到 GPU。
    unsafe { device.CreateTexture2D(&desc, Some(&data), Some(&mut texture)) }?;
    let texture: ID3D11Texture2D = texture.context("未返回 Tile Texture")?;
    let mut srv = None;
    // SAFETY: 当前线程持有 texture；默认 SRV 覆盖该单层 BGRA 纹理，输出槽有效。
    unsafe { device.CreateShaderResourceView(&texture, None, Some(&mut srv)) }?;
    shared.record(Metric::TileUpload, at.elapsed());
    srv.context("未返回 Tile SRV")
}
