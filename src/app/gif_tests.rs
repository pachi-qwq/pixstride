//! 通过生产 ImageLoader、Win32 消息和渲染线程验证 GIF 首帧，不代替人工桌面验收。
use super::*;
use crate::{
    diagnostics::Counter,
    image::{gif_tests, worker::ImageLoader},
};

#[test]
#[ignore = "显式生成仓库外 GIF/JPEG/PNG/BMP 人工验收与内存诊断样本"]
fn release_prepare_gif_poster_samples() -> Result<()> {
    let root = gif_tests::gui_root()?;
    ordinary_files(&root)?;
    let mut padded = gif_tests::sample(true);
    padded.extend_from_slice(&[0; 167]);
    for (name, data) in [
        ("partial-transparent.gif", gif_tests::sample(true)),
        ("padded-transparent.gif", padded),
        (
            "partial-opaque.gif",
            gif_tests::generated([4, 3], [1, 1, 2, 1], false, false, false),
        ),
        (
            "memory-full.gif",
            gif_tests::generated([2048, 1024], [0, 0, 2048, 1024], true, true, false),
        ),
        (
            "memory-partial.gif",
            gif_tests::generated([2048, 1024], [16, 16, 2016, 992], true, false, false),
        ),
        ("broken.gif", gif_tests::sample(false)[..46].to_vec()),
    ] {
        std::fs::write(root.join(name), data)?;
    }
    println!(
        "Generated self-authored synthetic test inputs in {}",
        root.display()
    );
    Ok(())
}

/// 自行编码普通图片，供 GIF 与 JPEG/PNG/BMP 实际往返诊断使用。
pub(super) fn ordinary_files(root: &std::path::Path) -> Result<()> {
    use windows::Win32::{Foundation::GENERIC_WRITE, Graphics::Imaging::*, System::Com::*};
    use windows::core::PCWSTR;
    let _com = crate::image::ComMta::initialize()?;
    // SAFETY: 当前线程 MTA 已初始化，所有 WIC 编码对象在 guard 前释放。
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        for (name, format) in [
            ("a.jpg", GUID_ContainerFormatJpeg),
            ("b.png", GUID_ContainerFormatPng),
            ("c.bmp", GUID_ContainerFormatBmp),
        ] {
            use std::os::windows::ffi::OsStrExt;
            let path = root.join(name);
            let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let stream = factory.CreateStream()?;
            stream.InitializeFromFilename(PCWSTR(wide.as_ptr()), GENERIC_WRITE.0)?;
            let encoder = factory.CreateEncoder(&format, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let (mut frame, mut options) = (None, None);
            encoder.CreateNewFrame(&mut frame, &mut options)?;
            let frame = frame.context("缺少编码帧")?;
            frame.Initialize(None)?;
            frame.SetSize(8, 6)?;
            let mut pixel_format = GUID_WICPixelFormat24bppBGR;
            frame.SetPixelFormat(&mut pixel_format)?;
            anyhow::ensure!(
                pixel_format == GUID_WICPixelFormat24bppBGR,
                "测试编码格式不符"
            );
            frame.WritePixels(6, 24, &[35u8; 144])?;
            frame.Commit()?;
            encoder.Commit()?;
        }
    }
    Ok(())
}

#[test]
#[ignore = "Release GIF 生产集成诊断，需要 Windows HWND 和 D3D11"]
fn release_gif_poster_viewer() -> Result<()> {
    let dir = gif_tests::Directory::new()?;
    ordinary_files(&dir.0)?;
    let poster = dir.0.join("d 局部.gif");
    let mut padded = gif_tests::sample(true);
    padded.extend_from_slice(&[0; 167]);
    std::fs::write(&poster, padded)?;
    let (shared, _event) = tests::test_shared()?;
    let mut loader = ImageLoader::start(crate::image::ImageBudget::default(), Arc::clone(&shared))?;
    shared.set_ui_ready_event(loader.ready_handle());
    let mut context = Box::new(WindowContext {
        shared: Arc::clone(&shared),
        close_started: false,
        drag_position: None,
        action: None,
        navigation: UiNavigation::default(),
    });
    let hwnd = crate::window::create_main_window(&mut context)?;
    crate::window::show_main_window(hwnd);
    let rs = Arc::clone(&shared);
    let raw = hwnd.0 as isize;
    let render = std::thread::spawn(move || crate::renderer::render_thread(raw, rs));
    let count = |counter| shared.metrics.lock().unwrap().counter(counter);
    let mut blocked_copy = None;
    let result = (|| -> Result<()> {
        request_open(&mut context, &loader, hwnd, poster.clone())?;
        tests::pump(&mut context, &loader, hwnd, || {
            count(Counter::ImageSelectionPresented) > 0
        })?;
        for name in ["a.jpg", "b.png", "c.bmp", "d 局部.gif"] {
            schedule_selection(&mut context, &loader, dir.0.join(name));
            let generation = context.navigation.generation;
            tests::pump(&mut context, &loader, hwnd, || {
                shared.metrics.lock().unwrap().presented_generation() == generation
            })?;
        }
        let before = count(Counter::GpuCurrentHit);
        for i in 0..30 {
            schedule_selection(
                &mut context,
                &loader,
                dir.0.join(if i % 2 == 0 { "a.jpg" } else { "d 局部.gif" }),
            );
            let generation = context.navigation.generation;
            tests::pump(&mut context, &loader, hwnd, || {
                shared.metrics.lock().unwrap().presented_generation() == generation
            })?;
        }
        assert!(count(Counter::GpuCurrentHit) >= before + 30);
        schedule_selection(&mut context, &loader, dir.0.join("a.jpg"));
        let generation = context.navigation.generation;
        tests::pump(&mut context, &loader, hwnd, || {
            shared.metrics.lock().unwrap().presented_generation() == generation
        })?;
        let cpu_hits = count(Counter::CpuCurrentHit);
        let scans = shared
            .metrics
            .lock()
            .unwrap()
            .sample_count(crate::diagnostics::Metric::GifStructure);
        shared.pending().clear_gpu_cache = true;
        schedule_selection(&mut context, &loader, poster.clone());
        let generation = context.navigation.generation;
        tests::pump(&mut context, &loader, hwnd, || {
            shared.metrics.lock().unwrap().presented_generation() == generation
        })?;
        assert_eq!(
            count(Counter::CpuCurrentHit),
            cpu_hits + 1,
            "GIF CPU 命中应只重传 GPU"
        );
        assert_eq!(
            shared
                .metrics
                .lock()
                .unwrap()
                .sample_count(crate::diagnostics::Metric::GifStructure),
            scans,
            "CPU 命中不应重复预检或 WIC 解码"
        );
        // 新 GIF CopyPixels 已完成但尚未发布，先显示 GPU 命中目标，随后验证旧结果无法覆盖。
        let slow = dir.0.join("slow.gif");
        std::fs::write(&slow, gif_tests::sample(true))?;
        let (entered, release) = loader.hold_copy_for_test();
        blocked_copy = Some(release);
        schedule_selection(&mut context, &loader, slow);
        tests::pump(&mut context, &loader, hwnd, || entered.try_recv().is_ok())?;
        let stale = context.navigation.generation;
        let cancelled = count(Counter::DecodeCancelled);
        schedule_selection(&mut context, &loader, poster.clone());
        let newest = context.navigation.generation;
        tests::pump(&mut context, &loader, hwnd, || {
            shared.metrics.lock().unwrap().presented_generation() == newest
        })?;
        assert!(!shared.can_present_image_generation(stale));
        blocked_copy.take().unwrap().send(())?;
        tests::pump(&mut context, &loader, hwnd, || {
            count(Counter::DecodeCancelled) > cancelled
        })?;
        assert_eq!(
            shared.metrics.lock().unwrap().presented_generation(),
            newest
        );
        // 文件修订使用合法的 comment 扩展改变长度，旧 GPU key 不得命中。
        let mut changed = gif_tests::sample(false);
        changed.pop();
        changed.extend_from_slice(&[0x21, 0xfe, 1, b'x', 0, 0x3b]);
        std::fs::write(&poster, changed)?;
        let misses = count(Counter::CpuCacheMiss);
        schedule_selection(&mut context, &loader, poster.clone());
        let revision = context.navigation.generation;
        tests::pump(&mut context, &loader, hwnd, || {
            shared.metrics.lock().unwrap().presented_generation() == revision
        })?;
        assert!(count(Counter::CpuCacheMiss) > misses);
        // GIF 结构拒绝属于非模态错误，后续选择仍然可显示。
        let broken = dir.0.join("broken.gif");
        std::fs::write(&broken, &gif_tests::sample(false)[..46])?;
        let errors = count(Counter::ImageLoadFailed);
        schedule_selection(&mut context, &loader, broken);
        tests::pump(&mut context, &loader, hwnd, || {
            count(Counter::ImageLoadFailed) > errors
        })?;
        schedule_selection(&mut context, &loader, poster);
        let recovered = context.navigation.generation;
        tests::pump(&mut context, &loader, hwnd, || {
            shared.metrics.lock().unwrap().presented_generation() == recovered
        })?;
        // 通过生产窗口消息/布局入口验证 Resize、Fit/100% 和最小化/恢复；没有目视断言。
        for action in [WindowAction::NativePixels, WindowAction::Fit] {
            let frames = shared
                .metrics
                .lock()
                .unwrap()
                .sample_count(crate::diagnostics::Metric::RenderCommands);
            context.action = Some(action);
            process_window_action(&mut context, &loader, hwnd);
            tests::pump(&mut context, &loader, hwnd, || {
                shared
                    .metrics
                    .lock()
                    .unwrap()
                    .sample_count(crate::diagnostics::Metric::RenderCommands)
                    > frames
            })?;
        }
        let frames = shared
            .metrics
            .lock()
            .unwrap()
            .sample_count(crate::diagnostics::Metric::RenderCommands);
        // SAFETY: 本线程拥有 HWND 和 WindowContext，MoveWindow 只改变本测试窗口的几何。
        unsafe { windows::Win32::UI::WindowsAndMessaging::MoveWindow(hwnd, 0, 0, 720, 480, true) }?;
        tests::pump(&mut context, &loader, hwnd, || {
            shared
                .metrics
                .lock()
                .unwrap()
                .sample_count(crate::diagnostics::Metric::RenderCommands)
                > frames
        })?;
        use windows::Win32::UI::WindowsAndMessaging::{
            SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, ShowWindow,
        };
        for cmd in [SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE] {
            let frames = shared
                .metrics
                .lock()
                .unwrap()
                .sample_count(crate::diagnostics::Metric::RenderCommands);
            // SAFETY: 操作自己创建并仍存活的 HWND；结果表示原可见状态，不是错误码。
            let _ = unsafe { ShowWindow(hwnd, cmd) };
            if cmd == SW_MINIMIZE {
                tests::pump(&mut context, &loader, hwnd, || {
                    shared.current_snapshot().width == 0
                })?;
            } else {
                tests::pump(&mut context, &loader, hwnd, || {
                    shared
                        .metrics
                        .lock()
                        .unwrap()
                        .sample_count(crate::diagnostics::Metric::RenderCommands)
                        > frames
                })?;
            }
        }
        // 活动 GIF 调用返回后的关闭：关闭先使所有任务失效，再归还像素并 join。
        let (entered, release) = loader.hold_copy_for_test();
        blocked_copy = Some(release);
        let closing = dir.0.join("closing.gif");
        std::fs::write(&closing, gif_tests::sample(true))?;
        schedule_selection(&mut context, &loader, closing);
        tests::pump(&mut context, &loader, hwnd, || entered.try_recv().is_ok())?;
        Ok(())
    })();
    shared.request_shutdown();
    loader.request_stop();
    if let Some(release) = blocked_copy {
        let _ = release.send(());
    }
    let rr = render
        .join()
        .map_err(|_| anyhow::anyhow!("GIF 诊断渲染线程 panic"))?;
    loader.join()?;
    // SAFETY: HWND 在其所属线程销毁，所有后台线程均已退出，WindowContext 仍存活。
    unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd) }?;
    println!(
        "GIF viewer integration: JPEG/PNG/BMP round trips, 30 GPU hits, stale result, revision, error recovery, Fit/100%, Resize/minimize/restore and closing\n{}",
        shared.metrics_report()
    );
    result?;
    rr
}
