//! 自行生成 GIF 并通过生产入口核对独立像素参考，Fixture 不依赖 Prototype 模块。
use super::*;
use std::{sync::Arc, time::SystemTime};

/// 测试独占的仓库外目录；退出时只清理本对象创建的目录。
pub(crate) struct Directory(pub(crate) PathBuf);
impl Directory {
    /// PIXSTRIDE_TEST_ROOT 可由本地诊断指定；CI 使用自身临时目录。
    pub(crate) fn new() -> Result<Self> {
        let root = std::env::var_os("PIXSTRIDE_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "gif-poster-{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            ));
        fs::create_dir_all(&root)?;
        Ok(Self(root))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn frame(out: &mut Vec<u8>, rect: [u16; 4], transparent: bool, local: bool) {
    out.extend_from_slice(&[0x21, 0xf9, 4, u8::from(transparent), 2, 0, 0, 0, 0x2c]);
    for v in rect {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.push(if local { 0x81 } else { 0 });
    if local {
        out.extend_from_slice(&[0, 0, 0, 255, 255, 0, 0, 255, 0, 0, 0, 255]);
    }
    out.push(2);
    let count = usize::from(rect[2]) * usize::from(rect[3]);
    let mut data = Vec::new();
    let (mut bits, mut n) = (0u32, 0);
    // 独立、低效但确定性的 LZW 写入：每个 literal 之前 clear，码宽恒为 3。
    for code in (0..count)
        .flat_map(|i| [4, if i % 2 == 0 { 1 } else { 0 }])
        .chain([5])
    {
        bits |= code << n;
        n += 3;
        while n >= 8 {
            data.push(bits as u8);
            bits >>= 8;
            n -= 8;
        }
    }
    if n > 0 {
        data.push(bits as u8);
    }
    for part in data.chunks(255) {
        out.push(part.len() as u8);
        out.extend_from_slice(part);
    }
    out.push(0);
}

/// 生成带有效绿色全局背景的 GIF；poster 仍必须保留透明画布。
pub(crate) fn generated(
    size: [u16; 2],
    rect: [u16; 4],
    transparent: bool,
    multiple: bool,
    local: bool,
) -> Vec<u8> {
    let mut out = b"GIF89a".to_vec();
    for v in size {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&[0x81, 2, 0, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]);
    frame(&mut out, rect, transparent, local);
    if multiple {
        frame(&mut out, [0, 0, size[0], size[1]], false, false);
    }
    out.push(0x3b);
    out
}

/// 小型局部首帧：4×3 画布，仅 (1,1) 为红，其余透明；后续帧不能影响 poster。
pub(crate) fn sample(multiple: bool) -> Vec<u8> {
    generated([4, 3], [1, 1, 2, 1], true, multiple, false)
}

pub(crate) fn decode(path: &Path) -> Result<DecodedImage> {
    decode_wic(
        path,
        &SourceIdentity::read(path)?,
        ImageBudget::default(),
        |_, _| {},
        || true,
    )
}

/// 保留合成 GUI 样本的独立根目录；只有显式 Release 诊断调用，不接触私人图片。
pub(crate) fn gui_root() -> Result<PathBuf> {
    let root = PathBuf::from(
        std::env::var_os("PIXSTRIDE_TEST_ROOT").context("设置仓库外 PIXSTRIDE_TEST_ROOT")?,
    )
    .join("gif-poster-gui");
    fs::create_dir_all(&root)?;
    Ok(root)
}

#[repr(C)]
#[derive(Default)]
struct Memory {
    cb: u32,
    faults: u32,
    values: [usize; 9],
}
#[link(name = "psapi")]
unsafe extern "system" {
    fn GetProcessMemoryInfo(
        process: windows::Win32::Foundation::HANDLE,
        info: *mut Memory,
        size: u32,
    ) -> i32;
}
/// 测试诊断复用的进程内存采样；包含 WIC、DLL、allocator 和测试框架。
pub(crate) fn memory(label: &str) -> Result<()> {
    let mut m = Memory {
        cb: std::mem::size_of::<Memory>() as u32,
        ..Default::default()
    };
    // SAFETY: 结构布局为 PROCESS_MEMORY_COUNTERS_EX，输出长度准确，当前进程伪句柄不需要关闭。
    anyhow::ensure!(
        unsafe {
            GetProcessMemoryInfo(
                windows::Win32::System::Threading::GetCurrentProcess(),
                &mut m,
                std::mem::size_of::<Memory>() as u32,
            )
        } != 0,
        "内存采样失败"
    );
    println!(
        "GIF process memory phase={label} ws={} peak_ws={} private_sample={} bytes (whole process, not WIC-only)",
        m.values[1], m.values[0], m.values[8]
    );
    Ok(())
}

#[test]
#[ignore = "Release GIF 生产解码内存与分项诊断；先用独立进程生成 GUI 样本"]
fn release_gif_poster_memory() -> Result<()> {
    use crate::diagnostics::{Metric, Metrics};
    let root = gui_root()?;
    let _com = ComMta::initialize()?;
    memory("before-factory")?;
    let metrics = std::cell::RefCell::new(Metrics::default());
    for name in ["memory-full.gif", "memory-partial.gif"] {
        let path = root.join(name);
        let identity = SourceIdentity::read(&path)?;
        let mut times = Vec::new();
        for i in 0..32 {
            let at = Instant::now();
            let image = decode_wic(
                &path,
                &identity,
                ImageBudget::default(),
                |m, d| metrics.borrow_mut().record(m, d),
                || true,
            )?;
            assert_eq!(image.byte_cost(), 8 * 1024 * 1024);
            times.push(at.elapsed().as_micros());
            if i == 0 || i == 31 {
                memory(name)?;
            }
        }
        times.sort_unstable();
        println!(
            "GIF production decode file={name} n=32 p50_us={} p95_us={} p99_us={} max_us={} owned_canvas_bytes=8388608 no_owned_patch_bitmap=true",
            times[15], times[30], times[31], times[31]
        );
    }
    memory("after-release")?;
    println!("{}", metrics.borrow().report());
    // 仅检查新增分项确实被生产路径记录。
    assert_eq!(metrics.borrow().sample_count(Metric::GifStructure), 64);
    Ok(())
}

#[test]
fn wic_poster_pixels_transparency_local_palette_and_cpu_revision() -> Result<()> {
    let _com = ComMta::initialize()?;
    let dir = Directory::new()?;
    let mut cache = super::cache::CpuCache::new(ImageBudget::default().cpu_cache_bytes);
    let mut previous = None;
    for (revision, (transparent, multiple, local)) in [
        (true, false, false),
        (true, true, false),
        (false, false, false),
        (true, false, true),
    ]
    .into_iter()
    .enumerate()
    {
        let path = dir.0.join("局部 首帧.gif");
        let mut bytes = generated([4, 3], [1, 1, 2, 1], transparent, multiple, local);
        // 在 Trailer 前加入合法 Comment，独立验证元数据修订，不依赖尾随零填充策略。
        bytes.pop();
        bytes.extend_from_slice(&[0x21, 0xfe, (revision + 1) as u8]);
        bytes.extend(std::iter::repeat_n(b'x', revision + 1));
        bytes.extend_from_slice(&[0, 0x3b]);
        fs::write(&path, bytes)?;
        let image = Arc::new(decode(&path)?);
        assert_eq!((image.width, image.height, image.stride), (4, 3, 16));
        assert_eq!(image.source.content, ImageContent::GifPosterV1);
        // 手工坐标表，完全独立于预检、WIC 元数据和运行时合成。
        let mut expected = vec![0; 48];
        expected[20..24].copy_from_slice(if local {
            &[0, 255, 255, 255]
        } else {
            &[0, 0, 255, 255]
        });
        if !transparent {
            expected[24..28].copy_from_slice(&[0, 0, 0, 255]);
        }
        assert_eq!(image.pixels, expected);
        cache.insert(Arc::clone(&image));
        if let Some(old) = previous {
            assert!(cache.get(&old).is_none());
        }
        assert!(Arc::ptr_eq(&cache.get(&image.source).unwrap(), &image));
        assert_eq!(cache.byte_cost(), 48);
        let mut wrong = image.source.clone();
        wrong.content = ImageContent::Static;
        assert!(
            cache.get(&wrong).is_none(),
            "poster 用途必须参与 CPU 缓存身份"
        );
        previous = Some(image.source.clone());
        println!(
            "GIF poster revision={revision} multi={multiple} transparent={transparent} local_palette={local} differing_channels=0 CPU_hit=true bytes=48"
        );
    }
    let full = dir.0.join("full.gif");
    fs::write(&full, generated([3, 2], [0, 0, 3, 2], false, false, false))?;
    let full = decode(&full)?;
    assert_eq!(
        full.pixels,
        [[0, 0, 255, 255], [0, 0, 0, 255]].repeat(3).concat()
    );
    let local_only = dir.0.join("local-only.gif");
    let mut bytes = generated([4, 3], [1, 1, 2, 1], true, false, true);
    bytes[10] &= 0x7f;
    bytes.drain(13..25);
    fs::write(&local_only, bytes)?;
    let local = decode(&local_only)?;
    let mut expected = vec![0; 48];
    expected[20..24].copy_from_slice(&[0, 255, 255, 255]);
    assert_eq!(
        local.pixels, expected,
        "无全局表时仍使用局部 palette 和透明画布"
    );
    let legacy = dir.0.join("legacy87.gif");
    let mut bytes = generated([4, 3], [1, 1, 2, 1], false, false, false);
    bytes[..6].copy_from_slice(b"GIF87a");
    bytes.drain(25..33);
    fs::write(&legacy, bytes)?;
    let legacy = decode(&legacy)?;
    let mut expected = vec![0; 48];
    expected[20..24].copy_from_slice(&[0, 0, 255, 255]);
    expected[24..28].copy_from_slice(&[0, 0, 0, 255]);
    assert_eq!(legacy.pixels, expected);
    Ok(())
}

#[test]
fn wic_zero_padding_preserves_pixels_and_invalidates_source_revision() -> Result<()> {
    let _com = ComMta::initialize()?;
    let dir = Directory::new()?;
    let path = dir.0.join("padded.gif");
    let bytes = sample(true);
    fs::write(&path, &bytes)?;
    let reference = Arc::new(decode(&path)?);
    let mut cache = super::cache::CpuCache::new(ImageBudget::default().cpu_cache_bytes);
    cache.insert(Arc::clone(&reference));
    let mut previous = reference.source.clone();
    for padding in [167, 3 * 8192 + 17] {
        let mut padded = bytes.clone();
        padded.resize(padded.len() + padding, 0);
        fs::write(&path, padded)?;
        let image = Arc::new(decode(&path)?);
        assert_eq!(image.pixels, reference.pixels);
        assert_eq!(image.source.content, ImageContent::GifPosterV1);
        assert_ne!(image.source, previous);
        cache.insert(Arc::clone(&image));
        assert!(cache.get(&previous).is_none());
        assert!(Arc::ptr_eq(&cache.get(&image.source).unwrap(), &image));
        assert_eq!(cache.byte_cost(), 48);
        previous = image.source.clone();
    }
    let mut mixed = bytes;
    mixed.extend_from_slice(&[0, 1, 0]);
    fs::write(&path, mixed)?;
    assert!(decode(&path).unwrap_err().to_string().contains("非零"));
    Ok(())
}

#[test]
fn malformed_budget_container_and_file_lock() -> Result<()> {
    let _com = ComMta::initialize()?;
    let dir = Directory::new()?;
    let path = dir.0.join("broken.gif");
    let valid = sample(false);
    for n in 0..valid.len() {
        fs::write(&path, &valid[..n])?;
        assert!(decode(&path).is_err(), "prefix={n}");
    }
    for (at, value) in [
        (11, 255),
        (10, 0x87),
        (34, 4),
        (31, 250),
        (33, 0x10),
        (28, 28),
    ] {
        let mut data = valid.clone();
        data[at] = value;
        fs::write(&path, data)?;
        assert!(decode(&path).is_err(), "bad field {at}");
    }
    let mut oversized = valid.clone();
    oversized[6..8].copy_from_slice(&20000u16.to_le_bytes());
    fs::write(&path, oversized)?;
    assert!(decode(&path).unwrap_err().is::<BudgetRejection>());
    fs::write(&path, &valid)?;
    let small_budget = ImageBudget {
        max_decoded_bytes: 47,
        ..Default::default()
    };
    assert!(
        decode_wic(
            &path,
            &SourceIdentity::read(&path)?,
            small_budget,
            |_, _| {},
            || true
        )
        .unwrap_err()
        .is::<BudgetRejection>()
    );
    let guard = super::gif::check_file(&path, ImageBudget::default(), &|| true)?;
    assert!(fs::OpenOptions::new().write(true).open(&path).is_err());
    drop(guard);
    assert!(fs::OpenOptions::new().write(true).open(&path).is_ok());
    let alias = dir.0.join("not-a-static.jpg");
    fs::write(&alias, valid)?;
    assert!(decode(&alias).is_err(), "GIF 内容不能绕过预检入口");
    fs::write(&path, b"not a GIF")?;
    assert!(decode(&path).is_err());
    fs::remove_file(&path)?;
    assert!(decode(&path).is_err());
    Ok(())
}
