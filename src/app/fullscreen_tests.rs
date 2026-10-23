//! 正式 Render Thread/Loader 的全屏联动；只使用生成的小型普通图、GIF 和 Tile PNG。
use super::*;
use crate::{diagnostics::Counter, image::animation::fixture, view_transform::ViewTransform};
use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    System::Threading::WaitForSingleObject,
    UI::{
        Input::KeyboardAndMouse::{VK_ESCAPE, VK_F11},
        WindowsAndMessaging::*,
    },
};

fn samples(root: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(root)?;
    gif_tests::ordinary_files(root)?;
    let mut frames = fixture::sample();
    for frame in &mut frames {
        frame.delay = 3;
    }
    fixture::write(&root.join("infinite.gif"), [4, 3], &frames, Some(0))?;
    fixture::write(&root.join("finite.gif"), [4, 3], &frames, Some(1))?;
    crate::large::stream::tests::generate(&root.join("large.png"), 20000, 65)?;
    Ok(())
}

fn key(hwnd: HWND, context: &mut WindowContext, loader: &ImageLoader, key: u16) {
    // SAFETY: 当前 UI 线程拥有此 HWND，只同步发送值类型键消息。
    unsafe {
        SendMessageW(
            hwnd,
            WM_KEYDOWN,
            Some(WPARAM(key as usize)),
            Some(LPARAM(1)),
        )
    };
    process_window_action(context, loader, hwnd);
}

fn quiet(context: &mut WindowContext, loader: &ImageLoader, hwnd: HWND) -> Result<()> {
    let at = Instant::now();
    // 有期限的观察窗口，不以 Sleep 推动正式渲染或 GIF 调度。
    tests::pump(context, loader, hwnd, || {
        at.elapsed() >= Duration::from_millis(220)
    })
}

#[test]
fn duplicate_client_size_does_not_publish_work_or_reset_view() -> Result<()> {
    let (shared, _wake) = tests::test_shared()?;
    shared.resize(800, 600);
    shared.activate_view(0, (601, 301));
    shared.zoom_at(3.0, (123.0, 77.0));
    shared.pan_by(27.0, -16.0);
    let before = shared.current_snapshot();
    shared.resize(800, 600);
    let after = shared.current_snapshot();
    assert_eq!(
        (before.sequence, before.zoom, before.pan),
        (after.sequence, after.zoom, after.pan)
    );
    Ok(())
}

#[test]
#[ignore = "Windows Release：保留供负责人 F11/Esc/GIF/Tile 人工验收的生成样本"]
fn release_prepare_fullscreen_samples() -> Result<()> {
    let root = std::env::var_os("PIXSTRIDE_TEST_ROOT")
        .map(PathBuf::from)
        .context("设置仓库外 PIXSTRIDE_TEST_ROOT")?
        .join("fullscreen-gui");
    samples(&root)?;
    println!("Fullscreen GUI samples retained at {}", root.display());
    Ok(())
}

#[test]
#[ignore = "Windows Release：正式渲染线程的全屏 Resize、静态/GIF/Tile、暂停与资源释放"]
fn release_fullscreen_viewer() -> Result<()> {
    let _dpi = window::fullscreen::tests::Dpi::new();
    let dir = crate::image::gif_tests::Directory::new()?;
    samples(&dir.0)?;
    let (shared, _wake) = tests::test_shared()?;
    let mut loader = ImageLoader::start(ImageBudget::default(), Arc::clone(&shared))?;
    shared.set_ui_ready_event(loader.ready_handle());
    let animation = loader.take_animation();
    let mut context = tests::window_context(Arc::clone(&shared));
    let hwnd = window::create_main_window(&mut context)?;
    window::show_main_window(hwnd);
    let rs = Arc::clone(&shared);
    let raw = hwnd.0 as isize;
    let render = thread::spawn(move || renderer::render_thread_with_animation(raw, rs, animation));
    let count = |c| shared.metrics.lock().unwrap().counter(c);
    let n = |m| shared.metrics.lock().unwrap().sample_count(m);
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
        request_open(&mut context, &loader, hwnd, dir.0.join("a.jpg"))?;
        tests::pump(&mut context, &loader, hwnd, || {
            shared.metrics.lock().unwrap().presented_generation() == 1
        })?;
        quiet(&mut context, &loader, hwnd)?;
        let uploads = n(Metric::ImageTextureUpload);
        let decodes = n(Metric::WicCopyPixels);
        let mut transitions = 0;
        for mode in [ViewMode::Fit, ViewMode::NativePixels] {
            shared.set_view_mode(mode);
            shared.zoom_at(2.0, (130.0, 100.0));
            shared.pan_by(13.0, -17.0);
            // a.jpg 的最终尺寸从正式已接受视图确认，后续检查使用相同尺寸。
            let image_size = shared.pending().view_image.unwrap().1;
            let observed = |s: RenderSnapshot| {
                ViewTransform::new(image_size, (s.width, s.height), s.view_mode, s.zoom, s.pan)
                    .unwrap()
                    .client_to_image((s.width as f64 / 2.0, s.height as f64 / 2.0))
            };
            let before = shared.current_snapshot();
            let expected = observed(before);
            for _ in 0..6 {
                let presents = n(Metric::Present);
                let resizes = n(Metric::Resize);
                key(hwnd, &mut context, &loader, VK_F11.0);
                transitions += 1;
                tests::pump(&mut context, &loader, hwnd, || {
                    n(Metric::Resize) > resizes && n(Metric::Present) > presents
                })?;
                let point = observed(shared.current_snapshot());
                assert!((point.0 - expected.0).abs() < 1e-7 && (point.1 - expected.1).abs() < 1e-7);
                assert_eq!(shared.image_generation(), 1);
                assert_eq!(n(Metric::WicCopyPixels), decodes);
                assert_eq!(n(Metric::ImageTextureUpload), uploads);
            }
            assert!(!context.fullscreen.is_fullscreen());
        }
        quiet(&mut context, &loader, hwnd)?;
        let idle = (n(Metric::Present), count(Counter::AnimationRenderWake));
        quiet(&mut context, &loader, hwnd)?;
        assert_eq!(
            idle,
            (n(Metric::Present), count(Counter::AnimationRenderWake))
        );
        schedule_selection(&mut context, &loader, dir.0.join("infinite.gif"));
        tests::pump(&mut context, &loader, hwnd, || {
            count(Counter::AnimationPresented) >= 5
        })?;
        let generation = shared.image_generation();
        let sessions = count(Counter::AnimationStarted);
        shared.set_view_mode(ViewMode::NativePixels);
        shared.zoom_at(4.0, (123.0, 77.0));
        for _ in 0..8 {
            let presents = count(Counter::AnimationPresented);
            key(hwnd, &mut context, &loader, VK_F11.0);
            tests::pump(&mut context, &loader, hwnd, || {
                count(Counter::AnimationPresented) > presents
            })?;
            assert_eq!(shared.image_generation(), generation);
            assert_eq!(count(Counter::AnimationStarted), sessions);
        }
        key(hwnd, &mut context, &loader, 0x20);
        tests::pump(&mut context, &loader, hwnd, || {
            count(Counter::AnimationPaused) > 0
        })?;
        key(hwnd, &mut context, &loader, VK_F11.0);
        // SAFETY: 最小化/恢复当前诊断 HWND，验证用户主动暂停不会被恢复操作覆盖。
        let _ = unsafe { ShowWindow(hwnd, SW_MINIMIZE) };
        assert!(shared.animation_control().2.user_paused);
        // SAFETY: 只恢复自有窗口；实际 WM_SIZE 沿正式路径发布。
        let _ = unsafe { ShowWindow(hwnd, SW_RESTORE) };
        shared.animation_dialog(true);
        shared.animation_dialog(false);
        key(hwnd, &mut context, &loader, VK_ESCAPE.0);
        quiet(&mut context, &loader, hwnd)?;
        let paused = n(Metric::Present);
        quiet(&mut context, &loader, hwnd)?;
        assert_eq!(n(Metric::Present), paused);
        assert!(shared.animation_control().2.user_paused);
        assert_eq!(count(Counter::AnimationStarted), sessions);
        key(hwnd, &mut context, &loader, 0x20);
        let presents = count(Counter::AnimationPresented);
        tests::pump(&mut context, &loader, hwnd, || {
            count(Counter::AnimationPresented) > presents
        })?;
        let finished = count(Counter::AnimationCompleted);
        schedule_selection(&mut context, &loader, dir.0.join("finite.gif"));
        tests::pump(&mut context, &loader, hwnd, || {
            count(Counter::AnimationCompleted) > finished
        })?;
        let sessions = count(Counter::AnimationStarted);
        key(hwnd, &mut context, &loader, VK_F11.0);
        key(hwnd, &mut context, &loader, VK_ESCAPE.0);
        quiet(&mut context, &loader, hwnd)?;
        let stable = n(Metric::Present);
        quiet(&mut context, &loader, hwnd)?;
        assert_eq!(n(Metric::Present), stable);
        assert_eq!(count(Counter::AnimationStarted), sessions);
        key(hwnd, &mut context, &loader, VK_F11.0);
        for name in ["b.png", "large.png", "c.bmp", "a.jpg"] {
            schedule_selection(&mut context, &loader, dir.0.join(name));
            let generation = shared.image_generation();
            tests::pump(&mut context, &loader, hwnd, || {
                shared.metrics.lock().unwrap().presented_generation() == generation
            })?;
            assert!(context.fullscreen.is_fullscreen());
        }
        assert!(count(Counter::GpuCurrentHit) > 0);
        let started = count(Counter::AnimationStarted);
        let presented = count(Counter::AnimationPresented);
        schedule_selection(&mut context, &loader, dir.0.join("infinite.gif"));
        tests::pump(&mut context, &loader, hwnd, || {
            count(Counter::AnimationStarted) > started
                && count(Counter::AnimationPresented) > presented
        })?;
        assert!(context.fullscreen.is_fullscreen());
        println!(
            "FULLSCREEN viewer static_transitions={transitions} gif_transitions=8 decode_reupload=0 idle_extra_present=0 pause_resume=true finite_ended_stable=true static_gif_tile_navigation=true"
        );
        Ok(())
    }));
    let stopped = Instant::now();
    // SAFETY: 正式 WM_CLOSE 只请求停止，Render/Worker 退出后才销毁 HWND。
    unsafe { SendMessageW(hwnd, WM_CLOSE, None, None) };
    loader.request_stop();
    let rr = render
        .join()
        .map_err(|_| anyhow!("全屏 Render Thread panic"))?;
    loader.join()?;
    if let Some(done) = shared.animation_done_handle() {
        // SAFETY: Shared 持有完成事件；测试期限不代表 WIC 可以被强制中断。
        assert_eq!(unsafe { WaitForSingleObject(done, 5000) }, WAIT_OBJECT_0);
    }
    shared.report_animation_completion();
    shared.join_completed_animation()?;
    // SAFETY: 所有 HWND 使用者已退出；Box/事件在 DestroyWindow 之后释放。
    unsafe { DestroyWindow(hwnd) }?;
    println!(
        "FULLSCREEN viewer shutdown_us={}\n{}",
        stopped.elapsed().as_micros(),
        shared.metrics_report()
    );
    match result {
        Ok(r) => r?,
        Err(p) => std::panic::resume_unwind(p),
    }
    rr
}
