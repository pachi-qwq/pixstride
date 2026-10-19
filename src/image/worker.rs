//! 在单个有界 WIC MTA 工作线程中处理当前图片、目录扫描和相邻预加载。

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

use anyhow::{Context, Result};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::Threading::{CreateEventW, SetEvent},
};

use crate::{
    app::Shared,
    diagnostics::{Counter, Metric},
};

use super::{
    ComMta, DecodedImage, ImageBudget, SourceIdentity, cache::CpuCache, decode_wic,
    navigation::ImageDirectory,
};

#[derive(Debug)]
pub(crate) enum LoaderMessage {
    Current {
        generation: u64,
        path: PathBuf,
        result: std::result::Result<Arc<DecodedImage>, String>,
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
    current: Option<DecodeJob>,
    scan: Option<ScanJob>,
    preloads: VecDeque<DecodeJob>,
    stopping: bool,
}

struct WorkerControl {
    state: Mutex<WorkState>,
    changed: Condvar,
}

impl WorkerControl {
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
            if let Some(job) = state.current.take() {
                return Some(Work::Current(job));
            }
            if let Some(job) = state.scan.take() {
                if job.generation == state.generation {
                    return Some(Work::Scan(job));
                }
            }
            while let Some(job) = state.preloads.pop_front() {
                if job.generation == state.generation {
                    return Some(Work::Preload(job));
                }
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

enum Work {
    Current(DecodeJob),
    Scan(ScanJob),
    Preload(DecodeJob),
}

/// 管理解码线程、最新请求和两个 UI 等待句柄。
pub(crate) struct ImageLoader {
    control: Arc<WorkerControl>,
    messages: Receiver<LoaderMessage>,
    ready_event: HANDLE,
    completed_event: HANDLE,
    thread: Option<JoinHandle<()>>,
}

impl ImageLoader {
    pub(crate) fn start(budget: ImageBudget, shared: Arc<Shared>) -> Result<Self> {
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
        let control = Arc::new(WorkerControl {
            state: Mutex::new(WorkState::default()),
            changed: Condvar::new(),
        });
        let (sender, messages) = mpsc::channel();
        let thread_control = Arc::clone(&control);
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
                        &sender,
                        HANDLE(ready_value as _),
                        budget,
                        &shared,
                    ),
                    Err(error) => send(
                        &sender,
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
                }
                return Err(error).context("创建 WIC 解码线程失败");
            }
        };

        Ok(Self {
            control,
            messages,
            ready_event,
            completed_event,
            thread: Some(thread),
        })
    }

    pub(crate) fn ready_handle(&self) -> HANDLE {
        self.ready_event
    }

    pub(crate) fn completed_handle(&self) -> HANDLE {
        self.completed_event
    }

    pub(crate) fn try_receive(&self) -> Option<LoaderMessage> {
        self.messages.try_recv().ok()
    }

    pub(crate) fn request_open(&self, generation: u64, path: PathBuf) {
        let directory = path.parent().unwrap_or_else(|| Path::new(".")).to_owned();
        let mut state = self.control.lock();
        state.generation = generation;
        state.current = Some(DecodeJob {
            generation,
            path: path.clone(),
        });
        state.scan = Some(ScanJob {
            generation,
            directory,
            current: path,
        });
        state.preloads.clear();
        self.control.changed.notify_one();
    }

    pub(crate) fn request_selection(&self, generation: u64, path: PathBuf, preloads: Vec<PathBuf>) {
        let mut state = self.control.lock();
        state.generation = generation;
        state.current = Some(DecodeJob { generation, path });
        state.scan = None;
        state.preloads = preloads
            .into_iter()
            .take(2)
            .map(|path| DecodeJob { generation, path })
            .collect();
        self.control.changed.notify_one();
    }

    pub(crate) fn request_preloads(&self, generation: u64, paths: Vec<PathBuf>) {
        let mut state = self.control.lock();
        if state.generation != generation || state.stopping {
            return;
        }
        state.preloads = paths
            .into_iter()
            .take(2)
            .map(|path| DecodeJob { generation, path })
            .collect();
        self.control.changed.notify_one();
    }

    pub(crate) fn request_stop(&self) {
        let mut state = self.control.lock();
        state.stopping = true;
        state.current = None;
        state.scan = None;
        state.preloads.clear();
        self.control.changed.notify_all();
    }

    pub(crate) fn join(&mut self) -> Result<()> {
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("WIC 解码线程发生未处理的 panic"))?;
        }
        Ok(())
    }
}

impl Drop for ImageLoader {
    fn drop(&mut self) {
        self.request_stop();
        let _ = self.join();
        // SAFETY: Drop 先停止并 join worker；线程不再使用这两个 UI 持有的事件句柄。
        unsafe {
            let _ = CloseHandle(self.ready_event);
            let _ = CloseHandle(self.completed_event);
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
) {
    let mut cache = CpuCache::new(budget.cpu_cache_bytes);
    while let Some(work) = control.next() {
        match work {
            Work::Current(job) => {
                let result = load_with_cache(&mut cache, &job.path, budget, shared);
                shared.set_cpu_cache_bytes(cache.byte_cost());
                if control.is_current(job.generation) {
                    send(
                        sender,
                        ready,
                        LoaderMessage::Current {
                            generation: job.generation,
                            path: job.path,
                            result: result.map_err(|error| format!("{error:#}")),
                        },
                    );
                }
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
                    continue;
                }
                let preload_started = Instant::now();
                match load_with_cache(&mut cache, &job.path, budget, shared) {
                    Ok(image) if control.is_current(job.generation) => {
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
            }
        }
    }
    shared.set_cpu_cache_bytes(0);
}

fn load_with_cache(
    cache: &mut CpuCache,
    path: &Path,
    budget: ImageBudget,
    shared: &Shared,
) -> Result<Arc<DecodedImage>> {
    let identity = SourceIdentity::read(path)?;
    if let Some(image) = cache.get(&identity) {
        shared.increment(Counter::CpuCacheHit);
        return Ok(image);
    }
    shared.increment(Counter::CpuCacheMiss);
    let started = Instant::now();
    let (decoded, timings) = decode_wic(path, budget)?;
    shared.record(Metric::ImageDecodeTotal, started.elapsed());
    shared.record(Metric::WicOpen, timings.open);
    shared.record(Metric::PixelFormatConversion, timings.conversion);
    shared.record(Metric::WicCopyPixels, timings.copy_pixels);
    let image = Arc::new(decoded);
    cache.insert(Arc::clone(&image));
    Ok(image)
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
    };

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
            });
            state.preloads.push_back(super::DecodeJob {
                generation: 1,
                path: PathBuf::from("older.jpg"),
            });
            state.generation = 2;
            state.current = Some(super::DecodeJob {
                generation: 2,
                path: PathBuf::from("latest.png"),
            });
            state.preloads.clear();
        }
        assert!(
            matches!(control.next(), Some(Work::Current(job)) if job.generation == 2 && job.path == Path::new("latest.png"))
        );
        assert!(!control.is_current(1));
    }
}
