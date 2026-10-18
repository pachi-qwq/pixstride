//! 创建应用状态、启动渲染线程并运行 Win32 消息循环。

use std::{
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::Instant,
};

use anyhow::{Context, Result, anyhow};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, HWND},
    System::Threading::CreateEventW,
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
    UI::WindowsAndMessaging::{
        DestroyWindow, DispatchMessageW, GetMessageW, MSG, PostMessageW, TranslateMessage,
    },
};

use crate::{
    diagnostics::{Metric, Metrics},
    renderer, window,
};

pub(crate) const WM_RENDER_STOPPED: u32 = 0x8000 + 1;

#[derive(Clone, Copy)]
pub(crate) struct RenderSnapshot {
    pub(crate) sequence: u64,
    pub(crate) texture_index: usize,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) requested_at: Instant,
}

struct PendingState {
    snapshot: RenderSnapshot,
    redraw: bool,
    shutdown: bool,
    shutdown_started: Option<Instant>,
}

/// UI 与 Render Thread 共享的有界状态。
///
/// 互斥锁中只保存最新快照和几个标志，不保存逐条输入消息。Windows auto-reset event 仅负责唤醒；
/// 多次输入在渲染线程读取前会自然合并成最新状态。`wake_event` 以整数保存，避免把 COM 或 HWND
/// 包装器跨线程传递；事件句柄在渲染线程退出并 join 后才由 UI 线程关闭。
pub(crate) struct Shared {
    pending: Mutex<PendingState>,
    wake_event: isize,
    metrics: Mutex<Metrics>,
    fatal_error: Mutex<Option<String>>,
}

impl Shared {
    fn pending(&self) -> MutexGuard<'_, PendingState> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn metrics_report(&self) -> String {
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .report()
    }

    pub(crate) fn take_pending(&self) -> (RenderSnapshot, bool, bool) {
        let mut pending = self.pending();
        let redraw = std::mem::take(&mut pending.redraw);
        (pending.snapshot, redraw, pending.shutdown)
    }

    pub(crate) fn current_snapshot(&self) -> RenderSnapshot {
        self.pending().snapshot
    }

    pub(crate) fn wake_event(&self) -> isize {
        self.wake_event
    }

    fn publish(&self, update: impl FnOnce(&mut PendingState)) -> std::time::Duration {
        let started = Instant::now();
        {
            let mut pending = self.pending();
            if pending.shutdown {
                return started.elapsed();
            }
            update(&mut pending);
            pending.snapshot.sequence = pending.snapshot.sequence.wrapping_add(1);
            pending.snapshot.requested_at = Instant::now();
            pending.redraw = true;
        }

        // SAFETY: `wake_event` 是窗口创建前生成的 auto-reset event。UI 作用域会在渲染线程 join 后才关闭它。
        // SAFETY: SetEvent 可跨线程调用；当前 UI 线程只传入仍由 WakeEvent 持有的有效句柄。
        unsafe {
            let _ = windows::Win32::System::Threading::SetEvent(HANDLE(self.wake_event as _));
        }
        started.elapsed()
    }

    pub(crate) fn resize(&self, width: u32, height: u32) {
        self.publish(|pending| {
            pending.snapshot.width = width;
            pending.snapshot.height = height;
        });
    }

    pub(crate) fn select_texture(&self, texture_index: usize) -> std::time::Duration {
        self.publish(|pending| pending.snapshot.texture_index = texture_index)
    }

    pub(crate) fn redraw(&self) {
        self.publish(|_| {});
    }

    pub(crate) fn request_shutdown(&self) {
        {
            let mut pending = self.pending();
            if pending.shutdown {
                return;
            }
            pending.shutdown = true;
            pending.shutdown_started = Some(Instant::now());
        }

        // SAFETY: 与 publish 相同，事件由外层 UI 生命周期持有，join 前不会关闭。
        unsafe {
            let _ = windows::Win32::System::Threading::SetEvent(HANDLE(self.wake_event as _));
        }
    }

    pub(crate) fn record(&self, metric: Metric, elapsed: std::time::Duration) {
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(metric, elapsed);
    }

    pub(crate) fn record_shutdown(&self) {
        let started = self.pending().shutdown_started;
        if let Some(started) = started {
            self.record(Metric::Shutdown, started.elapsed());
        }
    }

    pub(crate) fn set_fatal_error(&self, error: String) {
        *self
            .fatal_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
    }

    fn fatal_error(&self) -> Option<String> {
        self.fatal_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

pub(crate) struct WindowContext {
    pub(crate) shared: Arc<Shared>,
    pub(crate) close_started: bool,
    pub(crate) render_stopped: bool,
}

struct WakeEvent(HANDLE);

impl Drop for WakeEvent {
    fn drop(&mut self) {
        // SAFETY: WakeEvent 独占 CreateEventW 返回的句柄。它只会在渲染线程 join 后析构，届时已无等待者使用句柄。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub(crate) fn run() -> Result<()> {
    let process_started = Instant::now();
    // SAFETY: 本进程在创建任何 HWND 前设置 Per-Monitor DPI Awareness，后续不会再次更改进程级 DPI 模式。
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            .context("设置 Per-Monitor DPI Awareness V2 失败")?;
    }

    let wake_handle =
        // SAFETY: 空安全属性使用系统默认 ACL；无名称 auto-reset event 由 WakeEvent 在渲染线程 join 后关闭。
        unsafe { CreateEventW(None, false, false, None) }.context("创建渲染唤醒事件失败")?;
    let wake_event = WakeEvent(wake_handle);

    let initial_snapshot = RenderSnapshot {
        sequence: 0,
        texture_index: 0,
        width: 0,
        height: 0,
        requested_at: process_started,
    };
    let shared = Arc::new(Shared {
        pending: Mutex::new(PendingState {
            snapshot: initial_snapshot,
            redraw: true,
            shutdown: false,
            shutdown_started: None,
        }),
        wake_event: wake_event.0.0 as isize,
        metrics: Mutex::new(Metrics::default()),
        fatal_error: Mutex::new(None),
    });

    let mut context = Box::new(WindowContext {
        shared: Arc::clone(&shared),
        close_started: false,
        render_stopped: false,
    });
    let hwnd = window::create_main_window(&mut context).context("创建 Win32 主窗口失败")?;
    let startup_elapsed = process_started.elapsed();
    shared.record(Metric::Startup, startup_elapsed);
    println!(
        "Startup to CreateWindowExW return: {} us",
        startup_elapsed.as_micros()
    );

    let render_shared = Arc::clone(&shared);
    let raw_hwnd = hwnd.0 as isize;
    let render_thread = match thread::Builder::new()
        .name("pixstride-render".to_owned())
        .spawn(move || {
            let result = renderer::render_thread(raw_hwnd, Arc::clone(&render_shared));
            if let Err(error) = result {
                render_shared.set_fatal_error(format!("渲染线程失败：{error:#}"));
                render_shared.request_shutdown();
            }
            render_shared.record_shutdown();

            // SAFETY: UI 收到 WM_RENDER_STOPPED 前会保持 HWND 有效。渲染线程先释放全部 D3D11 资源，再发送完成通知。
            let _ = unsafe {
                PostMessageW(
                    Some(HWND(raw_hwnd as _)),
                    WM_RENDER_STOPPED,
                    Default::default(),
                    Default::default(),
                )
            };
        }) {
        Ok(thread) => thread,
        Err(error) => {
            // SAFETY: HWND 由 UI 线程创建，渲染线程尚未启动；DestroyWindow 分发销毁消息期间，WindowContext Box 仍存活。
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(error).context("创建渲染线程失败");
        }
    };

    window::show_main_window(hwnd);

    let mut message = MSG::default();
    loop {
        // SAFETY: 当前 UI 线程拥有此消息队列，并且每条取出的消息只分发一次。
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 == 0 {
            break;
        } else if result.0 == -1 {
            let error = windows::core::Error::from_thread();
            shared.request_shutdown();
            let _ = render_thread.join();
            // SAFETY: 渲染线程已 join，并释放了所有引用 HWND 的资源。
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(error).context("Win32 消息循环失败");
        } else {
            // SAFETY: `message` 由 GetMessageW 填充，并在同一线程的翻译和分发期间保持有效。
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    if !context.render_stopped {
        shared.request_shutdown();
    }
    render_thread
        .join()
        .map_err(|_| anyhow!("渲染线程发生未捕获的 panic"))?;
    if !context.render_stopped {
        // SAFETY: 渲染线程已 join 并释放 D3D11 资源；此时销毁 HWND 不会与 Swap Chain 操作并发。
        let _ = unsafe { DestroyWindow(hwnd) };
    }

    let report = shared.metrics_report();
    print!("{report}");

    if let Some(error) = shared.fatal_error() {
        return Err(anyhow!(error));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PendingState, RenderSnapshot, Shared, WakeEvent};
    use std::{
        sync::{Arc, Mutex},
        time::Instant,
    };
    use windows::{Win32::System::Threading::CreateEventW, core::Result};

    fn test_shared() -> Result<(Arc<Shared>, WakeEvent)> {
        // SAFETY: 测试独占此无名称 auto-reset event，并在同步状态断言结束后关闭；没有工作线程等待它。
        let event = WakeEvent(unsafe { CreateEventW(None, false, false, None)? });
        let shared = Arc::new(Shared {
            pending: Mutex::new(PendingState {
                snapshot: RenderSnapshot {
                    sequence: 0,
                    texture_index: 0,
                    width: 0,
                    height: 0,
                    requested_at: Instant::now(),
                },
                redraw: false,
                shutdown: false,
                shutdown_started: None,
            }),
            wake_event: event.0.0 as isize,
            metrics: Mutex::default(),
            fatal_error: Mutex::new(None),
        });
        Ok((shared, event))
    }

    #[test]
    fn rapid_updates_keep_latest_snapshot_instead_of_queueing_old_states() -> Result<()> {
        let (shared, _event) = test_shared()?;
        shared.select_texture(1);
        shared.resize(1200, 800);
        shared.select_texture(0);

        let (snapshot, redraw, shutdown) = shared.take_pending();
        assert_eq!(snapshot.sequence, 3);
        assert_eq!(snapshot.texture_index, 0);
        assert_eq!((snapshot.width, snapshot.height), (1200, 800));
        assert!(redraw);
        assert!(!shutdown);
        Ok(())
    }

    #[test]
    fn shutdown_is_terminal_for_later_state_updates() -> Result<()> {
        let (shared, _event) = test_shared()?;
        shared.request_shutdown();
        shared.select_texture(1);

        let (snapshot, redraw, shutdown) = shared.take_pending();
        assert_eq!(snapshot.sequence, 0);
        assert_eq!(snapshot.texture_index, 0);
        assert!(!redraw);
        assert!(shutdown);
        Ok(())
    }
}
