//! 使用正式控制器、消息状态和 Render Thread；隐藏窗口自动化不等同于负责人目视验收。
use super::*;
use crate::{
    diagnostics::Counter,
    image::{
        animation::fixture,
        gif_tests::{Directory, memory},
        worker::ImageLoader,
    },
};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

fn count(shared: &Shared, counter: Counter) -> u64 {
    shared.metrics.lock().unwrap().counter(counter)
}
#[test]
fn animation_intent_is_separate_from_dialog_hold_and_new_selection() -> Result<()> {
    let (shared, _event) = tests::test_shared()?;
    shared.set_image_generation(1);
    shared.toggle_animation_pause();
    assert!(
        !shared.animation_control().2.user_paused,
        "静态图片 Space 无效"
    );
    shared.animation_available(1, true);
    shared.toggle_animation_pause();
    shared.animation_dialog(true);
    shared.animation_dialog(false);
    assert!(shared.animation_control().2.user_paused);
    shared.set_navigation_generation(2, Some(1));
    shared.animation_available(1, true);
    assert!(
        !shared.animation_control().2.available,
        "旧 Session 不能改变新选择的播放入口"
    );
    assert!(!shared.animation_control().1, "长按进度期间不启动动画");
    assert!(!shared.animation_control().2.user_paused);
    shared.end_navigation_progress();
    assert!(shared.animation_control().1);
    shared.request_shutdown();
    assert!(!shared.animation_control().1);
    Ok(())
}
fn quiet(
    context: &mut Box<WindowContext>,
    loader: &ImageLoader,
    hwnd: windows::Win32::Foundation::HWND,
) -> Result<()> {
    // 这是明确的空闲观测窗口；播放状态断言由生产确认计数器门控，不依赖 Sleep 猜测线程状态。
    let at = Instant::now();
    tests::pump(context, loader, hwnd, || {
        at.elapsed() >= Duration::from_millis(220)
    })
}

#[test]
#[ignore = "Windows Release 正式 GIF 播放、暂停、Loop、导航和静态空闲集成诊断"]
fn release_gif_playback_viewer() -> Result<()> {
    memory("viewer-before")?;
    let dir = Directory::new()?;
    gif_tests::ordinary_files(&dir.0)?;
    let mut frames = fixture::sample();
    for f in &mut frames {
        f.delay = 3;
    }
    for (name, loops) in [
        ("once", None),
        ("twice", Some(1)),
        ("thrice", Some(2)),
        ("infinite", Some(0)),
    ] {
        fixture::write(&dir.0.join(format!("{name}.gif")), [4, 3], &frames, loops)?;
    }
    let (shared, _event) = tests::test_shared()?;
    let mut loader = ImageLoader::start(crate::image::ImageBudget::default(), Arc::clone(&shared))?;
    shared.set_ui_ready_event(loader.ready_handle());
    let animation = loader.take_animation();
    let mut context = Box::new(WindowContext {
        shared: Arc::clone(&shared),
        close_started: false,
        drag_position: None,
        fullscreen: Default::default(),
        action: None,
        navigation: UiNavigation::default(),
    });
    let hwnd = crate::window::create_main_window(&mut context)?;
    shared.resize(640, 480);
    let rs = Arc::clone(&shared);
    let raw = hwnd.0 as isize;
    let render =
        thread::spawn(move || crate::renderer::render_thread_with_animation(raw, rs, animation));
    let result = (|| -> Result<()> {
        for (name, updates) in [("once", 4), ("twice", 9), ("thrice", 14)] {
            let completed = count(&shared, Counter::AnimationCompleted);
            let presented = count(&shared, Counter::AnimationPresented);
            schedule_selection(&mut context, &loader, dir.0.join(format!("{name}.gif")));
            tests::pump(&mut context, &loader, hwnd, || {
                count(&shared, Counter::AnimationCompleted) > completed
            })?;
            assert_eq!(
                count(&shared, Counter::AnimationPresented) - presented,
                updates,
                "有限 Loop N/N+1 错误：{name}"
            );
            let wakes = count(&shared, Counter::AnimationRenderWake);
            let renders = shared.metrics.lock().unwrap().sample_count(Metric::Present);
            quiet(&mut context, &loader, hwnd)?;
            assert_eq!(
                count(&shared, Counter::AnimationRenderWake),
                wakes,
                "结束后仍有周期唤醒"
            );
            assert_eq!(
                shared.metrics.lock().unwrap().sample_count(Metric::Present),
                renders
            );
        }
        schedule_selection(&mut context, &loader, dir.0.join("infinite.gif"));
        let p = count(&shared, Counter::AnimationPresented);
        tests::pump(&mut context, &loader, hwnd, || {
            count(&shared, Counter::AnimationPresented) >= p + 4
        })?;
        let pauses = count(&shared, Counter::AnimationPaused);
        context.action = Some(WindowAction::ToggleAnimation);
        process_window_action(&mut context, &loader, hwnd);
        tests::pump(&mut context, &loader, hwnd, || {
            count(&shared, Counter::AnimationPaused) > pauses
        })?;
        let p = count(&shared, Counter::AnimationPresented);
        let w = count(&shared, Counter::AnimationRenderWake);
        quiet(&mut context, &loader, hwnd)?;
        assert_eq!(count(&shared, Counter::AnimationPresented), p);
        assert_eq!(count(&shared, Counter::AnimationRenderWake), w);
        // 手动暂停叠加最小化/对话框；解除临时原因不能改变用户意图。
        shared.resize(0, 0);
        shared.animation_dialog(true);
        shared.resize(640, 480);
        shared.animation_dialog(false);
        quiet(&mut context, &loader, hwnd)?;
        assert_eq!(count(&shared, Counter::AnimationPresented), p);
        assert!(shared.animation_control().2.user_paused);
        shared.toggle_animation_pause();
        tests::pump(&mut context, &loader, hwnd, || {
            count(&shared, Counter::AnimationPresented) > p
        })?;
        for dialog in [true, false] {
            let pauses = count(&shared, Counter::AnimationPaused);
            if dialog {
                shared.animation_dialog(true);
            } else {
                shared.resize(0, 0);
            }
            tests::pump(&mut context, &loader, hwnd, || {
                count(&shared, Counter::AnimationPaused) > pauses
            })?;
            let p = count(&shared, Counter::AnimationPresented);
            quiet(&mut context, &loader, hwnd)?;
            assert_eq!(count(&shared, Counter::AnimationPresented), p);
            if dialog {
                shared.animation_dialog(false);
            } else {
                shared.resize(640, 480);
            }
            tests::pump(&mut context, &loader, hwnd, || {
                count(&shared, Counter::AnimationPresented) > p
            })?;
        }
        for name in ["a.jpg", "b.png", "c.bmp", "infinite.gif", "a.jpg"] {
            schedule_selection(&mut context, &loader, dir.0.join(name));
            let generation = context.navigation.generation;
            tests::pump(&mut context, &loader, hwnd, || {
                shared.metrics.lock().unwrap().presented_generation() == generation
            })?;
        }
        // 静态 GPU 命中不得再次走 WIC；预加载已经存在的三种普通图。
        quiet(&mut context, &loader, hwnd)?;
        let copies = shared
            .metrics
            .lock()
            .unwrap()
            .sample_count(Metric::WicCopyPixels);
        let hits = count(&shared, Counter::GpuCurrentHit);
        for i in 0..20 {
            schedule_selection(
                &mut context,
                &loader,
                dir.0.join(if i % 2 == 0 { "b.png" } else { "a.jpg" }),
            );
            let generation = context.navigation.generation;
            tests::pump(&mut context, &loader, hwnd, || {
                shared.metrics.lock().unwrap().presented_generation() == generation
            })?;
        }
        assert_eq!(
            shared
                .metrics
                .lock()
                .unwrap()
                .sample_count(Metric::WicCopyPixels),
            copies
        );
        assert_eq!(count(&shared, Counter::GpuCurrentHit), hits + 20);
        let p = count(&shared, Counter::AnimationPresented);
        let wakes = count(&shared, Counter::AnimationRenderWake);
        quiet(&mut context, &loader, hwnd)?;
        assert_eq!(
            count(&shared, Counter::AnimationPresented),
            p,
            "旧动画覆盖静态图片"
        );
        assert_eq!(
            count(&shared, Counter::AnimationRenderWake),
            wakes,
            "静态空闲有周期唤醒"
        );
        // 重新激活后暂停关闭，覆盖两个 Credit 可被持有时的退出。
        schedule_selection(&mut context, &loader, dir.0.join("infinite.gif"));
        tests::pump(&mut context, &loader, hwnd, || {
            count(&shared, Counter::AnimationPresented) > p
        })?;
        let pauses = count(&shared, Counter::AnimationPaused);
        shared.toggle_animation_pause();
        tests::pump(&mut context, &loader, hwnd, || {
            count(&shared, Counter::AnimationPaused) > pauses
        })?;
        Ok(())
    })();
    let stopped = Instant::now();
    shared.request_shutdown();
    loader.request_stop();
    let rr = render.join().map_err(|_| anyhow!("播放渲染线程 panic"))?;
    loader.join()?;
    if let Some(done) = shared.animation_done_handle() {
        // SAFETY: Shared 持有 Completion，事件活到等待结束；诊断期限不承诺 WIC 可被抢占。
        anyhow::ensure!(
            unsafe { WaitForSingleObject(done, 5000) } == WAIT_OBJECT_0,
            "动画清理超时"
        );
    }
    shared.report_animation_completion();
    assert!(shared.join_completed_animation()?);
    // SAFETY: 所有会访问 HWND 的线程已退出，WindowContext 仍存活，当前线程拥有窗口。
    unsafe { DestroyWindow(hwnd) }?;
    println!(
        "Playback integration shutdown_us={} (CPU/submit timing, no photon/FPS claim)\n{}",
        stopped.elapsed().as_micros(),
        shared.metrics_report()
    );
    memory("viewer-after")?;
    result?;
    rr
}

#[test]
#[ignore = "Windows Release 慢动画 Worker、过期结果拒收及在途关闭诊断"]
fn release_animation_stale_underflow() -> Result<()> {
    let dir = Directory::new()?;
    gif_tests::ordinary_files(&dir.0)?;
    fixture::write(&dir.0.join("slow.gif"), [4, 3], &fixture::sample(), Some(0))?;
    let (shared, _event) = tests::test_shared()?;
    let mut loader = ImageLoader::start(crate::image::ImageBudget::default(), Arc::clone(&shared))?;
    shared.set_ui_ready_event(loader.ready_handle());
    let mut animation = loader.take_animation().unwrap();
    let (entered, release) = animation.hold_first_frame_for_test();
    let mut context = Box::new(WindowContext {
        shared: Arc::clone(&shared),
        close_started: false,
        drag_position: None,
        fullscreen: Default::default(),
        action: None,
        navigation: UiNavigation::default(),
    });
    let hwnd = crate::window::create_main_window(&mut context)?;
    shared.resize(640, 480);
    let rs = Arc::clone(&shared);
    let raw = hwnd.0 as isize;
    let render = thread::spawn(move || {
        crate::renderer::render_thread_with_animation(raw, rs, Some(animation))
    });
    let result = (|| -> Result<()> {
        schedule_selection(&mut context, &loader, dir.0.join("slow.gif"));
        tests::pump(&mut context, &loader, hwnd, || entered.try_recv().is_ok())?;
        tests::pump(&mut context, &loader, hwnd, || {
            count(&shared, Counter::AnimationUnderflow) > 0
        })?;
        let wakes = count(&shared, Counter::AnimationRenderWake);
        quiet(&mut context, &loader, hwnd)?;
        assert_eq!(
            count(&shared, Counter::AnimationRenderWake),
            wakes,
            "Underflow 不应周期唤醒"
        );
        schedule_selection(&mut context, &loader, dir.0.join("a.jpg"));
        let target = context.navigation.generation;
        tests::pump(&mut context, &loader, hwnd, || {
            shared.metrics.lock().unwrap().presented_generation() == target
        })?;
        assert_eq!(count(&shared, Counter::AnimationPresented), 0);
        Ok(())
    })();
    shared.request_shutdown();
    loader.request_stop();
    // 在解码门控尚未放行时 join Render：其析构只能发出协作取消，不能等待动画 Worker。
    let rr = render
        .join()
        .map_err(|_| anyhow!("慢 Worker 测试 Render panic"))?;
    let _ = release.send(());
    loader.join()?;
    if let Some(done) = shared.animation_done_handle() {
        // SAFETY: Shared 的 Completion 保活事件；这里只等待自制门控放行后的清理。
        anyhow::ensure!(
            unsafe { WaitForSingleObject(done, 5000) } == WAIT_OBJECT_0,
            "门控后清理未完成"
        );
        let completion = shared.animation_completion.lock().unwrap();
        let (usage, state) = completion.as_ref().unwrap().resources();
        assert_eq!(usage.animation, 0);
        assert_eq!(state.credits, 0);
        assert_eq!(state.queued, 0);
    }
    // SAFETY: Render 已退出，动画线程没有 HWND 引用，WindowContext 仍存活。
    unsafe { DestroyWindow(hwnd) }?;
    assert!(shared.join_completed_animation()?);
    result?;
    rr?;
    println!(
        "Slow Worker: underflow waits without polling, static navigation completes while blocked, Render exits before gate release, final credits/animation bytes=0"
    );
    Ok(())
}

#[test]
#[ignore = "Windows Release 独立进程播放性能，先生成 animation-fixtures 并设置 PIXSTRIDE_PLAYBACK_CASE"]
fn release_gif_playback_profile() -> Result<()> {
    let root = PathBuf::from(
        std::env::var_os("PIXSTRIDE_TEST_ROOT").context("设置仓库外 PIXSTRIDE_TEST_ROOT")?,
    )
    .join("animation-fixtures");
    let name = std::env::var("PIXSTRIDE_PLAYBACK_CASE").unwrap_or_else(|_| "small".into());
    anyhow::ensure!(
        ["small", "medium", "large", "partial", "short"].contains(&name.as_str()),
        "未知合成样本"
    );
    let goal = if name == "large" { 3 } else { 60 };
    memory("playback-before")?;
    let (shared, _event) = tests::test_shared()?;
    let mut loader = ImageLoader::start(crate::image::ImageBudget::default(), Arc::clone(&shared))?;
    shared.set_ui_ready_event(loader.ready_handle());
    let animation = loader.take_animation();
    let mut context = Box::new(WindowContext {
        shared: Arc::clone(&shared),
        close_started: false,
        drag_position: None,
        fullscreen: Default::default(),
        action: None,
        navigation: UiNavigation::default(),
    });
    let hwnd = crate::window::create_main_window(&mut context)?;
    shared.resize(800, 600);
    let rs = Arc::clone(&shared);
    let raw = hwnd.0 as isize;
    let render =
        thread::spawn(move || crate::renderer::render_thread_with_animation(raw, rs, animation));
    let result = (|| -> Result<()> {
        schedule_selection(&mut context, &loader, root.join(format!("{name}.gif")));
        tests::pump(&mut context, &loader, hwnd, || {
            count(&shared, Counter::AnimationPresented) >= goal
        })?;
        memory("playback-active")?;
        Ok(())
    })();
    let at = Instant::now();
    shared.request_shutdown();
    loader.request_stop();
    let rr = render
        .join()
        .map_err(|_| anyhow!("播放性能 Render panic"))?;
    loader.join()?;
    if let Some(done) = shared.animation_done_handle() {
        // SAFETY: Completion 仍活着；这项等待记录的是完成通知，不是强制中断 WIC。
        anyhow::ensure!(
            unsafe { WaitForSingleObject(done, 5000) } == WAIT_OBJECT_0,
            "动画退出未完成"
        );
        let completion = shared.animation_completion.lock().unwrap();
        let (usage, state) = completion.as_ref().unwrap().resources();
        assert_eq!(usage.animation, 0);
        assert_eq!(state.credits, 0);
    }
    shared.record(Metric::AnimationWorkerShutdown, at.elapsed());
    shared.report_animation_completion();
    assert!(shared.join_completed_animation()?);
    // SAFETY: Render 已退出，HWND 和 WindowContext 在当前测试线程销毁。
    unsafe { DestroyWindow(hwnd) }?;
    println!(
        "PLAYBACK case={name} goal={goal}\n{}",
        shared.metrics_report()
    );
    memory("playback-after")?;
    result?;
    rr
}
