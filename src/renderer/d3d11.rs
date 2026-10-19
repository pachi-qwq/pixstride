//! 在专属线程中创建 D3D11 设备、Swap Chain 和测试纹理，并按需绘制。

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result, anyhow, bail};
use windows::{
    Win32::{
        Foundation::{HANDLE, HWND},
        Foundation::{WAIT_EVENT, WAIT_OBJECT_0},
        Graphics::{
            Direct3D::{
                D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL,
                D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
                D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
            },
            Direct3D11::{
                D3D11_BIND_SHADER_RESOURCE, D3D11_BLEND_DESC, D3D11_BLEND_INV_SRC_ALPHA,
                D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_COLOR_WRITE_ENABLE_ALL,
                D3D11_COMPARISON_NEVER, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_RASTERIZER_DESC,
                D3D11_RENDER_TARGET_BLEND_DESC, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA,
                D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE2D_DESC, D3D11_VIEWPORT,
                D3D11CreateDevice, ID3D11BlendState, ID3D11Device, ID3D11DeviceContext,
                ID3D11PixelShader, ID3D11RasterizerState, ID3D11RenderTargetView,
                ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
            },
            Dxgi::{
                Common::{
                    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_UNKNOWN,
                    DXGI_SAMPLE_DESC,
                },
                DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
                DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT_FLIP_DISCARD,
                IDXGIAdapter, IDXGIFactory2, IDXGISwapChain2,
            },
        },
        System::Threading::{INFINITE, WaitForMultipleObjects},
    },
    core::{Error, Interface},
};

use crate::{
    app::{RenderSnapshot, RequestOrigin, Shared, ViewMode},
    diagnostics::Metric,
    image::{DecodedImage, ImageBudget, PixelFormat, SourceIdentity},
};

const TEST_TEXTURE_SIZE: u32 = 256;
const WAIT_HANDLES: usize = 2;
const VERTEX_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/test_vs.cso"));
const PIXEL_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/test_ps.cso"));

pub(crate) fn render_thread(hwnd_value: isize, shared: Arc<Shared>) -> Result<()> {
    // 线程边界上只传递 HWND 的整数表示；UI 收到渲染线程完成消息之前一直持有并保持原窗口有效。
    let hwnd = HWND(hwnd_value as _);
    let render_thread_started = Instant::now();
    let init_started = Instant::now();
    let mut renderer =
        Renderer::new(hwnd, &shared, render_thread_started).context("初始化 D3D11 渲染器失败")?;
    let init_elapsed = init_started.elapsed();
    shared.record(Metric::D3dInitialization, init_elapsed);
    println!("D3D11 initialization: {} us", init_elapsed.as_micros());
    let result = renderer.run(&shared);
    shared.set_gpu_cache_bytes(0);
    result
}

struct Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap_chain: IDXGISwapChain2,
    frame_latency_handle: HANDLE,
    render_target: Option<ID3D11RenderTargetView>,
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    rasterizer: ID3D11RasterizerState,
    textures: [ID3D11ShaderResourceView; 2],
    image_cache: HashMap<SourceIdentity, (ID3D11ShaderResourceView, u64)>,
    image_lru: VecDeque<SourceIdentity>,
    pending_preloads: VecDeque<Arc<DecodedImage>>,
    current_image: Option<SourceIdentity>,
    current_dimensions: (u32, u32),
    gpu_cache_bytes: u64,
    gpu_cache_budget: u64,
    width: u32,
    height: u32,
    first_present_reported: bool,
    first_image_present_reported: bool,
    pending_selection_present: Option<(Instant, bool)>,
    render_thread_started: Instant,
}

impl Renderer {
    fn new(hwnd: HWND, shared: &Shared, render_thread_started: Instant) -> Result<Self> {
        let device_started = Instant::now();
        let (device, feature_level, context, driver_type) = create_device()?;
        shared.record(Metric::DeviceCreation, device_started.elapsed());

        let dxgi_device = device.cast::<windows::Win32::Graphics::Dxgi::IDXGIDevice>()?;
        // SAFETY: `dxgi_device` 是通过 `cast` 获取并由当前作用域持有的 COM 接口；GetAdapter 返回独立持有的 Adapter 引用。
        let adapter = unsafe { dxgi_device.GetAdapter() }.context("获取 DXGI Adapter 失败")?;
        let adapter_name = adapter_name(&adapter)?;
        println!(
            "D3D11 device ready: driver={}, feature_level=0x{:04X}, adapter={}",
            driver_type_name(driver_type),
            feature_level.0,
            adapter_name
        );
        let factory: IDXGIFactory2 =
            // SAFETY: 调用期间 `adapter` 保持有效；windows-rs 会检查接口类型并构造其 COM 引用。
            unsafe { adapter.GetParent() }.context("获取 DXGI Factory2 失败")?;

        let swap_chain_started = Instant::now();
        let swap_chain_desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: 0,
            Height: 0,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: windows::Win32::Graphics::Dxgi::DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
        };

        let swap_chain1 =
            // SAFETY: UI 线程在渲染线程发送完成通知前保持 HWND 有效；两个单采样缓冲区满足 Flip Model 要求。
            unsafe { factory.CreateSwapChainForHwnd(&device, hwnd, &swap_chain_desc, None, None) }
                .context("创建 DXGI Flip Model Swap Chain 失败")?;
        let swap_chain: IDXGISwapChain2 = swap_chain1.cast()?;
        // SAFETY: `swap_chain` 是当前线程持有的有效 IDXGISwapChain2；渲染开始前设置帧延迟符合接口约束。
        unsafe { swap_chain.SetMaximumFrameLatency(1) }
            .context("设置 Swap Chain 最大帧延迟失败")?;
        // SAFETY: 帧延迟句柄由 DXGI 管理；只要 `swap_chain` 存活，该句柄就有效。
        let frame_latency_handle = unsafe { swap_chain.GetFrameLatencyWaitableObject() };
        if frame_latency_handle.0.is_null() {
            bail!("DXGI 未提供 Frame Latency Waitable Object");
        }
        shared.record(Metric::SwapChainCreation, swap_chain_started.elapsed());

        let shader_started = Instant::now();
        let mut vertex_shader = None;
        // SAFETY: 内嵌 DXBC 字节码由本 crate 的 HLSL 在构建时生成；输出槽是当前栈帧持有的有效空 Option。
        unsafe { device.CreateVertexShader(VERTEX_SHADER, None, Some(&mut vertex_shader)) }
            .context("创建 Vertex Shader 失败")?;
        let vertex_shader = vertex_shader.context("D3D11 未返回 Vertex Shader")?;
        let mut pixel_shader = None;
        // SAFETY: 内嵌 DXBC 字节码和输出槽满足 CreatePixelShader 的指针及生命周期要求；生成的 COM 接口立即由 Renderer 持有。
        unsafe { device.CreatePixelShader(PIXEL_SHADER, None, Some(&mut pixel_shader)) }
            .context("创建 Pixel Shader 失败")?;
        let pixel_shader = pixel_shader.context("D3D11 未返回 Pixel Shader")?;
        let sampler = create_sampler(&device)?;
        let blend = create_alpha_blend(&device)?;
        let rasterizer = create_rasterizer(&device)?;
        shared.record(Metric::ShaderInitialization, shader_started.elapsed());

        let texture_upload_started = Instant::now();
        let textures = [
            create_test_texture(&device, TestPattern::Checkerboard)?,
            create_test_texture(&device, TestPattern::Gradient)?,
        ];
        shared.record(Metric::TextureUpload, texture_upload_started.elapsed());

        let mut renderer = Self {
            device,
            context,
            swap_chain,
            frame_latency_handle,
            render_target: None,
            vertex_shader,
            pixel_shader,
            sampler,
            blend,
            rasterizer,
            textures,
            image_cache: HashMap::new(),
            image_lru: VecDeque::new(),
            pending_preloads: VecDeque::new(),
            current_image: None,
            current_dimensions: (TEST_TEXTURE_SIZE, TEST_TEXTURE_SIZE),
            gpu_cache_bytes: 0,
            gpu_cache_budget: ImageBudget::default().gpu_cache_bytes,
            width: 0,
            height: 0,
            first_present_reported: false,
            first_image_present_reported: false,
            pending_selection_present: None,
            render_thread_started,
        };

        let snapshot = shared.current_snapshot();
        if snapshot.width > 0 && snapshot.height > 0 {
            renderer.resize_resources(snapshot.width, snapshot.height, false, shared)?;
        }
        Ok(renderer)
    }

    fn run(&mut self, shared: &Shared) -> Result<()> {
        let mut snapshot = shared.current_snapshot();
        let mut needs_render = snapshot.width > 0 && snapshot.height > 0;

        loop {
            let wait_result = self.wait_for_work(shared, needs_render)?;
            if wait_result == WaitResult::Work {
                let work = shared.take_pending();
                if work.shutdown {
                    break;
                }
                if work.cancel_preloads {
                    self.pending_preloads.clear();
                }
                if work.redraw {
                    let delay = work.snapshot.requested_at.elapsed();
                    shared.record(Metric::RequestToRender, delay);
                    let metric = match work.snapshot.request_origin {
                        RequestOrigin::InitialWindowSize => Metric::RequestToRenderInitial,
                        RequestOrigin::KeyInput => Metric::RequestToRenderInput,
                        RequestOrigin::Resize => Metric::RequestToRenderResize,
                        RequestOrigin::Minimize => Metric::RequestToRenderMinimize,
                        RequestOrigin::Restore => Metric::RequestToRenderRestore,
                        RequestOrigin::Paint => Metric::RequestToRenderPaint,
                        RequestOrigin::ImageReady => Metric::ImageRequestToRender,
                    };
                    shared.record(metric, delay);
                }
                if let Some(image) = work.image {
                    // 当前请求变更后，旧选择的 GPU 预加载不再有价值。
                    self.pending_preloads.clear();
                    self.pending_selection_present = Some((
                        work.snapshot.requested_at,
                        self.image_cache.contains_key(&image.source),
                    ));
                    self.upload_image(image, true, shared)?;
                }
                for image in work.preloads {
                    if !self
                        .pending_preloads
                        .iter()
                        .any(|queued| queued.source == image.source)
                    {
                        if self.pending_preloads.len() == 2 {
                            self.pending_preloads.pop_front();
                        }
                        self.pending_preloads.push_back(image);
                    }
                }
                snapshot = work.snapshot;

                if snapshot.width > 0
                    && snapshot.height > 0
                    && (snapshot.width != self.width || snapshot.height != self.height)
                {
                    self.resize_resources(snapshot.width, snapshot.height, true, shared)?;
                }
                needs_render =
                    snapshot.width > 0 && snapshot.height > 0 && (needs_render || work.redraw);
                if !needs_render {
                    self.upload_one_preload(shared)?;
                }
                continue;
            }

            if needs_render {
                self.draw(&snapshot, shared)?;
                needs_render = false;
            }
            self.upload_one_preload(shared)?;
        }

        // SAFETY: Immediate Context 只有当前 Render Thread 一个所有者；退出时先 ClearState 释放命令引用，再释放 Render Target。
        unsafe {
            self.context.ClearState();
            self.context.Flush();
        }
        self.render_target.take();
        Ok(())
    }

    fn wait_for_work(&self, shared: &Shared, needs_render: bool) -> Result<WaitResult> {
        let wake = HANDLE(shared.wake_event() as _);
        let (handles, count) = if needs_render && self.render_target.is_some() {
            ([wake, self.frame_latency_handle], WAIT_HANDLES as u32)
        } else {
            ([wake, HANDLE::default()], 1)
        };

        // SAFETY: 两个句柄在调用期间都有效。唤醒事件在本线程 join 后才关闭；帧延迟句柄由 DXGI 所有，且 Swap Chain 活得更久。
        let result = unsafe { WaitForMultipleObjects(&handles[..count as usize], false, INFINITE) };
        if result == WAIT_OBJECT_0 {
            Ok(WaitResult::Work)
        } else if count == WAIT_HANDLES as u32 && result == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
            Ok(WaitResult::FrameReady)
        } else {
            Err(Error::from_thread()).context("等待渲染事件或帧就绪失败")
        }
    }

    fn resize_resources(
        &mut self,
        width: u32,
        height: u32,
        record_resize: bool,
        shared: &Shared,
    ) -> Result<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        let started = Instant::now();
        // SAFETY: 当前 Render Thread 独占 Immediate Context。调用 ResizeBuffers 前清除并提交上下文，解除旧 Back Buffer 的绑定。
        unsafe {
            self.context.ClearState();
            self.context.Flush();
        }
        self.render_target.take();

        if self.width != 0 || self.height != 0 {
            // SAFETY: 调用前已释放旧 Render Target 并清空 Immediate Context，解除旧 Back Buffer 引用。
            // SAFETY: Swap Chain 只由当前 Render Thread 使用，尺寸来自仍有效窗口的客户区状态。
            unsafe {
                self.swap_chain.ResizeBuffers(
                    0,
                    width,
                    height,
                    DXGI_FORMAT_UNKNOWN,
                    DXGI_SWAP_CHAIN_FLAG(DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0),
                )
            }
            .context("调整 DXGI Swap Chain 缓冲区失败")?;
        }

        let back_buffer: ID3D11Texture2D =
            // SAFETY: Swap Chain 由当前线程持有，索引 0 位于双缓冲范围内；windows-rs 检查并包装返回的 COM 接口。
            unsafe { self.swap_chain.GetBuffer(0) }.context("获取 Swap Chain Back Buffer 失败")?;
        let mut render_target = None;
        // SAFETY: `back_buffer` 是有效的 Swap Chain 纹理，输出槽在调用期间有效；返回的 COM View 保留到下次 Resize 或关闭。
        unsafe {
            self.device
                .CreateRenderTargetView(&back_buffer, None, Some(&mut render_target))
        }
        .context("创建 Render Target View 失败")?;
        self.render_target = Some(render_target.context("D3D11 未返回 Render Target View")?);
        self.width = width;
        self.height = height;
        // SAFETY: 单元素 Viewport 切片在调用期间保持有效，Immediate Context 只由当前 Render Thread 使用。
        unsafe {
            self.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
        }
        if record_resize {
            shared.record(Metric::Resize, started.elapsed());
        }
        Ok(())
    }

    fn draw(&mut self, snapshot: &RenderSnapshot, shared: &Shared) -> Result<()> {
        if snapshot.width == 0 || snapshot.height == 0 {
            return Ok(());
        }
        let Some(render_target) = self.render_target.as_ref() else {
            bail!("渲染目标尚未创建");
        };

        let started = Instant::now();
        let color = if snapshot.texture_index == 0 {
            [0.035, 0.045, 0.065, 1.0]
        } else {
            [0.015, 0.025, 0.04, 1.0]
        };
        let (srv, image_width, image_height) = if let Some(key) = self.current_image.as_ref() {
            let (srv, _) = self
                .image_cache
                .get(key)
                .context("当前图片纹理不在 GPU 缓存中")?;
            (
                srv.clone(),
                self.current_dimensions.0,
                self.current_dimensions.1,
            )
        } else {
            (
                self.textures[snapshot.texture_index].clone(),
                TEST_TEXTURE_SIZE,
                TEST_TEXTURE_SIZE,
            )
        };
        let (view_width, view_height) = fit_viewport(
            image_width,
            image_height,
            snapshot.width,
            snapshot.height,
            snapshot.view_mode,
        );
        let viewport = D3D11_VIEWPORT {
            TopLeftX: (snapshot.width as f32 - view_width) * 0.5,
            TopLeftY: (snapshot.height as f32 - view_height) * 0.5,
            Width: view_width,
            Height: view_height,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        // SAFETY: Renderer 在调用期间持有所有绑定的 COM 接口；Viewport、Render Target、SRV 和 Sampler 切片在调用期间有效。
        // SAFETY: Immediate Context 和 Swap Chain 只由当前 Render Thread 使用。
        unsafe {
            self.context
                .OMSetRenderTargets(Some(&[Some(render_target.clone())]), None);
            self.context.ClearRenderTargetView(render_target, &color);
            self.context.RSSetViewports(Some(&[viewport]));
            self.context.RSSetState(&self.rasterizer);
            self.context.OMSetBlendState(&self.blend, None, u32::MAX);
            self.context
                .IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.VSSetShader(&self.vertex_shader, None);
            self.context.PSSetShader(&self.pixel_shader, None);
            self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
            self.context
                .PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            self.context.Draw(3, 0);
        }
        shared.record(Metric::RenderCommands, started.elapsed());

        let present_started = Instant::now();
        // SAFETY: 当前 Render Thread 独占 Swap Chain；UI 收到渲染线程完成消息前会保持 HWND 有效。
        let present_status = unsafe {
            self.swap_chain
                .Present(1, windows::Win32::Graphics::Dxgi::DXGI_PRESENT(0))
        };
        if present_status.is_err() {
            let present_error = Error::from_hresult(present_status);
            // SAFETY: Present 返回失败后 Renderer 仍持有 Device，因此可查询设备移除原因；此调用不接收外部内存。
            let device_reason = unsafe { self.device.GetDeviceRemovedReason() };
            return Err(anyhow!(
                "Present 失败：{present_error}; DeviceRemovedReason：{}",
                device_reason
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "设备未报告移除原因".to_owned())
            ));
        }
        if !self.first_present_reported {
            shared.record(Metric::FirstPresent, self.render_thread_started.elapsed());
        }
        shared.record(Metric::Present, present_started.elapsed());
        if self.current_image.is_some() && !self.first_image_present_reported {
            shared.record(Metric::ImageFirstPresent, snapshot.requested_at.elapsed());
            self.first_image_present_reported = true;
        }
        if !self.first_present_reported {
            println!(
                "First Present call returned successfully; display timing is not measured here."
            );
            self.first_present_reported = true;
        }
        if let Some((requested_at, gpu_cache_hit)) = self.pending_selection_present.take() {
            shared.record(
                if gpu_cache_hit {
                    Metric::CurrentGpuHitToPresent
                } else {
                    Metric::CurrentGpuMissToPresent
                },
                requested_at.elapsed(),
            );
        }
        Ok(())
    }

    fn upload_image(
        &mut self,
        image: Arc<DecodedImage>,
        current: bool,
        shared: &Shared,
    ) -> Result<()> {
        if self.image_cache.contains_key(&image.source) {
            shared.increment(crate::diagnostics::Counter::GpuCacheHit);
            shared.increment(if current {
                crate::diagnostics::Counter::GpuCurrentHit
            } else {
                crate::diagnostics::Counter::GpuPreloadHit
            });
            if current {
                self.current_image = Some(image.source.clone());
                self.current_dimensions = (image.width, image.height);
                self.evict_gpu_cache();
                shared.set_gpu_cache_bytes(self.gpu_cache_bytes);
            }
            self.touch_gpu(&image.source);
            return Ok(());
        }
        shared.increment(crate::diagnostics::Counter::GpuCacheMiss);
        shared.increment(if current {
            crate::diagnostics::Counter::GpuCurrentMiss
        } else {
            crate::diagnostics::Counter::GpuPreloadMiss
        });
        if image.format != PixelFormat::Bgra8Premultiplied {
            bail!("Renderer 收到不支持的像素格式");
        }
        let started = Instant::now();
        let byte_cost = u64::from(image.stride) * u64::from(image.height);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: image.width,
            Height: image.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let data = D3D11_SUBRESOURCE_DATA {
            pSysMem: image.pixels.as_ptr().cast(),
            SysMemPitch: image.stride,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        // SAFETY: 像素 Vec 在同步上传期间保持存活，布局已在 WIC 解码前检查；D3D11 创建调用返回后才释放 CPU 数据。
        unsafe {
            self.device
                .CreateTexture2D(&desc, Some(&data), Some(&mut texture))
        }
        .context("创建图片 GPU Texture 失败")?;
        let texture = texture.context("D3D11 未返回图片 Texture")?;
        let mut view = None;
        // SAFETY: 创建 SRV 时仍持有 Texture，输出槽有效；成功返回的 COM 引用转由 GPU 缓存持有。
        unsafe {
            self.device
                .CreateShaderResourceView(&texture, None, Some(&mut view))
        }
        .context("创建图片 Shader Resource View 失败")?;
        let view = view.context("D3D11 未返回图片 Shader Resource View")?;
        let key = image.source.clone();
        self.image_cache.insert(key.clone(), (view, byte_cost));
        self.image_lru.push_back(key.clone());
        self.gpu_cache_bytes = self.gpu_cache_bytes.saturating_add(byte_cost);
        if current {
            self.current_image = Some(key);
            self.current_dimensions = (image.width, image.height);
        }
        self.evict_gpu_cache();
        shared.set_gpu_cache_bytes(self.gpu_cache_bytes);
        shared.record(Metric::ImageTextureUpload, started.elapsed());
        Ok(())
    }

    /// 每次最多上传一个预加载纹理；当前帧先 Present，随后再服务其余预加载。
    fn upload_one_preload(&mut self, shared: &Shared) -> Result<()> {
        if let Some(image) = self.pending_preloads.pop_front() {
            self.upload_image(image, false, shared)?;
            if !self.pending_preloads.is_empty() {
                shared.wake_render_thread();
            }
        }
        Ok(())
    }

    fn touch_gpu(&mut self, key: &SourceIdentity) {
        self.image_lru.retain(|item| item != key);
        self.image_lru.push_back(key.clone());
    }

    fn evict_gpu_cache(&mut self) {
        let mut skipped_current = false;
        while self.gpu_cache_bytes > self.gpu_cache_budget {
            let Some(key) = self.image_lru.pop_front() else {
                break;
            };
            if self.current_image.as_ref() == Some(&key) {
                self.image_lru.push_back(key);
                if skipped_current {
                    break;
                }
                skipped_current = true;
                continue;
            }
            skipped_current = false;
            if let Some((_, cost)) = self.image_cache.remove(&key) {
                self.gpu_cache_bytes = self.gpu_cache_bytes.saturating_sub(cost);
            }
        }
    }
}

fn fit_viewport(
    image_width: u32,
    image_height: u32,
    window_width: u32,
    window_height: u32,
    mode: ViewMode,
) -> (f32, f32) {
    let width = image_width as f32;
    let height = image_height as f32;
    let scale = match mode {
        ViewMode::Fit => (window_width as f32 / width).min(window_height as f32 / height),
        ViewMode::NativePixels => 1.0,
    };
    (width * scale, height * scale)
}

#[derive(PartialEq, Eq)]
enum WaitResult {
    Work,
    FrameReady,
}

#[derive(Clone, Copy)]
enum TestPattern {
    Checkerboard,
    Gradient,
}

fn create_device() -> Result<(
    ID3D11Device,
    D3D_FEATURE_LEVEL,
    ID3D11DeviceContext,
    D3D_DRIVER_TYPE,
)> {
    match create_device_with_driver(D3D_DRIVER_TYPE_HARDWARE) {
        Ok((device, level, context)) => Ok((device, level, context, D3D_DRIVER_TYPE_HARDWARE)),
        Err(hardware_error) => create_device_with_driver(D3D_DRIVER_TYPE_WARP)
            .map(|(device, level, context)| (device, level, context, D3D_DRIVER_TYPE_WARP))
            .map_err(|warp_error| {
                anyhow!("硬件 D3D11 初始化失败：{hardware_error:#}; WARP 回退失败：{warp_error:#}")
            }),
    }
}

fn driver_type_name(driver_type: D3D_DRIVER_TYPE) -> &'static str {
    if driver_type == D3D_DRIVER_TYPE_WARP {
        "WARP"
    } else {
        "hardware"
    }
}

fn adapter_name(adapter: &IDXGIAdapter) -> Result<String> {
    // SAFETY: `adapter` 是持有中的有效 IDXGIAdapter COM 引用；GetDesc 将描述写入其返回的值类型结构体。
    let desc = unsafe { adapter.GetDesc() }.context("读取 DXGI Adapter 名称失败")?;
    let end = desc
        .Description
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(desc.Description.len());
    Ok(String::from_utf16_lossy(&desc.Description[..end]))
}

fn create_device_with_driver(
    driver: windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE,
) -> Result<(ID3D11Device, D3D_FEATURE_LEVEL, ID3D11DeviceContext)> {
    let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
    // SAFETY: 调用不接收调用方的输出缓冲区；windows-rs 通过输出槽返回由包装器持有的 COM 接口。Device 和 Immediate Context
    // 在最终所有者 Render Thread 上创建。
    let mut device = None;
    let mut feature_level = D3D_FEATURE_LEVEL(0);
    let mut context = None;
    // SAFETY: Adapter 为 None 表示使用默认适配器，Software 为 HMODULE::default()；输出指针指向有效栈槽。
    // SAFETY: Feature Level 切片在调用期间有效，返回的 COM 接口由当前 Render Thread 立即接管。
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            windows::Win32::Foundation::HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut feature_level),
            Some(&mut context),
        )
    }
    .context("D3D11CreateDevice 调用失败")?;
    let device = device.context("D3D11CreateDevice 未返回 Device")?;
    let context = context.context("D3D11CreateDevice 未返回 Immediate Context")?;
    Ok((device, feature_level, context))
}

fn create_sampler(device: &ID3D11Device) -> Result<ID3D11SamplerState> {
    let desc = windows::Win32::Graphics::Direct3D11::D3D11_SAMPLER_DESC {
        Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D11_COMPARISON_NEVER,
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        MaxLOD: f32::MAX,
    };
    let mut sampler = None;
    // SAFETY: `desc` 已完整初始化，输出槽在调用期间有效；返回的 COM 引用转由 Renderer 持有。
    unsafe { device.CreateSamplerState(&desc, Some(&mut sampler)) }
        .context("创建线性采样器失败")?;
    sampler.context("D3D11 未返回 Sampler State")
}

fn create_alpha_blend(device: &ID3D11Device) -> Result<ID3D11BlendState> {
    let mut desc = D3D11_BLEND_DESC::default();
    desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
        BlendEnable: true.into(),
        SrcBlend: D3D11_BLEND_ONE,
        DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
        BlendOp: D3D11_BLEND_OP_ADD,
        SrcBlendAlpha: D3D11_BLEND_ONE,
        DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
        BlendOpAlpha: D3D11_BLEND_OP_ADD,
        RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
    };
    let mut blend = None;
    // SAFETY: 描述结构体已初始化，只有 RenderTarget[0] 启用标准 premultiplied alpha 混合；输出槽在调用期间有效。
    unsafe { device.CreateBlendState(&desc, Some(&mut blend)) }
        .context("创建透明图片混合状态失败")?;
    blend.context("D3D11 未返回 Blend State")
}

fn create_rasterizer(device: &ID3D11Device) -> Result<ID3D11RasterizerState> {
    let desc = D3D11_RASTERIZER_DESC {
        FillMode: windows::Win32::Graphics::Direct3D11::D3D11_FILL_SOLID,
        CullMode: windows::Win32::Graphics::Direct3D11::D3D11_CULL_NONE,
        FrontCounterClockwise: false.into(),
        DepthBias: 0,
        DepthBiasClamp: 0.0,
        SlopeScaledDepthBias: 0.0,
        DepthClipEnable: true.into(),
        ScissorEnable: false.into(),
        MultisampleEnable: false.into(),
        AntialiasedLineEnable: false.into(),
    };
    let mut rasterizer = None;
    // SAFETY: `desc` 已完整初始化，输出槽在调用期间有效；函数成功后由 Renderer 持有返回的 COM 引用。
    unsafe { device.CreateRasterizerState(&desc, Some(&mut rasterizer)) }
        .context("创建 Rasterizer State 失败")?;
    rasterizer.context("D3D11 未返回 Rasterizer State")
}

fn create_test_texture(
    device: &ID3D11Device,
    pattern: TestPattern,
) -> Result<ID3D11ShaderResourceView> {
    let pixels = make_test_pixels(pattern);
    let desc = D3D11_TEXTURE2D_DESC {
        Width: TEST_TEXTURE_SIZE,
        Height: TEST_TEXTURE_SIZE,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let data = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr().cast(),
        SysMemPitch: TEST_TEXTURE_SIZE * 4,
        SysMemSlicePitch: 0,
    };
    let mut texture = None;
    // SAFETY: `pixels` 含有 Width × Height × 4 个已初始化 RGBA 字节，并保持到 CreateTexture2D 返回。
    // SAFETY: 行跨度等于紧密排列的 RGBA8 行字节数，输出槽有效，成功后 D3D11 持有自己的资源引用。
    unsafe { device.CreateTexture2D(&desc, Some(&data), Some(&mut texture)) }
        .context("创建测试 GPU Texture 失败")?;
    let texture = texture.context("D3D11 未返回测试 Texture")?;
    let mut view = None;
    // SAFETY: 调用期间仍持有 `texture`，输出槽有效；返回的 View 持有独立 COM 引用并交给 Renderer。
    unsafe { device.CreateShaderResourceView(&texture, None, Some(&mut view)) }
        .context("创建测试纹理 Shader Resource View 失败")?;
    view.context("D3D11 未返回 Shader Resource View")
}

fn make_test_pixels(pattern: TestPattern) -> Vec<u8> {
    let mut pixels = vec![0; (TEST_TEXTURE_SIZE * TEST_TEXTURE_SIZE * 4) as usize];
    for y in 0..TEST_TEXTURE_SIZE {
        for x in 0..TEST_TEXTURE_SIZE {
            let offset = ((y * TEST_TEXTURE_SIZE + x) * 4) as usize;
            let rgba = match pattern {
                TestPattern::Checkerboard => {
                    let light = ((x / 32) + (y / 32)) % 2 == 0;
                    if light {
                        [40, 180, 235, 255]
                    } else {
                        [20, 30, 50, 255]
                    }
                }
                TestPattern::Gradient => [
                    (x * 255 / (TEST_TEXTURE_SIZE - 1)) as u8,
                    (y * 255 / (TEST_TEXTURE_SIZE - 1)) as u8,
                    ((x + y) * 255 / (2 * (TEST_TEXTURE_SIZE - 1))) as u8,
                    255,
                ],
            };
            pixels[offset..offset + 4].copy_from_slice(&rgba);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::{TEST_TEXTURE_SIZE, TestPattern, fit_viewport, make_test_pixels};
    use crate::app::ViewMode;

    #[test]
    fn generated_test_images_have_expected_rgba_size_and_alpha() {
        for pattern in [TestPattern::Checkerboard, TestPattern::Gradient] {
            let pixels = make_test_pixels(pattern);
            assert_eq!(
                pixels.len(),
                (TEST_TEXTURE_SIZE * TEST_TEXTURE_SIZE * 4) as usize
            );
            assert!(pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
        }
    }

    #[test]
    fn generated_patterns_are_distinct() {
        assert_ne!(
            make_test_pixels(TestPattern::Checkerboard),
            make_test_pixels(TestPattern::Gradient)
        );
    }

    #[test]
    fn fit_and_native_viewports_preserve_image_aspect_and_pixel_size() {
        assert_eq!(
            fit_viewport(400, 200, 1000, 600, ViewMode::Fit),
            (1000.0, 500.0)
        );
        assert_eq!(
            fit_viewport(400, 200, 1000, 600, ViewMode::NativePixels),
            (400.0, 200.0)
        );
    }
}
