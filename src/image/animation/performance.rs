//! 显式 Release 测量；样本生成和测量在不同进程运行，不报告播放 FPS 或 GPU 指标。
use super::*;
use super::{
    fixture::{Frame, sample, write},
    tests::{next, stop, wait_state},
};
use crate::image::gif_tests::memory;
use std::path::Path;

fn root() -> Result<PathBuf> {
    let path = PathBuf::from(
        std::env::var_os("PIXSTRIDE_TEST_ROOT").context("设置仓库外 PIXSTRIDE_TEST_ROOT")?,
    )
    .join("animation-fixtures");
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
fn repeated(size: [u16; 2], count: usize, partial: bool) -> Vec<Frame> {
    (0..count)
        .map(|i| {
            let rect = if partial {
                [(i % 400) as u16, ((i * 3) % 400) as u16, 64, 64]
            } else {
                [0, 0, size[0], size[1]]
            };
            Frame {
                rect,
                indices: vec![1 + (i % 3) as u8; usize::from(rect[2]) * usize::from(rect[3])],
                disposal: (i % 4) as u8,
                delay: (i % 11) as u16,
                transparent: false,
                palette: None,
            }
        })
        .collect()
}
#[test]
#[ignore = "生成自行拥有的小/中/大画布/局部帧 GIF，供独立 Release 进程测量"]
fn release_animation_prepare() -> Result<()> {
    let root = root()?;
    write(&root.join("small.gif"), [4, 3], &sample(), Some(0))?;
    write(
        &root.join("medium.gif"),
        [512, 256],
        &repeated([512, 256], 128, false),
        Some(1),
    )?;
    write(
        &root.join("large.gif"),
        [2048, 1024],
        &repeated([2048, 1024], 4, false),
        None,
    )?;
    write(
        &root.join("partial.gif"),
        [512, 512],
        &repeated([512, 512], 128, true),
        Some(0),
    )?;
    for name in ["small", "medium", "large", "partial"] {
        println!(
            "generated {name} bytes={}",
            std::fs::metadata(root.join(format!("{name}.gif")))?.len()
        );
    }
    Ok(())
}
fn measure(name: &str, delay: Duration) -> Result<()> {
    let path = root()?.join(format!("{name}.gif"));
    ensure!(
        Path::new(&path).is_file(),
        "先运行 release_animation_prepare"
    );
    memory("animation-before")?;
    let pool = CpuMemory::new(crate::image::ImageBudget::default().cpu_cache_bytes);
    let mut worker = AnimationWorker::new(pool.clone());
    let key = worker.start(Request {
        path,
        selection_generation: 1,
        passes: std::num::NonZeroU32::new(1),
        background: RestoreBackground::Transparent,
    })?;
    wait_state(&worker, |s| s.produced == 2);
    let paused_count = worker.snapshot().produced;
    // 这段人为停止消费只测背压；不得混入解码/合成耗时或解释成播放帧率。
    thread::sleep(Duration::from_millis(40));
    assert_eq!(worker.snapshot().produced, paused_count);
    assert_eq!(worker.snapshot().credits, 2);
    memory("animation-consumer-stopped")?;
    let count = worker.snapshot().metadata.context("缺少元数据")?.frames;
    for ordinal in 0..count {
        let frame = next(&worker, key)?;
        assert_eq!(frame.ordinal, u64::from(ordinal));
        if ordinal == 0 {
            memory("animation-first-consumed")?;
        }
        if !delay.is_zero() {
            thread::sleep(delay);
        }
    }
    wait_state(&worker, |s| s.phase == Phase::Completed);
    let usage = pool.usage();
    assert_eq!(usage.animation, 0);
    let stop = stop(&mut worker)?;
    memory("animation-after-worker-exit")?;
    println!(
        "ANIMATION workload={name} frames={count} consumer_delay_us={} credit_peak={} application_cpu_peak={} animation_peak={} final_bytes={} final_credits={} shutdown_us={} (no display/GPU timing)",
        delay.as_micros(),
        worker.snapshot().credit_peak,
        usage.peak,
        usage.animation_peak,
        pool.usage().total,
        worker.snapshot().credits,
        stop.as_micros()
    );
    println!("{}", worker.report());
    Ok(())
}
#[test]
#[ignore = "Windows Release 实际 WIC 小型动画测量"]
fn release_animation_small() -> Result<()> {
    measure("small", Duration::ZERO)
}
#[test]
#[ignore = "Windows Release 实际 WIC 中型动画、快速消费者"]
fn release_animation_medium_fast() -> Result<()> {
    measure("medium", Duration::ZERO)
}
#[test]
#[ignore = "Windows Release 实际 WIC 中型动画、慢速消费者"]
fn release_animation_medium_slow() -> Result<()> {
    measure("medium", Duration::from_millis(2))
}
#[test]
#[ignore = "Windows Release 实际 WIC 大画布、低帧数"]
fn release_animation_large() -> Result<()> {
    measure("large", Duration::ZERO)
}
#[test]
#[ignore = "Windows Release 实际 WIC 多局部帧测量"]
fn release_animation_partial() -> Result<()> {
    measure("partial", Duration::ZERO)
}

#[test]
#[ignore = "Windows Release 同步 CopyPixels 包围区间内取消；不宣称抢占 WIC"]
fn release_animation_cancel_copy_probe() -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let path = root()?.join("medium.gif");
    let pool = CpuMemory::new(crate::image::ImageBudget::default().cpu_cache_bytes);
    let mut worker = AnimationWorker::new(pool.clone());
    worker.start(super::tests::request(
        path.clone(),
        1,
        None,
        RestoreBackground::Transparent,
    ))?;
    wait_state(&worker, |s| s.produced == 2);
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let inside = Arc::new(AtomicBool::new(false));
    worker.core.as_ref().unwrap().lock().copy_probe = Some((Arc::clone(&inside), tx));
    worker.cancel();
    wait_state(&worker, |s| s.phase == Phase::Idle);
    let mut intervals = Vec::new();
    for generation in 2..34 {
        let key = worker.start(super::tests::request(
            path.clone(),
            generation,
            None,
            RestoreBackground::Transparent,
        ))?;
        rx.recv_timeout(Duration::from_secs(5))?;
        let in_window = inside.load(Ordering::SeqCst);
        let at = Instant::now();
        worker.cancel();
        wait_state(&worker, |s| s.phase == Phase::Idle);
        if in_window {
            intervals.push(at.elapsed().as_micros());
        }
        assert!(worker.poll(key).1.is_none());
        assert_eq!(worker.snapshot().credits, 0);
        assert_eq!(pool.usage().total, 0);
        while rx.try_recv().is_ok() {} // 仅清理测试通知；生产者不依赖此通道取得进展。
    }
    ensure!(
        !intervals.is_empty(),
        "没有取得有效 CopyPixels 包围区间样本"
    );
    intervals.sort_unstable();
    let n = intervals.len();
    let percentile = |p: usize| intervals[(n * p).div_ceil(100).saturating_sub(1)];
    println!(
        "ANIMATION cancel-to-session-idle in_copy_window_n={n} p50_us={} p95_us={} p99_us={} max_us={} (flag covers notification and synchronous CopyPixels; not a forced interruption)",
        percentile(50),
        percentile(95),
        percentile(99),
        intervals[n - 1]
    );
    println!(
        "ANIMATION cancel cleanup bytes={} credits={} worker_shutdown_us={}",
        pool.usage().total,
        worker.snapshot().credits,
        stop(&mut worker)?.as_micros()
    );
    Ok(())
}
