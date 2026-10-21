//! Stage D 隔离 GIF 实验入口；不被 Viewer 引用，也不扩大正式格式白名单。

#[path = "gif_probe/clock.rs"]
mod clock;
#[path = "gif_probe/compose.rs"]
mod compose;
#[path = "gif_probe/fixture.rs"]
mod fixture;
#[path = "gif_probe/flow.rs"]
mod flow;
#[path = "gif_probe/wic.rs"]
mod wic;

use anyhow::{Context, Result, ensure};
use flow::{Class, Gate, Output};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

/// 进程计数仅表示整个进程；不能将其与自有缓冲之差全部归因于 WIC。
#[repr(C)]
#[derive(Default)]
struct ProcessMemory {
    cb: u32,
    faults: u32,
    values: [usize; 9],
}
#[link(name = "psapi")]
unsafe extern "system" {
    fn GetProcessMemoryInfo(
        process: windows::Win32::Foundation::HANDLE,
        info: *mut ProcessMemory,
        size: u32,
    ) -> i32;
}
fn memory(label: &str) -> Result<()> {
    let mut m = ProcessMemory {
        cb: std::mem::size_of::<ProcessMemory>() as u32,
        ..Default::default()
    };
    // SAFETY: 计数结构与 PROCESS_MEMORY_COUNTERS_EX 相同布局；当前进程伪句柄无需关闭，输出长度准确。
    let ok = unsafe {
        GetProcessMemoryInfo(
            windows::Win32::System::Threading::GetCurrentProcess(),
            &mut m,
            std::mem::size_of::<ProcessMemory>() as u32,
        )
    };
    ensure!(ok != 0, "读取进程内存失败");
    println!(
        "memory phase={label} working_set={} peak_working_set={} private_bytes={}",
        m.values[1], m.values[0], m.values[8]
    );
    Ok(())
}
fn root() -> Result<PathBuf> {
    // CI 的普通 cargo test 可使用系统临时目录；手动实验必须显式指定仓库外目录。
    let base = match std::env::var_os("PIXSTRIDE_TEST_ROOT") {
        Some(path) => PathBuf::from(path),
        None if cfg!(test) => std::env::temp_dir(),
        None => anyhow::bail!("请指定仓库外 PIXSTRIDE_TEST_ROOT"),
    };
    let p = base.join("gif-prototype");
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

fn correctness(root: &Path) -> Result<()> {
    let _com = wic::Apartment::new()?;
    for loops in [None, Some(0), Some(1), Some(2)] {
        let path = root.join(format!("oracle-{loops:?}.gif"));
        let frames = fixture::sample();
        fixture::write(&path, [4, 3], &frames, loops)?;
        let session = wic::Session::open(&path)?;
        ensure!(
            session.size == [4, 3] && session.frames == 5 && session.loops == loops,
            "全局元数据不符"
        );
        let mut composer = compose::Composer::new(session.size, session.background)?;
        for (i, expected) in fixture::oracle().iter().enumerate() {
            let patch = session.frame(i as u32)?;
            ensure!(
                patch.delay == u32::from(frames[i].delay)
                    && patch.disposal == u32::from(frames[i].disposal),
                "帧元数据不符"
            );
            composer.apply(&patch)?;
            let mismatches = composer
                .pixels
                .iter()
                .zip(expected)
                .filter(|(a, b)| a != b)
                .count();
            println!(
                "oracle loops={loops:?} frame={i} rect={:?} delay_cs={} disposal={} differing_channels={mismatches}",
                patch.rect, patch.delay, patch.disposal
            );
            ensure!(
                mismatches == 0 && expected.len() == composer.pixels.len(),
                "GIF 合成与独立坐标 oracle 不符"
            );
        }
    }
    // 透明初帧不应把未覆盖像素变成局部帧的颜色；明确验证两种背景策略。
    let path = root.join("transparent.gif");
    let frame = fixture::Frame {
        rect: [1, 0, 2, 1],
        indices: vec![0, 2],
        disposal: 2,
        delay: 2,
        transparent: true,
        palette: None,
    };
    let second = fixture::Frame {
        rect: [0, 1, 1, 1],
        indices: vec![1],
        disposal: 1,
        delay: 2,
        transparent: false,
        palette: None,
    };
    fixture::write(&path, [4, 3], &[frame, second], None)?;
    let session = wic::Session::open(&path)?;
    let p = session.frame(0)?;
    ensure!(
        p.pixels[3] == 0 && p.pixels[7] == 255,
        "透明索引 Alpha 不符"
    );
    for background in [[0; 4], session.background] {
        let mut c = compose::Composer::new(session.size, background)?;
        c.apply(&p)?;
        ensure!(
            c.pixels[..4] == background
                && c.pixels[4..8] == background
                && c.pixels[8..12] == [0, 255, 0, 255],
            "透明像素未保留画布"
        );
        c.apply(&session.frame(1)?)?;
        let mut expected = background.repeat(12);
        expected[16..20].copy_from_slice(&[0, 0, 255, 255]);
        ensure!(c.pixels == expected, "透明帧 Disposal 2 恢复区域错误");
    }
    let mut f = fixture::sample();
    f[1].rect[0] = 4;
    let path = root.join("bad-rect.gif");
    fixture::write(&path, [4, 3], &f, None)?;
    ensure!(
        wic::Session::open(&path).and_then(|s| s.frame(1)).is_err(),
        "越界矩形未拒绝"
    );
    let path = root.join("truncated.gif");
    std::fs::write(&path, b"GIF89a\x04\x00")?;
    ensure!(wic::Session::open(&path).is_err(), "截断输入未拒绝");
    let path = root.join("truncated-pixels.gif");
    let data = std::fs::read(root.join("oracle-None.gif"))?;
    std::fs::write(&path, &data[..46])?;
    // WIC 可能容错补齐截断像素：此处只记录实际行为，不把成功当作完整性证明。
    match wic::Session::open(&path).and_then(|s| s.frame(0)) {
        Ok(p) => {
            ensure!(
                p.rect == [0, 0, 4, 3] && p.pixels.len() == 48,
                "截断输入破坏像素布局契约"
            );
            let differences = p
                .pixels
                .iter()
                .zip(&fixture::oracle()[0])
                .filter(|(a, b)| a != b)
                .count();
            println!(
                "truncation: WIC accepted incomplete LZW sub-block, differing_channels={differences}; no integrity guarantee"
            );
        }
        Err(error) => println!("truncation: WIC rejected incomplete LZW sub-block: {error}"),
    }
    let path = root.join("missing-control.gif");
    let mut data = std::fs::read(root.join("oracle-None.gif"))?;
    // 去掉首帧 8 字节 GCE，验证合法的缺失字段采用 0，而不是读取前一帧的值。
    data.drain(25..33);
    std::fs::write(&path, data)?;
    let p = wic::Session::open(&path)?.frame(0)?;
    ensure!(p.delay == 0 && p.disposal == 0, "缺失 GCE 的默认值不符");
    let mut f = fixture::sample();
    f[0].disposal = 7;
    let path = root.join("bad-disposal.gif");
    fixture::write(&path, [4, 3], &f, None)?;
    ensure!(
        wic::Session::open(&path).and_then(|s| s.frame(0)).is_err(),
        "未知 Disposal 未拒绝"
    );
    let path = root.join("zero-screen.gif");
    fixture::write(&path, [0, 3], &fixture::sample(), None)?;
    ensure!(wic::Session::open(&path).is_err(), "零宽度未拒绝");
    let path = root.join("bad-loop.gif");
    fixture::write(&path, [4, 3], &fixture::sample(), Some(1))?;
    let mut data = std::fs::read(&path)?;
    let extension = data
        .windows(11)
        .position(|s| s == b"NETSCAPE2.0")
        .context("缺少生成的 Loop 块")?;
    data[extension + 12] = 2;
    std::fs::write(&path, data)?;
    ensure!(wic::Session::open(&path).is_err(), "非法 Loop 数据未拒绝");
    let path = root.join("many-frames.gif");
    let small = fixture::Frame {
        rect: [0, 0, 1, 1],
        indices: vec![1],
        disposal: 1,
        delay: 2,
        transparent: false,
        palette: None,
    };
    fixture::write(&path, [1, 1], &vec![small; 10001], None)?;
    ensure!(wic::Session::open(&path).is_err(), "异常帧数未拒绝");
    // 丢弃部分画面输出仍必须执行所有合成步骤，最终像素应与逐帧显示一致。
    let s = wic::Session::open(&root.join("oracle-None.gif"))?;
    let mut c = compose::Composer::new(s.size, s.background)?;
    for i in 0..s.frames {
        c.apply(&s.frame(i)?)?;
    }
    ensure!(c.pixels == fixture::oracle()[4], "跳过显示破坏最终合成");
    ensure!(
        compose::Composer::new([u32::MAX, u32::MAX], [0; 4]).is_err(),
        "画布溢出未拒绝"
    );
    let mut clock = clock::Clock::new(
        fixture::sample()
            .iter()
            .map(|f| clock::delay_ms(u32::from(f.delay)))
            .collect(),
        Some(1),
    );
    let oracle = fixture::oracle();
    let mut canvas = compose::Composer::new(s.size, s.background)?;
    canvas.apply(&s.frame(0)?)?;
    let mut composited = 1;
    // 全部帧都已迟到，只选择最终画面显示；循环重启时重置画布，不继承上轮的局部残留。
    while let Some(index) = clock.advance(10000) {
        if index == 0 {
            canvas = compose::Composer::new(s.size, s.background)?;
        }
        canvas.apply(&s.frame(index as u32)?)?;
        ensure!(canvas.pixels == oracle[index], "迟到/循环合成错误");
        composited += 1;
    }
    ensure!(
        composited == 10 && clock.due().is_none(),
        "有限循环没有结束"
    );
    println!(
        "correctness: 20 composite frames matched; transparency/background checks and malformed-input characterization completed (see truncation outcome)"
    );
    Ok(())
}

fn worker_probe(path: &Path, frames_to_consume: usize) -> Result<()> {
    let gate = Gate::new();
    gate.start(1)?;
    // 原型从同一 512 MiB 池保留普通缓存额度，动画不是额外叠加的全局内存。
    let _static_cpu = gate.charge(Class::OrdinaryCpu, 256 * 1024 * 1024)?;
    let _static_gpu = gate.charge(Class::OrdinaryGpu, 384 * 1024 * 1024)?;
    let _gpu_reservation = gate.charge(Class::AnimationGpu, 128 * 1024 * 1024)?;
    let (tx, rx) = mpsc::sync_channel::<Arc<Output>>(2);
    let g = Arc::clone(&gate);
    let path = path.to_owned();
    let worker = std::thread::spawn(move || -> Result<()> {
        let _com = wic::Apartment::new()?;
        // 不透明的 codec 分配无法由 Vec 账本强制限制，先从同一子额度预留实验余量。
        let _codec = g.charge(Class::AnimationCpu, flow::CODEC_ALLOWANCE)?;
        let s = wic::Session::open(&path)?;
        memory("after-open")?;
        let canvas = u64::from(s.size[0]) * u64::from(s.size[1]) * 4;
        // 先预留画布、最大 patch 与 restore 的保守上界，实际像素只按需要分配。
        let _work = g.charge(Class::AnimationCpu, usize::try_from(canvas * 3)?)?;
        let mut c = compose::Composer::new(s.size, s.background)?;
        let mut durations = Vec::new();
        let mut composed = Vec::new();
        let mut patch_peak = 0;
        for i in 0..s.frames {
            let Some(credit) = g.credit(1) else {
                break;
            };
            let patch = s.frame(i)?;
            durations.push(patch.decode.as_micros());
            patch_peak = patch_peak.max(patch.pixels.len());
            // 无法抢占正在执行的 WIC 调用，返回后再次检查，不发布旧 Session。
            if !g.valid(1) {
                break;
            }
            let at = Instant::now();
            c.apply(&patch)?;
            composed.push(at.elapsed().as_micros());
            let output = Output::copy(&g, credit, &c.pixels, i, clock::delay_ms(patch.delay))?;
            if i == 0 || i + 1 == s.frames {
                memory("after-frame")?;
            }
            // 两个 Credit 包含消费者持有项；队列不可能在这里达到第三个元素，因此不阻塞 send。
            if let Err(error) = tx.try_send(output) {
                // 停止可发生在上次校验之后；断开的消费者不能让正常取消变成错误。
                if g.valid(1) {
                    return Err(anyhow::anyhow!("有界输出失败：{error}"));
                }
                break;
            }
        }
        durations.sort_unstable();
        composed.sort_unstable();
        let quantile = |v: &[u128], p: usize| {
            v.get((v.len() * p).div_ceil(100).saturating_sub(1))
                .copied()
                .unwrap_or(0)
        };
        println!(
            "wic frames={} open_us={} decode_p50_us={} decode_p95_us={} decode_p99_us={} decode_max_us={} compose_p50_us={} compose_p95_us={} canvas_bytes={canvas} patch_peak={patch_peak} restore_peak={}",
            durations.len(),
            s.open.as_micros(),
            quantile(&durations, 50),
            quantile(&durations, 95),
            quantile(&durations, 99),
            durations.last().copied().unwrap_or(0),
            quantile(&composed, 50),
            quantile(&composed, 95),
            c.restore_peak
        );
        Ok(())
    });
    let result = (|| -> Result<()> {
        for i in 0..frames_to_consume {
            let output = rx.recv_timeout(Duration::from_secs(10))?;
            ensure!(
                output.session == 1
                    && output.frame_index == i as u32
                    && output.delay_ms >= 20
                    && !output.pixels.is_empty(),
                "输出源或像素错误"
            );
            if i == 0 {
                gate.pause(true);
                std::thread::sleep(Duration::from_millis(40));
                gate.pause(false);
            }
            drop(output);
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    })();
    let stop = Instant::now();
    gate.stop();
    drop(rx);
    let joined = worker
        .join()
        .map_err(|_| anyhow::anyhow!("实验 Worker panic"))?;
    let stats = gate.stats();
    println!(
        "worker stop_us={} animation_bytes={} credits={} peak_reserved={} peak_credits={}; GPU reservation only, no GPU allocations",
        stop.elapsed().as_micros(),
        stats.0,
        stats.1,
        stats.2,
        stats.3
    );
    result?;
    joined?;
    ensure!(stats.0 == 0 && stats.1 == 0 && stats.3 <= 2, "资源未归还");
    Ok(())
}

/// 真正调用 WIC 后才检查停止状态；记录 stop 是否落在同步 API 调用区间内。
fn cancellation_probe(path: &Path) -> Result<()> {
    let mut samples = Vec::new();
    for _ in 0..32 {
        let gate = Gate::new();
        gate.start(1)?;
        let g = Arc::clone(&gate);
        let path = path.to_owned();
        let (entered, notification) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || -> Result<_> {
            let _com = wic::Apartment::new()?;
            let _codec = g.charge(Class::AnimationCpu, flow::CODEC_ALLOWANCE)?;
            let s = wic::Session::open(&path)?;
            let _patch = g.charge(
                Class::AnimationCpu,
                usize::try_from(u64::from(s.size[0]) * u64::from(s.size[1]) * 4)?,
            )?;
            let credit = g.credit(1).context("Session 已停止")?;
            let start = Instant::now();
            entered.send(())?;
            let patch = s.frame(0)?;
            let end = Instant::now();
            let valid = g.valid(1);
            drop((patch, credit));
            Ok((start, end, valid))
        });
        let entered = notification.recv_timeout(Duration::from_secs(10));
        let stop = Instant::now();
        gate.stop();
        let result = worker
            .join()
            .map_err(|_| anyhow::anyhow!("取消实验 Worker panic"))?;
        entered?;
        let (start, end, valid) = result?;
        if start <= stop && stop <= end {
            ensure!(!valid && gate.stats().1 == 0, "停止后仍接受旧帧");
            samples.push(stop.elapsed().as_micros());
        }
    }
    samples.sort_unstable();
    let q = |p: usize| {
        samples
            .get((samples.len() * p).div_ceil(100).saturating_sub(1))
            .copied()
    };
    println!(
        "WIC cancellation in_call_samples={} stop_to_join_p50_us={:?} p95_us={:?} p99_us={:?} max_us={:?}; other calls finished before stop, no hard cancellation deadline",
        samples.len(),
        q(50),
        q(95),
        q(99),
        samples.last()
    );
    ensure!(
        !samples.is_empty(),
        "没有取得调用期间的取消样本，需换用更大合成图复测"
    );
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let root = root()?;
    if args.get(1).is_some_and(|s| s == "cancel") {
        return cancellation_probe(Path::new(args.get(2).context("缺合成 GIF 路径")?));
    }
    if args.get(1).is_some_and(|s| s == "generate") {
        let frames = args.get(2).context("缺帧数")?.parse::<usize>()?;
        ensure!(frames <= 256, "实验生成上限256帧");
        let f = fixture::Frame {
            rect: [0, 0, 512, 256],
            indices: vec![1; 512 * 256],
            disposal: 3,
            delay: 2,
            transparent: false,
            palette: None,
        };
        fixture::write(
            &root.join(format!("memory-{frames}.gif")),
            [512, 256],
            &vec![f; frames],
            Some(0),
        )?;
        return Ok(());
    }
    if let Some(path) = args.get(1).filter(|s| *s != "self-test") {
        memory("before-wic")?;
        worker_probe(Path::new(path), args.get(2).context("缺消费帧数")?.parse()?)?;
        memory("after-worker")?;
    } else {
        correctness(&root)?;
        let path = root.join("oracle-None.gif");
        worker_probe(&path, 3)?;
        let mut c = clock::Clock::new(vec![clock::delay_ms(0), 20], Some(0));
        c.suspend(5, true, false);
        ensure!(c.due().is_none(), "暂停失败");
        c.suspend(100, false, false);
        ensure!(c.advance(115) == Some(1), "时钟失败");
        c.cancel();
        let g = Gate::new();
        g.start(1)?;
        g.switch(2);
        ensure!(!g.valid(1), "切换后旧 Session 仍有效");
        g.stop();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn wic_and_composition_oracle() -> anyhow::Result<()> {
        let root = super::root()?.join(format!("test-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let result = super::correctness(&root);
        // root 是本测试创建的专用子目录，不会清理手动实验生成的样本。
        std::fs::remove_dir_all(root)?;
        result
    }
}
