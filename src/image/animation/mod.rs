//! 单个惰性 MTA 动画生产者。UI 只发布意图；WIC、画布与合成状态归 Worker 独占。
//! 输出最多两个 Credit，包含队列、消费者和所有 Arc 克隆；本模块不安排播放或 Present。

mod compose;
mod decoder;
#[cfg(test)]
pub(crate) mod fixture;
#[cfg(test)]
mod performance;
#[cfg(test)]
pub(crate) mod tests;

use super::{
    SourceIdentity,
    memory::{CpuMemory, Pixels},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::Threading::{CreateEventW, ResetEvent, SetEvent},
};

/// 文件选择版本不随动画推进；Session ID 仅由同一个控制器单调分配。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionKey {
    pub(crate) selection_generation: u64,
    pub(crate) id: u64,
}
/// Disposal 2 的明确输入策略，不代表 Viewer 的最终默认值。
#[derive(Clone, Copy, Debug)]
pub(crate) enum RestoreBackground {
    Transparent,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "保留已验证的显式背景策略；Viewer 沿用透明画布")
    )]
    LogicalScreen,
}
/// 消费者请求的扫描轮数独立于 GIF 原始 Loop。None 持续生产，直至取消或关闭。
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) path: PathBuf,
    pub(crate) selection_generation: u64,
    pub(crate) passes: Option<std::num::NonZeroU32>,
    pub(crate) background: RestoreBackground,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Worker 的生产状态；Completed 不代表外部已释放所有帧，须同时查看 credits。
pub(crate) enum Phase {
    Idle,
    Starting,
    Running,
    Paused,
    Cancelling,
    Completed,
    Failed,
    Stopped,
}
/// 原始元数据只描述来源，不制定 Delay 下限或 Loop 播放政策。
#[derive(Clone, Debug)]
pub(crate) struct Metadata {
    pub(crate) source: SourceIdentity,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) frames: u32,
    pub(crate) raw_loop: Option<u16>,
    pub(crate) logical_background: Option<[u8; 4]>,
    pub(crate) worst_pixels: u64,
}
/// 完整 PBGRA8 画布。字段析构顺序为像素、计费、Credit，最后一个 Arc 释放才归还。
pub(crate) struct FrameOutput {
    pub(crate) session: SessionKey,
    pub(crate) metadata: Arc<Metadata>,
    pub(crate) ordinal: u64,
    pub(crate) frame_index: u32,
    pub(crate) pass: u32,
    pub(crate) delay_cs: u16,
    pixels: Pixels,
    _credit: Credit,
}
impl FrameOutput {
    /// 借用完整、紧密排列的 PBGRA8；切片只能活到此帧所有者释放之前。
    pub(crate) fn pixels(&self) -> &[u8] {
        &self.pixels.data
    }
}
#[derive(Clone, Debug)]
/// 当前目标的状态快照；不包含 COM 或借用像素，通知事件只是提示，须重新读取状态。
pub(crate) struct Snapshot {
    pub(crate) session: Option<SessionKey>,
    pub(crate) phase: Phase,
    pub(crate) metadata: Option<Arc<Metadata>>,
    pub(crate) error: Option<String>,
    pub(crate) credits: u8,
    pub(crate) credit_peak: u8,
    pub(crate) queued: usize,
    pub(crate) produced: u64,
    pub(crate) worker_started: bool,
}
/// 事件由 Core 的 Arc 保活。跨线程只发送 HANDLE 数值，绝不传 HWND 或 WIC 对象。
struct Event(isize);
impl Event {
    fn new() -> Result<Self> {
        // SAFETY: 无名 manual-reset event；最后一个 Core 所有者退出后才关闭。
        Ok(Self(
            unsafe { CreateEventW(None, true, false, None) }?.0 as isize,
        ))
    }
    fn handle(&self) -> HANDLE {
        HANDLE(self.0 as _)
    }
    fn set(&self) {
        // SAFETY: Event 的所有者存活，句柄没有借给任何释放者。
        let _ = unsafe { SetEvent(self.handle()) };
    }
    fn reset(&self) {
        // SAFETY: 与 set 相同，控制锁保证消费和通知之间不会丢失状态更新。
        let _ = unsafe { ResetEvent(self.handle()) };
    }
}
impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: Core 最后一个 Arc 已释放，Worker 和消费者均不再借用事件。
        let _ = unsafe { CloseHandle(self.handle()) };
    }
}
struct Target {
    key: SessionKey,
    request: Request,
    paused: bool,
}
struct State {
    target: Option<Target>,
    phase: Phase,
    metadata: Option<Arc<Metadata>>,
    error: Option<String>,
    shutdown: bool,
    credits: u8,
    peak: u8,
    produced: u64,
    queue: VecDeque<Arc<FrameOutput>>,
    metrics: crate::diagnostics::Metrics,
    #[cfg(test)]
    gate: Option<(
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
    )>,
    #[cfg(test)]
    copy_probe: Option<(
        Arc<std::sync::atomic::AtomicBool>,
        std::sync::mpsc::SyncSender<()>,
    )>,
}
struct Core {
    state: Mutex<State>,
    changed: Condvar,
    ready: Event,
    done: Event,
    pool: CpuMemory,
    #[cfg(test)]
    fail_init: bool,
}

/// UI 只观察资源清理完成；Arc 保活事件，不持有 COM、GPU 或 HWND。
#[derive(Clone)]
pub(crate) struct Completion(Arc<Core>);
impl Completion {
    /// 返回借用句柄；观察者必须活到 Windows 等待返回，不能关闭该句柄。
    pub(crate) fn handle(&self) -> HANDLE {
        self.0.done.handle()
    }
    /// done 置位后再核对最终 Credit 和计费；控制器析构时的快照还可能包含在途 WIC。
    pub(crate) fn resources(&self) -> (super::memory::Usage, Snapshot) {
        (self.0.pool.usage(), snapshot(&self.0.lock()))
    }
    /// 线程清理完成后输出完整 Worker 统计，避免遗漏退出时仍在完成的同步调用。
    pub(crate) fn report(&self) -> String {
        self.0.lock().metrics.report()
    }
}
impl Core {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn current(&self, key: SessionKey) -> bool {
        let s = self.lock();
        !s.shutdown && s.phase != Phase::Failed && s.target.as_ref().is_some_and(|t| t.key == key)
    }
    fn notify(&self) {
        self.ready.set();
        self.changed.notify_all();
    }
    fn phase(&self, key: SessionKey, phase: Phase) {
        let mut s = self.lock();
        if !s.shutdown && s.target.as_ref().is_some_and(|t| t.key == key) {
            s.phase = phase;
            self.notify();
        }
    }
    fn record(&self, metric: crate::diagnostics::Metric, duration: Duration) {
        self.lock().metrics.record(metric, duration);
    }
    fn credit(self: &Arc<Self>, key: SessionKey) -> Result<Credit> {
        let mut s = self.lock();
        loop {
            ensure!(
                !s.shutdown && s.target.as_ref().is_some_and(|t| t.key == key),
                "动画 Session 已失效"
            );
            let paused = s.target.as_ref().is_some_and(|t| t.paused);
            if !paused && s.credits < 2 {
                s.credits += 1;
                s.peak = s.peak.max(s.credits);
                s.phase = Phase::Running;
                return Ok(Credit(Arc::clone(self)));
            }
            let phase = if paused {
                Phase::Paused
            } else {
                Phase::Running
            };
            if s.phase != phase {
                s.phase = phase;
                self.notify();
            }
            let at = Instant::now();
            s = self
                .changed
                .wait(s)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !paused {
                s.metrics.record(
                    crate::diagnostics::Metric::AnimationBackpressure,
                    at.elapsed(),
                );
            }
        }
    }
    fn publish(&self, frame: Arc<FrameOutput>) -> bool {
        let mut s = self.lock();
        while !s.shutdown
            && s.target
                .as_ref()
                .is_some_and(|t| t.key == frame.session && t.paused)
        {
            if s.phase != Phase::Paused {
                s.phase = Phase::Paused;
                self.notify();
            }
            s = self
                .changed
                .wait(s)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if s.shutdown || !s.target.as_ref().is_some_and(|t| t.key == frame.session) {
            return false;
        }
        s.queue.push_back(frame);
        s.produced += 1;
        self.notify();
        true
    }
}
struct Credit(Arc<Core>);
impl Drop for Credit {
    fn drop(&mut self) {
        let mut s = self.0.lock();
        s.credits -= 1;
        self.0.changed.notify_all();
    }
}

/// Viewer 生命周期内唯一控制器；构造不建线程。会话替换不会新建替代 Worker。
pub(crate) struct AnimationWorker {
    pool: CpuMemory,
    core: Option<Arc<Core>>,
    thread: Option<JoinHandle<()>>,
    sequence: u64,
    #[cfg(test)]
    fail_init: bool,
    #[cfg(test)]
    initial_gate: Option<(
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
    )>,
}
impl AnimationWorker {
    /// 仅诊断：在真实 WIC 局部帧调用返回后门控发布，隔离慢 Worker 与 UI/Renderer 的生命周期。
    #[cfg(test)]
    pub(crate) fn hold_first_frame_for_test(
        &mut self,
    ) -> (
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::SyncSender<()>,
    ) {
        let (entered, notice) = std::sync::mpsc::sync_channel(1);
        let (release, gate) = std::sync::mpsc::sync_channel(1);
        self.initial_gate = Some((entered, gate));
        (notice, release)
    }
    pub(crate) fn new(pool: CpuMemory) -> Self {
        Self {
            pool,
            core: None,
            thread: None,
            sequence: 0,
            #[cfg(test)]
            fail_init: false,
            #[cfg(test)]
            initial_gate: None,
        }
    }
    /// 只发布文件路径，不在调用线程执行 I/O；COM 初始化失败通过状态通知，不创建替代线程。
    pub(crate) fn start(&mut self, request: Request) -> Result<SessionKey> {
        if let Some(core) = &self.core {
            ensure!(!core.lock().shutdown, "动画 Worker 已停止，不能重新启动");
        }
        self.sequence = self.sequence.checked_add(1).context("Session ID 已耗尽")?;
        let key = SessionKey {
            selection_generation: request.selection_generation,
            id: self.sequence,
        };
        let spawn = self.core.is_none();
        if spawn {
            let core = Arc::new(Core {
                state: Mutex::new(State {
                    target: None,
                    phase: Phase::Idle,
                    metadata: None,
                    error: None,
                    shutdown: false,
                    credits: 0,
                    peak: 0,
                    produced: 0,
                    queue: VecDeque::new(),
                    metrics: Default::default(),
                    #[cfg(test)]
                    gate: self.initial_gate.take(),
                    #[cfg(test)]
                    copy_probe: None,
                }),
                changed: Condvar::new(),
                ready: Event::new()?,
                done: Event::new()?,
                pool: self.pool.clone(),
                #[cfg(test)]
                fail_init: self.fail_init,
            });
            self.core = Some(Arc::clone(&core));
        }
        let core = self.core.as_ref().context("缺少动画控制器")?;
        {
            let mut s = core.lock();
            ensure!(!s.shutdown, "动画 Worker 已退出");
            s.target = Some(Target {
                key,
                request,
                paused: false,
            });
            s.phase = Phase::Starting;
            s.metadata = None;
            s.error = None;
            core.notify();
        }
        if spawn {
            let thread_core = Arc::clone(core);
            match thread::Builder::new()
                .name("pixstride-animation-mta".into())
                .spawn(move || thread_main(thread_core))
            {
                Ok(thread) => self.thread = Some(thread),
                Err(error) => {
                    let mut s = core.lock();
                    s.shutdown = true;
                    s.phase = Phase::Failed;
                    s.error = Some(error.to_string());
                    core.notify();
                    core.done.set();
                    return Err(error).context("创建动画 Worker 失败");
                }
            }
        }
        Ok(key)
    }
    /// 请求暂停/恢复；已进入的同步调用可以完成，Worker 确认 Paused 后不再发布帧。
    pub(crate) fn pause(&self, key: SessionKey, paused: bool) -> bool {
        let Some(core) = &self.core else {
            return false;
        };
        let mut s = core.lock();
        if matches!(s.phase, Phase::Completed | Phase::Failed | Phase::Stopped) {
            return false;
        }
        let Some(target) = s.target.as_mut().filter(|t| t.key == key) else {
            return false;
        };
        target.paused = paused;
        core.notify();
        true
    }
    /// 立即使旧 Session 的输出失效；队列由 Worker 清理，消费者已有引用仍按最后引用计费。
    pub(crate) fn cancel(&self) {
        if let Some(core) = &self.core {
            let mut s = core.lock();
            if s.shutdown {
                return;
            }
            s.target = None;
            s.phase = Phase::Cancelling;
            s.metadata = None;
            s.error = None;
            // 只失效并唤醒；队列中可能很大的像素由 Worker 在锁外释放，不在 UI 做清理。
            core.notify();
        }
    }
    /// 不阻塞调用者；等待 Credit、暂停和 Idle 都会被唤醒，WIC 只能等同步调用返回。
    pub(crate) fn shutdown(&self) {
        if let Some(core) = &self.core {
            let mut s = core.lock();
            s.shutdown = true;
            s.target = None;
            s.metadata = None;
            core.notify();
        }
    }
    /// 仅在线程已退出时 join；UI 不在本方法内等待不可控 WIC。
    pub(crate) fn try_join(&mut self) -> Result<bool> {
        if self.thread.as_ref().is_some_and(|t| !t.is_finished()) {
            return Ok(false);
        }
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("动画 Worker panic"))?;
        }
        Ok(true)
    }
    /// UI 收到 done 后回收线程句柄；done 在 COM、画布和队列清理后置位，不等待在途 WIC。
    pub(crate) fn join_completed(&mut self) -> Result<bool> {
        if let Some(core) = &self.core {
            // SAFETY: Core 持有无名事件；零超时只查询完成状态，不阻塞 UI 等待解码。
            if unsafe {
                windows::Win32::System::Threading::WaitForSingleObject(core.done.handle(), 0)
            } != windows::Win32::Foundation::WAIT_OBJECT_0
            {
                return Ok(false);
            }
        }
        if let Some(thread) = self.thread.take() {
            // done 后只剩线程函数返回和 Rust 线程尾声；UI 保活 Core，不在此析构外部资源。
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("动画 Worker panic"))?;
        }
        Ok(true)
    }
    /// ready/done 是借用句柄；使用者必须保证控制器或其 Core 仍存活，不得关闭句柄。
    pub(crate) fn ready_handle(&self) -> Option<HANDLE> {
        self.core.as_ref().map(|c| c.ready.handle())
    }
    /// 整个 Worker 的退出事件；单个 Session Completed 后线程仍可复用，不置位此事件。
    #[cfg(test)]
    pub(crate) fn done_handle(&self) -> Option<HANDLE> {
        self.core.as_ref().map(|c| c.done.handle())
    }
    pub(crate) fn completion(&self) -> Option<Completion> {
        self.core.as_ref().map(|c| Completion(Arc::clone(c)))
    }
    /// 消费者在提交前复核；Completed 帧仍有效，Failed、取消和替换后的帧无效。
    pub(crate) fn is_current(&self, frame: &FrameOutput) -> bool {
        self.core.as_ref().is_some_and(|c| c.current(frame.session))
    }
    /// 一次原子消费状态和至多一帧；旧 Session 从入口拒收，持有旧 Arc 的消费者提交前仍须复核。
    pub(crate) fn poll(&self, key: SessionKey) -> (Snapshot, Option<Arc<FrameOutput>>) {
        let Some(core) = &self.core else {
            return (self.snapshot(), None);
        };
        let mut s = core.lock();
        let frame = if !s.shutdown
            && s.phase != Phase::Failed
            && s.target.as_ref().is_some_and(|t| t.key == key)
            && s.queue.front().is_some_and(|f| f.session == key)
        {
            s.queue.pop_front()
        } else {
            None
        };
        let snapshot = snapshot(&s);
        if !s
            .queue
            .front()
            .is_some_and(|f| Some(f.session) == snapshot.session)
        {
            core.ready.reset();
        }
        (snapshot, frame)
    }
    /// 只读当前控制状态；队列可能仍包含待 Worker 退役的旧帧，不能直接据 queued 接收。
    pub(crate) fn snapshot(&self) -> Snapshot {
        self.core
            .as_ref()
            .map(|c| snapshot(&c.lock()))
            .unwrap_or(Snapshot {
                session: None,
                phase: Phase::Idle,
                metadata: None,
                error: None,
                credits: 0,
                credit_peak: 0,
                queued: 0,
                produced: 0,
                worker_started: false,
            })
    }
    /// 共同 CPU 像素池的实时量和历史峰值；不等同于进程 Working Set。
    pub(crate) fn memory_usage(&self) -> super::memory::Usage {
        self.pool.usage()
    }
    /// 汇总有界计时样本；仅诊断/退出时格式化，逐帧路径不写日志。
    #[cfg(test)]
    pub(crate) fn report(&self) -> String {
        self.core
            .as_ref()
            .map(|c| c.lock().metrics.report())
            .unwrap_or_default()
    }
}
impl Drop for AnimationWorker {
    fn drop(&mut self) {
        self.shutdown();
        // 若 WIC 尚未返回，仅分离这一个线程。它独占 COM 并持有 Core/事件/额度的 Arc，
        // 不访问 ImageLoader、HWND 或此控制器。正常流程由 UI 等 done 后调用 join_completed。
        let _ = self.try_join();
    }
}
fn snapshot(s: &State) -> Snapshot {
    Snapshot {
        session: s.target.as_ref().map(|t| t.key),
        phase: s.phase,
        metadata: s.metadata.clone(),
        error: s.error.clone(),
        credits: s.credits,
        credit_peak: s.peak,
        queued: s.queue.len(),
        produced: s.produced,
        worker_started: true,
    }
}
fn run(core: &Arc<Core>) -> Result<()> {
    #[cfg(test)]
    ensure!(!core.fail_init, "注入的 MTA 初始化失败");
    let _com = super::ComMta::initialize()?;
    let mut seen = 0;
    while let Some((key, request)) = next_request(core, seen) {
        seen = key.id;
        if let Err(error) = produce(core, key, &request) {
            let removed = {
                let mut s = core.lock();
                if !s.shutdown && s.target.as_ref().is_some_and(|t| t.key == key) {
                    s.phase = Phase::Failed;
                    s.error = Some(format!("{error:#}"));
                    let removed = std::mem::take(&mut s.queue);
                    core.notify();
                    removed
                } else {
                    VecDeque::new()
                }
            };
            drop(removed);
        }
    }
    Ok(())
}

fn next_request(core: &Core, seen: u64) -> Option<(SessionKey, Request)> {
    loop {
        let mut s = core.lock();
        let stale = s.queue.front().is_some_and(|frame| {
            s.shutdown
                || !s
                    .target
                    .as_ref()
                    .is_some_and(|target| target.key == frame.session)
        });
        if stale {
            let removed = std::mem::take(&mut s.queue);
            drop(s);
            drop(removed); // Credit 析构重新取锁；大缓冲释放也留在 Worker。
            continue;
        }
        if s.shutdown {
            return None;
        }
        if let Some(target) = &s.target {
            if target.key.id != seen {
                return Some((target.key, target.request.clone()));
            }
        } else if s.phase != Phase::Idle {
            s.phase = Phase::Idle;
            core.notify();
        }
        drop(
            core.changed
                .wait(s)
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
}

fn thread_main(core: Arc<Core>) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&core)));
    let removed = {
        let mut s = core.lock();
        if !matches!(result, Ok(Ok(()))) {
            s.error = Some(match result {
                Ok(Err(e)) => format!("{e:#}"),
                _ => "动画 Worker panic".into(),
            });
            s.phase = Phase::Failed;
        } else {
            s.phase = Phase::Stopped;
        }
        s.shutdown = true;
        s.target = None;
        let removed = std::mem::take(&mut s.queue);
        core.notify();
        removed
    };
    drop(removed); // Frame 的 Credit 析构必须在控制锁外执行。
    core.done.set();
}
fn produce(core: &Arc<Core>, key: SessionKey, request: &Request) -> Result<()> {
    use crate::diagnostics::Metric;
    let at = Instant::now();
    let initialized = decoder::Decoder::open(&request.path, &core.pool, &|| core.current(key));
    core.record(Metric::AnimationInit, at.elapsed());
    let mut decoder = initialized?;
    #[cfg(test)]
    {
        decoder.copy_probe = core.lock().copy_probe.clone();
    }
    let metadata = Arc::new(decoder.metadata.clone());
    {
        let mut s = core.lock();
        ensure!(
            !s.shutdown && s.target.as_ref().is_some_and(|t| t.key == key),
            "Session 已失效"
        );
        s.metadata = Some(Arc::clone(&metadata));
        core.notify();
    }
    let background = match request.background {
        RestoreBackground::Transparent => [0; 4],
        RestoreBackground::LogicalScreen => metadata.logical_background.unwrap_or([0; 4]),
    };
    let mut composer =
        compose::Composer::new(&core.pool, metadata.width, metadata.height, background)?;
    let (mut ordinal, mut pass) = (0u64, 0u32);
    loop {
        for index in 0..metadata.frames {
            let credit = core.credit(key)?;
            let current = || core.current(key);
            let at = Instant::now();
            let decoded = decoder.frame(index, &core.pool, &current);
            core.record(Metric::AnimationDecode, at.elapsed());
            let patch = decoded?;
            #[cfg(test)]
            {
                let gate = core.lock().gate.take();
                if let Some((started, release)) = gate {
                    let _ = started.send(());
                    let _ = release.recv_timeout(Duration::from_secs(10));
                }
            }
            ensure!(current(), "Session 已失效");
            let at = Instant::now();
            composer.apply(&patch, &core.pool, &current)?;
            core.record(Metric::AnimationCompose, at.elapsed());
            let at = Instant::now();
            let mut pixels = Pixels::new(&core.pool, composer.pixels.data.len())?;
            pixels.data.copy_from_slice(&composer.pixels.data);
            let frame = Arc::new(FrameOutput {
                session: key,
                metadata: Arc::clone(&metadata),
                ordinal,
                frame_index: index,
                pass,
                delay_cs: patch.delay,
                pixels,
                _credit: credit,
            });
            core.record(Metric::AnimationOutput, at.elapsed());
            if !core.publish(frame) {
                return Ok(());
            }
            ordinal = ordinal.checked_add(1).context("Frame Ordinal 已耗尽")?;
        }
        pass = pass.checked_add(1).context("动画扫描轮次溢出")?;
        if request.passes.is_some_and(|n| pass >= n.get()) {
            break;
        }
        composer.reset(); // 不跳过任何合成步骤；测试消费者可丢弃输出，但不能跳帧解码。
    }
    drop(composer);
    drop(decoder);
    core.phase(key, Phase::Completed);
    Ok(())
}
