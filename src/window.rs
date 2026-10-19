//! 创建 Win32 主窗口，并把输入和尺寸变化转换为最新渲染状态。

use std::{ffi::c_void, time::Instant};

use anyhow::{Context, Result};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{BeginPaint, EndPaint, HBRUSH, PAINTSTRUCT},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{
                GetKeyState, ReleaseCapture, SetCapture, VK_1, VK_CONTROL, VK_F, VK_LEFT, VK_O,
                VK_RIGHT,
            },
            WindowsAndMessaging::{
                CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW,
                DefWindowProcW, GWLP_USERDATA, GetWindowLongPtrW, IDC_ARROW, LoadCursorW,
                RegisterClassW, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos,
                ShowWindow, WM_CAPTURECHANGED, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
                WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
                WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SIZE, WNDCLASSW,
                WS_OVERLAPPEDWINDOW,
            },
        },
    },
    core::{Error, w},
};

use crate::{
    app::{WindowAction, WindowContext},
    diagnostics::Metric,
};

const WINDOW_WIDTH: i32 = 1100;
const WINDOW_HEIGHT: i32 = 760;

pub(crate) fn create_main_window(context: &mut Box<WindowContext>) -> Result<HWND> {
    // SAFETY: 模块句柄来自当前进程；返回的 HINSTANCE 在程序运行期间有效，这里只用于注册窗口类。
    let module = unsafe { GetModuleHandleW(None) }.context("获取模块句柄失败")?;
    let instance = HINSTANCE(module.0);
    let class_name = w!("PixstrideStageAWindow");
    // SAFETY: None 表示加载 IDC_ARROW 标识的系统光标资源；该资源不依赖调用方缓冲区或短期句柄。
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.context("加载系统箭头光标失败")?;
    let window_class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hCursor: cursor,
        hbrBackground: HBRUSH::default(),
        lpszClassName: class_name,
        ..Default::default()
    };

    // SAFETY: `window_class` 及其回调指针在注册期间有效；引用的字符串均为静态 UTF-16 字面量。
    if unsafe { RegisterClassW(&window_class) } == 0 {
        return Err(Error::from_thread()).context("注册 Win32 窗口类失败");
    }

    let context_pointer = (&mut **context) as *mut WindowContext as *const c_void;
    // SAFETY: Box 中的 WindowContext 地址稳定，且其生命周期长于 HWND。WM_NCCREATE 将指针存入 GWLP_USERDATA。
    // SAFETY: UI 线程在 Box 析构前通过 WM_NCDESTROY 清除此指针；创建期间 lpCreateParams 保持有效。
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            class_name,
            w!("Pixstride — Stage A D3D11 test window"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            None,
            None,
            Some(instance),
            Some(context_pointer),
        )
    }
    .context("CreateWindowExW 创建主窗口失败")?;

    Ok(hwnd)
}

pub(crate) fn show_main_window(hwnd: HWND) {
    // SAFETY: 当前 UI 线程只对自己创建的 HWND 调用 ShowWindow。
    unsafe {
        let _ = ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_SHOW);
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: WM_NCCREATE 的 lParam 指向有效的 CREATESTRUCTW；调用方保证创建参数中的指针覆盖窗口生命周期。
        // SAFETY: lParam 仅在当前 WM_NCCREATE 回调中解引用，且来自本模块传给 CreateWindowExW 的结构体。
        let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        let context = create.lpCreateParams as *mut WindowContext;
        // SAFETY: 该指针来自本模块调用 CreateWindowExW 时传入的参数，且可存入指针宽度的窗口数据槽。
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, context as isize) };
    }

    // SAFETY: 只有 UI 线程的窗口过程读取此槽。WM_NCCREATE 初始化，WM_NCDESTROY 清除；消息循环退出后才释放 Box。
    let context_pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowContext;
    if context_pointer.is_null() {
        // SAFETY: WM_NCCREATE 之前或 WM_NCDESTROY 之后的消息使用系统默认处理，不访问应用指针。
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }

    // SAFETY: 该指针指向 WM_NCCREATE 安装的活动 Box；回调由窗口所属 UI 线程串行执行，不会并发修改它。
    let context = unsafe { &mut *context_pointer };

    match message {
        WM_MOUSEWHEEL if !context.close_started => {
            let delta = ((wparam.0 >> 16) as u16) as i16;
            context
                .shared
                .zoom_by(1.2f32.powf(f32::from(delta) / 120.0));
            LRESULT(0)
        }
        WM_LBUTTONDOWN if !context.close_started => {
            context.drag_position = Some(mouse_position(lparam));
            // SAFETY: 当前 UI 线程的 HWND 仍有效；捕获在松键、失焦或系统取消时释放。
            unsafe {
                SetCapture(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE if !context.close_started => {
            if let Some(previous) = context.drag_position {
                let next = mouse_position(lparam);
                context
                    .shared
                    .pan_by((next.0 - previous.0) as f32, (next.1 - previous.1) as f32);
                context.drag_position = Some(next);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_CAPTURECHANGED => {
            context.drag_position = None;
            if message == WM_LBUTTONUP {
                // SAFETY: ReleaseCapture 只释放当前线程鼠标捕获，不访问外部缓冲区。
                let _ = unsafe { ReleaseCapture() };
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if !context.close_started {
                let dimensions = lparam.0 as u32;
                context.shared.resize(dimensions & 0xffff, dimensions >> 16);
            }
            LRESULT(0)
        }
        WM_KEYDOWN if !context.close_started => {
            let input_started = Instant::now();
            // SAFETY: GetKeyState 查询当前 UI 线程键盘状态，不接收指针或外部缓冲区。
            let control_down = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            context.action = match (wparam.0 as u32, control_down) {
                (key, true) if key == VK_O.0 as u32 => Some(WindowAction::Open),
                (key, _) if key == VK_LEFT.0 as u32 => Some(WindowAction::Previous),
                (key, _) if key == VK_RIGHT.0 as u32 => Some(WindowAction::Next),
                (key, false) if key == VK_F.0 as u32 => Some(WindowAction::Fit),
                (key, false) if key == VK_1.0 as u32 => Some(WindowAction::NativePixels),
                _ => None,
            };
            context
                .shared
                .record(Metric::InputHandling, input_started.elapsed());
            LRESULT(0)
        }
        WM_KEYUP
            if !context.close_started
                && matches!(wparam.0 as u16, key if key == VK_LEFT.0 || key == VK_RIGHT.0) =>
        {
            let direction = if wparam.0 as u16 == VK_LEFT.0 { -1 } else { 1 };
            context.navigation_key_released(direction, Instant::now());
            LRESULT(0)
        }
        WM_KILLFOCUS if !context.close_started => {
            context.drag_position = None;
            // SAFETY: 失焦时放弃当前线程的捕获，避免恢复焦点后错误延续旧拖动。
            let _ = unsafe { ReleaseCapture() };
            context.shared.end_navigation_progress();
            context.action = Some(WindowAction::StopNavigation);
            LRESULT(0)
        }
        WM_DPICHANGED if !context.close_started => {
            // SAFETY: WM_DPICHANGED 规定 lParam 指向建议的 RECT，且仅在本回调期间有效；SetWindowPos 会同步复制坐标。
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            let width = suggested.right - suggested.left;
            let height = suggested.bottom - suggested.top;
            // SAFETY: `hwnd` 属于当前 UI 线程，建议矩形使用屏幕像素坐标。
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
            LRESULT(0)
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            // SAFETY: BeginPaint 与 EndPaint 在拥有窗口的 UI 线程上配对调用，完成 WM_PAINT 验证。
            unsafe {
                BeginPaint(hwnd, &mut paint);
                let _ = EndPaint(hwnd, &paint);
            }
            if !context.close_started {
                context.shared.redraw();
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_CLOSE => {
            if !context.close_started {
                context.close_started = true;
                context.shared.request_shutdown();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: PostQuitMessage 只向当前 UI 线程的消息队列投递退出消息。
            unsafe { windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // SAFETY: 调用方释放 Box 前先清空窗口回调上下文。
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            // SAFETY: 窗口数据槽已清空，DefWindowProcW 负责剩余的非客户区销毁过程。
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => {
            // SAFETY: 未处理的消息继续使用 Win32 默认行为。
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
    }
}

fn mouse_position(value: LPARAM) -> (i32, i32) {
    (
        (value.0 as u16 as i16) as i32,
        ((value.0 >> 16) as u16 as i16) as i32,
    )
}
