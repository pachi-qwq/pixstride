//! 创建应用状态、启动渲染线程并运行 Win32 消息循环。

use std::{
    collections::VecDeque,
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::Instant,
};

use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_EVENT, WAIT_FAILED, WAIT_OBJECT_0},
    System::Threading::{CreateEventW, GetCurrentThreadId, INFINITE, SetEvent},
    UI::Controls::Dialogs::{
        CommDlgExtendedError, GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR,
        OFN_PATHMUSTEXIST, OPENFILENAMEW,
    },
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
    UI::WindowsAndMessaging::{
        DestroyWindow, DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW,
        QS_ALLINPUT, TranslateMessage, WM_QUIT,
    },
    UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW, SetWindowTextW},
};
use windows::core::w;

use crate::{
    diagnostics::{Counter, Metric, Metrics},
    image::{
        DecodedImage, ImageBudget,
        navigation::{ImageDirectory, absolute_path},
        worker::{ImageLoader, LoaderMessage},
    },
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
    pub(crate) view_mode: ViewMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewMode {
    Fit,
    NativePixels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestOrigin {
    InitialWindowSize,
    KeyInput,
    Resize,
    Minimize,
    Restore,
    Paint,
    ImageReady,
}

struct PendingState {
    snapshot: RenderSnapshot,
    redraw: bool,
    shutdown: bool,
    shutdown_started: Option<Instant>,
    image_update: Option<Arc<DecodedImage>>,
    preload_uploads: VecDeque<Arc<DecodedImage>>,
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

    pub(crate) fn take_pending(
        &self,
    ) -> (
        RenderSnapshot,
        bool,
        bool,
        Option<Arc<DecodedImage>>,
        Vec<Arc<DecodedImage>>,
    ) {
        let mut pending = self.pending();
        let redraw = std::mem::take(&mut pending.redraw);
        (
            pending.snapshot,
            redraw,
            pending.shutdown,
            pending.image_update.take(),
            pending.preload_uploads.drain(..).collect(),
        )
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

    pub(crate) fn set_image(&self, image: Arc<DecodedImage>, requested_at: Instant) {
        {
            let mut pending = self.pending();
            if pending.shutdown {
                return;
            }
            pending.snapshot.sequence = pending.snapshot.sequence.wrapping_add(1);
            pending.snapshot.requested_at = requested_at;
            pending.snapshot.request_origin = RequestOrigin::ImageReady;
            pending.image_update = Some(image);
            pending.redraw = true;
        }
        // SAFETY: 事件句柄由 UI 生命周期持有，并且只会在渲染线程 join 后关闭。
        unsafe {
            let _ = SetEvent(HANDLE(self.wake_event as _));
        }
    }

    pub(crate) fn preload_image(&self, image: Arc<DecodedImage>) {
        {
            let mut pending = self.pending();
            if pending.shutdown {
                return;
            }
            if !pending
                .preload_uploads
                .iter()
                .any(|existing| existing.source == image.source)
            {
                if pending.preload_uploads.len() == 2 {
                    pending.preload_uploads.pop_front();
                }
                pending.preload_uploads.push_back(image);
            }
        }
        // 预加载只唤醒渲染线程处理资源，不要求提交一帧。
        // SAFETY: 与工作事件的其他调用相同，句柄由 UI 外层持有到渲染线程 join 后。
        unsafe {
            let _ = SetEvent(HANDLE(self.wake_event as _));
        }
    }

    pub(crate) fn set_view_mode(&self, mode: ViewMode) {
        self.publish(RequestOrigin::Paint, |pending| {
            pending.snapshot.view_mode = mode;
        });
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

    pub(crate) fn increment(&self, counter: Counter) {
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .increment(counter);
    }

    pub(crate) fn set_cpu_cache_bytes(&self, cpu: u64) {
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_cpu_cache_bytes(cpu);
    }

    pub(crate) fn set_gpu_cache_bytes(&self, gpu: u64) {
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_gpu_cache_bytes(gpu);
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
    pub(crate) action: Option<WindowAction>,
    navigation: UiNavigation,
}

#[derive(Clone, Copy)]
pub(crate) enum WindowAction {
    Open,
    Previous,
    Next,
    Fit,
    NativePixels,
}

#[derive(Default)]
struct UiNavigation {
    generation: u64,
    current_path: Option<PathBuf>,
    directory: Option<ImageDirectory>,
    pending_steps: i32,
    requested_at: Option<Instant>,
    fallback: Option<(Option<PathBuf>, Option<ImageDirectory>)>,
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
        view_mode: ViewMode::Fit,
    };
    let shared = Arc::new(Shared {
        pending: Mutex::new(PendingState {
            snapshot: initial_snapshot,
            redraw: true,
            shutdown: false,
            shutdown_started: None,
            image_update: None,
            preload_uploads: VecDeque::new(),
        }),
        wake_event: wake_event.0.0 as isize,
        metrics: Mutex::new(Metrics::default()),
        fatal_error: Mutex::new(None),
    });

    let mut loader = ImageLoader::start(ImageBudget::default(), Arc::clone(&shared))?;
    let mut context = Box::new(WindowContext {
        shared: Arc::clone(&shared),
        close_started: false,
        action: None,
        navigation: UiNavigation::default(),
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
    let mut render_thread = Some(
        match thread::Builder::new()
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
        },
    );

    window::show_main_window(hwnd);

    let initial_argument = std::env::args_os().nth(1).map(PathBuf::from);
    let mut startup_argument_error = None;
    let has_extra_arguments = std::env::args_os().nth(2).is_some();
    if has_extra_arguments {
        startup_argument_error = Some("Pixstride 一次只接受一个图片路径参数。".to_owned());
    }
    if let Some(path) = initial_argument.filter(|_| !has_extra_arguments) {
        if let Err(error) = request_open(&mut context, &loader, hwnd, path) {
            startup_argument_error = Some(format!("无法打开命令行图片：{error:#}"));
        }
    }
    if let Some(error) = startup_argument_error {
        show_error(hwnd, &error);
    }

    let mut message = MSG::default();
    let mut loop_error = None;
    let mut render_done = false;
    let mut loader_done = false;
    let mut loader_stop_sent = false;
    let mut render_join_error = None;

    while !render_done || !loader_done {
        let mut handles = Vec::with_capacity(3);
        if !render_done {
            handles.push(completion_event.0);
        }
        handles.push(loader.ready_handle());
        if !loader_done {
            handles.push(loader.completed_handle());
        }
        let message_index = handles.len() as u32;
        let render_index = (!render_done).then_some(0usize);
        let ready_index = usize::from(!render_done);
        let completed_index = ready_index + 1;
        // SAFETY: 三个事件句柄在各自线程 join 前保持有效；同时等待当前 UI 消息队列，不轮询。
        let result =
            unsafe { MsgWaitForMultipleObjects(Some(&handles), false, INFINITE, QS_ALLINPUT) };
        let render_signaled = render_index.is_some() && result == WAIT_OBJECT_0;
        if render_signaled {
            if let Some(thread) = render_thread.take() {
                render_join_error = thread.join().err();
            } else {
                loop_error = Some(anyhow!("渲染线程完成事件触发时找不到线程句柄"));
            }
            render_done = true;
            context.close_started = true;
        } else if result == WAIT_EVENT(WAIT_OBJECT_0.0 + ready_index as u32) {
            process_loader_messages(&mut context, &loader, hwnd);
        } else if !loader_done && result == WAIT_EVENT(WAIT_OBJECT_0.0 + completed_index as u32) {
            loader_done = true;
            if let Err(error) = loader.join() {
                loop_error = Some(error.context("等待 WIC 解码线程退出失败"));
                context.close_started = true;
                shared.request_shutdown();
            }
            process_loader_messages(&mut context, &loader, hwnd);
        } else if result == WAIT_EVENT(WAIT_OBJECT_0.0 + message_index) {
            // SAFETY: UI 线程独占消息队列；取出的消息仅在本线程翻译和分发。
            while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                if message.message == WM_QUIT {
                    context.close_started = true;
                    shared.request_shutdown();
                    continue;
                }
                // SAFETY: PeekMessageW 已从本线程队列取出消息，窗口过程仍在 HWND 所属线程执行。
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                process_window_action(&mut context, &loader, hwnd);
            }
        } else if result == WAIT_FAILED {
            loop_error = Some(
                anyhow!(windows::core::Error::from_thread())
                    .context("等待窗口、解码结果或工作线程完成失败"),
            );
            context.close_started = true;
            shared.request_shutdown();
        }

        if context.close_started && !loader_stop_sent {
            loader.request_stop();
            loader_stop_sent = true;
        }
    }

    if !context.close_started {
        context.close_started = true;
    }
    if !loader_stop_sent {
        loader.request_stop();
    }
    // SAFETY: 渲染线程和解码线程均已 join；HWND 仍由当前 UI 线程拥有。
    let destroy_result = unsafe { DestroyWindow(hwnd) };

    let report = shared.metrics_report();
    print!("{report}");

    if let Err(destroy_error) = destroy_result {
        let _ = Box::into_raw(context);
        return Err(destroy_error).context("工作线程退出后销毁 Win32 主窗口失败");
    }
    if render_join_error.is_some() {
        return Err(anyhow!("渲染线程发生未处理的 panic；窗口资源已完成清理"));
    }
    if let Some(error) = loop_error {
        return Err(error);
    }
    if let Some(error) = shared.fatal_error() {
        return Err(anyhow!(error));
    }
    Ok(())
}

fn request_open(
    context: &mut WindowContext,
    loader: &ImageLoader,
    hwnd: windows::Win32::Foundation::HWND,
    path: PathBuf,
) -> Result<()> {
    if !crate::image::is_supported_path(&path) {
        bail!("只支持 JPEG、PNG 和 BMP 图片：{}", path.display());
    }
    let path = absolute_path(&path)?;
    context.navigation.generation = context.navigation.generation.wrapping_add(1).max(1);
    if context.navigation.fallback.is_none() {
        context.navigation.fallback = Some((
            context.navigation.current_path.take(),
            context.navigation.directory.take(),
        ));
    }
    context.navigation.current_path = Some(path.clone());
    context.navigation.pending_steps = 0;
    context.navigation.requested_at = Some(Instant::now());
    loader.request_open(context.navigation.generation, path.clone());
    set_window_title(hwnd, &format!("正在打开：{}", path.display()));
    Ok(())
}

fn process_window_action(
    context: &mut WindowContext,
    loader: &ImageLoader,
    hwnd: windows::Win32::Foundation::HWND,
) {
    let Some(action) = context.action.take() else {
        return;
    };
    match action {
        WindowAction::Open => match choose_image(hwnd) {
            Ok(Some(path)) => {
                if let Err(error) = request_open(context, loader, hwnd, path) {
                    show_error(hwnd, &format!("无法打开图片：{error:#}"));
                }
            }
            Ok(None) => {}
            Err(error) => show_error(hwnd, &format!("文件选择失败：{error:#}")),
        },
        WindowAction::Previous => navigate(context, loader, -1),
        WindowAction::Next => navigate(context, loader, 1),
        WindowAction::Fit => context.shared.set_view_mode(ViewMode::Fit),
        WindowAction::NativePixels => context.shared.set_view_mode(ViewMode::NativePixels),
    }
}

fn navigate(context: &mut WindowContext, loader: &ImageLoader, delta: isize) {
    if let Some(directory) = context.navigation.directory.as_mut() {
        if directory.step(delta) {
            let path = directory.current().to_owned();
            schedule_selection(context, loader, path);
        }
    } else if context.navigation.current_path.is_some() {
        context.navigation.pending_steps = context
            .navigation
            .pending_steps
            .saturating_add(delta as i32)
            .clamp(-10_000, 10_000);
    } else {
        let current = context.shared.current_snapshot().texture_index;
        context
            .shared
            .select_texture(if delta < 0 { 0 } else { 1 - current });
    }
}

fn schedule_selection(context: &mut WindowContext, loader: &ImageLoader, path: PathBuf) {
    context.navigation.generation = context.navigation.generation.wrapping_add(1).max(1);
    context.navigation.current_path = Some(path.clone());
    context.navigation.requested_at = Some(Instant::now());
    let preloads = context
        .navigation
        .directory
        .as_ref()
        .map(ImageDirectory::neighbors)
        .unwrap_or_default();
    loader.request_selection(context.navigation.generation, path, preloads);
}

fn process_loader_messages(
    context: &mut WindowContext,
    loader: &ImageLoader,
    hwnd: windows::Win32::Foundation::HWND,
) {
    while let Some(message) = loader.try_receive() {
        match message {
            LoaderMessage::Current {
                generation,
                path,
                result,
            } => {
                if generation != context.navigation.generation
                    || context.navigation.current_path.as_deref() != Some(path.as_path())
                {
                    continue;
                }
                match result {
                    Ok(image) => {
                        if context.navigation.pending_steps != 0 {
                            continue;
                        }
                        context.navigation.fallback = None;
                        context.shared.set_image(
                            image,
                            context.navigation.requested_at.unwrap_or_else(Instant::now),
                        );
                        set_window_title(hwnd, &path.display().to_string());
                    }
                    Err(error) => {
                        if context.navigation.pending_steps != 0 {
                            continue;
                        }
                        restore_navigation(context, hwnd);
                        show_error(hwnd, &format!("图片加载失败：{error}"));
                    }
                }
            }
            LoaderMessage::Directory { generation, result } => {
                if generation != context.navigation.generation {
                    continue;
                }
                match result {
                    Ok(directory) => {
                        context.navigation.directory = Some(directory);
                        let steps = std::mem::take(&mut context.navigation.pending_steps);
                        if steps != 0 {
                            let Some(directory) = context.navigation.directory.as_mut() else {
                                continue;
                            };
                            for _ in 0..steps.unsigned_abs() {
                                if !directory.step(steps.signum() as isize) {
                                    break;
                                }
                            }
                            let path = directory.current().to_owned();
                            if context.navigation.current_path.as_deref() != Some(path.as_path()) {
                                schedule_selection(context, loader, path);
                                continue;
                            }
                            schedule_selection(context, loader, path);
                            continue;
                        }
                        if let Some(directory) = context.navigation.directory.as_ref() {
                            loader.request_preloads(generation, directory.neighbors());
                        }
                    }
                    Err(error) => show_error(hwnd, &format!("图片目录扫描失败：{error}")),
                }
            }
            LoaderMessage::Preloaded { generation, image } => {
                if generation == context.navigation.generation {
                    context.shared.preload_image(image);
                }
            }
            LoaderMessage::Fatal(error) => {
                show_error(hwnd, &error);
                context.close_started = true;
                context.shared.request_shutdown();
            }
        }
    }
}

fn restore_navigation(context: &mut WindowContext, hwnd: windows::Win32::Foundation::HWND) {
    if let Some((path, directory)) = context.navigation.fallback.take() {
        context.navigation.generation = context.navigation.generation.wrapping_add(1).max(1);
        context.navigation.current_path = path.clone();
        context.navigation.directory = directory;
        context.navigation.pending_steps = 0;
        context.navigation.requested_at = None;
        if let Some(path) = path {
            set_window_title(hwnd, &path.display().to_string());
        } else {
            set_window_title(hwnd, "");
        }
    }
}

fn choose_image(hwnd: windows::Win32::Foundation::HWND) -> Result<Option<PathBuf>> {
    let filter: Vec<u16> = "图片文件\0*.jpg;*.jpeg;*.png;*.bmp\0所有文件\0*.*\0\0"
        .encode_utf16()
        .collect();
    let mut file = vec![0u16; 32_768];
    let title = w!("打开图片");
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: windows::core::PCWSTR(filter.as_ptr()),
        lpstrFile: windows::core::PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: title,
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: `dialog`、filter 和 file 均在模态对话框调用期间保持可写且有效；调用发生于 HWND 所属 UI 线程。
    if unsafe { GetOpenFileNameW(&mut dialog) }.as_bool() {
        let length = file
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(file.len());
        return Ok(Some(PathBuf::from(OsString::from_wide(&file[..length]))));
    }
    // SAFETY: 上一次 GetOpenFileNameW 已返回；该无参数 API 只读取当前线程的 Common Dialog 错误状态。
    let error = unsafe { CommDlgExtendedError() };
    if error.0 == 0 {
        Ok(None)
    } else {
        bail!("Windows Common Dialog 错误码：0x{:08X}", error.0)
    }
}

fn show_error(hwnd: windows::Win32::Foundation::HWND, message: &str) {
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    // SAFETY: 两个 UTF-16 字符串在同步 MessageBoxW 调用期间保持有效，owner HWND 仍由 UI 线程拥有。
    unsafe {
        let _ = MessageBoxW(
            Some(hwnd),
            windows::core::PCWSTR(text.as_ptr()),
            w!("Pixstride"),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn set_window_title(hwnd: windows::Win32::Foundation::HWND, title: &str) {
    let text: Vec<u16> = format!("Pixstride — {title}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: UTF-16 标题在同步调用期间保持有效，HWND 属于当前 UI 线程。
    unsafe {
        let _ = SetWindowTextW(hwnd, windows::core::PCWSTR(text.as_ptr()));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompletionEvent, CompletionSignal, Metrics, PendingState, RenderSnapshot, Shared,
        WakeEvent, run_render_worker,
    };
    use anyhow::Result;
    use std::{
        collections::VecDeque,
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
                    view_mode: super::ViewMode::Fit,
                },
                redraw: false,
                shutdown: false,
                shutdown_started: None,
                image_update: None,
                preload_uploads: VecDeque::new(),
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
        let (_, _, shutdown, _, _) = shared.take_pending();
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

        let (snapshot, redraw, shutdown, _, _) = shared.take_pending();
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

        let (snapshot, redraw, shutdown, _, _) = shared.take_pending();
        assert_eq!(snapshot.sequence, 0);
        assert_eq!(snapshot.texture_index, 0);
        assert!(!redraw);
        assert!(shutdown);
        Ok(())
    }
}
