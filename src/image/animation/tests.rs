//! 动画后台协议的确定性测试与显式 Windows Release 诊断；不驱动屏幕播放。
use super::fixture::{oracle, sample, write};
use super::*;
use crate::image::ImageBudget;
use crate::image::gif_tests::Directory;
use windows::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject};

pub(super) fn request(
    path: PathBuf,
    generation: u64,
    passes: Option<u32>,
    background: RestoreBackground,
) -> Request {
    Request {
        path,
        selection_generation: generation,
        passes: passes.map(|n| std::num::NonZeroU32::new(n).unwrap()),
        background,
    }
}
pub(super) fn wait_state(
    worker: &AnimationWorker,
    predicate: impl Fn(&Snapshot) -> bool,
) -> Snapshot {
    let core = worker.core.as_ref().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut s = core.lock();
    loop {
        let result = snapshot(&s);
        if predicate(&result) {
            return result;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "状态等待超时：{result:?}");
        s = core.changed.wait_timeout(s, remaining).unwrap().0;
    }
}
pub(super) fn next(worker: &AnimationWorker, key: SessionKey) -> Result<Arc<FrameOutput>> {
    let at = Instant::now();
    loop {
        let (s, frame) = worker.poll(key);
        if let Some(frame) = frame {
            return Ok(frame);
        }
        ensure!(
            !matches!(s.phase, Phase::Failed | Phase::Stopped),
            "动画错误：{s:?}"
        );
        ensure!(at.elapsed() < Duration::from_secs(5), "等待帧超时：{s:?}");
        let handle = worker.ready_handle().context("没有 ready event")?;
        // SAFETY: 测试持有控制器，不关闭借用事件；5 秒是测试失败期限，不是 WIC 退出承诺。
        unsafe { WaitForSingleObject(handle, 5000) };
    }
}
pub(super) fn stop(worker: &mut AnimationWorker) -> Result<Duration> {
    let at = Instant::now();
    worker.shutdown();
    if let Some(handle) = worker.done_handle() {
        // SAFETY: 控制器活到 join 后，借用完成事件只用于测试等待。
        ensure!(
            unsafe { WaitForSingleObject(handle, 5000) } == WAIT_OBJECT_0,
            "动画退出超时"
        );
    }
    if let Some(thread) = worker.thread.take() {
        thread.join().map_err(|_| anyhow::anyhow!("Worker panic"))?;
    }
    ensure!(worker.try_join()?, "结束后不能 join");
    Ok(at.elapsed())
}

#[test]
fn animation_pixels_raw_metadata_and_two_passes() -> Result<()> {
    let dir = Directory::new()?;
    let pool = CpuMemory::new(ImageBudget::default().cpu_cache_bytes);
    let mut worker = AnimationWorker::new(pool.clone());
    assert!(!worker.snapshot().worker_started);
    assert!(worker.ready_handle().is_none());
    for loops in [None, Some(0), Some(1), Some(2)] {
        let path = dir.0.join("pixels.gif");
        write(&path, [4, 3], &sample(), loops)?;
        for background in [
            RestoreBackground::LogicalScreen,
            RestoreBackground::Transparent,
        ] {
            let key = worker.start(request(path.clone(), 20, Some(2), background))?;
            let reference = oracle();
            for ordinal in 0..10 {
                let frame = next(&worker, key)?;
                let mut expected = reference[ordinal % 5].clone();
                if matches!(background, RestoreBackground::Transparent) {
                    for pixel in expected.chunks_exact_mut(4) {
                        if pixel == [0, 0, 0, 255] {
                            pixel.fill(0);
                        }
                    }
                }
                assert_eq!(
                    frame.pixels(),
                    expected,
                    "ordinal={ordinal} background={background:?}"
                );
                assert_eq!(frame.session, key);
                assert_eq!(frame.ordinal, ordinal as u64);
                assert_eq!(frame.frame_index, ordinal as u32 % 5);
                assert_eq!(frame.pass, ordinal as u32 / 5);
                assert_eq!(frame.delay_cs, [0, 1, 2, 10, 3][ordinal % 5]);
                assert_eq!(frame.metadata.raw_loop, loops);
                assert_eq!(
                    frame.metadata.source.content,
                    crate::image::ImageContent::GifAnimationV1
                );
            }
            wait_state(&worker, |s| s.phase == Phase::Completed);
            assert_eq!(worker.memory_usage().animation, 0);
        }
    }
    stop(&mut worker)?;
    assert_eq!(pool.usage().total, 0);
    Ok(())
}

#[test]
fn animation_credits_pause_cancel_replacement_and_last_arc() -> Result<()> {
    let dir = Directory::new()?;
    let path = dir.0.join("credits.gif");
    write(&path, [4, 3], &sample(), Some(0))?;
    let pool = CpuMemory::new(4096);
    let mut worker = AnimationWorker::new(pool.clone());
    let key = worker.start(request(
        path.clone(),
        1,
        None,
        RestoreBackground::Transparent,
    ))?;
    wait_state(&worker, |s| s.produced == 2);
    let thread_id = worker.thread.as_ref().unwrap().thread().id();
    let one = next(&worker, key)?;
    let two = next(&worker, key)?;
    let clone = Arc::clone(&one);
    assert_eq!(worker.snapshot().credits, 2);
    assert!(worker.pause(key, true));
    wait_state(&worker, |s| s.phase == Phase::Paused);
    drop(one);
    drop(two); // clone 仍持第一个 Credit；暂停时归还另一个也不继续生产。
    thread::sleep(Duration::from_millis(20));
    assert_eq!(worker.snapshot().produced, 2);
    assert_eq!(worker.snapshot().credits, 1);
    assert!(worker.pause(key, false));
    let held = next(&worker, key)?;
    let new = worker.start(request(
        path.clone(),
        2,
        Some(1),
        RestoreBackground::Transparent,
    ))?;
    assert_eq!(worker.thread.as_ref().unwrap().thread().id(), thread_id);
    assert!(!worker.is_current(&clone));
    assert!(worker.poll(key).1.is_none());
    assert_eq!(worker.snapshot().credits, 2);
    drop(clone);
    let new_frame = next(&worker, new)?;
    assert_eq!(new_frame.session, new);
    assert_eq!(new_frame.ordinal, 0);
    worker.cancel();
    wait_state(&worker, |s| s.phase == Phase::Idle);
    assert!(!worker.is_current(&new_frame));
    assert_eq!(worker.snapshot().credits, 2);
    assert_eq!(pool.usage().animation, 96);
    drop(held);
    assert_eq!(worker.snapshot().credits, 1);
    drop(new_frame);
    assert_eq!(pool.usage().animation, 0);
    assert_eq!(worker.snapshot().credit_peak, 2);
    stop(&mut worker)?;
    Ok(())
}

#[test]
fn animation_shutdown_while_two_outputs_are_held() -> Result<()> {
    let dir = Directory::new()?;
    let path = dir.0.join("stop.gif");
    write(&path, [4, 3], &sample(), None)?;
    let pool = CpuMemory::new(4096);
    let mut worker = AnimationWorker::new(pool.clone());
    let key = worker.start(request(path, 1, None, RestoreBackground::Transparent))?;
    let one = next(&worker, key)?;
    let two = next(&worker, key)?;
    stop(&mut worker)?;
    assert_eq!(worker.snapshot().phase, Phase::Stopped);
    assert_eq!(worker.snapshot().credits, 2);
    assert_eq!(pool.usage().animation, 96);
    drop(one);
    drop(two);
    assert_eq!(pool.usage().total, 0);
    assert_eq!(worker.snapshot().credits, 0);
    Ok(())
}

#[test]
fn animation_replacement_during_synchronous_work_and_nonblocking_drop() -> Result<()> {
    let dir = Directory::new()?;
    let path = dir.0.join("gate.gif");
    write(&path, [4, 3], &sample(), None)?;
    let pool = CpuMemory::new(4096);
    let mut worker = AnimationWorker::new(pool.clone());
    let (entered, started) = std::sync::mpsc::sync_channel(1);
    let (release, finish) = std::sync::mpsc::channel();
    worker.initial_gate = Some((entered, finish));
    let old = worker.start(request(
        path.clone(),
        1,
        None,
        RestoreBackground::Transparent,
    ))?;
    started.recv_timeout(Duration::from_secs(5))?;
    let id = worker.thread.as_ref().unwrap().thread().id();
    let mut latest = old;
    for generation in 2..102 {
        latest = worker.start(request(
            path.clone(),
            generation,
            Some(1),
            RestoreBackground::Transparent,
        ))?;
    }
    assert_eq!(worker.thread.as_ref().unwrap().thread().id(), id);
    assert!(!worker.try_join()?);
    assert_eq!(worker.snapshot().produced, 0);
    release.send(())?;
    let frame = next(&worker, latest)?;
    assert_eq!(frame.session, latest);
    drop(frame);
    worker.cancel();
    wait_state(&worker, |s| s.phase == Phase::Idle);
    stop(&mut worker)?;
    assert_eq!(pool.usage().total, 0);

    // gate 位于真实 CopyPixels 返回后，用于模拟不可抢占阶段，不冒称在 WIC 内部挂起。
    let mut worker = AnimationWorker::new(pool.clone());
    let (entered, started) = std::sync::mpsc::sync_channel(1);
    let (release, finish) = std::sync::mpsc::channel();
    worker.initial_gate = Some((entered, finish));
    worker.start(request(path, 3, None, RestoreBackground::Transparent))?;
    started.recv_timeout(Duration::from_secs(5))?;
    let core = Arc::clone(worker.core.as_ref().unwrap());
    drop(worker); // 不在 UI 析构路径上 join，Core/COM/事件仍由同一后台线程保活。
    assert!(core.lock().shutdown);
    release.send(())?;
    // SAFETY: core 的 Arc 保留 done 事件，线程退出前不释放句柄。
    assert_eq!(
        unsafe { WaitForSingleObject(core.done.handle(), 5000) },
        WAIT_OBJECT_0
    );
    assert_eq!(pool.usage().total, 0);
    Ok(())
}

#[test]
fn animation_failures_revisions_and_repeated_sessions() -> Result<()> {
    let dir = Directory::new()?;
    let path = dir.0.join("revision.gif");
    write(&path, [4, 3], &sample(), None)?;
    let pool = CpuMemory::new(4096);
    let mut worker = AnimationWorker::new(pool.clone());
    let key = worker.start(request(
        path.clone(),
        1,
        Some(1),
        RestoreBackground::Transparent,
    ))?;
    let frame = next(&worker, key)?;
    let old = frame.metadata.source.clone();
    drop(frame);
    worker.cancel();
    wait_state(&worker, |s| s.phase == Phase::Idle);
    write(&path, [4, 3], &sample(), Some(0))?;
    let key = worker.start(request(
        path.clone(),
        2,
        Some(1),
        RestoreBackground::Transparent,
    ))?;
    let frame = next(&worker, key)?;
    assert_ne!(old, frame.metadata.source);
    drop(frame);
    worker.cancel();
    wait_state(&worker, |s| s.phase == Phase::Idle);
    for _ in 0..20 {
        let key = worker.start(request(
            path.clone(),
            3,
            None,
            RestoreBackground::Transparent,
        ))?;
        drop(next(&worker, key)?);
        worker.cancel();
        wait_state(&worker, |s| s.phase == Phase::Idle);
        assert_eq!(pool.usage().total, 0);
        assert_eq!(worker.snapshot().credits, 0);
    }
    std::fs::write(&path, b"GIF89a")?;
    worker.start(request(
        path.clone(),
        4,
        Some(1),
        RestoreBackground::Transparent,
    ))?;
    assert!(
        wait_state(&worker, |s| s.phase == Phase::Failed)
            .error
            .is_some()
    );
    stop(&mut worker)?;
    let mut failed = AnimationWorker::new(pool.clone());
    failed.fail_init = true;
    failed.start(request(path, 5, None, RestoreBackground::Transparent))?;
    assert!(
        wait_state(&failed, |s| s.phase == Phase::Failed)
            .error
            .is_some()
    );
    assert!(
        failed
            .start(request(
                dir.0.join("unused.gif"),
                6,
                None,
                RestoreBackground::Transparent
            ))
            .is_err()
    );
    stop(&mut failed)?;
    assert_eq!(pool.usage().total, 0);
    Ok(())
}

#[test]
fn animation_budget_and_invalid_metadata_do_not_change_poster_policy() -> Result<()> {
    let dir = Directory::new()?;
    let path = dir.0.join("metadata.gif");
    let mut frames = sample();
    frames.truncate(1);
    write(&path, [8192, 8192], &frames, None)?;
    let pool = CpuMemory::new(ImageBudget::default().cpu_cache_bytes);
    let mut worker = AnimationWorker::new(pool.clone());
    worker.start(request(
        path.clone(),
        1,
        Some(1),
        RestoreBackground::Transparent,
    ))?;
    assert!(
        wait_state(&worker, |s| s.phase == Phase::Failed)
            .error
            .unwrap()
            .contains("最坏")
    );
    assert_eq!(pool.usage().total, 0);
    write(&path, [4, 3], &frames, Some(0))?;
    let mut bytes = std::fs::read(&path)?;
    let ext = bytes[25..44].to_vec();
    bytes.splice(44..44, ext);
    std::fs::write(&path, &bytes)?;
    worker.start(request(
        path.clone(),
        2,
        Some(1),
        RestoreBackground::Transparent,
    ))?;
    assert!(
        wait_state(&worker, |s| s.phase == Phase::Failed)
            .error
            .unwrap()
            .contains("重复 Loop")
    );
    let _com = crate::image::ComMta::initialize()?;
    assert!(
        crate::image::gif_tests::decode(&path).is_ok(),
        "Poster 仍只检查 Loop 的结构边界"
    );
    stop(&mut worker)?;
    Ok(())
}

#[test]
fn animation_shares_static_loader_budget_and_preserves_cpu_hits() -> Result<()> {
    use crate::image::worker::{ImageLoader, LoaderMessage};
    let dir = Directory::new()?;
    let a = dir.0.join("a.gif");
    let b = dir.0.join("b.gif");
    let small = dir.0.join("small.gif");
    let poster = crate::image::gif_tests::generated([8, 8], [0, 0, 8, 8], false, false, false);
    std::fs::write(&a, &poster)?;
    std::fs::write(&b, &poster)?;
    write(&small, [4, 3], &sample(), None)?;
    let (shared, _event) = crate::app::tests::test_shared()?;
    let budget = ImageBudget {
        cpu_cache_bytes: 512,
        ..Default::default()
    };
    let mut loader = ImageLoader::start(budget, Arc::clone(&shared))?;
    let load = |loader: &ImageLoader, generation, path: PathBuf| -> Result<()> {
        shared.set_navigation_generation(generation, None);
        loader.request_identity(generation, path, Instant::now(), Vec::new(), None);
        loop {
            if let Some(message) = loader.try_receive() {
                match message {
                    LoaderMessage::IdentityResolved {
                        generation: g,
                        path,
                        requested_at,
                        preloads,
                        result,
                    } if g == generation => {
                        // 测试消费者模拟现有 UI 的 GPU miss 答复，不能省略身份→解码握手。
                        loader.request_decode(crate::app::ResolvedSelection {
                            generation: g,
                            path,
                            requested_at,
                            preloads,
                            identity: result.map_err(|e| anyhow::anyhow!(e))?,
                        });
                    }
                    LoaderMessage::Current {
                        generation: g,
                        result,
                        ..
                    } if g == generation => {
                        let (image, _) = result.map_err(|e| anyhow::anyhow!(e))?;
                        assert!(image._charge.is_some());
                        return Ok(());
                    }
                    _ => {}
                }
            } else {
                // SAFETY: loader 持有 ready 事件；不在正式 UI 中使用此测试等待。
                ensure!(
                    unsafe { WaitForSingleObject(loader.ready_handle(), 5000) } == WAIT_OBJECT_0,
                    "静态 Worker 超时"
                );
            }
        }
    };
    load(&loader, 1, a.clone())?;
    load(&loader, 2, b)?;
    assert_eq!(loader.animation().memory_usage().total, 512);
    loader
        .animation()
        .start(request(small, 2, None, RestoreBackground::Transparent))?;
    assert!(
        wait_state(loader.animation(), |s| s.phase == Phase::Failed)
            .error
            .unwrap()
            .contains("CPU")
    );
    assert_eq!(loader.animation().memory_usage().peak, 512);
    assert_eq!(loader.animation().memory_usage().animation, 0);
    load(&loader, 3, a)?; // 动画拒绝没有破坏仍有效的静态缓存。
    assert_eq!(loader.animation().memory_usage().total, 512);
    stop(loader.animation())?;
    loader.request_stop();
    loader.join()?;
    assert_eq!(loader.animation().memory_usage().total, 0);
    Ok(())
}

#[test]
fn animation_first_partial_matches_poster_and_legacy_missing_gce() -> Result<()> {
    let dir = Directory::new()?;
    let path = dir.0.join("first.gif");
    let mut padded = crate::image::gif_tests::sample(false);
    padded.extend_from_slice(&[0; 167]);
    std::fs::write(&path, padded)?;
    let _com = crate::image::ComMta::initialize()?;
    let poster = crate::image::gif_tests::decode(&path)?;
    let mut worker = AnimationWorker::new(CpuMemory::new(4096));
    let key = worker.start(request(
        path.clone(),
        1,
        Some(1),
        RestoreBackground::LogicalScreen,
    ))?;
    let frame = next(&worker, key)?;
    assert_eq!(frame.pixels(), poster.pixels);
    assert_eq!(frame.delay_cs, 2);
    assert_ne!(frame.metadata.source.content, poster.source.content);
    drop(frame);
    wait_state(&worker, |s| s.phase == Phase::Completed);
    let mut legacy = crate::image::gif_tests::generated([4, 3], [1, 1, 2, 1], false, false, false);
    legacy[..6].copy_from_slice(b"GIF87a");
    legacy.drain(25..33);
    std::fs::write(&path, legacy)?;
    let key = worker.start(request(path, 2, Some(1), RestoreBackground::Transparent))?;
    let frame = next(&worker, key)?;
    assert_eq!(frame.delay_cs, 0);
    assert_eq!(frame.metadata.raw_loop, None);
    let mut expected = vec![0; 48];
    expected[20..24].copy_from_slice(&[0, 0, 255, 255]);
    expected[24..28].copy_from_slice(&[0, 0, 0, 255]);
    assert_eq!(frame.pixels(), expected);
    drop(frame);
    stop(&mut worker)?;
    Ok(())
}

#[test]
fn animation_disposal_two_uses_explicit_background_not_viewer_background() -> Result<()> {
    let dir = Directory::new()?;
    let path = dir.0.join("background.gif");
    let frames = vec![
        fixture::Frame {
            rect: [0, 0, 2, 1],
            indices: vec![1, 1],
            disposal: 2,
            delay: u16::MAX,
            transparent: false,
            palette: None,
        },
        fixture::Frame {
            rect: [1, 0, 1, 1],
            indices: vec![3],
            disposal: 1,
            delay: 0,
            transparent: false,
            palette: None,
        },
    ];
    write(&path, [2, 1], &frames, None)?;
    let mut data = std::fs::read(&path)?;
    data[11] = 2;
    std::fs::write(&path, data)?;
    let mut worker = AnimationWorker::new(CpuMemory::new(4096));
    for mode in [
        RestoreBackground::Transparent,
        RestoreBackground::LogicalScreen,
    ] {
        let key = worker.start(request(path.clone(), 1, Some(1), mode))?;
        let first = next(&worker, key)?;
        assert_eq!(first.delay_cs, u16::MAX);
        drop(first);
        let second = next(&worker, key)?;
        assert_eq!(second.metadata.logical_background, Some([0, 255, 0, 255]));
        let left = if matches!(mode, RestoreBackground::Transparent) {
            [0, 0, 0, 0]
        } else {
            [0, 255, 0, 255]
        };
        assert_eq!(second.pixels(), [left, [255, 0, 0, 255]].concat());
        drop(second);
        wait_state(&worker, |s| s.phase == Phase::Completed);
    }
    stop(&mut worker)?;
    Ok(())
}

#[test]
fn animation_failure_discards_queued_frames_and_invalidates_held_output() -> Result<()> {
    let dir = Directory::new()?;
    let path = dir.0.join("pressure.gif");
    write(&path, [4, 3], &sample(), None)?;
    for consume_first in [false, true] {
        let pool = CpuMemory::new(4096);
        let mut worker = AnimationWorker::new(pool.clone());
        let (entered, started) = std::sync::mpsc::sync_channel(1);
        let (release, finish) = std::sync::mpsc::channel();
        worker.initial_gate = Some((entered, finish));
        let key = worker.start(request(
            path.clone(),
            1,
            None,
            RestoreBackground::Transparent,
        ))?;
        started.recv_timeout(Duration::from_secs(5))?;
        // 第一帧仍在既有 CopyPixels 后门控中，先为第二帧安放门控，避免抢先跑过检查点。
        let (second_entered, second_started) = std::sync::mpsc::sync_channel(1);
        let (second_release, second_finish) = std::sync::mpsc::channel();
        worker.core.as_ref().unwrap().lock().gate = Some((second_entered, second_finish));
        release.send(())?;
        second_started.recv_timeout(Duration::from_secs(5))?;
        assert_eq!(worker.snapshot().produced, 1);
        assert_eq!(worker.snapshot().credits, 2);
        let held = if consume_first {
            Some(next(&worker, key)?)
        } else {
            assert_eq!(worker.snapshot().queued, 1);
            None
        };
        // Worker 停在第二帧合成前，画布、局部帧和第一份输出都不再变化。
        // 仅“收到输出”不能保证上一份局部帧已析构；必须在稳定分配边界注入压力。
        let pressure = pool.reserve(4096 - pool.usage().total, false)?;
        assert_eq!(pool.usage().total, 4096);
        second_release.send(())?;
        // Failed 先失效输出，队列像素随后在锁外析构；等 Credit 通知确认清理完成。
        let failed = wait_state(&worker, |s| {
            s.phase == Phase::Failed && s.credits == u8::from(consume_first)
        });
        assert!(failed.error.as_ref().unwrap().contains("CPU"));
        assert!(worker.poll(key).1.is_none());
        assert_eq!(worker.snapshot().queued, 0);
        if let Some(frame) = &held {
            assert!(!worker.is_current(frame));
            assert_eq!(worker.snapshot().credits, 1);
            assert_eq!(pool.usage().animation, 48);
        } else {
            assert_eq!(worker.snapshot().credits, 0);
            assert_eq!(pool.usage().animation, 0);
        }
        drop(pressure);
        drop(held);
        assert_eq!(pool.usage().total, 0);
        stop(&mut worker)?;
    }
    Ok(())
}

#[test]
fn animation_pressure_sample_can_be_invalidated_by_patch_release() -> Result<()> {
    let pool = CpuMemory::new(4096);
    let canvas = Pixels::new(&pool, 48)?;
    let first_output = Pixels::new(&pool, 48)?;
    let second_output = Pixels::new(&pool, 48)?;
    let second_patch = Pixels::new(&pool, 16)?;
    // 重现与原 CI 快照一致的顺序：发布第二份输出时局部帧仍存活，测试据此填满额度。
    let pressure = pool.reserve(4096 - pool.usage().total, false)?;
    assert_eq!(pool.usage().total, 4096);
    drop(second_patch);
    drop(first_output);
    // 释放共 64 字节后，第三帧的 8 字节局部帧和 48 字节输出都能成功分配。
    // 因此原测试等待 Failed 并不成立；此处额度和分配器的行为本来就是正确的。
    let third_patch = Pixels::new(&pool, 8)?;
    let third_output = Pixels::new(&pool, 48)?;
    assert_eq!(pool.usage().total, 4088);
    drop((canvas, second_output, third_patch, third_output, pressure));
    assert_eq!(pool.usage().total, 0);
    Ok(())
}

#[test]
fn animation_paused_cancel_shutdown_and_queued_replacement() -> Result<()> {
    let dir = Directory::new()?;
    let path = dir.0.join("paused.gif");
    write(&path, [4, 3], &sample(), None)?;
    let pool = CpuMemory::new(4096);
    let mut worker = AnimationWorker::new(pool.clone());
    let first = worker.start(request(
        path.clone(),
        1,
        None,
        RestoreBackground::Transparent,
    ))?;
    wait_state(&worker, |s| s.produced == 2);
    let thread = worker.thread.as_ref().unwrap().thread().id();
    let second = worker.start(request(
        path.clone(),
        2,
        None,
        RestoreBackground::Transparent,
    ))?;
    assert!(worker.poll(first).1.is_none());
    let frame = next(&worker, second)?;
    assert_eq!(frame.session, second);
    drop(frame);
    assert_eq!(worker.thread.as_ref().unwrap().thread().id(), thread);
    assert!(worker.pause(second, true));
    wait_state(&worker, |s| s.phase == Phase::Paused);
    worker.cancel();
    wait_state(&worker, |s| s.phase == Phase::Idle);
    assert_eq!(pool.usage().total, 0);
    assert_eq!(worker.snapshot().credits, 0);
    let third = worker.start(request(path, 3, None, RestoreBackground::Transparent))?;
    assert!(worker.pause(third, true));
    wait_state(&worker, |s| s.phase == Phase::Paused);
    stop(&mut worker)?;
    assert_eq!(pool.usage().total, 0);
    assert_eq!(worker.snapshot().credits, 0);
    Ok(())
}
