//! 视图状态与真实 Win32 输入/渲染线程回归；隐藏窗口不冒充人工 GUI 验收。
use super::*;
use crate::view_transform::ViewTransform;

#[test]
fn accepted_selection_resets_view_but_frames_and_stale_results_do_not() -> Result<()> {
    let (shared, _wake) = tests::test_shared()?;
    shared.resize(1001, 601);
    shared.set_image_generation(1);
    shared.activate_view(1, (400, 200));
    shared.set_view_mode(ViewMode::NativePixels);
    shared.zoom_at(2.0, (123.0, 77.0));
    shared.pan_by(13.0, -17.0);
    let before = shared.current_snapshot();
    shared.activate_view(1, (400, 200));
    let after = shared.current_snapshot();
    assert_eq!(
        (before.zoom, before.pan, before.sequence),
        (after.zoom, after.pan, after.sequence)
    );
    assert_eq!(shared.image_generation(), 1);
    shared.set_image_generation(2);
    // 加载期间旧图仍有原视图；过期结果无权重置它。
    assert!(shared.activate_view(1, (400, 200)).is_none());
    assert_eq!(shared.current_snapshot().pan, before.pan);
    shared.activate_view(2, (200, 400));
    let s = shared.current_snapshot();
    assert_eq!(s.view_mode, ViewMode::NativePixels);
    assert_eq!((s.zoom, s.pan), (1.0, (0.0, 0.0)));
    shared.zoom_at(1.2, (60.0, 90.0));
    shared.pan_by(f64::NAN, f64::INFINITY);
    assert!(shared.current_snapshot().pan.0.is_finite());
    Ok(())
}

#[test]
fn resize_and_minimize_preserve_source_center_and_user_mode() -> Result<()> {
    for mode in [ViewMode::Fit, ViewMode::NativePixels] {
        let (shared, _wake) = tests::test_shared()?;
        shared.resize(1000, 600);
        shared.activate_view(0, (701, 1400));
        shared.set_view_mode(mode);
        shared.zoom_at(3.0, (123.0, 77.0));
        shared.pan_by(37.0, -59.0);
        let source_center = |s: RenderSnapshot| {
            ViewTransform::new((701, 1400), (s.width, s.height), s.view_mode, s.zoom, s.pan)
                .unwrap()
                .client_to_image((f64::from(s.width) * 0.5, f64::from(s.height) * 0.5))
        };
        let old = shared.current_snapshot();
        let point = source_center(old);
        // 只使用物理客户区尺寸；150%/200% 不会改变 NativePixels 的一比一倍率。
        for client in [(1500, 900), (2000, 1200), (1000, 600)] {
            shared.resize(0, 0);
            shared.resize(client.0, client.1);
            let s = shared.current_snapshot();
            let now = source_center(s);
            assert!((now.0 - point.0).abs() < 1e-7 && (now.1 - point.1).abs() < 1e-7);
            assert_eq!(s.view_mode, mode);
        }
        assert!((shared.current_snapshot().pan.0 - old.pan.0).abs() < 1e-7);
        shared.set_view_mode(ViewMode::Fit);
        assert_eq!(
            (
                shared.current_snapshot().zoom,
                shared.current_snapshot().pan
            ),
            (1.0, (0.0, 0.0))
        );
    }
    Ok(())
}

#[test]
fn progress_geometry_uses_existing_generation_watermark() -> Result<()> {
    let (shared, _wake) = tests::test_shared()?;
    shared.resize(800, 600);
    shared.set_navigation_generation(3, Some(1));
    assert!(shared.claim_image_generation(1));
    shared.activate_view(1, (100, 200));
    shared.pan_by(37.0, 13.0);
    assert!(shared.claim_image_generation(2));
    shared.activate_view(2, (500, 300));
    assert_eq!(shared.current_snapshot().pan, (0.0, 0.0));
    shared.pan_by(27.0, 19.0);
    assert!(shared.activate_view(1, (100, 200)).is_none());
    assert_eq!(shared.current_snapshot().pan, (27.0, 19.0));
    shared.end_navigation_progress();
    assert!(shared.activate_view(2, (500, 300)).is_none());
    shared.activate_view(3, (400, 200));
    assert_eq!(shared.image_generation(), 3);
    assert_eq!(shared.current_snapshot().pan, (0.0, 0.0));
    Ok(())
}

#[test]
#[ignore = "Windows Release：生产鼠标坐标、GIF 视口持续性、普通图/Tile 导航与空闲"]
fn release_view_transform_input_and_playback() -> Result<()> {
    use crate::image::{animation::fixture, gif_tests::Directory};
    use windows::Win32::{
        Foundation::{HWND, LPARAM, POINT, WPARAM},
        Graphics::Gdi::ClientToScreen,
        System::Threading::WaitForSingleObject,
        UI::WindowsAndMessaging::*,
    };
    let dir = Directory::new()?;
    gif_tests::ordinary_files(&dir.0)?;
    let mut frames = fixture::sample();
    for f in &mut frames {
        f.delay = 3;
    }
    fixture::write(&dir.0.join("motion.gif"), [4, 3], &frames, Some(0))?;
    crate::large::stream::tests::generate(&dir.0.join("large.png"), 20000, 65)?;
    let (shared, _wake) = tests::test_shared()?;
    let mut loader = ImageLoader::start(ImageBudget::default(), Arc::clone(&shared))?;
    shared.set_ui_ready_event(loader.ready_handle());
    let animation = loader.take_animation();
    let mut context = Box::new(WindowContext {
        shared: Arc::clone(&shared),
        close_started: false,
        drag_position: None,
        action: None,
        navigation: UiNavigation::default(),
    });
    let hwnd = window::create_main_window(&mut context)?;
    let rs = Arc::clone(&shared);
    let raw = hwnd.0 as isize;
    let render = thread::spawn(move || renderer::render_thread_with_animation(raw, rs, animation));
    let count = |counter| shared.metrics.lock().unwrap().counter(counter);
    let present_count = || shared.metrics.lock().unwrap().sample_count(Metric::Present);
    let quiet = |c: &mut WindowContext| {
        let at = Instant::now();
        tests::pump(c, &loader, hwnd, || {
            at.elapsed() >= Duration::from_millis(220)
        })
    };
    fn packed(x: i32, y: i32) -> LPARAM {
        LPARAM((u32::from(x as i16 as u16) | (u32::from(y as i16 as u16) << 16)) as isize)
    }
    fn send(hwnd: HWND, msg: u32, w: usize, l: LPARAM) {
        // SAFETY: 测试只向本线程创建且仍存活的 HWND 同步发送值类型消息，不传递外部指针。
        unsafe { SendMessageW(hwnd, msg, Some(WPARAM(w)), Some(l)) };
    }
    let result = (|| -> Result<()> {
        // 隐藏窗口置于负屏幕坐标；不改变显示器设置或操作其他应用。
        // SAFETY: HWND 属于本线程，尺寸为有效正值，系统同步复制参数。
        unsafe {
            SetWindowPos(
                hwnd,
                None,
                -1400,
                -800,
                700,
                500,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }?;
        request_open(&mut context, &loader, hwnd, dir.0.join("motion.gif"))?;
        tests::pump(&mut context, &loader, hwnd, || {
            count(Counter::AnimationPresented) >= 7
        })?;
        let generation = context.navigation.generation;
        let sessions = count(Counter::AnimationStarted);
        shared.set_view_mode(ViewMode::NativePixels);
        let before = shared.current_snapshot();
        let mut point = POINT { x: 123, y: 77 };
        // SAFETY: 有效 HWND 和栈上 POINT，转换不会移动真实鼠标。
        assert!(unsafe { ClientToScreen(hwnd, &mut point) }.as_bool());
        assert!(point.x < 0 && point.y < 0);
        let ox = ((f64::from(before.width) - 4.0) * 0.5).floor();
        let oy = ((f64::from(before.height) - 3.0) * 0.5).floor();
        send(hwnd, WM_MOUSEWHEEL, 15usize << 16, packed(point.x, point.y));
        let s = shared.current_snapshot();
        assert!(s.zoom > 1.0 && s.zoom < 1.2);
        let u = (123.0 - ox, 77.0 - oy);
        let v =
            ViewTransform::new((4, 3), (s.width, s.height), s.view_mode, s.zoom, s.pan).unwrap();
        let anchor = v.image_to_client(u);
        assert!((anchor.0 - 123.0).abs() < 1e-7 && (anchor.1 - 77.0).abs() < 1e-7);
        let p0 = s.pan;
        send(hwnd, WM_LBUTTONDOWN, 1, packed(50, 60));
        send(hwnd, WM_MOUSEMOVE, 1, packed(63, 41));
        assert_eq!(shared.current_snapshot().pan, (p0.0 + 13.0, p0.1 - 19.0));
        send(hwnd, WM_CAPTURECHANGED, 0, LPARAM(0));
        let pan = shared.current_snapshot().pan;
        send(hwnd, WM_MOUSEMOVE, 1, packed(900, 900));
        assert_eq!(shared.current_snapshot().pan, pan);
        let stable = shared.current_snapshot();
        let p = count(Counter::AnimationPresented);
        tests::pump(&mut context, &loader, hwnd, || {
            count(Counter::AnimationPresented) >= p + 10
        })?;
        assert_eq!(
            (
                shared.current_snapshot().zoom,
                shared.current_snapshot().pan
            ),
            (stable.zoom, stable.pan)
        );
        assert_eq!(shared.image_generation(), generation);
        assert_eq!(
            count(Counter::AnimationStarted),
            sessions,
            "几何输入/Loop 不重启 Session"
        );
        let pauses = count(Counter::AnimationPaused);
        shared.toggle_animation_pause();
        tests::pump(&mut context, &loader, hwnd, || {
            count(Counter::AnimationPaused) > pauses
        })?;
        shared.zoom_at(1.2, (80.0, 70.0));
        shared.pan_by(-11.0, 17.0);
        let paused = shared.current_snapshot();
        quiet(&mut context)?;
        shared.resize(0, 0);
        shared.resize(paused.width, paused.height);
        shared.animation_dialog(true);
        shared.animation_dialog(false);
        assert!(shared.animation_control().2.user_paused);
        assert_eq!(
            (
                shared.current_snapshot().zoom,
                shared.current_snapshot().pan
            ),
            (paused.zoom, paused.pan)
        );
        shared.toggle_animation_pause();
        let p = count(Counter::AnimationPresented);
        tests::pump(&mut context, &loader, hwnd, || {
            count(Counter::AnimationPresented) >= p + 3
        })?;
        assert_eq!(
            (
                shared.current_snapshot().zoom,
                shared.current_snapshot().pan
            ),
            (paused.zoom, paused.pan)
        );
        assert_eq!(count(Counter::AnimationStarted), sessions);
        shared.set_view_mode(ViewMode::NativePixels);
        for name in [
            "a.jpg",
            "b.png",
            "c.bmp",
            "large.png",
            "motion.gif",
            "a.jpg",
        ] {
            send(hwnd, WM_LBUTTONDOWN, 1, packed(1, 2));
            schedule_selection(&mut context, &loader, dir.0.join(name));
            assert!(context.drag_position.is_none());
            let generation = context.navigation.generation;
            tests::pump(&mut context, &loader, hwnd, || {
                shared.metrics.lock().unwrap().presented_generation() == generation
            })?;
            let s = shared.current_snapshot();
            assert_eq!(
                (s.view_mode, s.zoom, s.pan),
                (ViewMode::NativePixels, 1.0, (0.0, 0.0))
            );
        }
        quiet(&mut context)?;
        let copies = shared
            .metrics
            .lock()
            .unwrap()
            .sample_count(Metric::WicCopyPixels);
        let uploads = shared
            .metrics
            .lock()
            .unwrap()
            .sample_count(Metric::ImageTextureUpload);
        for i in 0..120 {
            let n = present_count();
            let factor = if i % 2 == 0 { 1.2 } else { 1.0 / 1.2 };
            shared.zoom_at(factor, (123.0, 77.0));
            shared.pan_by(1.0, -1.0);
            tests::pump(&mut context, &loader, hwnd, || present_count() > n)?;
        }
        assert_eq!(
            shared
                .metrics
                .lock()
                .unwrap()
                .sample_count(Metric::WicCopyPixels),
            copies
        );
        assert_eq!(
            shared
                .metrics
                .lock()
                .unwrap()
                .sample_count(Metric::ImageTextureUpload),
            uploads
        );
        quiet(&mut context)?;
        let n = present_count();
        let wakes = count(Counter::AnimationRenderWake);
        quiet(&mut context)?;
        assert_eq!(present_count(), n);
        assert_eq!(count(Counter::AnimationRenderWake), wakes);
        send(hwnd, WM_LBUTTONDOWN, 1, packed(1, 2));
        send(hwnd, WM_KILLFOCUS, 0, LPARAM(0));
        assert!(context.drag_position.is_none());
        send(hwnd, WM_LBUTTONDOWN, 1, packed(1, 2));
        send(hwnd, WM_CANCELMODE, 0, LPARAM(0));
        assert!(context.drag_position.is_none());
        println!(
            "VIEW input negative_screen=true fractional_delta=15 navigation_cases=6 static_transform_steps=120 decode_reupload=0 extra_sessions=0 idle_extra_present=0"
        );
        Ok(())
    })();
    shared.request_shutdown();
    loader.request_stop();
    let rr = render.join().map_err(|_| anyhow!("视口渲染线程 panic"))?;
    loader.join()?;
    if let Some(done) = shared.animation_done_handle() {
        // SAFETY: Shared 持有完成事件直到清理结束；这是测试期限，不承诺抢占 WIC。
        assert_eq!(unsafe { WaitForSingleObject(done, 5000) }, WAIT_OBJECT_0);
    }
    shared.join_completed_animation()?;
    // SAFETY: Render 已 join，所有 HWND 使用者退出；WindowContext 仍存活。
    unsafe { DestroyWindow(hwnd) }?;
    println!("{}", shared.metrics_report());
    result?;
    rr
}
