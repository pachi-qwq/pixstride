//! 自有 HWND 的状态/失败事务验证，不修改显示器设置或其他应用窗口。
use super::*;
use std::{sync::Arc, time::Instant};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};

pub(crate) struct Dpi(DPI_AWARENESS_CONTEXT);
impl Dpi {
    pub(crate) fn new() -> Self {
        // SAFETY: 只改变当前诊断 UI 线程的 DPI 上下文；Drop 恢复，不改变显示器缩放设置。
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}
impl Drop for Dpi {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: 旧上下文由当前线程的 SetThreadDpiAwarenessContext 返回，恢复发生在同一线程。
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}

struct Window {
    hwnd: HWND,
    context: Box<WindowContext>,
    _wake: crate::app::WakeEvent,
    _dpi: Dpi,
}
impl Window {
    fn new() -> Result<Self> {
        let dpi = Dpi::new();
        let (shared, wake) = crate::app::tests::test_shared()?;
        let mut context = crate::app::tests::window_context(shared);
        let hwnd = crate::window::create_main_window(&mut context)?;
        // SAFETY: 只显示本测试创建的窗口，不激活或移动其他程序。
        let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        Ok(Self {
            hwnd,
            context,
            _wake: wake,
            _dpi: dpi,
        })
    }
    fn key(&self, key: u16, repeat: bool) {
        // SAFETY: 自有存活 HWND，值类型键消息，调用线程与窗口一致。
        unsafe {
            SendMessageW(
                self.hwnd,
                WM_KEYDOWN,
                Some(WPARAM(key as usize)),
                Some(LPARAM(if repeat { 1 << 30 } else { 1 })),
            )
        };
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        FAILURE.with(|f| f.borrow_mut().clear());
        // SAFETY: 没有后台线程使用此诊断窗口，Box 在 DestroyWindow 返回后才析构。
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

#[test]
fn removed_monitor_fallback_handles_negative_and_overflowing_coordinates() -> Result<()> {
    assert_eq!(
        fit_to_work(rect(8000, 9000, 800, 600), rect(-1920, -200, 1920, 1040))?,
        rect(-800, 240, 800, 600)
    );
    assert_eq!(
        fit_to_work(
            rect(-8000, -9000, 2000, 1600),
            rect(-1920, -200, 1920, 1040)
        )?,
        rect(-1920, -200, 1920, 1040)
    );
    assert!(
        dimensions(RECT {
            left: i32::MIN,
            right: i32::MAX,
            top: 0,
            bottom: 1
        })
        .is_err()
    );
    assert!(dimensions(RECT::default()).is_err());
    assert!(fit_to_work(rect(0, 0, 1, 1), RECT::default()).is_err());
    assert!(validate_owner(HWND::default()).is_err());
    Ok(())
}

#[test]
fn native_fullscreen_state_restores_normal_maximized_and_failed_transactions() -> Result<()> {
    let mut w = Window::new()?;
    let target = display_for_window(w.hwnd)?;
    position(
        w.hwnd,
        fit_to_work(
            rect(target.work.left + 50, target.work.top + 60, 650, 430),
            target.work,
        )?,
    )?;
    let original = capture(w.hwnd)?;
    let original_rect = window_rect(w.hwnd)?;
    let shared = Arc::clone(&w.context.shared);
    shared.set_image_generation(17);
    shared.activate_view(17, (701, 1400));
    shared.set_view_mode(crate::app::ViewMode::NativePixels);
    shared.zoom_at(3.0, (31.0, 77.0));
    shared.pan_by(37.0, -59.0);
    let center = |s: crate::app::RenderSnapshot| {
        crate::view_transform::ViewTransform::new(
            (701, 1400),
            (s.width, s.height),
            s.view_mode,
            s.zoom,
            s.pan,
        )
        .unwrap()
        .client_to_image((s.width as f64 * 0.5, s.height as f64 * 0.5))
    };
    let before = shared.current_snapshot();
    let observed = center(before);
    w.key(VK_ESCAPE.0, false);
    assert!(!w.context.close_started);
    for i in 0..8 {
        w.key(VK_F11.0, false);
        assert!(w.context.fullscreen.is_fullscreen());
        assert_eq!(window_rect(w.hwnd)?, target.rect);
        assert_eq!(
            (
                shared.current_snapshot().width,
                shared.current_snapshot().height
            ),
            (
                (target.rect.right - target.rect.left) as u32,
                (target.rect.bottom - target.rect.top) as u32
            ),
            "全屏客户区必须覆盖 rcMonitor，而非 rcWork 或带边框的矩形"
        );
        assert_eq!(
            get_style(w.hwnd, GWL_STYLE)? & (WS_OVERLAPPEDWINDOW.0 | WS_MAXIMIZE.0),
            0
        );
        assert_eq!(
            get_style(w.hwnd, GWL_EXSTYLE)? & !FRAME_EX,
            original.ex_style & !FRAME_EX
        );
        assert_eq!(shared.image_generation(), 17);
        let actual = center(shared.current_snapshot());
        assert!((actual.0 - observed.0).abs() < 1e-7 && (actual.1 - observed.1).abs() < 1e-7);
        w.key(VK_F11.0, true);
        assert!(w.context.fullscreen.is_fullscreen(), "F11 自动重复不能退出");
        let sequence = shared.current_snapshot().sequence;
        // SAFETY: 只向自有 HWND 注入显示器变化通知，不改变真实显示器配置。
        unsafe { SendMessageW(w.hwnd, WM_DISPLAYCHANGE, None, None) };
        assert_eq!(
            shared.current_snapshot().sequence,
            sequence,
            "相同范围不能重复 Resize"
        );
        w.key(if i % 2 == 0 { VK_ESCAPE.0 } else { VK_F11.0 }, false);
        assert!(!w.context.fullscreen.is_fullscreen());
        assert_eq!(window_rect(w.hwnd)?, original_rect);
        assert_eq!(get_style(w.hwnd, GWL_STYLE)?, original.style);
        assert_eq!(get_style(w.hwnd, GWL_EXSTYLE)?, original.ex_style);
        assert_eq!(
            capture(w.hwnd)?.placement.rcNormalPosition,
            original.placement.rcNormalPosition
        );
        let s = shared.current_snapshot();
        assert!((s.pan.0 - before.pan.0).abs() < 1e-7 && (s.pan.1 - before.pan.1).abs() < 1e-7);
    }
    // SAFETY: 只最大化本诊断 HWND；同步恢复后检查原生 Placement，而非仅检查应用枚举。
    let _ = unsafe { ShowWindow(w.hwnd, SW_MAXIMIZE) };
    let maximized = capture(w.hwnd)?;
    // SAFETY: 自有存活 HWND，只查询状态。
    assert!(unsafe { IsZoomed(w.hwnd) }.as_bool());
    w.key(VK_F11.0, false);
    assert!(w.context.fullscreen.is_fullscreen());
    assert_eq!(window_rect(w.hwnd)?, target.rect);
    // SAFETY: 验证最大化来源的全屏仍可最小化/恢复，原最大化快照不能被覆盖。
    let _ = unsafe { ShowWindow(w.hwnd, SW_MINIMIZE) };
    assert!(w.context.fullscreen.is_fullscreen());
    // SAFETY: 仅恢复本测试的 HWND；实际 WM_SIZE 重新对齐显示器。
    let _ = unsafe { ShowWindow(w.hwnd, SW_RESTORE) };
    assert_eq!(window_rect(w.hwnd)?, target.rect);
    w.key(VK_ESCAPE.0, false);
    // SAFETY: HWND 始终由本测试线程拥有。
    assert!(unsafe { IsZoomed(w.hwnd) }.as_bool());
    assert_eq!(
        capture(w.hwnd)?.placement.showCmd,
        maximized.placement.showCmd
    );
    assert_eq!(
        capture(w.hwnd)?.placement.rcNormalPosition,
        maximized.placement.rcNormalPosition
    );
    // SAFETY: 把自有窗口恢复为普通状态以测试事务失败。
    let _ = unsafe { ShowWindow(w.hwnd, SW_RESTORE) };
    let normal = capture(w.hwnd)?;
    for step in [Step::Style, Step::ExStyle, Step::Position] {
        FAILURE.with(|f| f.borrow_mut().push_back(step));
        // SAFETY: context 为当前 HWND 关联的活动 Box，短借用在 API 调用前结束。
        assert!(unsafe { change(w.hwnd, &mut *w.context, true) }.is_err());
        assert!(matches!(w.context.fullscreen.mode, Mode::Windowed));
        assert_eq!(get_style(w.hwnd, GWL_STYLE)?, normal.style);
        assert_eq!(get_style(w.hwnd, GWL_EXSTYLE)?, normal.ex_style);
        assert_eq!(
            capture(w.hwnd)?.placement.rcNormalPosition,
            normal.placement.rcNormalPosition
        );
    }
    for step in [Step::Style, Step::ExStyle, Step::Placement, Step::Frame] {
        w.key(VK_F11.0, false);
        FAILURE.with(|f| f.borrow_mut().push_back(step));
        // SAFETY: 退出失败时会尝试回到全屏，仍保留相同恢复快照。
        assert!(unsafe { change(w.hwnd, &mut *w.context, false) }.is_err());
        assert!(w.context.fullscreen.is_fullscreen());
        w.key(VK_ESCAPE.0, false);
        assert!(matches!(w.context.fullscreen.mode, Mode::Windowed));
    }
    FAILURE.with(|f| f.borrow_mut().extend([Step::Style, Step::Placement]));
    // SAFETY: 确定性模拟进入与恢复均失败，Box/窗口仍然有效。
    assert!(unsafe { change(w.hwnd, &mut *w.context, true) }.is_err());
    assert!(matches!(w.context.fullscreen.mode, Mode::Recovery(_)));
    w.key(VK_ESCAPE.0, false);
    assert!(matches!(w.context.fullscreen.mode, Mode::Windowed));
    assert_eq!(get_style(w.hwnd, GWL_STYLE)?, normal.style);
    // 模拟原显示器身份已消失和完全屏外的保存位置，不改系统显示器设置。
    w.key(VK_F11.0, false);
    let Mode::Fullscreen(mut saved) = w.context.fullscreen.mode else {
        unreachable!()
    };
    saved.monitor.device = [u16::MAX; 32];
    saved.placement.rcNormalPosition = rect(500000, 500000, 650, 430);
    w.context.fullscreen.mode = Mode::Fullscreen(saved);
    w.key(VK_ESCAPE.0, false);
    let work = display_for_window(w.hwnd)?.work;
    let actual = window_rect(w.hwnd)?;
    assert_eq!(fit_to_work(actual, work)?, actual);
    w.key(VK_F11.0, false);
    // SAFETY: 仅向自有 HWND 发送带 Alt 上下文的系统 F4，不注册或注入全局键盘事件。
    unsafe {
        SendMessageW(
            w.hwnd,
            WM_SYSKEYDOWN,
            Some(WPARAM(0x73)),
            Some(LPARAM(1 | (1 << 29))),
        );
    }
    let mut message = MSG::default();
    // SAFETY: 分发系统键默认处理投递到本测试线程的消息，不处理其他线程/应用的队列。
    unsafe {
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            DispatchMessageW(&message);
        }
    }
    assert!(
        w.context.close_started,
        "保留 DefWindowProc 的 Alt+F4 关闭路径"
    );
    println!(
        "FULLSCREEN native: normal/maximized restoration, repeat/Esc, geometry center, failure rollback/retry, simulated missing-monitor fallback passed"
    );
    Ok(())
}

#[test]
#[ignore = "Windows Release：重复全屏耗时、真实窗口资源与 DPI/最小化通知"]
fn release_fullscreen_window_lifecycle() -> Result<()> {
    use windows::Win32::System::Threading::{
        GR_GDIOBJECTS, GR_USEROBJECTS, GetCurrentProcess, GetGuiResources, GetProcessHandleCount,
    };
    let w = Window::new()?;
    let target = display_for_window(w.hwnd)?;
    let original = capture(w.hwnd)?;
    let mut enters = Vec::new();
    let mut exits = Vec::new();
    for _ in 0..4 {
        w.key(VK_F11.0, false);
        w.key(VK_ESCAPE.0, false);
    }
    let resources = || {
        // SAFETY: 伪句柄代表本进程，只读取系统计数，不关闭该句柄。
        let process = unsafe { GetCurrentProcess() };
        let mut handles = 0;
        // SAFETY: 栈上可写计数，进程伪句柄有效。
        unsafe { GetProcessHandleCount(process, &mut handles) }.unwrap();
        // SAFETY: 只查询本进程的 GDI/USER 资源计数。
        (
            handles,
            unsafe { GetGuiResources(process, GR_GDIOBJECTS) },
            unsafe { GetGuiResources(process, GR_USEROBJECTS) },
        )
    };
    let before = resources();
    w.key(VK_F11.0, false);
    // SAFETY: 模拟系统移动自有全屏窗口，正式 WM_MOVE 将其对齐最近显示器，不修改真实显示配置。
    unsafe {
        SetWindowPos(
            w.hwnd,
            None,
            target.rect.left + 25,
            target.rect.top + 30,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }?;
    assert_eq!(window_rect(w.hwnd)?, target.rect);
    w.key(VK_ESCAPE.0, false);
    for _ in 0..40 {
        let at = Instant::now();
        w.key(VK_F11.0, false);
        enters.push(at.elapsed().as_micros());
        assert!(w.context.fullscreen.is_fullscreen());
        assert_eq!(window_rect(w.hwnd)?, target.rect);
        let mut suggested = rect(50, 60, 400, 300);
        // SAFETY: 同步 DPI 通知使用本栈上的有效 RECT，返回后不保留指针；不改变真实 DPI。
        unsafe {
            SendMessageW(
                w.hwnd,
                WM_DPICHANGED,
                Some(WPARAM(144 | (144 << 16))),
                Some(LPARAM((&mut suggested as *mut RECT) as isize)),
            )
        };
        assert_eq!(window_rect(w.hwnd)?, target.rect);
        let at = Instant::now();
        w.key(VK_ESCAPE.0, false);
        exits.push(at.elapsed().as_micros());
        assert_eq!(get_style(w.hwnd, GWL_STYLE)?, original.style);
    }
    let after = resources();
    assert_eq!(after, before, "预热后反复切换不能累积自有句柄");
    w.key(VK_F11.0, false);
    // SAFETY: 最小化/恢复只操作本测试窗口，不修改其他应用。
    let _ = unsafe { ShowWindow(w.hwnd, SW_MINIMIZE) };
    assert_eq!(
        (
            w.context.shared.current_snapshot().width,
            w.context.shared.current_snapshot().height
        ),
        (0, 0)
    );
    assert!(w.context.fullscreen.is_fullscreen());
    // SAFETY: 恢复本诊断窗口，WM_SIZE 会重新检查当前显示器范围。
    let _ = unsafe { ShowWindow(w.hwnd, SW_RESTORE) };
    assert_eq!(window_rect(w.hwnd)?, target.rect);
    fn report(name: &str, mut values: Vec<u128>) {
        values.sort_unstable();
        println!(
            "FULLSCREEN {name} n={} p50_us={} p95_us={} max_us={}",
            values.len(),
            values[(values.len() as f64 * 0.5).ceil() as usize - 1],
            values[(values.len() as f64 * 0.95).ceil() as usize - 1],
            values.last().unwrap()
        );
    }
    report("enter", enters);
    report("exit", exits);
    // SAFETY: 当前 HWND/线程均存活，只查询 DPI，不更改系统设置。
    println!(
        "FULLSCREEN resources before={before:?} after={after:?} actual_dpi={} monitor={:?} (synthetic DPI message is not physical mixed-DPI acceptance)",
        unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(w.hwnd) },
        target.rect
    );
    // Drop 在全屏状态销毁窗口，Box 随后释放。
    Ok(())
}
