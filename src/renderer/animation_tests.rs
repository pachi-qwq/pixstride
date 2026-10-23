//! 实际 WIC 输出经生产上传函数进入 D3D11；回读参考来自人工坐标表，不由上传器生成。
use super::*;
use crate::image::{
    ImageBudget,
    animation::{
        fixture,
        tests::{next, stop, wait_state},
    },
    gif_tests::{Directory, memory},
    memory::CpuMemory,
};
use windows::Win32::Graphics::Direct3D11::*;

#[test]
fn frame_order_rejects_duplicates_gaps_invalid_pass_and_overflow() {
    assert!(valid_frame_order(None, 0, 0, 0, 5));
    assert!(valid_frame_order(Some(4), 5, 0, 1, 5));
    for (last, ordinal, index, pass, frames) in [
        (None, 1, 1, 0, 5),
        (Some(1), 1, 1, 0, 5),
        (Some(1), 3, 3, 0, 5),
        (Some(4), 5, 0, 0, 5),
        (None, 0, 0, 0, 0),
        (Some(u64::MAX), 0, 0, 0, 5),
    ] {
        assert!(!valid_frame_order(last, ordinal, index, pass, frames));
    }
}

#[test]
fn selection_generation_and_session_id_are_independent() {
    let a = SessionKey {
        selection_generation: 7,
        id: 1,
    };
    assert_ne!(
        a,
        SessionKey {
            selection_generation: 8,
            id: 1
        }
    );
    assert_ne!(
        a,
        SessionKey {
            selection_generation: 7,
            id: 2
        }
    );
}

fn readback(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
) -> Result<Vec<u8>> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: texture 是本测试持有的有效 COM 引用，desc 是有效输出。
    unsafe { texture.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    let mut staging = None;
    // SAFETY: staging 尺寸与格式来自源纹理，无初始指针。
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging)) }?;
    let staging = staging.context("缺少 staging")?;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: 当前线程独占 Immediate Context；Map(READ) 等待这份复制完成，不使用 Frame Latency 充当 Fence。
    unsafe {
        context.CopyResource(&staging, texture);
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
    }
    let mut result = Vec::new();
    for y in 0..desc.Height as usize {
        // SAFETY: 行数与宽度来自同一纹理描述，Map 成功且 RowPitch 由 D3D11 提供；指针只在 Unmap 前借用。
        result.extend_from_slice(unsafe {
            std::slice::from_raw_parts(
                (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                desc.Width as usize * 4,
            )
        });
    }
    // SAFETY: 所有行借用已结束，配对成功的 Map。
    unsafe { context.Unmap(&staging, 0) };
    Ok(result)
}

#[test]
#[ignore = "Windows Release 动画 Texture 更新、GPU 回读、Credit 与共享预算诊断"]
fn release_animation_gpu_frames() -> Result<()> {
    memory("playback-gpu-before")?;
    let dir = Directory::new()?;
    let path = dir.0.join("pixels.gif");
    fixture::write(&path, [4, 3], &fixture::sample(), Some(1))?;
    let pool = CpuMemory::new(ImageBudget::default().cpu_cache_bytes);
    let mut worker = AnimationWorker::new(pool.clone());
    let key = worker.start(Request {
        path,
        selection_generation: 1,
        passes: std::num::NonZeroU32::new(2),
        background: RestoreBackground::Transparent,
    })?;
    let (device, _, context, _) = create_device()?;
    let mut gpu = GpuFrames::default();
    let mut texture_id = None;
    let oracle = fixture::oracle();
    let mut times = Vec::new();
    for ordinal in 0..10 {
        let frame = next(&worker, key)?;
        let mut expected = oracle[ordinal % 5].clone();
        for pixel in expected.chunks_exact_mut(4) {
            if pixel == [0, 0, 0, 255] {
                pixel.fill(0);
            }
        }
        let source = frame.metadata.source.clone();
        if ordinal == 0 {
            assert!(
                gpu.upload(
                    &device,
                    &context,
                    &frame,
                    &source,
                    512 * 1024 * 1024,
                    512 * 1024 * 1024
                )
                .is_err()
            );
            assert_eq!(gpu.bytes(), 0, "共享总额度不足不应分配资源");
        }
        let at = Instant::now();
        gpu.upload(&device, &context, &frame, &source, 0, 512 * 1024 * 1024)?;
        assert!(gpu.view(&source, Some(1)).is_some());
        assert!(
            gpu.view(&source, Some(2)).is_none(),
            "同一源的新 Generation 不能复用旧 Session 画面"
        );
        times.push(at.elapsed().as_micros());
        let texture = gpu.active.as_ref().unwrap().texture.clone();
        let id = texture.as_raw();
        if let Some(old) = texture_id {
            assert_eq!(id, old, "同尺寸所有帧应复用同一纹理");
        } else {
            texture_id = Some(id);
        }
        // 先释放 CPU 帧，再等待 GPU 回读；证明源像素无需保活到 GPU 执行完成。
        drop(frame);
        assert_eq!(
            readback(&device, &context, &texture)?,
            expected,
            "ordinal={ordinal}"
        );
    }
    wait_state(&worker, |s| s.phase == Phase::Completed);
    assert_eq!(worker.snapshot().credits, 0);
    assert_eq!(pool.usage().total, 0);
    gpu.retire(&device, &context)?;
    assert_eq!(gpu.bytes(), 48, "退役 Query 完成前仍须计费");
    // 后续 READ Map 是独立测试同步点，确保排在 EVENT Query 后的 GPU 工作已完成。
    let texture = gpu.retired[0].slot.texture.clone();
    readback(&device, &context, &texture)?;
    gpu.collect(&context)?;
    assert_eq!(gpu.bytes(), 0);
    let shutdown = stop(&mut worker)?;
    times.sort_unstable();
    println!(
        "GPU animation exact_pixels=120 uploads=10 texture_objects=1 payload_peak={} upload_p50_us={} upload_p95_us={} shutdown_us={} credits={} bytes={}",
        gpu.peak,
        times[4],
        times[9],
        shutdown.as_micros(),
        worker.snapshot().credits,
        pool.usage().total
    );
    memory("playback-gpu-after")?;
    Ok(())
}

#[test]
#[ignore = "Windows Release 门控松键竞态：旧进度结果与最新 GPU 探测同批次"]
fn release_stale_progress_keeps_latest_gpu_probe() -> Result<()> {
    use windows::{
        Win32::{
            Foundation::CloseHandle,
            System::Threading::{CreateEventW, WaitForSingleObject},
            UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW},
        },
        core::w,
    };
    struct Notify(HANDLE);
    impl Drop for Notify {
        fn drop(&mut self) {
            // SAFETY: 诊断已 join 唯一 Render Thread，之后没有发布者使用此 UI 通知事件。
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    let dir = Directory::new()?;
    let path = dir.0.join("poster.gif");
    fixture::write(&path, [4, 3], &fixture::sample(), Some(0))?;
    let _com = crate::image::ComMta::initialize()?;
    let image = Arc::new(crate::image::gif_tests::decode(&path)?);
    let (shared, _event) = crate::app::tests::test_shared()?;
    // SAFETY: 无名 auto-reset event，由此测试保活至 Render join 后。
    let notification = Notify(unsafe { CreateEventW(None, false, false, None) }?);
    shared.set_ui_ready_event(notification.0);
    shared.resize(128, 96);
    shared.set_navigation_generation(3, Some(1));
    shared.set_image(2, Arc::clone(&image), Instant::now(), false);
    shared.request_gpu_probe(crate::app::ResolvedSelection {
        generation: 3,
        path: path.clone(),
        identity: image.source.clone(),
        requested_at: Instant::now(),
        preloads: vec![],
    });
    // SAFETY: 使用系统 STATIC 类，不携带短期 Rust 指针；窗口只由当前测试线程销毁。
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("batch handoff"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            128,
            96,
            None,
            None,
            None,
            None,
        )
    }?;
    let (entered_tx, entered) = std::sync::mpsc::sync_channel(1);
    let (release, gate) = std::sync::mpsc::sync_channel(1);
    let rs = Arc::clone(&shared);
    let raw = hwnd.0 as isize;
    let thread = std::thread::spawn(move || -> Result<()> {
        let mut renderer = Renderer::new(HWND(raw as _), &rs, Instant::now())?;
        renderer.batch_gate = Some((entered_tx, gate));
        renderer.run(&rs)
    });
    let result = (|| -> Result<()> {
        entered.recv_timeout(Duration::from_secs(5))?;
        // Render 已取走同一批次的旧图片 2 与最新探测 3；此时松键只失效图片 2。
        shared.end_navigation_progress();
        assert!(!shared.can_present_image_generation(2));
        release.send(())?;
        // SAFETY: UI 通知事件仍存活，测试超时只标记失败，不改变生产等待策略。
        anyhow::ensure!(
            unsafe { WaitForSingleObject(notification.0, 5000) }
                == windows::Win32::Foundation::WAIT_OBJECT_0,
            "旧进度被拒收时丢失了同批次最新 GPU 探测答复"
        );
        assert!(
            matches!(shared.take_gpu_probe_reply(), Some(GpuProbeReply::Miss(s)) if s.generation == 3)
        );
        Ok(())
    })();
    let _ = release.try_send(());
    shared.request_shutdown();
    let rr = thread
        .join()
        .map_err(|_| anyhow!("批次门控测试 Render panic"))?;
    // SAFETY: Render 已 join，当前测试线程仍拥有窗口。
    unsafe { DestroyWindow(hwnd) }?;
    result?;
    rr?;
    println!(
        "Stale batch handoff: obsolete image rejected, latest GPU probe delivered, no final-target starvation"
    );
    Ok(())
}
