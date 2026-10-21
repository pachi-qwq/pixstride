//! 用独立的轻量身份校验线程和单个有界 WIC MTA 线程处理图片选择、目录扫描与预加载。

use std::{
    cell::RefCell,
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::Threading::{CreateEventW, SetEvent},
};

use crate::{
    app::{ResolvedSelection, Shared},
    diagnostics::{Counter, ImageTaskTrace, Metric},
};

use super::{
    BudgetRejection, ComMta, DecodeCancelled, DecodedImage, ImageBudget, SourceIdentity,
    cache::CpuCache, check_decode_current, decode_wic_reserved, navigation::ImageDirectory,
};

const PRELOAD_IDLE_DELAY: Duration = Duration::from_millis(120);

#[derive(Debug)]
pub(crate) enum LoaderMessage {
    IdentityResolved {
        generation: u64,
        path: PathBuf,
        requested_at: Instant,
        preloads: Vec<PathBuf>,
        result: std::result::Result<SourceIdentity, String>,
    },
    Current {
        generation: u64,
        path: PathBuf,
        requested_at: Instant,
        result: std::result::Result<(Arc<DecodedImage>, bool), String>,
    },
    Large {
        generation: u64,
        path: PathBuf,
        requested_at: Instant,
    },
    Directory {
        generation: u64,
        result: std::result::Result<ImageDirectory, String>,
    },
    Preloaded {
        generation: u64,
        image: Arc<DecodedImage>,
    },
    Fatal(String),
}

#[derive(Clone)]
struct DecodeJob {
    generation: u64,
    path: PathBuf,
    requested_at: Instant,
    queued_at: Instant,
    queued_behind_preload: bool,
    queued_behind_current: Option<u64>,
}

#[derive(Clone)]
struct IdentityJob {
    generation: u64,
    path: PathBuf,
    requested_at: Instant,
    preloads: Vec<PathBuf>,
}

#[derive(Default)]
struct IdentityState {
    generation: u64,
    current: Option<IdentityJob>,
    stopping: bool,
}

struct IdentityControl {
    state: Mutex<IdentityState>,
    changed: Condvar,
}

impl IdentityControl {
    fn lock(&self) -> MutexGuard<'_, IdentityState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn next(&self) -> Option<IdentityJob> {
        let mut state = self.lock();
        loop {
            if state.stopping {
                return None;
            }
            if let Some(job) = state.current.take() {
                return Some(job);
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn is_current(&self, generation: u64) -> bool {
        let state = self.lock();
        !state.stopping && state.generation == generation
    }
}

#[derive(Clone)]
struct ScanJob {
    generation: u64,
    directory: PathBuf,
    current: PathBuf,
}

#[derive(Default)]
struct WorkState {
    generation: u64,
    progress_floor: Option<u64>,
    selection_pending: bool,
    preload_running: bool,
    active_current_generation: Option<u64>,
    current: Option<DecodeJob>,
    scan: Option<ScanJob>,
    preloads: VecDeque<DecodeJob>,
    preload_not_before: Option<Instant>,
    stopping: bool,
    #[cfg(test)]
    test_gate: Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>,
    #[cfg(test)]
    test_copy_delay: Duration,
    #[cfg(test)]
    test_copy_gate: Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>,
}

struct WorkerControl {
    state: Mutex<WorkState>,
    changed: Condvar,
}

impl WorkerControl {
    // 身份校验、扫描和预加载仍只处理最新选择；只有当前图片解码可在同方向长按区间内完成。
    fn can_finish_current(&self, generation: u64) -> bool {
        let state = self.lock();
        !state.stopping
            && generation <= state.generation
            && (generation == state.generation
                || state
                    .progress_floor
                    .is_some_and(|floor| generation >= floor))
    }

    fn lock(&self) -> MutexGuard<'_, WorkState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn next(&self) -> Option<Work> {
        let mut state = self.lock();
        loop {
            if state.stopping {
                return None;
            }
            #[cfg(test)]
            if let Some((started, release)) = state.test_gate.take() {
                return Some(Work::TestGate(started, release));
            }
            if let Some(job) = state.current.take() {
                state.active_current_generation = Some(job.generation);
                return Some(Work::Current(job));
            }
            // 身份校验和 GPU 探测尚未结束时，保留 Worker 给当前选择，避免先进入大目录扫描或旧预加载。
            if state.selection_pending {
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            }
            if let Some(job) = state.scan.take() {
                if job.generation == state.generation {
                    return Some(Work::Scan(job));
                }
            }
            while state
                .preloads
                .front()
                .is_some_and(|job| job.generation != state.generation)
            {
                state.preloads.pop_front();
            }
            if !state.preloads.is_empty() {
                let remaining = state
                    .preload_not_before
                    .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                    .unwrap_or_default();
                if remaining.is_zero() {
                    let job = state.preloads.pop_front();
                    state.preload_running = job.is_some();
                    if state.preloads.is_empty() {
                        state.preload_not_before = None;
                    }
                    return job.map(Work::Preload);
                }
                state = self
                    .changed
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
                continue;
            }
            state.preload_not_before = None;
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn is_current(&self, generation: u64) -> bool {
        let state = self.lock();
        !state.stopping && state.generation == generation
    }
}

enum Work {
    Current(DecodeJob),
    Scan(ScanJob),
    Preload(DecodeJob),
    #[cfg(test)]
    TestGate(mpsc::SyncSender<()>, mpsc::Receiver<()>),
}

/// 管理一个身份线程、一个 WIC 线程、最新请求和三个 UI 等待句柄。
pub(crate) struct ImageLoader {
    control: Arc<WorkerControl>,
    identity_control: Arc<IdentityControl>,
    messages: Receiver<LoaderMessage>,
    ready_event: HANDLE,
    completed_event: HANDLE,
    identity_completed_event: HANDLE,
    thread: Option<JoinHandle<()>>,
    identity_thread: Option<JoinHandle<()>>,
    detached_for_process_exit: bool,
    animation: super::animation::AnimationWorker,
}

impl ImageLoader {
    /// 只用于验证：让解码线程停在有期限的闸门，身份校验和 GPU 命中必须仍可完成。
    #[cfg(test)]
    pub(crate) fn hold_decode_for_test(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel();
        self.control.lock().test_gate = Some((started_tx, release_rx));
        self.control.changed.notify_one();
        (started_rx, release_tx)
    }

    /// 仅用于回归：暂停已进入 CopyPixels 的当前请求，验证旧结果完成前发生的新 UI 意图。
    #[cfg(test)]
    pub(crate) fn hold_copy_for_test(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel();
        self.control.lock().test_copy_gate = Some((started_tx, release_rx));
        (started_rx, release_tx)
    }
    pub(crate) fn start(budget: ImageBudget, shared: Arc<Shared>) -> Result<Self> {
        let cpu = super::memory::CpuMemory::new(budget.cpu_cache_bytes);
        let decode_cpu = cpu.clone();
        // SAFETY: 无名称 auto-reset event 仅由 UI 持有，worker 每次向 channel 写结果后置位。
        let ready_event = unsafe { CreateEventW(None, false, false, None) }
            .context("创建解码结果通知事件失败")?;
        // SAFETY: 无名称 manual-reset event 在 worker 退出时置位；UI 在 join 之后关闭句柄。
        let completed_event = match unsafe { CreateEventW(None, true, false, None) } {
            Ok(event) => event,
            Err(error) => {
                // SAFETY: 第一个事件当前只有 start 函数持有，线程尚未启动。
                unsafe {
                    let _ = CloseHandle(ready_event);
                }
                return Err(error).context("创建解码线程完成事件失败");
            }
        };
        // SAFETY: 独立的 manual-reset event 只表示身份校验线程结束，UI 在线程 join 或进程退出前保留句柄。
        let identity_completed_event = match unsafe { CreateEventW(None, true, false, None) } {
            Ok(event) => event,
            Err(error) => {
                // SAFETY: 两个已创建事件仍由 start 函数独占，尚未启动任何线程。
                unsafe {
                    let _ = CloseHandle(ready_event);
                    let _ = CloseHandle(completed_event);
                }
                return Err(error).context("创建文件身份线程完成事件失败");
            }
        };
        let control = Arc::new(WorkerControl {
            state: Mutex::new(WorkState::default()),
            changed: Condvar::new(),
        });
        let identity_control = Arc::new(IdentityControl {
            state: Mutex::new(IdentityState::default()),
            changed: Condvar::new(),
        });
        let (sender, messages) = mpsc::channel();
        let thread_control = Arc::clone(&control);
        let decode_sender = sender.clone();
        let decode_shared = Arc::clone(&shared);
        let ready_value = ready_event.0 as isize;
        let completion_value = completed_event.0 as isize;
        let thread = match thread::Builder::new()
            .name("pixstride-wic-mta".to_owned())
            .spawn(move || {
                let _completion = CompletionSignal(completion_value);
                let apartment = ComMta::initialize();
                match apartment {
                    Ok(_apartment) => worker_loop(
                        &thread_control,
                        &decode_sender,
                        HANDLE(ready_value as _),
                        budget,
                        &decode_shared,
                        decode_cpu,
                    ),
                    Err(error) => send(
                        &decode_sender,
                        HANDLE(ready_value as _),
                        LoaderMessage::Fatal(format!("WIC 工作线程初始化失败：{error:#}")),
                    ),
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                // SAFETY: 线程未启动，两个事件句柄仍由当前函数独占。
                unsafe {
                    let _ = CloseHandle(ready_event);
                    let _ = CloseHandle(completed_event);
                    let _ = CloseHandle(identity_completed_event);
                }
                return Err(error).context("创建 WIC 解码线程失败");
            }
        };

        let identity_thread_control = Arc::clone(&identity_control);
        let identity_sender = sender.clone();
        let identity_ready_value = ready_event.0 as isize;
        let identity_completion_value = identity_completed_event.0 as isize;
        let identity_shared = Arc::clone(&shared);
        let identity_thread = match thread::Builder::new()
            .name("pixstride-file-identity".to_owned())
            .spawn(move || {
                let _completion = CompletionSignal(identity_completion_value);
                identity_loop(
                    &identity_thread_control,
                    &identity_sender,
                    HANDLE(identity_ready_value as _),
                    &identity_shared,
                );
            }) {
            Ok(thread) => thread,
            Err(error) => {
                {
                    let mut state = control
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.stopping = true;
                    control.changed.notify_all();
                }
                let _ = thread.join();
                // SAFETY: WIC worker 已 join；身份线程未启动，所有三个事件都不再被线程访问。
                unsafe {
                    let _ = CloseHandle(ready_event);
                    let _ = CloseHandle(completed_event);
                    let _ = CloseHandle(identity_completed_event);
                }
                return Err(error).context("创建文件身份校验线程失败");
            }
        };

        Ok(Self {
            control,
            identity_control,
            messages,
            ready_event,
            completed_event,
            identity_completed_event,
            thread: Some(thread),
            identity_thread: Some(identity_thread),
            detached_for_process_exit: false,
            animation: super::animation::AnimationWorker::new(cpu),
        })
    }

    /// 返回同一个惰性动画控制器；D1-b1 不从 UI 自动启动 Session。
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "正式播放入口在 D1-b2 接入；本轮只用测试消费者")
    )]
    pub(crate) fn animation(&mut self) -> &mut super::animation::AnimationWorker {
        &mut self.animation
    }

    pub(crate) fn ready_handle(&self) -> HANDLE {
        self.ready_event
    }

    pub(crate) fn completed_handle(&self) -> HANDLE {
        self.completed_event
    }

    /// 借出身份线程的完成事件；句柄仍归 ImageLoader 所有。
    pub(crate) fn identity_completed_handle(&self) -> HANDLE {
        self.identity_completed_event
    }

    pub(crate) fn try_receive(&self) -> Option<LoaderMessage> {
        self.messages.try_recv().ok()
    }

    pub(crate) fn request_open(&self, generation: u64, path: PathBuf, requested_at: Instant) {
        let directory = path.parent().unwrap_or_else(|| Path::new(".")).to_owned();
        let mut state = self.control.lock();
        state.generation = generation;
        state.selection_pending = true;
        state.current = None;
        state.scan = Some(ScanJob {
            generation,
            directory,
            current: path.clone(),
        });
        state.preloads.clear();
        state.preload_not_before = None;
        self.control.changed.notify_one();
        drop(state);
        self.request_identity(generation, path, requested_at, Vec::new(), None);
    }

    /// 替换待处理身份校验和预加载；同方向长按中的活动解码可按进度下界完成，不在调用线程读文件。
    pub(crate) fn request_identity(
        &self,
        generation: u64,
        path: PathBuf,
        requested_at: Instant,
        preloads: Vec<PathBuf>,
        progress_floor: Option<u64>,
    ) {
        // 文件选择变化严格取消旧动画；动画帧本身不递增文件 Generation。
        self.animation.cancel();
        {
            let mut decode = self.control.lock();
            if decode.stopping {
                return;
            }
            decode.generation = generation;
            decode.progress_floor = progress_floor;
            decode.selection_pending = true;
            decode.current = None;
            if decode
                .scan
                .as_ref()
                .is_some_and(|scan| scan.generation != generation)
            {
                decode.scan = None;
            }
            decode.preloads.clear();
            decode.preload_not_before = None;
            self.control.changed.notify_one();
        }
        let mut state = self.identity_control.lock();
        state.generation = generation;
        state.current = Some(IdentityJob {
            generation,
            path,
            requested_at,
            preloads: preloads.into_iter().take(2).collect(),
        });
        self.identity_control.changed.notify_one();
    }

    /// 松键或反向后禁止旧任务作为进度帧完成，最新目标及其单槽请求不受影响。
    pub(crate) fn end_navigation_progress(&self) {
        self.control.lock().progress_floor = None;
        self.control.changed.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn set_copy_delay_for_test(&self, delay: Duration) {
        self.control.lock().test_copy_delay = delay;
    }

    /// GPU 未命中后提交一次解码；调用者和 Worker 都必须使用同一选择代际。
    pub(crate) fn request_decode(&self, selection: ResolvedSelection) {
        let mut state = self.control.lock();
        if state.stopping || state.generation != selection.generation {
            return;
        }
        state.selection_pending = false;
        state.generation = selection.generation;
        state.current = Some(DecodeJob {
            generation: selection.generation,
            path: selection.path,
            requested_at: selection.requested_at,
            queued_at: Instant::now(),
            queued_behind_preload: state.preload_running,
            queued_behind_current: state.active_current_generation,
        });
        if state
            .scan
            .as_ref()
            .is_some_and(|scan| scan.generation != selection.generation)
        {
            state.scan = None;
        }
        state.preloads = selection
            .preloads
            .into_iter()
            .take(2)
            .map(|path| DecodeJob {
                generation: selection.generation,
                path,
                requested_at: selection.requested_at,
                queued_at: Instant::now(),
                queued_behind_preload: false,
                queued_behind_current: None,
            })
            .collect();
        state.preload_not_before =
            (!state.preloads.is_empty()).then(|| Instant::now() + PRELOAD_IDLE_DELAY);
        self.control.changed.notify_one();
    }

    pub(crate) fn request_preloads(&self, generation: u64, paths: Vec<PathBuf>) {
        let mut state = self.control.lock();
        if state.generation != generation || state.stopping {
            return;
        }
        state.selection_pending = false;
        state.preloads = paths
            .into_iter()
            .take(2)
            .map(|path| DecodeJob {
                generation,
                path,
                requested_at: Instant::now(),
                queued_at: Instant::now(),
                queued_behind_preload: false,
                queued_behind_current: None,
            })
            .collect();
        state.preload_not_before =
            (!state.preloads.is_empty()).then(|| Instant::now() + PRELOAD_IDLE_DELAY);
        self.control.changed.notify_one();
    }

    pub(crate) fn request_stop(&self) {
        self.animation.shutdown();
        let mut state = self.control.lock();
        state.stopping = true;
        state.current = None;
        state.scan = None;
        state.preloads.clear();
        state.preload_not_before = None;
        self.control.changed.notify_all();
        let mut identity = self.identity_control.lock();
        identity.stopping = true;
        identity.current = None;
        self.identity_control.changed.notify_all();
    }

    /// 超过关闭等待期限后分离线程；进程即将退出，Windows 会回收其仍持有的句柄和资源。
    pub(crate) fn detach_for_process_exit(&mut self) {
        self.request_stop();
        // 丢弃 JoinHandle 会分离线程。不能在 WIC 调用仍运行时关闭它可能使用的通知句柄。
        self.thread.take();
        self.identity_thread.take();
        self.detached_for_process_exit = true;
    }

    pub(crate) fn join(&mut self) -> Result<()> {
        // 不在 UI 上等待不可抢占的动画 WIC；未完成线程由其 Arc 状态继续拥有资源。
        self.animation.try_join()?;
        let decode = self.thread.take().map(JoinHandle::join);
        let identity = self.identity_thread.take().map(JoinHandle::join);
        let decode_error = decode.is_some_and(|result| result.is_err());
        let identity_error = identity.is_some_and(|result| result.is_err());
        match (decode_error, identity_error) {
            (false, false) => Ok(()),
            (true, true) => Err(anyhow::anyhow!("WIC 解码线程和文件身份线程发生 panic")),
            (true, false) => Err(anyhow::anyhow!("WIC 解码线程发生未处理的 panic")),
            (false, true) => Err(anyhow::anyhow!("文件身份校验线程发生未处理的 panic")),
        }
    }
}

impl Drop for ImageLoader {
    fn drop(&mut self) {
        if self.detached_for_process_exit {
            return;
        }
        self.request_stop();
        let _ = self.join();
        // SAFETY: Drop 先停止并 join 两个 worker；它们和渲染线程都不再使用三个 UI 持有的事件句柄。
        unsafe {
            let _ = CloseHandle(self.ready_event);
            let _ = CloseHandle(self.completed_event);
            let _ = CloseHandle(self.identity_completed_event);
        }
    }
}

struct CompletionSignal(isize);

impl Drop for CompletionSignal {
    fn drop(&mut self) {
        // SAFETY: UI 直到 worker join 后才关闭完成事件；此 guard 仅在线程退出时置位。
        let _ = unsafe { SetEvent(HANDLE(self.0 as _)) };
    }
}

fn worker_loop(
    control: &WorkerControl,
    sender: &mpsc::Sender<LoaderMessage>,
    ready: HANDLE,
    budget: ImageBudget,
    shared: &Shared,
    cpu: super::memory::CpuMemory,
) {
    let mut cache = CpuCache::with_pool(budget.cpu_cache_bytes, cpu);
    while let Some(work) = control.next() {
        match work {
            #[cfg(test)]
            Work::TestGate(started, release) => {
                let _ = started.send(());
                let _ = release.recv_timeout(Duration::from_secs(5));
            }
            Work::Current(job) => {
                let result = service_image_job(&mut cache, &job, control, budget, shared, true);
                shared.set_cpu_cache_bytes(cache.byte_cost());
                shared.record(Metric::ImageRequestToWorker, job.requested_at.elapsed());
                if control.can_finish_current(job.generation)
                    && shared.can_present_image_generation(job.generation)
                {
                    let large = result
                        .as_ref()
                        .err()
                        .is_some_and(|e| e.is::<BudgetRejection>())
                        && is_png(&job.path)
                        && shared.is_image_generation_current(job.generation);
                    let message = if large {
                        LoaderMessage::Large {
                            generation: job.generation,
                            path: job.path,
                            requested_at: job.requested_at,
                        }
                    } else {
                        LoaderMessage::Current {
                            generation: job.generation,
                            path: job.path,
                            requested_at: job.requested_at,
                            result: result.map_err(|e| format!("{e:#}")),
                        }
                    };
                    send(sender, ready, message);
                }
                control.lock().active_current_generation = None;
            }
            Work::Scan(job) => {
                if !control.is_current(job.generation) {
                    continue;
                }
                let started = Instant::now();
                let result = ImageDirectory::scan(&job.directory, &job.current);
                shared.record(Metric::DirectoryScan, started.elapsed());
                if control.is_current(job.generation) {
                    send(
                        sender,
                        ready,
                        LoaderMessage::Directory {
                            generation: job.generation,
                            result: result.map_err(|error| format!("{error:#}")),
                        },
                    );
                }
            }
            Work::Preload(job) => {
                if !control.is_current(job.generation) {
                    control.lock().preload_running = false;
                    continue;
                }
                let preload_started = Instant::now();
                match service_image_job(&mut cache, &job, control, budget, shared, false) {
                    Ok((image, _)) if control.is_current(job.generation) => {
                        shared.increment(Counter::PreloadDecoded);
                        send(
                            sender,
                            ready,
                            LoaderMessage::Preloaded {
                                generation: job.generation,
                                image,
                            },
                        );
                    }
                    Ok(_) | Err(_) => {}
                }
                shared.record(Metric::PreloadDecode, preload_started.elapsed());
                shared.set_cpu_cache_bytes(cache.byte_cost());
                control.lock().preload_running = false;
            }
        }
    }
    shared.set_cpu_cache_bytes(0);
}

/// 为同一个请求关联所有分项。队列来源使用同一选择 Generation，不增加新的版本域。
fn service_image_job(
    cache: &mut CpuCache,
    job: &DecodeJob,
    control: &WorkerControl,
    budget: ImageBudget,
    shared: &Shared,
    current: bool,
) -> Result<(Arc<DecodedImage>, bool)> {
    let started = Instant::now();
    let queue = started.saturating_duration_since(job.queued_at);
    if current {
        shared.record(Metric::ImageWorkerQueueWait, queue);
        if job.queued_behind_preload {
            shared.record(Metric::QueueBehindPreload, queue);
        }
        if job.queued_behind_current.is_some() {
            shared.record(Metric::QueueBehindCurrent, queue);
        }
    }
    let stages = RefCell::new(Vec::with_capacity(16));
    let result = load_with_cache(
        cache,
        &job.path,
        budget,
        shared,
        current,
        || {
            if current {
                control.can_finish_current(job.generation)
                    && shared.can_present_image_generation(job.generation)
            } else {
                control.is_current(job.generation)
            }
        },
        |metric, elapsed| {
            #[cfg(test)]
            if matches!(metric, Metric::WicCopyPixels) {
                // 在调用返回后模拟不可抢占工作，不持有队列锁，也不改变正式程序的等待机制。
                if current {
                    let gate = control.lock().test_copy_gate.take();
                    if let Some((started, release)) = gate {
                        let _ = started.send(());
                        let _ = release.recv_timeout(Duration::from_secs(5));
                    }
                }
                let delay = control.lock().test_copy_delay;
                thread::sleep(delay);
            }
            shared.record(metric, elapsed);
            stages.borrow_mut().push((metric, elapsed));
        },
    );
    let service = started.elapsed();
    if current {
        shared.record(Metric::ImageWorkerService, service);
    }
    let cancelled = result
        .as_ref()
        .err()
        .is_some_and(|error| error.is::<DecodeCancelled>());
    if cancelled {
        shared.increment(Counter::DecodeCancelled);
    }
    if (current && queue >= Duration::from_millis(100))
        || service >= Duration::from_millis(100)
        || (result.is_err() && !cancelled)
    {
        shared.record_image_task(ImageTaskTrace {
            generation: job.generation,
            file_name: job
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            preload: !current,
            queued_behind_current: job.queued_behind_current,
            queued_behind_preload: job.queued_behind_preload,
            queue,
            service,
            still_current: control.is_current(job.generation),
            outcome: task_outcome(&result, &job.path, current),
            stages: stages.into_inner(),
        });
    }
    result
}

/// PNG 普通预算拒绝是路由决定；预加载不启动 Builder，其他错误仍保留错误标签。
fn task_outcome(result: &Result<(Arc<DecodedImage>, bool)>, path: &Path, current: bool) -> String {
    match result {
        Ok((_, true)) => "cpu-hit".to_owned(),
        Ok((_, false)) => "decoded".to_owned(),
        Err(error) if error.is::<DecodeCancelled>() => "cancelled-stale".to_owned(),
        Err(error) if is_png(path) && error.is::<BudgetRejection>() => if current {
            "route-to-tiles"
        } else {
            "skip-large-preload"
        }
        .to_owned(),
        Err(error) => format!("error: {}", error.root_cause())
            .chars()
            .take(512)
            .collect(),
    }
}

fn identity_loop(
    control: &IdentityControl,
    sender: &mpsc::Sender<LoaderMessage>,
    ready: HANDLE,
    shared: &Shared,
) {
    while let Some(job) = control.next() {
        let queue_started = Instant::now();
        shared.record(
            Metric::IdentityQueueWait,
            queue_started.saturating_duration_since(job.requested_at),
        );
        let identity_started = Instant::now();
        let result = SourceIdentity::read(&job.path);
        shared.record(Metric::SourceIdentityRead, identity_started.elapsed());
        if control.is_current(job.generation) {
            send(
                sender,
                ready,
                LoaderMessage::IdentityResolved {
                    generation: job.generation,
                    path: job.path,
                    requested_at: job.requested_at,
                    preloads: job.preloads,
                    result: result.map_err(|error| format!("{error:#}")),
                },
            );
        }
    }
}

fn is_png(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
}

fn load_with_cache(
    cache: &mut CpuCache,
    path: &Path,
    budget: ImageBudget,
    shared: &Shared,
    current: bool,
    is_current: impl Fn() -> bool,
    record: impl Fn(Metric, Duration),
) -> Result<(Arc<DecodedImage>, bool)> {
    check_decode_current(&is_current)?;
    let identity_started = Instant::now();
    let identity = match SourceIdentity::read(path) {
        Ok(identity) => identity,
        Err(error) => {
            record(Metric::SourceIdentityRead, identity_started.elapsed());
            return Err(error);
        }
    };
    record(Metric::SourceIdentityRead, identity_started.elapsed());
    check_decode_current(&is_current)?;
    let lookup_started = Instant::now();
    if let Some(image) = cache.get(&identity) {
        shared.increment(Counter::CpuCacheHit);
        shared.increment(if current {
            Counter::CpuCurrentHit
        } else {
            Counter::CpuPreloadHit
        });
        record(Metric::CpuCacheLookup, lookup_started.elapsed());
        return Ok((image, true));
    }
    shared.increment(Counter::CpuCacheMiss);
    shared.increment(if current {
        Counter::CpuCurrentMiss
    } else {
        Counter::CpuPreloadMiss
    });
    record(Metric::CpuCacheLookup, lookup_started.elapsed());
    if let Some(error) = cache.rejection(&identity) {
        shared.increment(Counter::BudgetRejectionHit);
        return Err(error.into());
    }
    if is_png(path) {
        crate::large::cache::ensure_local(path)?;
        if let Ok(size) = crate::large::stream::inspect(path) {
            if let Err(e) = super::checked_layout(size.width, size.height, budget) {
                if let Some(rejection) = e.downcast_ref::<BudgetRejection>() {
                    cache.remember_rejection(identity, rejection.clone());
                }
                return Err(e);
            }
        }
    }
    let decode_started = Instant::now();
    let result = decode_wic_reserved(path, &identity, budget, &record, is_current, &mut |len| {
        cache.allocate(len)
    });
    record(
        if current {
            Metric::CurrentWicTotal
        } else {
            Metric::PreloadWicTotal
        },
        decode_started.elapsed(),
    );
    if result
        .as_ref()
        .err()
        .is_some_and(|error| !error.is::<DecodeCancelled>())
    {
        shared.increment(Counter::ImageDecodeFailed);
    }
    if let Some(error) = result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<BudgetRejection>())
    {
        cache.remember_rejection(identity, error.clone());
    }
    let decoded = result?;
    let image = Arc::new(decoded);
    cache.insert(Arc::clone(&image));
    Ok((image, false))
}

fn send(sender: &mpsc::Sender<LoaderMessage>, event: HANDLE, message: LoaderMessage) {
    if sender.send(message).is_ok() {
        // SAFETY: `event` 由 ImageLoader 持有，且只有在 worker join 后才关闭；SetEvent 唤醒 UI 等待者。
        let _ = unsafe { SetEvent(event) };
    }
}

#[cfg(test)]
mod tests {
    use super::{Work, WorkState, WorkerControl};
    use std::{
        path::{Path, PathBuf},
        sync::{Condvar, Mutex},
        time::{Duration, Instant},
    };

    #[test]
    fn large_png_routing_is_distinct_from_an_actual_load_error() {
        let error =
            super::super::checked_layout(20000, 1, super::ImageBudget::default()).unwrap_err();
        let result = Err(error);
        assert_eq!(
            super::task_outcome(&result, Path::new("large.PNG"), true),
            "route-to-tiles"
        );
        assert_eq!(
            super::task_outcome(&result, Path::new("large.png"), false),
            "skip-large-preload"
        );
        assert!(super::task_outcome(&result, Path::new("large.jpg"), true).starts_with("error:"));
        let failed = Err(anyhow::anyhow!("file unavailable"));
        assert!(super::task_outcome(&failed, Path::new("missing.png"), true).starts_with("error:"));
    }

    #[test]
    fn latest_selection_replaces_pending_work_and_old_preloads() {
        let control = WorkerControl {
            state: Mutex::new(WorkState::default()),
            changed: Condvar::new(),
        };
        {
            let mut state = control.lock();
            state.generation = 1;
            state.current = Some(super::DecodeJob {
                generation: 1,
                path: PathBuf::from("old.jpg"),
                requested_at: Instant::now(),
                queued_at: Instant::now(),
                queued_behind_preload: false,
                queued_behind_current: None,
            });
            state.preloads.push_back(super::DecodeJob {
                generation: 1,
                path: PathBuf::from("older.jpg"),
                requested_at: Instant::now(),
                queued_at: Instant::now(),
                queued_behind_preload: false,
                queued_behind_current: None,
            });
            state.generation = 2;
            state.current = Some(super::DecodeJob {
                generation: 2,
                path: PathBuf::from("latest.png"),
                requested_at: Instant::now(),
                queued_at: Instant::now(),
                queued_behind_preload: false,
                queued_behind_current: None,
            });
            state.preloads.clear();
        }
        assert!(
            matches!(control.next(), Some(Work::Current(job)) if job.generation == 2 && job.path == Path::new("latest.png"))
        );
        assert!(!control.is_current(1));
    }

    #[test]
    fn a_running_old_preload_becomes_stale_and_current_work_runs_next() {
        let control = WorkerControl {
            state: Mutex::new(WorkState::default()),
            changed: Condvar::new(),
        };
        {
            let mut state = control.lock();
            state.generation = 1;
            state.preloads.push_back(super::DecodeJob {
                generation: 1,
                path: PathBuf::from("slow-preload.jpg"),
                requested_at: Instant::now(),
                queued_at: Instant::now(),
                queued_behind_preload: false,
                queued_behind_current: None,
            });
        }

        let active = control.next().expect("预加载任务已取出");
        assert!(matches!(active, Work::Preload(_)));
        {
            let mut state = control.lock();
            state.generation = 2;
            state.current = Some(super::DecodeJob {
                generation: 2,
                path: PathBuf::from("latest.png"),
                requested_at: Instant::now(),
                queued_at: Instant::now(),
                queued_behind_preload: false,
                queued_behind_current: None,
            });
            state.preloads.clear();
        }

        assert!(!control.is_current(1));
        assert!(matches!(control.next(), Some(Work::Current(job)) if job.generation == 2));
    }

    #[test]
    fn current_selection_preempts_a_delayed_preload() {
        let control = WorkerControl {
            state: Mutex::new(WorkState::default()),
            changed: Condvar::new(),
        };
        {
            let mut state = control.lock();
            state.generation = 3;
            state.current = Some(super::DecodeJob {
                generation: 3,
                path: PathBuf::from("selected.jpg"),
                requested_at: Instant::now(),
                queued_at: Instant::now(),
                queued_behind_preload: false,
                queued_behind_current: None,
            });
            state.preloads.push_back(super::DecodeJob {
                generation: 3,
                path: PathBuf::from("neighbor.jpg"),
                requested_at: Instant::now(),
                queued_at: Instant::now(),
                queued_behind_preload: false,
                queued_behind_current: None,
            });
            state.preload_not_before = Some(Instant::now() + Duration::from_secs(5));
        }

        assert!(
            matches!(control.next(), Some(Work::Current(job)) if job.path == Path::new("selected.jpg"))
        );
    }

    #[test]
    fn progress_range_applies_only_to_current_work_and_closes_on_release() {
        let control = WorkerControl {
            state: Mutex::new(WorkState::default()),
            changed: Condvar::new(),
        };
        {
            let mut state = control.lock();
            state.generation = 8;
            state.progress_floor = Some(3);
        }
        assert!(control.can_finish_current(4));
        assert!(!control.is_current(4));
        assert!(!control.can_finish_current(2));
        assert!(!control.can_finish_current(9));
        control.lock().progress_floor = None;
        assert!(!control.can_finish_current(7));
        assert!(control.can_finish_current(8));
        control.lock().stopping = true;
        assert!(!control.can_finish_current(8));
    }

    #[test]
    fn identity_requests_coalesce_and_stop_wakes_a_waiter() {
        let control = std::sync::Arc::new(super::IdentityControl {
            state: Mutex::new(super::IdentityState::default()),
            changed: Condvar::new(),
        });
        for generation in 1..=200 {
            let mut state = control.lock();
            state.generation = generation;
            state.current = Some(super::IdentityJob {
                generation,
                path: PathBuf::from("latest.jpg"),
                requested_at: Instant::now(),
                preloads: Vec::new(),
            });
        }
        assert_eq!(control.next().unwrap().generation, 200);
        assert!(!control.is_current(199));
        let waiter = std::sync::Arc::clone(&control);
        let thread = std::thread::spawn(move || waiter.next().is_none());
        {
            let mut state = control.lock();
            state.stopping = true;
            control.changed.notify_all();
        }
        assert!(thread.join().unwrap());
    }
}
