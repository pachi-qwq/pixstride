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
    time::Instant,
};
use windows::Win32::Graphics::{
    Direct3D11::*,
    Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
};

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
}
/// 延迟到首次大图才创建，不给普通图片启动路径增加线程或 GPU 对象。
pub(super) struct Tiles {
    worker: Worker,
    scene: Option<Scene>,
    shader: ID3D11VertexShader,
    constants: ID3D11Buffer,
    peak: u64,
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
        Ok(Self {
            worker: Worker::start(shared)?,
            scene: None,
            shader: shader.context("缺少 Tile Shader")?,
            constants: constants.context("缺少常量缓冲")?,
            peak: 0,
        })
    }
    pub(super) fn request(&self, request: Request) {
        self.worker.build(request);
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
        let mut changed = None;
        for reply in self.worker.drain() {
            match reply {
                Reply::Ready {
                    generation,
                    cache,
                    preview,
                    started,
                } if shared.is_image_generation_current(generation) => {
                    let srv = upload(device, &preview, shared)?;
                    let bytes = preview.bytes.len() as u64;
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
                    });
                    shared.claim_image_generation(generation);
                    changed = Some(generation);
                }
                Reply::Tile { generation, pixels }
                    if shared.is_image_generation_current(generation) =>
                {
                    if let Some(scene) = self.scene.as_mut().filter(|s| s.generation == generation)
                    {
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
                            anyhow::ensure!(
                                scene.bytes + bytes <= GPU_BUDGET,
                                "Tile GPU 缓存预算不足"
                            );
                            scene
                                .textures
                                .insert(pixels.id, (upload(device, &pixels, shared)?, bytes));
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
            self.peak = self.peak.max(scene.bytes);
            if scene.sequence != Some(snapshot.sequence)
                && shared.is_image_generation_current(scene.generation)
            {
                scene.pieces = view::pieces(&scene.cache.plan, snapshot, scene.preview);
                scene.sequence = Some(snapshot.sequence);
                scene.view_started = snapshot.requested_at;
                scene.view_reported = false;
                let ids = scene
                    .pieces
                    .iter()
                    .filter(|p| !scene.textures.contains_key(&p.id))
                    .map(|p| p.id)
                    .collect();
                self.worker.view(
                    scene.generation,
                    snapshot.sequence,
                    Arc::clone(&scene.cache),
                    ids,
                );
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
                shared.record(Metric::TileViewport, s.view_started.elapsed());
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
    }
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
