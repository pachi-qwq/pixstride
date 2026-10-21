//! 使用同一 JPEG 码流的无方向参考结果验证实际 WIC 方向链，不比较 JPEG 编码前的颜色。

use super::*;
use std::sync::Arc;
use windows::Win32::{Foundation::GENERIC_WRITE, Graphics::Imaging::*};

/// 自行生成的非方形 JPEG；临时文件与正式仓库、私人样本隔离。
pub(crate) struct Fixture {
    pub(crate) root: std::path::PathBuf,
    jpeg: Vec<u8>,
}

impl Fixture {
    /// 在调用线程的 MTA 内生成 JPEG；返回对象拥有文件，并在析构时清理专用目录。
    pub(crate) fn new() -> Result<Self> {
        let root = std::env::var_os("PIXSTRIDE_TEST_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "exif-{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            ));
        fs::create_dir_all(&root)?;
        let path = root.join("reference.jpg");
        // SAFETY: 调用者在本线程持有 ComMta；所有编码接口在函数返回前释放，文件在专用目录。
        unsafe {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let stream = factory.CreateStream()?;
            let wide = wide_nul(&path);
            stream.InitializeFromFilename(PCWSTR(wide.as_ptr()), GENERIC_WRITE.0)?;
            let encoder = factory.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let (mut frame, mut options) = (None, None);
            encoder.CreateNewFrame(&mut frame, &mut options)?;
            let frame = frame.context("缺少 JPEG Frame")?;
            frame.Initialize(None)?;
            frame.SetSize(96, 64)?;
            let mut format = GUID_WICPixelFormat24bppBGR;
            frame.SetPixelFormat(&mut format)?;
            anyhow::ensure!(format == GUID_WICPixelFormat24bppBGR, "JPEG 编码格式变化");
            let mut pixels = Vec::new();
            for y in 0..64 {
                for x in 0..96 {
                    let c = if (32..64).contains(&x) && (20..44).contains(&y) {
                        [240, 240, 240]
                    } else {
                        match (x >= 48, y >= 32) {
                            (false, false) => [12, 20, 230],
                            (true, false) => [20, 220, 15],
                            (false, true) => [230, 30, 20],
                            (true, true) => [20, 220, 230],
                        }
                    };
                    pixels.extend_from_slice(&c);
                }
            }
            frame.WritePixels(64, 96 * 3, &pixels)?;
            frame.Commit()?;
            encoder.Commit()?;
        }
        Ok(Self {
            jpeg: fs::read(path)?,
            root,
        })
    }

    /// 只插入 APP1；原 JPEG 压缩扫描保持逐字节相同。tag 为 TIFF type 和 inline value。
    pub(crate) fn write(&self, name: &str, tag: Option<(u16, u16)>) -> Result<std::path::PathBuf> {
        let mut out = self.jpeg[..2].to_vec();
        if let Some((kind, value)) = tag {
            let mut exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0".to_vec();
            exif.extend_from_slice(&274u16.to_le_bytes());
            exif.extend_from_slice(&kind.to_le_bytes());
            exif.extend_from_slice(&(if kind == 7 { 2u32 } else { 1 }).to_le_bytes());
            exif.extend_from_slice(&value.to_le_bytes());
            exif.extend_from_slice(&[0; 6]);
            out.extend_from_slice(&[0xff, 0xe1]);
            out.extend_from_slice(&u16::try_from(exif.len() + 2)?.to_be_bytes());
            out.extend_from_slice(&exif);
        }
        out.extend_from_slice(&self.jpeg[2..]);
        let path = self.root.join(name);
        fs::write(&path, out)?;
        Ok(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// 独立坐标 oracle：逐源像素映射，与 WIC 枚举/FlipRotator 无共享算法。
pub(crate) fn expected(reference: &DecodedImage, orientation: u16) -> (u32, u32, Vec<u8>) {
    let (w, h) = (reference.width, reference.height);
    let (ow, oh) = if (5..=8).contains(&orientation) {
        (h, w)
    } else {
        (w, h)
    };
    let mut out = vec![0; (ow * oh * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                8 => (y, w - 1 - x),
                _ => (x, y),
            };
            let a = ((y * w + x) * 4) as usize;
            let b = ((dy * ow + dx) * 4) as usize;
            out[b..b + 4].copy_from_slice(&reference.pixels[a..a + 4]);
        }
    }
    (ow, oh, out)
}

/// 通过生产入口解码，调用者负责在同一线程保持 ComMta 存活。
pub(crate) fn decode(path: &Path) -> Result<DecodedImage> {
    decode_wic(
        path,
        &SourceIdentity::read(path)?,
        ImageBudget::default(),
        |_, _| {},
        || true,
    )
}

#[test]
fn exif_all_eight_pixel_maps_and_cpu_cache_revisions() -> Result<()> {
    let _com = ComMta::initialize()?;
    let fixture = Fixture::new()?;
    let reference = decode(&fixture.write("none.jpg", None)?)?;
    let mut cache = super::cache::CpuCache::new(ImageBudget::default().cpu_cache_bytes);
    let mut failures = Vec::new();
    let mut previous = None;
    for orientation in 1..=8 {
        let path = fixture.write("selected.jpg", Some((3, orientation)))?;
        // 内容修订使用不同长度，避免依赖文件系统时间精度。
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(&vec![0; usize::from(orientation)])?;
        let image = Arc::new(decode(&path)?);
        if let Some(key) = &previous {
            assert_ne!(key, &image.source);
        }
        let (w, h, pixels) = expected(&reference, orientation);
        assert_eq!(
            (image.width, image.height),
            (w, h),
            "orientation={orientation}"
        );
        let max_diff = image
            .pixels
            .iter()
            .zip(&pixels)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        if max_diff > 1 {
            failures.push((orientation, max_diff));
        }
        let matches: Vec<_> = (1..=8)
            .filter(|other| {
                let (rw, rh, p) = expected(&reference, *other);
                rw == image.width && rh == image.height && p == image.pixels
            })
            .collect();
        cache.insert(Arc::clone(&image));
        if let Some(key) = previous {
            assert!(cache.get(&key).is_none(), "CPU 不得复用旧文件修订");
        }
        let hit = cache.get(&image.source).unwrap();
        assert!(Arc::ptr_eq(&hit, &image));
        assert_eq!(cache.byte_cost(), image.byte_cost());
        println!(
            "EXIF orientation={orientation} size={w}x{h} max_channel_diff={max_diff} CPU_hit=true actual_matches={matches:?}"
        );
        previous = Some(image.source.clone());
    }
    for (name, tag) in [
        ("missing.jpg", None),
        ("zero.jpg", Some((3, 0))),
        ("nine.jpg", Some((3, 9))),
        ("wrong-type.jpg", Some((7, 6))),
    ] {
        let image = decode(&fixture.write(name, tag)?)?;
        assert_eq!(image.pixels, reference.pixels, "{name}");
        assert_eq!((image.width, image.height), (96, 64));
    }
    assert!(failures.is_empty(), "EXIF 像素差异：{failures:?}");
    Ok(())
}

use std::time::SystemTime;
