//! 定义图片像素契约、WIC 解码和普通图片的资源预算。

mod cache;
pub(crate) mod navigation;
pub(crate) mod worker;

use std::{
    ffi::OsStr,
    fs,
    hash::Hash,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::{Duration, Instant, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use windows::{
    Win32::{
        Foundation::GENERIC_READ,
        Graphics::Imaging::{
            CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource,
            IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
            WICBitmapTransformFlipHorizontal, WICBitmapTransformFlipVertical,
            WICBitmapTransformOptions, WICBitmapTransformRotate0, WICBitmapTransformRotate90,
            WICBitmapTransformRotate180, WICBitmapTransformRotate270,
            WICDecodeMetadataCacheOnDemand,
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
            StructuredStorage::PROPVARIANT,
            StructuredStorage::{PropVariantClear, PropVariantToUInt16},
        },
    },
    core::{Interface, PCWSTR, w},
};

/// 普通图片的资源限制。默认值是可调整的工程初值，不代表格式或 WIC 的理论上限。
#[derive(Clone, Copy, Debug)]
pub(crate) struct ImageBudget {
    pub(crate) max_dimension: u32,
    pub(crate) max_decoded_bytes: u64,
    pub(crate) cpu_cache_bytes: u64,
    pub(crate) gpu_cache_bytes: u64,
}

impl Default for ImageBudget {
    fn default() -> Self {
        Self {
            max_dimension: 16_384,
            max_decoded_bytes: 256 * 1024 * 1024,
            cpu_cache_bytes: 512 * 1024 * 1024,
            gpu_cache_bytes: 512 * 1024 * 1024,
        }
    }
}

/// 当前解码契约使用 WIC 的 32-bit premultiplied BGRA 像素。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PixelFormat {
    Bgra8Premultiplied,
}

/// 由路径、文件长度和最后修改时间组成的源修订键。
///
/// 它能识别常见的替换和修改，但元数据相同的原位内容更改仍可能碰撞；后续若实测需要，
/// 可增加内容摘要。键用于缓存，不宣称是不可伪造的文件身份。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SourceIdentity {
    pub(crate) path: PathBuf,
    pub(crate) length: u64,
    modified_nanos: Option<u128>,
}

impl SourceIdentity {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let canonical = fs::canonicalize(path)
            .with_context(|| format!("无法解析图片路径：{}", path.display()))?;
        let metadata = fs::metadata(&canonical)
            .with_context(|| format!("无法读取图片信息：{}", canonical.display()))?;
        if !metadata.is_file() {
            bail!("所选路径不是文件：{}", canonical.display());
        }
        let modified_nanos = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        Ok(Self {
            path: canonical,
            length: metadata.len(),
            modified_nanos,
        })
    }
}

/// WIC 解码后的、可在线程间安全传递的拥有型像素数据。
///
/// `pixels` 采用 premultiplied BGRA8，`stride` 是每行字节数；数据不包含 WIC COM 接口，
/// 因而可以通过 `Arc` 交给渲染线程。GPU 纹理创建使用相同的 BGRA 通道顺序。
#[derive(Debug)]
pub(crate) struct DecodedImage {
    pub(crate) source: SourceIdentity,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: PixelFormat,
    pub(crate) stride: u32,
    pub(crate) pixels: Vec<u8>,
}

pub(crate) struct WicTimings {
    pub(crate) open: Duration,
    pub(crate) conversion: Duration,
    pub(crate) copy_pixels: Duration,
}

impl DecodedImage {
    pub(crate) fn byte_cost(&self) -> u64 {
        self.pixels.len() as u64
    }
}

/// 计算紧密排列的 BGRA8 缓冲区布局，并在分配前拒绝超限图片。
pub(crate) fn checked_layout(
    width: u32,
    height: u32,
    budget: ImageBudget,
) -> Result<(u32, usize, u64)> {
    if width == 0 || height == 0 {
        bail!("图片尺寸不能为零");
    }
    if width > budget.max_dimension || height > budget.max_dimension {
        bail!(
            "图片尺寸 {width}×{height} 超过单张纹理边长上限 {}；超大图片路径尚未实现",
            budget.max_dimension
        );
    }
    let stride = u64::from(width)
        .checked_mul(4)
        .context("图片行跨度计算溢出")?;
    let bytes = stride
        .checked_mul(u64::from(height))
        .context("图片像素缓冲区大小计算溢出")?;
    if bytes > budget.max_decoded_bytes {
        bail!(
            "图片解码需要 {bytes} 字节，超过当前单图 CPU 预算 {} 字节",
            budget.max_decoded_bytes
        );
    }
    let stride = u32::try_from(stride).context("图片行跨度超过 WIC 支持范围")?;
    let allocation = usize::try_from(bytes).context("图片缓冲区超过当前进程地址范围")?;
    if allocation > u32::MAX as usize {
        bail!("图片缓冲区超过 WIC CopyPixels 的 32-bit 长度上限");
    }
    Ok((stride, allocation, bytes))
}

/// 返回首批支持的普通静态图片扩展名。
pub(crate) fn is_supported_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "bmp"
            )
        })
}

/// 在线程内初始化 MTA，并确保该线程退出前调用 `CoUninitialize`。
pub(crate) struct ComMta;

impl ComMta {
    pub(crate) fn initialize() -> Result<Self> {
        // SAFETY: 仅在专用解码线程调用，线程尚未创建其他 COM apartment；调用成功后由 Drop 配对清理。
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .context("初始化 WIC 解码线程的 COM MTA 失败")?;
        Ok(Self)
    }
}

impl Drop for ComMta {
    fn drop(&mut self) {
        // SAFETY: 此 guard 只在 CoInitializeEx 成功后创建，并在同一线程析构。
        unsafe { CoUninitialize() };
    }
}

/// 使用当前线程的 WIC MTA 解码 JPEG、PNG 或 BMP 的首帧。
pub(crate) fn decode_wic(path: &Path, budget: ImageBudget) -> Result<(DecodedImage, WicTimings)> {
    if !is_supported_path(path) {
        bail!("不支持的图片扩展名：{}", path.display());
    }
    let identity = SourceIdentity::read(path)?;
    let wide_path = wide_nul(&identity.path);
    let open_started = Instant::now();
    // SAFETY: 当前线程已初始化 MTA；CoCreateInstance 返回由本地变量拥有的进程内接口，所有 WIC 对象都在同一线程释放。
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .context("创建 WIC Imaging Factory 失败")?;
    // SAFETY: UTF-16 路径以 NUL 结尾且在 WIC 调用期间保持有效；只读打开文件，元数据按需读取。
    let decoder = unsafe {
        factory.CreateDecoderFromFilename(
            PCWSTR(wide_path.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )
    }
    .with_context(|| format!("WIC 无法打开图片：{}", identity.path.display()))?;
    // SAFETY: 当前支持格式按单帧静态图处理；索引 0 从 Decoder 的有效帧范围读取。
    let frame = unsafe { decoder.GetFrame(0) }.context("读取图片首帧失败")?;
    let orientation = exif_orientation(&frame);
    let transform = orientation_transform(orientation);
    let open_elapsed = open_started.elapsed();

    let source: IWICBitmapSource = if transform == WICBitmapTransformRotate0 {
        frame.cast().context("转换 WIC Bitmap Source 接口失败")?
    } else {
        // SAFETY: WIC Factory、frame 和 rotator 均在当前 MTA 线程持有；Initialize 保留源 COM 引用。
        let rotator = unsafe { factory.CreateBitmapFlipRotator() }
            .context("创建 EXIF Orientation 转换器失败")?;
        // SAFETY: 转换选项来自 EXIF Orientation 的已知映射值；源帧只在当前线程访问。
        unsafe { rotator.Initialize(&frame, transform) }.context("应用 EXIF Orientation 失败")?;
        rotator
            .cast()
            .context("转换旋转后的 WIC Bitmap Source 接口失败")?
    };

    let mut width = 0;
    let mut height = 0;
    // SAFETY: 输出指针指向当前栈帧内两个有效 u32 槽，source 由当前线程持有。
    unsafe { source.GetSize(&mut width, &mut height) }.context("读取图片尺寸失败")?;
    let (stride, buffer_len, _) = checked_layout(width, height, budget)?;

    let conversion_started = Instant::now();
    // SAFETY: Factory 与 source 属于当前 MTA 线程；转换器输出与 GPU 目标格式约定一致的 PBGRA8。
    let converter = unsafe { factory.CreateFormatConverter() }
        .context("创建 WIC Pixel Format Converter 失败")?;
    // SAFETY: `source` 在转换器初始化期间有效；输出 GUID 指向静态常量，palette=None 表示真彩色转换。
    unsafe {
        converter.Initialize(
            &source,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
    }
    .context("将图片转换为 premultiplied BGRA8 失败")?;
    let conversion_elapsed = conversion_started.elapsed();

    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(buffer_len)
        .context("为图片像素缓冲区预留内存失败")?;
    pixels.resize(buffer_len, 0);
    let copy_started = Instant::now();
    // SAFETY: stride 与宽度匹配 BGRA8，每行有 stride 字节，已分配长度等于 stride × height；WIC 只写入此切片。
    unsafe { converter.CopyPixels(std::ptr::null(), stride, &mut pixels) }
        .context("WIC 解码图片像素失败")?;
    let copy_elapsed = copy_started.elapsed();

    let after = SourceIdentity::read(&identity.path)?;
    if identity != after {
        bail!("图片在解码期间发生变化，已丢弃本次结果");
    }
    Ok((
        DecodedImage {
            source: identity,
            width,
            height,
            format: PixelFormat::Bgra8Premultiplied,
            stride,
            pixels,
        },
        WicTimings {
            open: open_elapsed,
            conversion: conversion_elapsed,
            copy_pixels: copy_elapsed,
        },
    ))
}

fn wide_nul(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn exif_orientation(frame: &windows::Win32::Graphics::Imaging::IWICBitmapFrameDecode) -> u16 {
    // SAFETY: frame 是当前 MTA 线程持有的有效接口；Metadata Query Reader 也只在本函数和该线程使用。
    let Ok(reader) = (unsafe { frame.GetMetadataQueryReader() }) else {
        return 1;
    };
    let mut value = PROPVARIANT::default();
    // SAFETY: reader 是当前 MTA 线程拥有的接口，`value` 是可写的零初始化 PROPVARIANT 槽。
    if unsafe { reader.GetMetadataByName(w!("/app1/ifd/{ushort=274}"), &mut value) }.is_err() {
        // SAFETY: PROPVARIANT 以零值初始化，即使查询失败也必须在此线程清理可能已写入的字段。
        let _ = unsafe { PropVariantClear(&mut value) };
        return 1;
    }
    // SAFETY: PropVariantToUInt16 只读取 GetMetadataByName 填充的 PROPVARIANT；类型转换失败时回退到无旋转。
    let orientation = unsafe { PropVariantToUInt16(&value) }.unwrap_or(1);
    // SAFETY: value 由 WIC 填充，并且当前函数独占其生命周期；清理其中可能分配的 COM 内存。
    let _ = unsafe { PropVariantClear(&mut value) };
    if (1..=8).contains(&orientation) {
        orientation
    } else {
        1
    }
}

fn orientation_transform(value: u16) -> WICBitmapTransformOptions {
    match value {
        2 => WICBitmapTransformFlipHorizontal,
        3 => WICBitmapTransformRotate180,
        4 => WICBitmapTransformFlipVertical,
        5 => WICBitmapTransformOptions(
            WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0,
        ),
        6 => WICBitmapTransformRotate90,
        7 => WICBitmapTransformOptions(
            WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0,
        ),
        8 => WICBitmapTransformRotate270,
        _ => WICBitmapTransformRotate0,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CLSID_WICImagingFactory, ComMta, IWICImagingFactory, ImageBudget, PixelFormat,
        checked_layout, decode_wic, is_supported_path, orientation_transform, wide_nul,
    };
    use anyhow::{Context, Result};
    use std::path::{Path, PathBuf};
    use windows::Win32::Graphics::Imaging::{
        WICBitmapTransformFlipHorizontal, WICBitmapTransformRotate0, WICBitmapTransformRotate90,
    };

    fn unique_test_dir(prefix: &str) -> PathBuf {
        let root = std::env::var_os("PIXSTRIDE_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn computes_checked_bgra_layout() {
        let (stride, length, bytes) = checked_layout(3, 2, ImageBudget::default()).unwrap();
        assert_eq!((stride, length, bytes), (12, 24, 24));
    }

    #[test]
    fn rejects_zero_overflow_budget_and_d3d_texture_dimensions() {
        let budget = ImageBudget::default();
        assert!(checked_layout(0, 1, budget).is_err());
        assert!(checked_layout(u32::MAX, u32::MAX, budget).is_err());
        assert!(checked_layout(16_385, 1, budget).is_err());
        assert!(checked_layout(9000, 9000, budget).is_err());
    }

    #[test]
    fn accepts_only_stage_b_static_extensions_case_insensitively() {
        for name in ["photo.JPG", "image.jpeg", "image.PnG", "scan.bmp"] {
            assert!(is_supported_path(Path::new(name)));
        }
        for name in ["animation.gif", "camera.raw", "vector.svg", "text.png.exe"] {
            assert!(!is_supported_path(Path::new(name)));
        }
    }

    #[test]
    fn maps_exif_orientation_to_wic_transform() {
        assert_eq!(orientation_transform(1), WICBitmapTransformRotate0);
        assert_eq!(
            orientation_transform(2).0,
            WICBitmapTransformFlipHorizontal.0
        );
        assert_eq!(orientation_transform(6), WICBitmapTransformRotate90);
    }

    #[test]
    fn wic_decodes_generated_bmp_to_premultiplied_bgra() {
        let root = unique_test_dir("pixstride-wic-bmp");
        let path = root.join("测试.bmp");
        let mut bmp = vec![0u8; 62];
        bmp[0..2].copy_from_slice(b"BM");
        bmp[2..6].copy_from_slice(&62u32.to_le_bytes());
        bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&2i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&1i32.to_le_bytes());
        bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
        bmp[28..30].copy_from_slice(&24u16.to_le_bytes());
        bmp[34..38].copy_from_slice(&8u32.to_le_bytes());
        bmp[54..62].copy_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
        std::fs::write(&path, bmp).unwrap();

        let thread_path = path.clone();
        let decoded = std::thread::spawn(move || {
            let _com = ComMta::initialize()?;
            decode_wic(&thread_path, ImageBudget::default())
        })
        .join()
        .unwrap()
        .unwrap()
        .0;
        assert_eq!((decoded.width, decoded.height, decoded.stride), (2, 1, 8));
        assert_eq!(decoded.pixels, [0, 0, 255, 255, 0, 255, 0, 255]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn wic_round_trips_generated_jpeg_and_png_samples() {
        use windows::{
            Win32::{
                Foundation::GENERIC_WRITE,
                Graphics::Imaging::{
                    GUID_ContainerFormatJpeg, GUID_ContainerFormatPng,
                    GUID_WICPixelFormat32bppBGRA, WICBitmapEncoderNoCache,
                },
                System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
            },
            core::PCWSTR,
        };

        let root = unique_test_dir("pixstride-wic-formats");
        let thread_root = root.clone();
        std::thread::spawn(move || -> Result<()> {
            let _com = ComMta::initialize()?;
            // SAFETY: 测试线程已初始化 MTA，Factory 与编码器/流对象均只在该线程创建和释放。
            let factory: IWICImagingFactory =
                unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }?;
            for (extension, container) in [
                ("jpg", &GUID_ContainerFormatJpeg),
                ("png", &GUID_ContainerFormatPng),
            ] {
                let path = thread_root.join(format!("sample.{extension}"));
                let wide = wide_nul(&path);
                // SAFETY: 路径以 NUL 结尾且在编码器写入期间保持有效；文件位于测试专用临时目录。
                let stream = unsafe { factory.CreateStream()? };
                unsafe { stream.InitializeFromFilename(PCWSTR(wide.as_ptr()), GENERIC_WRITE.0) }?;
                // SAFETY: container 是 WIC 提供的静态 GUID；Encoder 在当前 MTA 中使用。
                let encoder = unsafe { factory.CreateEncoder(container, std::ptr::null()) }?;
                // SAFETY: stream 在 encoder Commit 前保持存活，NoCache 不保留调用方像素缓冲区。
                unsafe { encoder.Initialize(&stream, WICBitmapEncoderNoCache) }?;
                let (mut frame, mut options) = (None, None);
                // SAFETY: 两个输出槽为有效栈地址，返回接口由当前线程持有。
                unsafe { encoder.CreateNewFrame(&mut frame, &mut options) }?;
                let frame = frame.context("WIC 编码器未返回 Frame")?;
                // SAFETY: None 使用编码器默认属性；Frame 与 Encoder 属于同一 apartment。
                unsafe { frame.Initialize(None) }?;
                // SAFETY: 尺寸为测试内固定的小型 2×1 图，Format GUID 可由 Frame 修改为实际编码格式。
                unsafe { frame.SetSize(2, 1) }?;
                let mut format = GUID_WICPixelFormat32bppBGRA;
                // SAFETY: format 是可写 GUID 槽，帧只在当前 MTA 线程使用。
                unsafe { frame.SetPixelFormat(&mut format) }?;
                let pixels = if extension == "png" {
                    [0, 0, 255, 128, 0, 255, 0, 255]
                } else {
                    [0, 0, 255, 255, 0, 255, 0, 255]
                };
                // SAFETY: 两像素 BGRA 行长为 8 字节，slice 长度和 stride 均匹配 2×1 图像。
                unsafe { frame.WritePixels(1, 8, &pixels) }?;
                // SAFETY: 编码器和 Frame 均已初始化，Commit 按顺序完成文件写入。
                unsafe {
                    frame.Commit()?;
                    encoder.Commit()?;
                }
            }
            for extension in ["jpg", "png"] {
                let decoded = decode_wic(
                    &thread_root.join(format!("sample.{extension}")),
                    ImageBudget::default(),
                )?
                .0;
                assert_eq!((decoded.width, decoded.height), (2, 1));
                assert_eq!(decoded.format, PixelFormat::Bgra8Premultiplied);
                assert_eq!(decoded.pixels.len(), 8);
                if extension == "png" {
                    assert_eq!(decoded.pixels[3], 128);
                    assert!(decoded.pixels[2] <= decoded.pixels[3]);
                }
            }
            Ok(())
        })
        .join()
        .unwrap()
        .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}
