//! 定义图片像素契约、WIC 解码和普通图片的资源预算。

#[expect(
    dead_code,
    reason = "D1-b1 后台协议由测试消费者验证；正式播放消费者在 D1-b2 接入"
)]
pub(crate) mod animation;
mod cache;
mod gif;
#[cfg(test)]
pub(crate) mod gif_tests;
pub(crate) mod memory;
pub(crate) mod navigation;
#[cfg(test)]
pub(crate) mod orientation_tests;
pub(crate) mod worker;

use std::{
    ffi::OsStr,
    fs,
    hash::Hash,
    os::windows::{ffi::OsStrExt, fs::MetadataExt},
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

/// 缓存内容的固定语义；GIF poster 不与原始局部帧或未来动画帧共用身份。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ImageContent {
    Static,
    GifPosterV1,
    GifAnimationV1,
}

/// 由路径、长度、修改时间和固定输出语义组成的内存缓存键。
///
/// 它能识别常见的替换和修改，但元数据相同的原位内容更改仍可能碰撞；后续若实测需要，
/// 可增加内容摘要。键用于缓存，不宣称是不可伪造的文件身份。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SourceIdentity {
    pub(crate) path: PathBuf,
    pub(crate) length: u64,
    modified_nanos: Option<u128>,
    pub(crate) content: ImageContent,
}

impl SourceIdentity {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        // PNG 可能被路由到大图路径；先查属性，避免 canonicalize 的句柄访问先触发云端取回。
        // 复用同一次 metadata 结果，不给有效 PNG 的 GPU 快速路径增加第二次元数据查询。
        let png_metadata = if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
        {
            let metadata = fs::metadata(path)
                .with_context(|| format!("无法读取 PNG 属性：{}", path.display()))?;
            crate::large::cache::check_local_attributes(metadata.file_attributes())?;
            Some(metadata)
        } else {
            None
        };
        let canonical = fs::canonicalize(path)
            .with_context(|| format!("无法解析图片路径：{}", path.display()))?;
        let metadata = match png_metadata {
            Some(metadata) => metadata,
            None => fs::metadata(&canonical)
                .with_context(|| format!("无法读取图片信息：{}", canonical.display()))?,
        };
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
            content: if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("gif"))
            {
                ImageContent::GifPosterV1
            } else {
                ImageContent::Static
            },
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
    // 正式解码始终持有票据；None 只供 cfg(test) 手工构造的极小像素 Fixture。
    pub(crate) _charge: Option<memory::Charge>,
}

/// 离开一个阶段时立即记录耗时；`?` 提前返回也保留失败路径的样本。
struct StageTimer<'a> {
    started: Instant,
    record: &'a dyn Fn(crate::diagnostics::Metric, Duration),
    metric: crate::diagnostics::Metric,
}

impl Drop for StageTimer<'_> {
    fn drop(&mut self) {
        (self.record)(self.metric, self.started.elapsed());
    }
}

/// 解码阶段之间发现选择已过期或正在退出；它不是损坏图片或资源不足错误。
#[derive(Debug)]
pub(crate) struct DecodeCancelled;

/// 可由源修订与固定预算重复确定的拒绝；临时读取或解码错误不使用此类型。
#[derive(Clone, Debug)]
pub(crate) enum BudgetRejection {
    Dimension { width: u32, height: u32, limit: u32 },
    DecodedBytes { bytes: u64, limit: u64 },
}

impl std::fmt::Display for BudgetRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dimension {
                width,
                height,
                limit,
            } => write!(
                f,
                "图片尺寸 {width}×{height} 超过单张纹理边长上限 {limit}；该格式不在普通单纹理路径内"
            ),
            Self::DecodedBytes { bytes, limit } => write!(
                f,
                "图片解码需要 {bytes} 字节，超过当前单图 CPU 预算 {limit} 字节"
            ),
        }
    }
}

impl std::error::Error for BudgetRejection {}

impl std::fmt::Display for DecodeCancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("图片任务已过期或正在退出")
    }
}

impl std::error::Error for DecodeCancelled {}

/// 只在 WIC 调用之间合作式退出，不尝试终止正在执行的 COM 调用。
pub(crate) fn check_decode_current(is_current: &impl Fn() -> bool) -> Result<()> {
    if is_current() {
        Ok(())
    } else {
        Err(DecodeCancelled.into())
    }
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
        return Err(BudgetRejection::Dimension {
            width,
            height,
            limit: budget.max_dimension,
        }
        .into());
    }
    let stride = u64::from(width)
        .checked_mul(4)
        .context("图片行跨度计算溢出")?;
    let bytes = stride
        .checked_mul(u64::from(height))
        .context("图片像素缓冲区大小计算溢出")?;
    if bytes > budget.max_decoded_bytes {
        return Err(BudgetRejection::DecodedBytes {
            bytes,
            limit: budget.max_decoded_bytes,
        }
        .into());
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
                "jpg" | "jpeg" | "png" | "bmp" | "gif"
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

/// 使用当前线程的 WIC MTA 解码静态图或经有界预检的 GIF poster。
///
/// `identity` 是调用者在后台读取的源修订；完成后再次校验，变化则返回错误。
/// `record` 同步接收阶段耗时，包括提前失败的阶段；不得阻塞或写入大量日志。
/// `is_current` 在同步 WIC 调用之间检查选择是否仍有效；不能抢占正在阻塞的调用。
#[cfg(test)]
pub(crate) fn decode_wic(
    path: &Path,
    identity: &SourceIdentity,
    budget: ImageBudget,
    record: impl Fn(crate::diagnostics::Metric, Duration),
    is_current: impl Fn() -> bool,
) -> Result<DecodedImage> {
    let pool = memory::CpuMemory::new(budget.cpu_cache_bytes);
    decode_wic_reserved(path, identity, budget, record, is_current, &mut |len| {
        memory::allocate(len, pool.reserve(len as u64, false)?)
    })
}

/// 正式 Worker 从同一 CPU 池分配像素；回调只在分配点运行，可先淘汰自己的 LRU 条目。
pub(crate) fn decode_wic_reserved(
    path: &Path,
    identity: &SourceIdentity,
    budget: ImageBudget,
    record: impl Fn(crate::diagnostics::Metric, Duration),
    is_current: impl Fn() -> bool,
    allocate: &mut impl FnMut(usize) -> Result<(Vec<u8>, memory::Charge)>,
) -> Result<DecodedImage> {
    use crate::diagnostics::Metric;
    let timer = |metric| StageTimer {
        started: Instant::now(),
        record: &record,
        metric,
    };
    let _total_timer = timer(Metric::ImageDecodeTotal);
    check_decode_current(&is_current)?;
    if !is_supported_path(path) {
        bail!("不支持的图片扩展名：{}", path.display());
    }
    let identity = identity.clone();
    let gif = if identity.content == ImageContent::GifPosterV1 {
        let started = Instant::now();
        let result = gif::check_file(&identity.path, budget, &is_current);
        record(Metric::GifStructure, started.elapsed());
        Some(result?)
    } else {
        None
    };
    let wide_path = wide_nul(&identity.path);
    let open_timer = timer(Metric::WicOpen);
    let factory_timer = timer(Metric::WicFactoryCreation);
    // SAFETY: 当前线程已初始化 MTA；CoCreateInstance 返回由本地变量拥有的进程内接口，所有 WIC 对象都在同一线程释放。
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .context("创建 WIC Imaging Factory 失败")?;
    drop(factory_timer);
    check_decode_current(&is_current)?;
    let decoder_timer = timer(Metric::WicDecoderOpen);
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
    drop(decoder_timer);
    check_decode_current(&is_current)?;
    // SAFETY: 当前线程拥有 decoder，返回 GUID 不借用 COM 内存。
    let container = unsafe { decoder.GetContainerFormat() }?;
    if let Some(gif) = gif {
        anyhow::ensure!(
            container == windows::Win32::Graphics::Imaging::GUID_ContainerFormatGif,
            "GIF 扩展名与容器不符"
        );
        drop(open_timer);
        return gif::decode_poster(
            (&factory, &decoder),
            &identity,
            &gif,
            budget,
            &record,
            &is_current,
            allocate,
        );
    }
    anyhow::ensure!(
        container != windows::Win32::Graphics::Imaging::GUID_ContainerFormatGif,
        "GIF 内容需要使用 .gif 入口，已拒绝未预检的局部帧"
    );
    let frame_timer = timer(Metric::WicFrameMetadata);
    let get_frame_timer = timer(Metric::WicGetFrame);
    // SAFETY: 当前支持格式按单帧静态图处理；索引 0 从 Decoder 的有效帧范围读取。
    let frame = unsafe { decoder.GetFrame(0) }.context("读取图片首帧失败")?;
    drop(get_frame_timer);
    check_decode_current(&is_current)?;
    // 先检查原始尺寸；旋转不改变总像素字节，也不会改变两个边长均须合规的结论。
    let source_budget_timer = timer(Metric::ImageSourceBudget);
    let (mut source_width, mut source_height) = (0, 0);
    // SAFETY: 两个输出槽是有效 u32，frame 始终由当前 MTA 线程持有。
    unsafe { frame.GetSize(&mut source_width, &mut source_height) }
        .context("读取图片原始尺寸失败")?;
    checked_layout(source_width, source_height, budget)?;
    drop(source_budget_timer);
    let exif_timer = timer(Metric::WicExif);
    let orientation = exif_orientation(&frame);
    drop(exif_timer);
    let transform = orientation_transform(orientation);
    drop(frame_timer);
    drop(open_timer);
    check_decode_current(&is_current)?;

    let layout_timer = timer(Metric::ImageLayout);

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
    drop(layout_timer);
    check_decode_current(&is_current)?;

    let conversion_timer = timer(Metric::PixelFormatConversion);
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
    drop(conversion_timer);
    check_decode_current(&is_current)?;

    let allocation_timer = timer(Metric::PixelAllocation);
    let (mut pixels, charge) = allocate(buffer_len)?;
    drop(allocation_timer);
    check_decode_current(&is_current)?;
    let copy_timer = timer(Metric::WicCopyPixels);
    // SAFETY: stride 与宽度匹配 BGRA8，每行有 stride 字节，已分配长度等于 stride × height；WIC 只写入此切片。
    unsafe { converter.CopyPixels(std::ptr::null(), stride, &mut pixels) }
        .context("WIC 解码图片像素失败")?;
    drop(copy_timer);
    check_decode_current(&is_current)?;

    let verification_timer = timer(Metric::SourceIdentityVerify);
    let after = SourceIdentity::read(&identity.path)?;
    drop(verification_timer);
    check_decode_current(&is_current)?;
    if identity != after {
        bail!("图片在解码期间发生变化，已丢弃本次结果");
    }
    Ok(DecodedImage {
        source: identity,
        width,
        height,
        format: PixelFormat::Bgra8Premultiplied,
        stride,
        pixels,
        _charge: Some(charge),
    })
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
            // 像素实验确认此组合先翻转后旋转，EXIF 5 对应主对角线转置。
            WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0,
        ),
        6 => WICBitmapTransformRotate90,
        7 => WICBitmapTransformOptions(
            WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0,
        ),
        8 => WICBitmapTransformRotate270,
        _ => WICBitmapTransformRotate0,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CLSID_WICImagingFactory, ComMta, IWICImagingFactory, ImageBudget, PixelFormat,
        SourceIdentity, checked_layout, decode_wic, is_supported_path, orientation_transform,
        wide_nul,
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

    /// 对自行生成的 JPEG 比较原尺寸与解码器原生缩小，不读取用户图片。
    #[test]
    #[ignore = "显式运行的 Release JPEG 解码诊断"]
    fn release_jpeg_native_scale_probe() -> Result<()> {
        use std::time::Instant;
        use windows::Win32::{
            Foundation::GENERIC_WRITE,
            Graphics::Imaging::{
                GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR,
                GUID_WICPixelFormat32bppBGRA, IWICBitmapSourceTransform, WICBitmapEncoderNoCache,
            },
            System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        };
        use windows::core::{Interface, PCWSTR};
        let root = unique_test_dir("pixstride-jpeg-native");
        let path = root.join("generated.jpg");
        let thread_path = path.clone();
        std::thread::spawn(move || -> Result<()> {
            let _com = ComMta::initialize()?;
            // SAFETY: 此测试线程已初始化 MTA；所有 WIC 对象在该线程创建和释放。
            let factory: IWICImagingFactory =
                unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }?;
            let wide = wide_nul(&thread_path);
            {
                // SAFETY: 路径以 NUL 结尾且始终存活；输出位于测试独占目录，非用户原图。
                let stream = unsafe { factory.CreateStream()? };
                // SAFETY: UTF-16 缓冲区有效；此测试独占新文件的写入。
                unsafe { stream.InitializeFromFilename(PCWSTR(wide.as_ptr()), GENERIC_WRITE.0) }?;
                // SAFETY: JPEG GUID 是静态常量；编码接口只在当前 MTA 使用。
                let encoder =
                    unsafe { factory.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null()) }?;
                // SAFETY: 流活到 Commit，编码器尚未初始化。
                unsafe { encoder.Initialize(&stream, WICBitmapEncoderNoCache) }?;
                let (mut frame, mut options) = (None, None);
                // SAFETY: 输出槽是可写的栈变量，编码器在当前线程存活。
                unsafe { encoder.CreateNewFrame(&mut frame, &mut options) }?;
                let frame = frame.context("JPEG 编码器未返回帧")?;
                // SAFETY: 帧只在当前 MTA 使用，默认属性有效。
                unsafe { frame.Initialize(None) }?;
                // SAFETY: 固定的 4096×3072 测试图，尺寸不超过当前预算。
                unsafe { frame.SetSize(4096, 3072) }?;
                let mut format = GUID_WICPixelFormat24bppBGR;
                // SAFETY: format 是可写 GUID，成功后检查实际格式以保证行布局。
                unsafe { frame.SetPixelFormat(&mut format) }?;
                assert_eq!(format, GUID_WICPixelFormat24bppBGR);
                let mut row = vec![0u8; 4096 * 3];
                let mut seed = 1u32;
                for _ in 0..3072 {
                    for value in &mut row {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        *value = seed as u8;
                    }
                    // SAFETY: 单行 BGR24 切片恰好有 4096×3 字节，stride 与帧宽一致。
                    unsafe { frame.WritePixels(1, 4096 * 3, &row) }?;
                }
                // SAFETY: 顺序提交帧和编码器，流在作用域退出前保持存活。
                unsafe {
                    frame.Commit()?;
                    encoder.Commit()?;
                }
            }
            let identity = SourceIdentity::read(&thread_path)?;
            for round in 0..8 {
                let full_started = Instant::now();
                let full = decode_wic(
                    &thread_path,
                    &identity,
                    ImageBudget::default(),
                    |_, _| {},
                    || true,
                )?;
                let full_time = full_started.elapsed();
                drop(full);
                let reduced_started = Instant::now();
                // SAFETY: 路径有效且只读；Decoder 与 Frame 都归当前 MTA 线程所有。
                let decoder = unsafe {
                    factory.CreateDecoderFromFilename(
                        PCWSTR(wide.as_ptr()),
                        None,
                        super::GENERIC_READ,
                        super::WICDecodeMetadataCacheOnDemand,
                    )
                }?;
                // SAFETY: 测试文件有一帧；接口保留到像素复制完成。
                let frame = unsafe { decoder.GetFrame(0) }?;
                let transform: IWICBitmapSourceTransform = frame.cast()?;
                let (mut width, mut height) = (1024, 768);
                let mut format = GUID_WICPixelFormat32bppBGRA;
                // SAFETY: 两个尺寸与 GUID 输出槽有效，返回值直接用于 CopyPixels。
                unsafe {
                    transform.GetClosestSize(&mut width, &mut height)?;
                    transform.GetClosestPixelFormat(&mut format)?;
                }
                assert_eq!(format, GUID_WICPixelFormat32bppBGRA);
                // SAFETY: 变换选项来自静态 WIC 常量，无外部指针。
                assert!(
                    unsafe { transform.DoesSupportTransform(WICBitmapTransformRotate0) }?.as_bool()
                );
                let (stride, allocation, _) =
                    checked_layout(width, height, ImageBudget::default())?;
                let mut pixels = vec![0u8; allocation];
                // SAFETY: 尺寸和格式已协商，切片长度为 stride×height，输出为 BGRA8；只读取整个测试图。
                unsafe {
                    transform.CopyPixels(
                        std::ptr::null(),
                        width,
                        height,
                        &format,
                        WICBitmapTransformRotate0,
                        stride,
                        &mut pixels,
                    )
                }?;
                println!(
                    "jpeg-scale round={round} full_us={} reduced_us={} reduced={}x{} bytes={}",
                    full_time.as_micros(),
                    reduced_started.elapsed().as_micros(),
                    width,
                    height,
                    pixels.len()
                );
                assert!(pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
            }
            Ok(())
        })
        .join()
        .unwrap()?;
        Ok(())
    }

    fn write_test_bmp(path: &Path) {
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
        std::fs::write(path, bmp).unwrap();
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
    fn accepts_static_and_gif_poster_extensions_case_insensitively() {
        for name in [
            "photo.JPG",
            "image.jpeg",
            "image.PnG",
            "scan.bmp",
            "poster.GiF",
        ] {
            assert!(is_supported_path(Path::new(name)));
        }
        for name in ["camera.raw", "vector.svg", "text.png.exe"] {
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
        write_test_bmp(&path);

        let thread_path = path.clone();
        let decoded = std::thread::spawn(move || {
            let _com = ComMta::initialize()?;
            let identity = SourceIdentity::read(&thread_path)?;
            decode_wic(
                &thread_path,
                &identity,
                ImageBudget::default(),
                |_, _| {},
                || true,
            )
        })
        .join()
        .unwrap()
        .unwrap();
        assert_eq!((decoded.width, decoded.height, decoded.stride), (2, 1, 8));
        assert_eq!(decoded.pixels, [0, 0, 255, 255, 0, 255, 0, 255]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn wic_returns_an_error_for_truncated_image_data() {
        let root = unique_test_dir("pixstride-wic-truncated");
        let path = root.join("broken.jpg");
        std::fs::write(&path, [0xff, 0xd8, 0xff, 0x00]).unwrap();

        let thread_path = path.clone();
        let result = std::thread::spawn(move || {
            let _com = ComMta::initialize()?;
            let identity = SourceIdentity::read(&thread_path)?;
            decode_wic(
                &thread_path,
                &identity,
                ImageBudget::default(),
                |_, _| {},
                || true,
            )
        })
        .join()
        .unwrap();
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn source_budget_rejection_precedes_exif_and_pixel_allocation() {
        use crate::diagnostics::Metric;
        let root = unique_test_dir("pixstride-budget-preflight");
        let path = root.join("small.bmp");
        write_test_bmp(&path);
        std::thread::spawn(move || {
            let _com = ComMta::initialize().unwrap();
            let identity = SourceIdentity::read(&path).unwrap();
            let stages = std::cell::RefCell::new(Vec::new());
            let budget = ImageBudget {
                max_decoded_bytes: 4,
                ..ImageBudget::default()
            };
            let error = decode_wic(
                &path,
                &identity,
                budget,
                |metric, _| stages.borrow_mut().push(metric),
                || true,
            )
            .unwrap_err();
            assert!(error.is::<super::BudgetRejection>());
            let stages = stages.into_inner();
            assert!(
                stages
                    .iter()
                    .any(|metric| matches!(metric, Metric::ImageSourceBudget))
            );
            assert!(!stages.iter().any(|metric| matches!(
                metric,
                Metric::WicExif | Metric::PixelAllocation | Metric::WicCopyPixels
            )));
        })
        .join()
        .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_wic_open_still_records_total_and_stage_timing() {
        use crate::diagnostics::Metric;
        let root = unique_test_dir("pixstride-wic-failed-timing");
        let path = root.join("broken.jpg");
        std::fs::write(&path, [0xff, 0xd8]).unwrap();
        let metrics = std::thread::spawn(move || {
            let _com = ComMta::initialize().unwrap();
            let identity = SourceIdentity::read(&path).unwrap();
            let metrics = std::cell::RefCell::new(Vec::new());
            let result = decode_wic(
                &path,
                &identity,
                ImageBudget::default(),
                |metric, _| metrics.borrow_mut().push(metric),
                || true,
            );
            assert!(result.is_err());
            metrics.into_inner()
        })
        .join()
        .unwrap();
        assert!(
            metrics
                .iter()
                .any(|metric| matches!(metric, Metric::WicOpen))
        );
        assert!(
            metrics
                .iter()
                .any(|metric| matches!(metric, Metric::ImageDecodeTotal))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn source_revision_changes_after_replacement_and_deleted_file_is_rejected() {
        let root = unique_test_dir("pixstride-source-revision");
        let path = root.join("sample.bmp");
        std::fs::write(&path, [1, 2]).unwrap();
        let old = SourceIdentity::read(&path).unwrap();
        std::fs::write(&path, [1, 2, 3]).unwrap();
        assert_ne!(SourceIdentity::read(&path).unwrap(), old);
        std::fs::remove_file(&path).unwrap();
        assert!(SourceIdentity::read(&path).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stale_request_is_cancelled_before_opening_wic() {
        let identity = SourceIdentity::read(Path::new("Cargo.toml")).unwrap();
        let stages = std::cell::RefCell::new(Vec::new());
        let result = decode_wic(
            Path::new("unused.jpg"),
            &identity,
            ImageBudget::default(),
            |metric, _| stages.borrow_mut().push(metric),
            || false,
        );
        assert!(result.unwrap_err().is::<super::DecodeCancelled>());
        assert!(
            !stages
                .borrow()
                .iter()
                .any(|metric| matches!(metric, crate::diagnostics::Metric::WicOpen))
        );
    }

    #[test]
    fn stale_request_after_wic_open_skips_allocation_and_copy_pixels() {
        use crate::diagnostics::Metric;
        let root = unique_test_dir("pixstride-wic-cancel-open");
        let path = root.join("test.bmp");
        write_test_bmp(&path);
        std::thread::spawn(move || {
            let _com = ComMta::initialize().unwrap();
            let identity = SourceIdentity::read(&path).unwrap();
            let active = std::cell::Cell::new(true);
            let stages = std::cell::RefCell::new(Vec::new());
            let result = decode_wic(
                &path,
                &identity,
                ImageBudget::default(),
                |metric, _| {
                    if matches!(metric, Metric::WicOpen) {
                        active.set(false);
                    }
                    stages.borrow_mut().push(metric);
                },
                || active.get(),
            );
            assert!(result.unwrap_err().is::<super::DecodeCancelled>());
            assert!(
                !stages.borrow().iter().any(|metric| matches!(
                    metric,
                    Metric::PixelAllocation | Metric::WicCopyPixels
                ))
            );
        })
        .join()
        .unwrap();
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
                let path = thread_root.join(format!("sample.{extension}"));
                let identity = SourceIdentity::read(&path)?;
                let decoded =
                    decode_wic(&path, &identity, ImageBudget::default(), |_, _| {}, || true)?;
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
