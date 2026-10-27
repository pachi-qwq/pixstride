//! UI 线程的无边框全屏事务；保存一次原窗口状态，不拥有 HWND 或任何 GPU 资源。

use anyhow::{Context, Result, bail};
use windows::{
    Win32::{
        Foundation::{
            ERROR_SUCCESS, GetLastError, HWND, LPARAM, LRESULT, RECT, SetLastError, WPARAM,
        },
        Graphics::Gdi::{
            GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL,
            MONITORINFO, MONITORINFOEXW, MonitorFromRect, MonitorFromWindow,
        },
        System::Threading::GetCurrentThreadId,
        UI::{
            Input::KeyboardAndMouse::{VK_ESCAPE, VK_F11},
            WindowsAndMessaging::*,
        },
    },
    core::Error,
};

use crate::app::WindowContext;

const FRAME_EX: u32 =
    WS_EX_WINDOWEDGE.0 | WS_EX_CLIENTEDGE.0 | WS_EX_DLGMODALFRAME.0 | WS_EX_STATICEDGE.0;

/// 状态只存在于 WindowContext，随 HWND 在 UI 线程结束；过渡失败仍保留可重试的恢复快照。
#[derive(Clone, Copy, Default)]
pub(crate) struct State {
    mode: Mode,
}

#[derive(Clone, Copy, Default)]
enum Mode {
    #[default]
    Windowed,
    Entering,
    Fullscreen(Saved),
    Exiting,
    Recovery(Saved),
}

#[derive(Clone, Copy)]
struct Saved {
    style: u32,
    ex_style: u32,
    placement: WINDOWPLACEMENT,
    // 屏幕坐标仅用于显示器可用性判断；绝不把 placement 的工作区坐标传给 SetWindowPos。
    monitor: Display,
}

#[derive(Clone, Copy)]
struct Display {
    rect: RECT,
    work: RECT,
    device: [u16; 32],
}

impl State {
    #[cfg(test)]
    pub(crate) fn is_fullscreen(self) -> bool {
        matches!(self.mode, Mode::Fullscreen(_))
    }
}

/// 在建立 WindowContext 的独占借用前处理可能同步重入的窗口事务。
///
/// # Safety
/// `context` 必须是 HWND 所属 UI 线程安装的活动 Box 指针；整个调用期间不得销毁 HWND/Box。
/// SetWindowPos/SetWindowPlacement 会重入窗口过程，因此每次指针借用只覆盖字段读写，不跨 Win32 调用。
pub(super) unsafe fn handle_message(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    context: *mut WindowContext,
) -> Option<LRESULT> {
    // SAFETY: 调用方保证活动 Box；这里只复制值，借用在进入任何窗口 API 前结束。
    if unsafe { (*context).close_started } {
        return None;
    }
    // SAFETY: 同一 UI 线程拥有状态，复制不产生跨回调的引用。
    let mode = unsafe { (*context).fullscreen.mode };
    if message == WM_KEYDOWN {
        let key = wparam.0 as u16;
        if key == VK_F11.0 || (key == VK_ESCAPE.0 && !matches!(mode, Mode::Windowed)) {
            // 重复只被消费，不改变现有导航动作；KeyUp 仍走原有导航清理。
            if lparam.0 & (1 << 30) == 0 {
                // SAFETY: 拖动字段先清空再释放 Capture，不在重入期间保留上下文借用。
                let dragging = unsafe { (*context).drag_position.take().is_some() };
                if dragging {
                    // SAFETY: 只释放当前 UI 线程的 Capture；不会操作其他应用的窗口。
                    let _ =
                        unsafe { windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture() };
                }
                // SAFETY: 同一窗口和 UI 线程，事务仅短暂访问 context。
                if let Err(error) = unsafe { change(hwnd, context, key == VK_F11.0) } {
                    eprintln!("全屏切换失败：{error:#}");
                }
            }
            return Some(LRESULT(0));
        }
    }
    if message == WM_DPICHANGED && matches!(mode, Mode::Entering | Mode::Fullscreen(_)) {
        // 全屏按当前显示器物理范围定位，不能使用带窗口边框假设的 DPI 建议矩形。
        // SAFETY: 指针仍有效；Capture 释放前结束字段借用。
        let dragging = unsafe { (*context).drag_position.take().is_some() };
        if dragging {
            // SAFETY: 当前窗口线程释放自身鼠标捕获。
            let _ = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture() };
        }
        if matches!(mode, Mode::Fullscreen(_)) {
            // SAFETY: UI 串行事务，reflow 在调用窗口 API 前写入 Entering 防止递归定位。
            if let Err(error) = unsafe { reflow(hwnd, context) } {
                eprintln!("全屏 DPI 更新失败：{error:#}");
            }
        }
        return Some(LRESULT(0));
    }
    if (message == WM_DISPLAYCHANGE
        || message == WM_MOVE
        || (message == WM_SIZE && wparam.0 != SIZE_MINIMIZED as usize))
        && matches!(mode, Mode::Fullscreen(_))
    {
        // SAFETY: 显示器更新、移动或最小化恢复后覆盖最近显示器；矩形未变时不发 SetWindowPos。
        if let Err(error) = unsafe { reflow(hwnd, context) } {
            eprintln!("全屏显示器更新失败：{error:#}");
        }
    }
    None
}

unsafe fn change(hwnd: HWND, context: *mut WindowContext, toggle: bool) -> Result<()> {
    validate_owner(hwnd)?;
    // SAFETY: 只复制当前状态，借用不跨下面的同步窗口调用。
    let mode = unsafe { (*context).fullscreen.mode };
    match mode {
        Mode::Windowed if toggle => {
            let saved = capture(hwnd)?;
            // SAFETY: UI 线程独占此字段；先发布过渡，重入 DPI 不会执行普通窗口定位。
            unsafe { (*context).fullscreen.mode = Mode::Entering };
            if let Err(error) = cover(hwnd, saved) {
                // SAFETY: 回滚恢复普通窗口，DPI 重入应使用普通窗口的建议矩形。
                unsafe { (*context).fullscreen.mode = Mode::Exiting };
                let restored = restore(hwnd, saved);
                // SAFETY: 事务返回后再提交状态；失败保留原快照，不谎报已全屏。
                unsafe {
                    (*context).fullscreen.mode = if restored.is_ok() {
                        Mode::Windowed
                    } else {
                        Mode::Recovery(saved)
                    };
                }
                return Err(error).context(match restored {
                    Ok(()) => "进入全屏失败，已恢复原窗口",
                    Err(_) => "进入全屏及回滚均失败，F11/Esc 可重试恢复",
                });
            }
            // SAFETY: 样式、位置及实际矩形均确认成功，才标记 Fullscreen。
            unsafe { (*context).fullscreen.mode = Mode::Fullscreen(saved) };
        }
        Mode::Fullscreen(saved) | Mode::Recovery(saved) => {
            // SAFETY: 明确退出阶段，重入 DPI 使用普通窗口建议矩形。
            unsafe { (*context).fullscreen.mode = Mode::Exiting };
            if let Err(error) = restore(hwnd, saved) {
                // 恢复失败尝试一次回到无边框；不循环重试，不丢失最初的快照。
                // SAFETY: 先提交过渡状态，避免 cover 内的 DPI 消息重置尺寸。
                unsafe { (*context).fullscreen.mode = Mode::Entering };
                let covered = cover(hwnd, saved).is_ok();
                // SAFETY: 所有同步窗口调用结束后才确认实际状态。
                unsafe {
                    (*context).fullscreen.mode = if covered {
                        Mode::Fullscreen(saved)
                    } else {
                        Mode::Recovery(saved)
                    };
                }
                return Err(error).context("退出全屏失败，原恢复快照仍保留，可按 F11/Esc 重试");
            }
            // SAFETY: 成功恢复后清理本次快照。
            unsafe { (*context).fullscreen.mode = Mode::Windowed };
        }
        // 同步重入时不再启动第二个事务，不能覆盖最初的快照。
        Mode::Windowed | Mode::Entering | Mode::Exiting => {}
    }
    Ok(())
}

unsafe fn reflow(hwnd: HWND, context: *mut WindowContext) -> Result<()> {
    // SAFETY: 调用方保证窗口和 context 存活，仅复制状态。
    let Mode::Fullscreen(saved) = (unsafe { (*context).fullscreen.mode }) else {
        return Ok(());
    };
    // SAFETY: 当前 UI 线程创建的 HWND；最小化期间不擅自恢复可见性。
    if unsafe { IsIconic(hwnd) }.as_bool() {
        return Ok(());
    }
    let target = display_for_window(hwnd)?.rect;
    if window_rect(hwnd)? == target {
        return Ok(());
    }
    // SAFETY: 过渡标记阻止同步 WM_SIZE/WM_DPICHANGED 递归启动第二次定位。
    unsafe { (*context).fullscreen.mode = Mode::Entering };
    let result = position(hwnd, target);
    // SAFETY: 定位调用已结束；失败保留快照供恢复，不标记一个未经确认的全屏。
    unsafe {
        (*context).fullscreen.mode = if result.is_ok() {
            Mode::Fullscreen(saved)
        } else {
            Mode::Recovery(saved)
        };
    }
    result
}

fn validate_owner(hwnd: HWND) -> Result<()> {
    // SAFETY: 只查询句柄，不解引用外部内存；无效 HWND 返回线程 ID 0。
    let owner = unsafe { GetWindowThreadProcessId(hwnd, None) };
    // SAFETY: GetCurrentThreadId 无参数且不访问调用方缓冲区。
    if owner == 0 || owner != unsafe { GetCurrentThreadId() } {
        bail!("全屏操作只能由活动 HWND 的所属 UI 线程执行");
    }
    Ok(())
}

fn capture(hwnd: HWND) -> Result<Saved> {
    // SAFETY: 当前 UI 线程拥有有效窗口；拒绝对已经最小化的窗口开启新事务。
    if unsafe { IsIconic(hwnd) }.as_bool() {
        bail!("最小化窗口不开始新的全屏事务");
    }
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: placement 长度正确，栈缓冲区可写，系统同步填写。
    unsafe { GetWindowPlacement(hwnd, &mut placement) }?;
    Ok(Saved {
        style: get_style(hwnd, GWL_STYLE)?,
        ex_style: get_style(hwnd, GWL_EXSTYLE)?,
        placement,
        monitor: display_for_window(hwnd)?,
    })
}

fn cover(hwnd: HWND, saved: Saved) -> Result<()> {
    let target = display_for_window(hwnd)?.rect;
    set_style(
        hwnd,
        GWL_STYLE,
        saved.style & !(WS_OVERLAPPEDWINDOW.0 | WS_MAXIMIZE.0),
    )?;
    set_style(hwnd, GWL_EXSTYLE, get_style(hwnd, GWL_EXSTYLE)? & !FRAME_EX)?;
    position(hwnd, target)?;
    if window_rect(hwnd)? != target {
        bail!("窗口实际范围与显示器全屏矩形不一致");
    }
    Ok(())
}

fn restore(hwnd: HWND, saved: Saved) -> Result<()> {
    set_style(hwnd, GWL_STYLE, saved.style)?;
    // 只恢复曾修改的扩展边框位，保留其余扩展样式；不增加 Topmost。
    let ex = (get_style(hwnd, GWL_EXSTYLE)? & !FRAME_EX) | (saved.ex_style & FRAME_EX);
    set_style(hwnd, GWL_EXSTYLE, ex)?;
    checkpoint(Step::Placement)?;
    // SAFETY: saved 来自当前 HWND 的 GetWindowPlacement，length 未改变；工作区坐标原样交还同一 API。
    // SetWindowPlacement 自行处理完全屏外的旧位置和显示器配置变化。
    unsafe { SetWindowPlacement(hwnd, &saved.placement) }?;
    checkpoint(Step::Frame)?;
    // SAFETY: 仅要求重新计算非客户区，不重复设置位置或 Z 序。
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED
                | SWP_NOMOVE
                | SWP_NOSIZE
                | SWP_NOZORDER
                | SWP_NOACTIVATE
                | SWP_NOOWNERZORDER,
        )
    }?;
    // 原显示器已不可用时，对普通窗口再约束到当前最近的工作区；最大化交给原生 Placement。
    // SAFETY: RECT 为栈上有效缓冲区，只用于查询当前显示器，不复用失效 HMONITOR。
    let old_monitor = unsafe { MonitorFromRect(&saved.monitor.rect, MONITOR_DEFAULTTONULL) };
    let original_available = !old_monitor.is_invalid()
        && display(old_monitor).is_ok_and(|m| m.device == saved.monitor.device);
    // SAFETY: HWND 仍由本 UI 线程持有，只查询当前 show state。
    if !original_available && !unsafe { IsZoomed(hwnd) }.as_bool() {
        let rect = window_rect(hwnd)?;
        let safe = fit_to_work(rect, display_for_window(hwnd)?.work)?;
        if rect != safe {
            position(hwnd, safe)?;
        }
    }
    Ok(())
}

fn display_for_window(hwnd: HWND) -> Result<Display> {
    // SAFETY: 有效 HWND，Win32 按最大相交面积选择，完全屏外时选择最近显示器。
    display(unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) })
}

fn display(monitor: HMONITOR) -> Result<Display> {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: info 的头和 cbSize 与 MONITORINFOEXW 一致，系统只在调用期间写入该栈缓冲区。
    if !unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
        return Err(Error::from_thread()).context("读取目标显示器范围失败");
    }
    dimensions(info.monitorInfo.rcMonitor)?;
    dimensions(info.monitorInfo.rcWork)?;
    Ok(Display {
        rect: info.monitorInfo.rcMonitor,
        work: info.monitorInfo.rcWork,
        device: info.szDevice,
    })
}

pub(super) fn window_rect(hwnd: HWND) -> Result<RECT> {
    let mut rect = RECT::default();
    // SAFETY: 有效 HWND 或由 Win32 返回失败；rect 在本次同步查询期间可写。
    unsafe { GetWindowRect(hwnd, &mut rect) }?;
    Ok(rect)
}

pub(super) fn get_style(hwnd: HWND, index: WINDOW_LONG_PTR_INDEX) -> Result<u32> {
    // SAFETY: 只查询窗口样式槽；先清 LastError 以区分合法零样式和失败。
    unsafe { SetLastError(ERROR_SUCCESS) };
    // SAFETY: index 为 GWL_STYLE/GWL_EXSTYLE，返回整数而非借用。
    let value = unsafe { GetWindowLongPtrW(hwnd, index) };
    // SAFETY: 查询同一线程刚执行的 Win32 调用错误码。
    if value == 0 && unsafe { GetLastError() } != ERROR_SUCCESS {
        return Err(Error::from_thread()).context("读取窗口样式失败");
    }
    Ok(value as u32)
}

fn set_style(hwnd: HWND, index: WINDOW_LONG_PTR_INDEX, value: u32) -> Result<()> {
    checkpoint(if index == GWL_STYLE {
        Step::Style
    } else {
        Step::ExStyle
    })?;
    // SAFETY: 同一 UI 线程修改自有窗口的样式整数槽；清错误码允许原样式为零。
    unsafe { SetLastError(ERROR_SUCCESS) };
    // SAFETY: 只设置 Style/ExStyle，不替换过程或上下文指针。
    let old = unsafe { SetWindowLongPtrW(hwnd, index, value as isize) };
    // SAFETY: 读取当前调用的线程错误码。
    if old == 0 && unsafe { GetLastError() } != ERROR_SUCCESS {
        return Err(Error::from_thread()).context("设置窗口样式失败");
    }
    Ok(())
}

fn dimensions(rect: RECT) -> Result<(i32, i32)> {
    let width = i64::from(rect.right) - i64::from(rect.left);
    let height = i64::from(rect.bottom) - i64::from(rect.top);
    if width <= 0 || height <= 0 || width > i64::from(i32::MAX) || height > i64::from(i32::MAX) {
        bail!("窗口/显示器矩形尺寸无效");
    }
    Ok((width as i32, height as i32))
}

fn position(hwnd: HWND, rect: RECT) -> Result<()> {
    let (width, height) = dimensions(rect)?;
    checkpoint(Step::Position)?;
    // SAFETY: 本 UI 线程拥有 HWND；矩形为验证过的物理屏幕坐标，不改变激活、owner 或 Z 序。
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            rect.left,
            rect.top,
            width,
            height,
            SWP_FRAMECHANGED | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
    }?;
    Ok(())
}

/// 仅用于失去原显示器后的安全回退；i64 中间值避免负坐标与跨度溢出。
fn fit_to_work(rect: RECT, work: RECT) -> Result<RECT> {
    let (width, height) = dimensions(rect)?;
    let (ww, wh) = dimensions(work)?;
    let width = i64::from(width.min(ww));
    let height = i64::from(height.min(wh));
    let left = i64::from(rect.left).clamp(i64::from(work.left), i64::from(work.right) - width);
    let top = i64::from(rect.top).clamp(i64::from(work.top), i64::from(work.bottom) - height);
    Ok(RECT {
        left: left as i32,
        top: top as i32,
        right: (left + width) as i32,
        bottom: (top + height) as i32,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Style,
    ExStyle,
    Position,
    Placement,
    Frame,
}

fn checkpoint(_step: Step) -> Result<()> {
    #[cfg(test)]
    FAILURE.with(|steps| {
        let mut steps = steps.borrow_mut();
        if steps.front() == Some(&_step) {
            steps.pop_front();
            bail!("确定性注入窗口事务失败");
        }
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FAILURE: std::cell::RefCell<std::collections::VecDeque<Step>> = const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

#[cfg(test)]
#[path = "fullscreen_tests.rs"]
pub(crate) mod tests;
