//! 回读正式绘制的 Render Target；独立按屏幕像素中心计算双线性采样与预乘混合。
use super::*;
use crate::image::{
    ComMta,
    animation::{
        AnimationWorker, Phase, Request, RestoreBackground, fixture,
        tests::{next, stop, wait_state},
    },
    gif_tests::{Directory, decode},
    memory::CpuMemory,
};
use windows::Win32::{
    Graphics::Direct3D11::*,
    UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW},
};
use windows::core::w;

struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: 本线程创建隐藏窗口；Renderer 和所有 GPU 引用先于 guard 析构。
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

#[test]
#[ignore = "Windows Release：过期帧取消后保留已取得的 DXGI 准入，工作通知仍优先"]
fn release_frame_admission_survives_stale_selection() -> Result<()> {
    use windows::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{CreateEventW, SetEvent, WaitForSingleObject},
    };
    // SAFETY: 系统 STATIC 类，无外部指针；当前线程拥有 HWND 并在 Renderer 后销毁。
    let window = Window(unsafe {
        CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("frame admission"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            128,
            96,
            None,
            None,
            None,
            None,
        )
    }?);
    let (shared, _wake) = crate::app::tests::test_shared()?;
    shared.resize(128, 96);
    let mut r = Renderer::new(window.0, &shared, Instant::now())?;
    r.resize_resources(128, 96, false, &shared)?;
    // 显式拿走一次真实帧就绪通知；模拟该通知对应的候选在提交前过期。
    // SAFETY: Renderer 保活 DXGI 句柄；测试等待期限不改变正式无限等待策略。
    assert_eq!(
        unsafe { WaitForSingleObject(r.frame_latency_handle, 5000) },
        WAIT_OBJECT_0
    );
    // SAFETY: 同一存活句柄立即再次探测；未 Present，验证通知确已被消费而非始终置位。
    assert_eq!(
        unsafe { WaitForSingleObject(r.frame_latency_handle, 0) },
        windows::Win32::Foundation::WAIT_TIMEOUT
    );
    r.frame_available = true;
    shared.set_image_generation(1);
    shared.set_image_generation(2);
    assert!(!shared.can_present_image_generation(1));
    // 无后续通知的确定性替身：新选择不能依赖另一次 DXGI 事件才能提交。
    // SAFETY: 此事件只由本测试使用，直到恢复 Renderer 句柄后才关闭。
    let unsignaled = unsafe { CreateEventW(None, false, false, None) }?;
    let real = std::mem::replace(&mut r.frame_latency_handle, unsignaled);
    let result = (|| -> Result<()> {
        assert_eq!(r.wait_for_work(&shared, true)?, WaitResult::Work);
        let _ = shared.take_pending();
        assert_eq!(r.wait_for_work(&shared, true)?, WaitResult::FrameReady);
        assert!(r.frame_available);
        r.draw(&shared.current_snapshot(), &shared)?;
        assert!(!r.frame_available, "一次 Present 消费一次准入");
        r.frame_available = true;
        // SAFETY: 唤醒句柄由 _wake 持有；显式工作事件必须优先于已取得的准入。
        unsafe { SetEvent(HANDLE(shared.wake_event() as _)) }?;
        assert_eq!(r.wait_for_work(&shared, false)?, WaitResult::Work);
        Ok(())
    })();
    r.frame_latency_handle = real;
    // SAFETY: Renderer 不再引用替身，当前线程是其唯一所有者。
    unsafe { CloseHandle(unsignaled) }?;
    result?;
    println!(
        "FRAME admission stale_skipped=true next_selection_ready=true work_priority=true idle_polling=false"
    );
    Ok(())
}

fn target_pixels(r: &Renderer) -> Result<Vec<u8>> {
    // SAFETY: Render Target 由本 Renderer 持有；引用在只读 staging 复制结束前保持有效。
    let source: ID3D11Texture2D =
        unsafe { r.render_target.as_ref().unwrap().GetResource() }?.cast()?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: source 存活；desc 指向本栈帧可写结构。
    unsafe { source.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    let mut staging = None;
    // SAFETY: 沿用 Render Target 的尺寸/格式，当前测试只创建小缓冲，无外部初始化指针。
    unsafe { r.device.CreateTexture2D(&desc, None, Some(&mut staging)) }?;
    let staging = staging.context("缺少几何回读 staging")?;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: 同一线程独占 Context；Map(READ) 等待复制完成，不以 Present 或帧延迟事件代替同步。
    unsafe {
        r.context.CopyResource(&staging, &source);
        r.context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
    }
    let mut bytes = Vec::new();
    for y in 0..desc.Height as usize {
        // SAFETY: Map 已成功，行数/宽度来自同一纹理；RowPitch 保证有效行容量，借用不越过 Unmap。
        bytes.extend_from_slice(unsafe {
            std::slice::from_raw_parts(
                (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                desc.Width as usize * 4,
            )
        });
    }
    // SAFETY: 所有行切片均已复制并结束借用，配对成功的 Map。
    unsafe { r.context.Unmap(&staging, 0) };
    Ok(bytes)
}

// 参考公式不调用 ViewTransform，也不读取 GPU 常量作为预期。
// 仅在矩形内采样；线性纹理位置为 u-0.5，边界使用 CLAMP，最后与固定查看器背景预乘混合。
fn compare(
    actual: &[u8],
    pixels: &[u8],
    size: (u32, u32),
    client: (u32, u32),
    scale: f64,
    offset: (f64, f64),
) {
    let background = [0.035, 0.045, 0.065, 1.0];
    for y in 0..client.1 {
        for x in 0..client.0 {
            let u = (f64::from(x) + 0.5 - offset.0) / scale;
            let v = (f64::from(y) + 0.5 - offset.1) / scale;
            let mut sample = [0.0; 4];
            if u >= 0.0 && v >= 0.0 && u < f64::from(size.0) && v < f64::from(size.1) {
                let tx = u - 0.5;
                let ty = v - 0.5;
                let fx = tx - tx.floor();
                let fy = ty - ty.floor();
                for (dy, wy) in [(0, 1.0 - fy), (1, fy)] {
                    for (dx, wx) in [(0, 1.0 - fx), (1, fx)] {
                        let sx = (tx.floor() as i64 + dx).clamp(0, i64::from(size.0) - 1) as usize;
                        let sy = (ty.floor() as i64 + dy).clamp(0, i64::from(size.1) - 1) as usize;
                        let p = &pixels[(sy * size.0 as usize + sx) * 4..][..4];
                        for (c, source) in [2, 1, 0, 3].into_iter().enumerate() {
                            sample[c] += f64::from(p[source]) / 255.0 * wx * wy;
                        }
                    }
                }
            }
            let pixel = &actual[((y * client.0 + x) * 4) as usize..][..4];
            for c in 0..4 {
                let expected =
                    ((sample[c] + background[c] * (1.0 - sample[3])) * 255.0).round() as i32;
                // 8-bit UNORM 量化与硬件双线性系数允许最多两个通道单位；坐标错误不能靠放宽容差通过。
                assert!(
                    (i32::from(pixel[c]) - expected).abs() <= 2,
                    "pixel=({x},{y}) channel={c} actual={pixel:?} expected={expected} sample={sample:?} scale={scale} offset={offset:?}",
                );
            }
        }
    }
}

#[test]
#[ignore = "Windows Release：普通/动画实际 Quad 几何、透明混合与缓存回读"]
fn release_view_transform_gpu_pixels() -> Result<()> {
    let _com = ComMta::initialize()?;
    let dir = Directory::new()?;
    let path = dir.0.join("owned-layout.bmp");
    std::fs::write(&path, b"owned-gpu-fixture")?;
    let mut pixels = Vec::new();
    for y in 0..6 {
        for x in 0..8 {
            let a = [64u8, 128, 255][(x + y) % 3];
            pixels.extend_from_slice(&[(x * 7) as u8, (y * 9) as u8, (x + y) as u8 * 5, a]);
        }
    }
    let image = Arc::new(DecodedImage {
        source: SourceIdentity::read(&path)?,
        width: 8,
        height: 6,
        stride: 32,
        format: PixelFormat::Bgra8Premultiplied,
        pixels,
        gif_poster: None,
        _charge: None,
    });
    let (shared, _wake) = crate::app::tests::test_shared()?;
    // SAFETY: STATIC 为系统类，隐藏窗口由当前线程创建，guard 在 Renderer 之后释放。
    let window = Window(unsafe {
        CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("Pixstride view geometry"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            128,
            96,
            None,
            None,
            None,
            None,
        )
    }?);
    let mut r = Renderer::new(window.0, &shared, Instant::now())?;
    shared.set_image_generation(1);
    r.upload_image(Arc::clone(&image), true, &shared)?;
    r.selection_generation = Some(1);
    shared.activate_view(1, (8, 6));
    let srv = r.image_cache[&image.source].0.as_raw();
    let uploads = shared.metrics_report();
    let mut checked = 0usize;
    // 首次绑定使用 Swap Chain 的 HWND 初始尺寸；下一次 ResizeBuffers 才明确指定
    // 本专项的客户区。回读必须按资源真实行宽读取，不能把窗口初始行宽当成测试行宽。
    r.resize_resources(1, 1, false, &shared)?;
    for (client, mode, zoom, pan, scale, offset) in [
        ((64, 48), ViewMode::Fit, 1.0, (0.0, 0.0), 8.0, (0.0, 0.0)),
        (
            (17, 13),
            ViewMode::NativePixels,
            1.0,
            (0.0, 0.0),
            1.0,
            (4.0, 3.0),
        ),
        (
            (17, 13),
            ViewMode::NativePixels,
            2.0,
            (-3.0, 2.0),
            2.0,
            (-3.0, 2.0),
        ),
        (
            (17, 13),
            ViewMode::Fit,
            1.5,
            (3.0, -2.0),
            3.1875,
            (-1.25, -5.0625),
        ),
        (
            (31, 23),
            ViewMode::NativePixels,
            0.5,
            (0.25, -0.75),
            0.5,
            (13.25, 8.75),
        ),
        (
            (17, 13),
            ViewMode::Fit,
            1.0,
            (1e9, -1e9),
            2.125,
            (1e9, -1e9 + 0.125),
        ),
    ] {
        r.resize_resources(client.0, client.1, false, &shared)?;
        let mut s = shared.current_snapshot();
        (s.width, s.height, s.view_mode, s.zoom, s.pan) = (client.0, client.1, mode, zoom, pan);
        r.draw_content(&s)?;
        let actual = target_pixels(&r)?;
        assert_eq!(actual.len(), client.0 as usize * client.1 as usize * 4);
        compare(&actual, &image.pixels, (8, 6), client, scale, offset);
        assert_eq!(r.image_cache[&image.source].0.as_raw(), srv);
        assert_eq!(r.gpu_cache_bytes, 8 * 6 * 4);
        checked += (client.0 * client.1) as usize;
    }
    assert_eq!(
        shared.metrics_report(),
        uploads,
        "仅几何绘制不新增解码、纹理上传或计时器计数"
    );

    let gif = dir.0.join("animated.gif");
    fixture::write(&gif, [4, 3], &fixture::sample(), Some(1))?;
    let poster = Arc::new(decode(&gif)?);
    shared.set_image_generation(2);
    r.upload_image(Arc::clone(&poster), true, &shared)?;
    r.selection_generation = Some(2);
    shared.activate_view(2, (4, 3));
    let pool = CpuMemory::new(ImageBudget::default().cpu_cache_bytes);
    let mut worker = AnimationWorker::new(pool.clone());
    let key = worker.start(Request {
        path: gif,
        selection_generation: 2,
        passes: std::num::NonZeroU32::new(2),
        background: RestoreBackground::Transparent,
    })?;
    r.resize_resources(20, 16, false, &shared)?;
    let mut s = shared.current_snapshot();
    (s.width, s.height, s.view_mode, s.zoom, s.pan) =
        (20, 16, ViewMode::NativePixels, 3.0, (1.0, -2.0));
    for ordinal in 0..10 {
        let frame = next(&worker, key)?;
        r.animation_gpu.upload(
            &r.device,
            &r.context,
            &frame,
            &poster.source,
            r.gpu_cache_bytes,
            r.gpu_cache_budget,
        )?;
        drop(frame);
        r.draw_content(&s)?;
        let mut expected = fixture::oracle()[ordinal % 5].clone();
        for pixel in expected.chunks_exact_mut(4) {
            if pixel == [0, 0, 0, 255] {
                pixel.fill(0);
            }
        }
        compare(
            &target_pixels(&r)?,
            &expected,
            (4, 3),
            (20, 16),
            3.0,
            (5.0, 1.0),
        );
        assert_eq!(shared.image_generation(), 2);
        assert_eq!(s.pan, (1.0, -2.0));
        checked += 320;
    }
    wait_state(&worker, |s| s.phase == Phase::Completed);
    stop(&mut worker)?;
    assert_eq!(pool.usage().total, 0);
    assert_eq!(worker.snapshot().credits, 0);
    println!(
        "VIEW GPU pixels_checked={checked} ordinary_cases=6 gif_frames=10 native_integer_alignment=true channel_tolerance=2 (not screen/display timing)"
    );
    drop(r);
    Ok(())
}

#[test]
#[ignore = "Windows Release：自有 HWND 全屏/恢复后的真实 Swap Chain 和独立像素参考"]
fn release_fullscreen_gpu_pixels() -> Result<()> {
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::{Input::KeyboardAndMouse::VK_F11, WindowsAndMessaging::*},
    };
    let _dpi = crate::window::fullscreen::tests::Dpi::new();
    let dir = Directory::new()?;
    let path = dir.0.join("fullscreen-pixels.bmp");
    std::fs::write(&path, b"owned-pixel-reference")?;
    let pixels = vec![0, 0, 128, 128, 0, 255, 0, 255, 255, 0, 0, 255, 0, 0, 0, 0];
    let image = Arc::new(DecodedImage {
        source: SourceIdentity::read(&path)?,
        width: 2,
        height: 2,
        stride: 8,
        format: PixelFormat::Bgra8Premultiplied,
        pixels,
        gif_poster: None,
        _charge: None,
    });
    let (shared, _wake) = crate::app::tests::test_shared()?;
    let mut context = crate::app::tests::window_context(Arc::clone(&shared));
    let hwnd = crate::window::create_main_window(&mut context)?;
    let window = Window(hwnd);
    // SAFETY: 本线程的自有 HWND；不激活，窗口尺寸变化由正式窗口过程发布。
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            100,
            100,
            480,
            320,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }?;
    let mut r = Renderer::new(hwnd, &shared, Instant::now())?;
    // 首次获取真实 Back Buffer 后，后续 ResizeBuffers 才明确指定客户区。
    r.resize_resources(1, 1, false, &shared)?;
    shared.set_image_generation(1);
    r.upload_image(Arc::clone(&image), true, &shared)?;
    r.selection_generation = Some(1);
    shared.activate_view(1, (2, 2));
    let device = r.device.as_raw();
    let chain = r.swap_chain.as_raw();
    let srv = r.image_cache[&image.source].0.as_raw();
    fn is_exclusive(r: &Renderer) -> Result<bool> {
        let mut exclusive = windows::core::BOOL::default();
        // SAFETY: Swap Chain 仍由本线程独占，BOOL 输出在本次同步查询期间可写；无需输出显示器接口。
        unsafe { r.swap_chain.GetFullscreenState(Some(&mut exclusive), None) }?;
        Ok(exclusive.as_bool())
    }
    assert!(!is_exclusive(&r)?);
    // SAFETY: 只向自有 HWND 注入系统 Alt+Enter；Factory 已禁止 DXGI 自动模式切换，不修改显示配置。
    unsafe {
        SendMessageW(
            hwnd,
            WM_SYSKEYDOWN,
            Some(WPARAM(0x0d)),
            Some(LPARAM(1 | (1 << 29))),
        )
    };
    let mut message = MSG::default();
    // SAFETY: 分发本测试线程的消息以检验实际关联，不依赖 Sleep 或只检查调用返回值。
    unsafe {
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            DispatchMessageW(&message);
        }
    }
    assert!(!context.fullscreen.is_fullscreen());
    assert!(!is_exclusive(&r)?);
    let mut checked = 0u64;
    for mode in [ViewMode::Fit, ViewMode::NativePixels] {
        shared.set_view_mode(mode);
        shared.zoom_at(
            if mode == ViewMode::Fit { 1.2 } else { 16.0 },
            (180.0, 130.0),
        );
        shared.pan_by(13.0, -7.0);
        for _ in 0..4 {
            let s = shared.current_snapshot();
            let size = (s.width, s.height);
            r.resize_resources(size.0, size.1, true, &shared)?;
            r.draw_content(&s)?;
            // 独立原图中心/整数边界公式，不调用待测 ViewTransform。
            let base = if mode == ViewMode::Fit {
                (size.0 as f64 / 2.0).min(size.1 as f64 / 2.0)
            } else {
                1.0
            };
            let scale = base * s.zoom;
            let center = if mode == ViewMode::Fit {
                (size.0 as f64 / 2.0, size.1 as f64 / 2.0)
            } else {
                (
                    ((size.0 as f64 - 2.0) / 2.0).floor() + 1.0,
                    ((size.1 as f64 - 2.0) / 2.0).floor() + 1.0,
                )
            };
            compare(
                &target_pixels(&r)?,
                &image.pixels,
                (2, 2),
                size,
                scale,
                (center.0 - scale + s.pan.0, center.1 - scale + s.pan.1),
            );
            checked += u64::from(size.0) * u64::from(size.1);
            r.draw(&s, &shared)?;
            assert_eq!(r.device.as_raw(), device);
            assert_eq!(r.swap_chain.as_raw(), chain);
            assert!(!is_exclusive(&r)?);
            assert_eq!(r.image_cache[&image.source].0.as_raw(), srv);
            assert_eq!(r.gpu_cache_bytes, 16);
            assert_eq!(shared.image_generation(), 1);
            // SAFETY: 本线程同步切换自有 HWND；无跨线程 COM 或额外纹理。
            unsafe {
                SendMessageW(
                    hwnd,
                    WM_KEYDOWN,
                    Some(WPARAM(VK_F11.0 as usize)),
                    Some(LPARAM(1)),
                )
            };
        }
        assert!(!context.fullscreen.is_fullscreen());
    }
    println!(
        "FULLSCREEN GPU pixels_checked={checked} cases=8 tolerance=2 device_swap_chain_srv_reused=true dxgi_exclusive=false alt_enter_ignored=true static_payload=16 bytes"
    );
    drop(r);
    drop(window);
    Ok(())
}
