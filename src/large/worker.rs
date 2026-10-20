//! 独立于 WIC 的单个 Builder/Tile I/O 线程；最新任务槽、两项答复和独立 CPU 缓存限制并发资源。

use super::cache::{self, CacheBudget, DiskCache, TilePixels};
use crate::app::Shared;
use anyhow::{Context, Result};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, Weak, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// 复用 UI 选择 Generation，只传普通路径和时间，不传 HWND 或 COM 对象。
pub(crate) struct Request {
    pub(crate) generation: u64,
    pub(crate) path: PathBuf,
    pub(crate) started: Instant,
}
/// 只传不可变缓存引用和像素所有权；GPU 对象从不出现在后台答复中。
pub(crate) enum Reply {
    Ready {
        generation: u64,
        cache: Arc<DiskCache>,
        preview: Arc<TilePixels>,
        started: Instant,
    },
    Tile {
        generation: u64,
        pixels: Arc<TilePixels>,
        sequence: u64,
        ready_at: Instant,
        read: cache::ReadProfile,
        cpu_hit: bool,
        queue: Duration,
        backpressure: Duration,
    },
    Error {
        generation: u64,
        message: String,
    },
}
struct ViewJob {
    generation: u64,
    sequence: u64,
    cache: Arc<DiskCache>,
    ids: Vec<usize>,
    queued_at: Instant,
}
#[derive(Default)]
struct State {
    stopping: bool,
    failed: bool,
    build: Option<Request>,
    view: Option<ViewJob>,
    replies: VecDeque<Reply>,
    sequence: u64,
}
struct Control {
    state: Mutex<State>,
    changed: Condvar,
    shared: Weak<Shared>,
}
impl Control {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn valid(&self, generation: u64) -> bool {
        !self.lock().stopping
            && self
                .shared
                .upgrade()
                .is_some_and(|s| s.is_image_generation_current(generation))
    }
    fn publish(&self, generation: u64, mut reply: Reply) {
        let at = Instant::now();
        let mut s = self.lock();
        while s.replies.len() >= 2 && !s.stopping {
            s = self
                .changed
                .wait(s)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        // 持有停止锁直到唤醒完成；Drop 取得同一锁后才允许 Render Thread 结束其事件生命周期。
        if let Some(shared) = self
            .shared
            .upgrade()
            .filter(|shared| !s.stopping && shared.is_image_generation_current(generation))
        {
            if let Reply::Tile {
                ready_at,
                backpressure,
                ..
            } = &mut reply
            {
                *backpressure = at.elapsed();
                shared.record(
                    crate::diagnostics::Metric::TileReplyBackpressure,
                    *backpressure,
                );
                *ready_at = Instant::now();
            }
            s.replies.push_back(reply);
            shared.wake_render_thread();
        }
    }
}

/// 线程只操作文件和像素；Renderer 独占此句柄并在 GPU 资源销毁前请求停止。
pub(crate) struct Worker {
    control: Arc<Control>,
    thread: Option<JoinHandle<()>>,
    done: mpsc::Receiver<()>,
}
impl Worker {
    /// 延迟创建一个可睡眠的文件 Worker；不初始化 COM，也不借用 Windows 事件句柄。
    pub(crate) fn start(shared: &Arc<Shared>) -> Result<Self> {
        let control = Arc::new(Control {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            shared: Arc::downgrade(shared),
        });
        let c = Arc::clone(&control);
        let (tx, done) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("pixstride-tiles".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&c)));
                if let Some(shared) = c.shared.upgrade().filter(|_| result.is_err()) {
                    c.lock().failed=true;
                    let generation = shared.image_generation();
                    c.publish(
                        generation,
                        Reply::Error {
                            generation,
                                message: "大图后台线程发生 panic，已停止接收工作；可浏览普通图片，重试大图需重启程序".into(),
                        },
                    );
                }
                let _ = tx.send(());
            })?;
        Ok(Self {
            control,
            thread: Some(thread),
            done,
        })
    }
    /// 覆盖待处理构建和视口槽；正在进行的扫描通过共享 Generation 在下个取消点退出。
    pub(crate) fn build(&self, request: Request) -> Result<()> {
        let mut s = self.control.lock();
        anyhow::ensure!(
            !s.failed && !s.stopping,
            "大图线程已退出，重试大图需重启程序"
        );
        s.build = Some(request);
        s.view = None;
        s.replies.clear();
        self.control.changed.notify_all();
        Ok(())
    }
    /// 覆盖视口需求，序号来自现有 Render Snapshot；调用方最多传 MAX_VISIBLE 个索引。
    pub(crate) fn view(
        &self,
        generation: u64,
        sequence: u64,
        cache: Arc<DiskCache>,
        ids: Vec<usize>,
    ) {
        let mut s = self.control.lock();
        if s.failed || s.stopping {
            return;
        }
        s.sequence = sequence;
        s.view = Some(ViewJob {
            generation,
            sequence,
            cache,
            ids,
            queued_at: Instant::now(),
        });
        self.control.changed.notify_all();
    }
    /// Render Thread 取走最多两个答复并唤醒背压等待者；此处不读文件。
    pub(crate) fn drain(&self) -> Vec<Reply> {
        let mut s = self.control.lock();
        let r = s.replies.drain(..).collect();
        self.control.changed.notify_all();
        r
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        {
            let mut s = self.control.lock();
            s.stopping = true;
            s.build = None;
            s.view = None;
            s.replies.clear();
            self.control.changed.notify_all();
        }
        // 本地文件读通常在下个取消点退出；网络/驱动卡住时不让关闭窗口无界等待。
        // 被分离的线程不持有 HWND、D3D11 或借用事件，停止标志禁止它再发布唤醒。
        if self.done.recv_timeout(Duration::from_secs(2)).is_ok() {
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        } else {
            eprintln!("大图 I/O 未在 2 秒内返回，已停止接收结果，半成品将在下次构建清理");
        }
    }
}

fn configuration() -> Result<(PathBuf, CacheBudget)> {
    let root = std::env::var_os("PIXSTRIDE_TILE_CACHE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Pixstride/tiles-v1"))
        })
        .context("无法确定专用 Tile Cache 目录")?;
    let mut budget = CacheBudget::default();
    if let Ok(value) = std::env::var("PIXSTRIDE_TILE_CACHE_GIB") {
        let gib: u64 = value
            .parse()
            .context("PIXSTRIDE_TILE_CACHE_GIB 必须为正整数")?;
        anyhow::ensure!(
            (1..=10).contains(&gib),
            "磁盘缓存上限必须在 1–10 GiB；超过 2 GiB 需负责人批准"
        );
        budget.disk_bytes = gib * 1024 * 1024 * 1024;
    }
    Ok((root, budget))
}
fn run(control: &Control) {
    let mut cache_key = PathBuf::new();
    let mut cpu: VecDeque<Arc<TilePixels>> = VecDeque::new();
    let mut peak_cpu = 0usize;
    let mut hits = 0u64;
    let mut misses = 0u64;
    loop {
        let (build, view) = {
            let mut s = control.lock();
            while !s.stopping && s.build.is_none() && s.view.is_none() {
                s = control
                    .changed
                    .wait(s)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if s.stopping {
                println!(
                    "Tile CPU resident peak={peak_cpu} bytes; hits={hits} misses={misses}; excludes bounded reply/upload staging"
                );
                return;
            }
            if let Some(b) = s.build.take() {
                (Some(b), None)
            } else {
                (None, s.view.take())
            }
        };
        if let Some(job) = build {
            cpu.clear();
            cache_key.clear();
            let result = (|| -> Result<_> {
                let (root, budget) = configuration()?;
                let (cache, r) = cache::build_or_open(&job.path, &root, budget, || {
                    control.valid(job.generation)
                })?;
                if let Some(scan) = &r.scan {
                    println!(
                        "PNG scan: {}x{}, rows={}, owned_row_bytes={}",
                        scan.size.width, scan.size.height, scan.rows, scan.owned_row_bytes
                    );
                    println!(
                        "PNG phases us: read_rows={} convert_rows={} pyramid_visitor={} (visitor includes writes); build phases={:?}",
                        scan.decode_rows.as_micros(),
                        scan.convert_rows.as_micros(),
                        scan.visitor.as_micros(),
                        r.phases
                    );
                }
                let preview = cache
                    .plan
                    .levels
                    .iter()
                    .find(|l| l.columns == 1 && l.rows == 1)
                    .context("缺少预览层")?
                    .first;
                let pixels = Arc::new(cache.read_tile(preview)?);
                println!(
                    "Tile cache: hit={} build_or_open_us={} scan_us={:?} data_bytes={} tiles={} builder_owned_estimate={} decoder_allowance={}",
                    r.cache_hit,
                    r.elapsed.as_micros(),
                    r.scan.as_ref().map(|s| s.elapsed.as_micros()),
                    cache.plan.bytes,
                    cache.plan.tiles.len(),
                    r.builder_owned_bytes,
                    super::stream::StreamBudget::default().decoder_bytes
                );
                Ok((Arc::new(cache), pixels))
            })();
            if control.valid(job.generation) {
                match result {
                    Ok((cache, preview)) => {
                        cache_key = cache.directory.clone();
                        cpu.clear();
                        control.publish(
                            job.generation,
                            Reply::Ready {
                                generation: job.generation,
                                cache,
                                preview,
                                started: job.started,
                            },
                        );
                    }
                    Err(e) => control.publish(
                        job.generation,
                        Reply::Error {
                            generation: job.generation,
                            message: format!("{e:#}"),
                        },
                    ),
                }
            }
        }
        if let Some(job) = view {
            if job.cache.directory != cache_key {
                cpu.clear();
                cache_key = job.cache.directory.clone();
            }
            for id in job.ids {
                if !control.valid(job.generation) || control.lock().sequence != job.sequence {
                    break;
                }
                let at = Instant::now();
                let queue = job.queued_at.elapsed();
                if let Some(s) = control.shared.upgrade() {
                    s.record(crate::diagnostics::Metric::TileWorkerQueue, queue);
                }
                let found = cpu
                    .iter()
                    .position(|p| p.id == id)
                    .and_then(|i| cpu.remove(i));
                let cpu_hit = found.is_some();
                let mut read = cache::ReadProfile::default();
                if cpu_hit {
                    hits += 1;
                } else {
                    misses += 1;
                }
                let pixels = match found {
                    Some(p) => p,
                    None => match job.cache.read_tile_profiled(id) {
                        Ok((p, profile)) => {
                            read = profile;
                            if let Some(s) = control.shared.upgrade() {
                                s.record(
                                    crate::diagnostics::Metric::TileAllocation,
                                    profile.allocation,
                                );
                                s.record(
                                    crate::diagnostics::Metric::TileFileRead,
                                    profile.file_read,
                                );
                                s.record(crate::diagnostics::Metric::TileCrc, profile.crc);
                            }
                            Arc::new(p)
                        }
                        Err(e) => {
                            control.publish(
                                job.generation,
                                Reply::Error {
                                    generation: job.generation,
                                    message: format!("{e:#}"),
                                },
                            );
                            break;
                        }
                    },
                };
                if let Some(s) = control.shared.upgrade() {
                    s.record(crate::diagnostics::Metric::TileRead, at.elapsed());
                    s.record(
                        if cpu_hit {
                            crate::diagnostics::Metric::TileCpuHit
                        } else {
                            crate::diagnostics::Metric::TileDiskRead
                        },
                        at.elapsed(),
                    );
                }
                cpu.push_back(Arc::clone(&pixels));
                while cpu.iter().map(|p| p.bytes.len()).sum::<usize>() > 64 * 1024 * 1024 {
                    cpu.pop_front();
                }
                peak_cpu = peak_cpu.max(cpu.iter().map(|p| p.bytes.len()).sum());
                if control.valid(job.generation) && control.lock().sequence == job.sequence {
                    control.publish(
                        job.generation,
                        Reply::Tile {
                            generation: job.generation,
                            pixels,
                            sequence: job.sequence,
                            ready_at: Instant::now(),
                            read,
                            cpu_hit,
                            queue,
                            backpressure: Duration::ZERO,
                        },
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_replies_stop_a_blocked_publisher_and_reject_stale_generations() -> Result<()> {
        let (shared, _event) = crate::app::tests::test_shared()?;
        shared.set_image_generation(1);
        let control = Arc::new(Control {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            shared: Arc::downgrade(&shared),
        });
        for _ in 0..2 {
            control.publish(
                1,
                Reply::Error {
                    generation: 1,
                    message: String::new(),
                },
            );
        }
        assert_eq!(control.lock().replies.len(), 2);
        let c = Arc::clone(&control);
        let thread = thread::spawn(move || {
            c.publish(
                1,
                Reply::Error {
                    generation: 1,
                    message: String::new(),
                },
            )
        });
        {
            let mut s = control.lock();
            s.stopping = true;
            control.changed.notify_all();
        }
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("发布线程 panic"))?;
        assert_eq!(control.lock().replies.len(), 2);
        {
            let mut s = control.lock();
            s.stopping = false;
            s.replies.clear();
        }
        shared.set_image_generation(2);
        control.publish(
            1,
            Reply::Error {
                generation: 1,
                message: String::new(),
            },
        );
        assert!(control.lock().replies.is_empty());
        assert!(!control.valid(1));
        assert!(control.valid(2));
        shared.request_shutdown();
        assert!(!control.valid(2));
        Ok(())
    }
    #[test]
    fn latest_build_slot_replaces_old_work_and_idle_shutdown_joins() -> Result<()> {
        let (shared, _event) = crate::app::tests::test_shared()?;
        let control = Arc::new(Control {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            shared: Arc::downgrade(&shared),
        });
        let (_tx, done) = mpsc::channel();
        let worker = Worker {
            control: Arc::clone(&control),
            thread: None,
            done,
        };
        for generation in 1..=1000 {
            worker.build(Request {
                generation,
                path: PathBuf::from("own.png"),
                started: Instant::now(),
            })?;
        }
        assert_eq!(control.lock().build.as_ref().unwrap().generation, 1000);
        control.lock().failed = true;
        assert!(
            worker
                .build(Request {
                    generation: 1001,
                    path: PathBuf::from("new.png"),
                    started: Instant::now()
                })
                .is_err()
        );
        assert_eq!(control.lock().build.as_ref().unwrap().generation, 1000);
        // 测试构造没有线程；不触发正式 Drop 的两秒等待。
        drop(_tx);
        drop(worker);
        let worker = Worker::start(&shared)?;
        let at = Instant::now();
        drop(worker);
        assert!(at.elapsed() < Duration::from_secs(1));
        Ok(())
    }

    #[test]
    fn switching_source_clears_old_replies_and_keeps_only_latest_viewport() -> Result<()> {
        let root = super::super::stream::tests::root("switch");
        let image = root.join("own.png");
        super::super::stream::tests::generate(&image, 9, 11)?;
        let (cache, _) =
            cache::build_or_open(&image, &root.join("tiles"), CacheBudget::default(), || true)?;
        let cache = Arc::new(cache);
        let (shared, _event) = crate::app::tests::test_shared()?;
        shared.set_image_generation(1);
        let control = Arc::new(Control {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            shared: Arc::downgrade(&shared),
        });
        let (tx, done) = mpsc::channel();
        let worker = Worker {
            control: Arc::clone(&control),
            thread: None,
            done,
        };
        control.publish(
            1,
            Reply::Error {
                generation: 1,
                message: "old".into(),
            },
        );
        for generation in 2..=100 {
            shared.set_image_generation(generation);
            worker.build(Request {
                generation,
                path: image.clone(),
                started: Instant::now(),
            })?;
            worker.view(generation, generation, Arc::clone(&cache), vec![0]);
        }
        let state = control.lock();
        assert!(state.replies.is_empty());
        assert_eq!(state.build.as_ref().unwrap().generation, 100);
        assert_eq!(state.view.as_ref().unwrap().sequence, 100);
        drop(state);
        control.publish(
            1,
            Reply::Error {
                generation: 1,
                message: "late".into(),
            },
        );
        assert!(worker.drain().is_empty());
        drop(tx);
        drop(worker);
        drop(cache);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
