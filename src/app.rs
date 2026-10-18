//! 创建应用状态、启动渲染线程并运行 Win32 消息循环。

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::Instant,
};

use anyhow::{Context, Result, anyhow};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_EVENT, WAIT_OBJECT_0},
    System::Threading::{CreateEventW, GetCurrentThreadId, INFINITE, SetEvent},
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
    UI::WindowsAndMessaging::{
        DestroyWindow, DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW,
        QS_ALLINPUT, TranslateMessage, WM_QUIT,
    },
};

use crate::{
    diagnostics::{Metric, Metrics},
    renderer, window,
};

#[derive(Clone, Copy)]
pub(crate) struct RenderSnapshot {
    pub(crate) sequence: u64,
    pub(crate) texture_index: usize,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) requested_at: Instant,
    pub(crate) request_origin: RequestOrigin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestOrigin {
    InitialWindowSize,
    KeyInput,
    Resize,
    Minimize,
    Restore,
    Paint,
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

    fn publish(
        &self,
        origin: RequestOrigin,
        update: impl FnOnce(&mut PendingState),
    ) -> std::time::Duration {
        let started = Instant::now();
        {
            let mut pending = self.pending();
            if pending.shutdown {
                return started.elapsed();
            }
            update(&mut pending);
            pending.snapshot.sequence = pending.snapshot.sequence.wrapping_add(1);
            pending.snapshot.requested_at = Instant::now();
            pending.snapshot.request_origin = origin;
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
        let origin = {
            let pending = self.pending();
            if width == 0 || height == 0 {
                RequestOrigin::Minimize
            } else if pending.snapshot.width == 0 || pending.snapshot.height == 0 {
                if pending.snapshot.sequence == 0 {
                    RequestOrigin::InitialWindowSize
                } else {
                    RequestOrigin::Restore
                }
            } else {
                RequestOrigin::Resize
            }
        };
        self.publish(origin, |pending| {
            pending.snapshot.width = width;
            pending.snapshot.height = height;
        });
    }

    pub(crate) fn select_texture(&self, texture_index: usize) -> std::time::Duration {
        self.publish(RequestOrigin::KeyInput, |pending| {
            pending.snapshot.texture_index = texture_index
        })
    }

    pub(crate) fn redraw(&self) {
        self.publish(RequestOrigin::Paint, |_| {});
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

struct CompletionEvent(HANDLE);

impl CompletionEvent {
    fn new() -> Result<Self> {
        // SAFETY: 创建无名称 manual-reset event；UI 线程持有句柄，并在渲染线程 join 后才关闭。
        let event =
            unsafe { CreateEventW(None, true, false, None) }.context("创建渲染线程完成事件失败")?;
        Ok(Self(event))
    }
}

impl Drop for CompletionEvent {
    fn drop(&mut self) {
        // SAFETY: 完成事件由 CompletionEvent 独占；析构前渲染线程已 join，不再访问该句柄。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct CompletionSignal {
    event_value: isize,
    ui_thread_id: u32,
}

impl Drop for CompletionSignal {
    fn drop(&mut self) {
        // SAFETY: UI 线程持有完成事件，且 join 返回前不会关闭；此 guard 只在渲染线程退出时置位。
        let signaled = unsafe { SetEvent(HANDLE(self.event_value as _)) }.is_ok();
        if !signaled {
            // SAFETY: UI 线程 ID 在创建窗口后取得，线程队列已存在；WM_QUIT 是完成事件失效时的退出兜底。
            let _ = unsafe {
                windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    self.ui_thread_id,
                    WM_QUIT,
                    Default::default(),
                    Default::default(),
                )
            };
        }
    }
}

fn run_render_worker(shared: &Shared, render: impl FnOnce() -> Result<()>) {
    match catch_unwind(AssertUnwindSafe(render)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            shared.set_fatal_error(format!("渲染线程失败：{error:#}"));
            shared.request_shutdown();
        }
        Err(_) => {
            shared.set_fatal_error("渲染线程发生 panic；资源已在栈展开时释放".to_owned());
            shared.request_shutdown();
        }
    }
    shared.record_shutdown();
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
    let completion_event = CompletionEvent::new()?;

    let initial_snapshot = RenderSnapshot {
        sequence: 0,
        texture_index: 0,
        width: 0,
        height: 0,
        requested_at: process_started,
        request_origin: RequestOrigin::InitialWindowSize,
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
    // SAFETY: 此 ID 属于当前 UI 线程；创建窗口后其消息队列已建立，并在 render thread 退出前保持存活。
    let ui_thread_id = unsafe { GetCurrentThreadId() };
    let completion_event_value = completion_event.0.0 as isize;
    let render_thread = match thread::Builder::new()
        .name("pixstride-render".to_owned())
        .spawn(move || {
            let _completion_signal = CompletionSignal {
                event_value: completion_event_value,
                ui_thread_id,
            };
            run_render_worker(&render_shared, || {
                renderer::render_thread(raw_hwnd, Arc::clone(&render_shared))
            });
        }) {
        Ok(thread) => thread,
        Err(error) => {
            context.close_started = true;
            // SAFETY: HWND 由当前 UI 线程创建，渲染线程尚未启动；Box 在销毁消息完成前仍然存活。
            if let Err(destroy_error) = unsafe { DestroyWindow(hwnd) } {
                // 销毁失败时窗口过程仍可能读取 GWLP_USERDATA，故保留 Box 直到进程退出，避免悬空指针。
                let _ = Box::into_raw(context);
                return Err(error).context(format!(
                    "创建渲染线程失败，且销毁 HWND 也失败：{destroy_error}"
                ));
            }
            return Err(error).context("创建渲染线程失败");
        }
    };

    window::show_main_window(hwnd);

    let mut message = MSG::default();
    let mut loop_error = None;
    'message_loop: loop {
        // SAFETY: UI 线程持有完成事件到 join 之后；此调用同时等待该事件和当前线程消息队列，不做轮询。
        let result = unsafe {
            MsgWaitForMultipleObjects(Some(&[completion_event.0]), false, INFINITE, QS_ALLINPUT)
        };
        if result == WAIT_OBJECT_0 {
            break;
        }
        if result == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
            // SAFETY: 当前 UI 线程独占其消息队列；输出结构在调用期间有效，PM_REMOVE 取出消息供本线程处理。
            while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                if message.message == WM_QUIT {
                    shared.request_shutdown();
                    break 'message_loop;
                }
                // SAFETY: PeekMessageW 已从当前 UI 队列取出消息；翻译和分发均在窗口所属线程执行。
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            continue;
        }
        loop_error = Some(
            anyhow!(windows::core::Error::from_thread()).context("等待窗口消息或渲染线程完成失败"),
        );
        shared.request_shutdown();
        break;
    }

    if !context.close_started {
        context.close_started = true;
    }
    let join_result = render_thread.join();
    // SAFETY: 完成事件已置位且线程已 join，渲染器已释放所有 D3D11/Swap Chain 引用；HWND 仍由 UI 线程拥有。
    let destroy_result = unsafe { DestroyWindow(hwnd) };

    let report = shared.metrics_report();
    print!("{report}");

    if let Err(destroy_error) = destroy_result {
        // 销毁失败时操作系统仍可能向 HWND 分发消息，保留窗口过程依赖的 Box，避免释放后悬空访问。
        let _ = Box::into_raw(context);
        return Err(destroy_error).context("渲染线程退出后销毁 Win32 主窗口失败");
    }
    if join_result.is_err() {
        return Err(anyhow!(
            "渲染线程发生未处理的 panic；窗口和诊断资源已完成清理"
        ));
    }
    if let Some(error) = loop_error {
        return Err(error);
    }

    if let Some(error) = shared.fatal_error() {
        return Err(anyhow!(error));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CompletionEvent, CompletionSignal, Metrics, PendingState, RenderSnapshot, Shared,
        WakeEvent, run_render_worker,
    };
    use anyhow::Result;
    use std::{
        sync::{Arc, Mutex},
        time::Instant,
    };
    use windows::Win32::{
        Foundation::WAIT_OBJECT_0,
        System::Threading::{CreateEventW, GetCurrentThreadId, WaitForSingleObject},
    };

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
                    request_origin: super::RequestOrigin::InitialWindowSize,
                },
                redraw: false,
                shutdown: false,
                shutdown_started: None,
            }),
            wake_event: event.0.0 as isize,
            metrics: Mutex::new(Metrics::default()),
            fatal_error: Mutex::new(None),
        });
        Ok((shared, event))
    }

    #[test]
    fn render_worker_panic_sets_shutdown_and_signals_ui_completion() -> Result<()> {
        let (shared, _wake_event) = test_shared()?;
        let completion = CompletionEvent::new()?;
        let signal = CompletionSignal {
            event_value: completion.0.0 as isize,
            // SAFETY: 测试当前线程同时模拟消息循环所有者，且其消息队列在测试期间存活。
            ui_thread_id: unsafe { GetCurrentThreadId() },
        };

        run_render_worker(&shared, || panic!("模拟渲染线程 panic"));
        drop(signal);

        // SAFETY: 测试独占完成事件，WaitForSingleObject 只等待已由 guard 置位的有效句柄，超时为零不会阻塞。
        assert_eq!(
            unsafe { WaitForSingleObject(completion.0, 0) },
            WAIT_OBJECT_0
        );
        let (_, _, shutdown) = shared.take_pending();
        assert!(shutdown);
        assert_eq!(
            shared.fatal_error().as_deref(),
            Some("渲染线程发生 panic；资源已在栈展开时释放")
        );
        assert!(
            shared
                .metrics_report()
                .contains("关闭请求到渲染线程退出: n=1")
        );
        Ok(())
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
        assert_eq!(snapshot.request_origin, super::RequestOrigin::KeyInput);
        assert_eq!((snapshot.width, snapshot.height), (1200, 800));
        assert!(redraw);
        assert!(!shutdown);
        Ok(())
    }

    #[test]
    fn resize_origin_distinguishes_initial_size_minimize_and_restore() -> Result<()> {
        let (shared, _event) = test_shared()?;
        shared.resize(800, 600);
        assert_eq!(
            shared.current_snapshot().request_origin,
            super::RequestOrigin::InitialWindowSize
        );

        shared.resize(900, 650);
        assert_eq!(
            shared.current_snapshot().request_origin,
            super::RequestOrigin::Resize
        );

        shared.resize(0, 0);
        assert_eq!(
            shared.current_snapshot().request_origin,
            super::RequestOrigin::Minimize
        );

        shared.resize(900, 650);
        assert_eq!(
            shared.current_snapshot().request_origin,
            super::RequestOrigin::Restore
        );
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
