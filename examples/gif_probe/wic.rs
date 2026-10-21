//! 线程内拥有 WIC COM；只导出帧局部 PBGRA 像素，不把 COM 接口发送给消费者。

use anyhow::{Context, Result, ensure};
use std::{
    os::windows::ffi::OsStrExt,
    path::Path,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::GENERIC_READ,
        Graphics::Imaging::*,
        System::Com::{
            StructuredStorage::{PROPVARIANT, PropVariantClear, PropVariantToUInt32},
            *,
        },
    },
    core::PCWSTR,
};

/// 线程内的 MTA 初始化配对；Rc 标记阻止 guard 被移动到其他线程析构。
pub struct Apartment(std::marker::PhantomData<std::rc::Rc<()>>);
impl Apartment {
    /// 在当前线程初始化 COM；不允许与已有 STA 初始化混用。
    pub fn new() -> Result<Self> {
        // SAFETY: 实验的调用线程尚未初始化其他 apartment；成功后 guard 在同一线程配对释放。
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self(std::marker::PhantomData))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: 只有初始化成功才构造 guard，且不跨线程移动。
        unsafe { CoUninitialize() };
    }
}

struct Value(PROPVARIANT);
impl Drop for Value {
    fn drop(&mut self) {
        // SAFETY: 值由 WIC 填充，当前 guard 独占并在借用结束后清理。
        let _ = unsafe { PropVariantClear(&mut self.0) };
    }
}
fn query(r: &IWICMetadataQueryReader, name: &str) -> Result<Value> {
    let wide: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
    let mut value = Value(PROPVARIANT::default());
    // SAFETY: 字符串以 NUL 结尾，输出为已初始化 PROPVARIANT；WIC reader 属于当前线程。
    unsafe { r.GetMetadataByName(PCWSTR(wide.as_ptr()), &mut value.0) }?;
    Ok(value)
}
fn integer(r: &IWICMetadataQueryReader, name: &str) -> Result<u32> {
    let value = query(r, name)?;
    // SAFETY: value 在转换期间存活，转换失败通过 Result 返回，不解引用其未检查的 union。
    Ok(unsafe { PropVariantToUInt32(&value.0) }?)
}
fn blob(r: &IWICMetadataQueryReader, name: &str) -> Result<Vec<u8>> {
    let value = query(r, name)?;
    // SAFETY: 先检查 VT_VECTOR|VT_UI1，再读取对应 caub，WIC 分配的切片仅在 guard 存活期间借用。
    unsafe {
        let v = &value.0.Anonymous.Anonymous;
        ensure!(v.vt.0 == 0x1011, "GIF 扩展元数据类型不符");
        let bytes = v.Anonymous.caub;
        ensure!(bytes.cElems <= 65536, "GIF 扩展元数据超过预算");
        if bytes.cElems == 0 {
            return Ok(Vec::new());
        }
        ensure!(!bytes.pElems.is_null(), "GIF 扩展指针为空");
        Ok(std::slice::from_raw_parts(bytes.pElems, bytes.cElems as usize).to_vec())
    }
}

/// 只在创建它的 MTA Worker 内使用，向外发送拥有型像素而非 COM 接口。
pub struct Session {
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
    decoder: IWICBitmapDecoder,
    factory: IWICImagingFactory,
    pub size: [u32; 2],
    pub frames: u32,
    pub background: [u8; 4],
    pub loops: Option<u16>,
    pub open: Duration,
}
/// WIC 解出的局部帧；PBGRA 像素长度为 rect 的宽×高×4。
pub struct Patch {
    pub rect: [u32; 4],
    pub disposal: u32,
    pub delay: u32,
    pub pixels: Vec<u8>,
    pub decode: Duration,
}

impl Session {
    /// 打开只读 GIF 并验证帧数、全局尺寸和实验预算，不预解码全部帧。
    pub fn open(path: &Path) -> Result<Self> {
        let at = Instant::now();
        let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: Apartment guard 由调用者持有；以下接口始终在本线程释放，文件以只读方式打开。
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }?;
        // SAFETY: 宽路径在调用期间有效，结果 COM 由 Session 独占。
        let decoder = unsafe {
            factory.CreateDecoderFromFilename(
                PCWSTR(wide.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
        }?;
        // SAFETY: decoder 存活且仅由当前线程调用；帧数不用于无界分配。
        let frames = unsafe { decoder.GetFrameCount() }?;
        ensure!((1..=10000).contains(&frames), "GIF 帧数超限");
        // SAFETY: 当前 decoder 支持元数据查询，返回接口在本函数内使用。
        let r = unsafe { decoder.GetMetadataQueryReader() }?;
        let size = [
            integer(&r, "/logscrdesc/Width")?,
            integer(&r, "/logscrdesc/Height")?,
        ];
        let cost = u64::from(size[0])
            .checked_mul(u64::from(size[1]))
            .and_then(|n| n.checked_mul(4))
            .context("GIF 尺寸溢出")?;
        ensure!(
            size.iter().all(|n| *n > 0 && *n <= 16384)
                && cost * 5 <= super::flow::PIXEL_LIMIT as u64,
            "GIF 合成工作集超过子预算"
        );
        let mut background = [0; 4];
        // SAFETY: palette 输出接口仅在当前线程使用，颜色槽有固定上限。
        let palette = unsafe { factory.CreatePalette() }?;
        // SAFETY: decoder 与 palette 同一 apartment；不存在全局表时保留透明背景，并在报告记录。
        if unsafe { decoder.CopyPalette(&palette) }.is_ok() {
            let index = integer(&r, "/logscrdesc/BackgroundColorIndex")? as usize;
            let mut colors = [0u32; 256];
            let mut actual = 0;
            // SAFETY: 256 个有效颜色槽及计数输出，GetColors 不超出切片。
            unsafe { palette.GetColors(&mut colors, &mut actual) }?;
            ensure!(index < (actual as usize), "背景索引越界");
            background = colors[index].to_le_bytes();
        }
        let loops = match blob(&r, "/appext/Application") {
            Ok(application) if application == b"NETSCAPE2.0" || application == b"ANIMEXTS1.0" => {
                let data = blob(&r, "/appext/Data")?;
                ensure!(
                    data.len() >= 4 && data[0] == 3 && data[1] == 1,
                    "Loop 扩展格式错误"
                );
                Some(u16::from_le_bytes([data[2], data[3]]))
            }
            _ => None,
        };
        Ok(Self {
            _thread_bound: std::marker::PhantomData,
            decoder,
            factory,
            size,
            frames,
            background,
            loops,
            open: at.elapsed(),
        })
    }
    /// 按索引获取局部像素；同步 WIC 调用执行期间无法协作式取消。
    pub fn frame(&self, index: u32) -> Result<Patch> {
        ensure!(index < self.frames, "帧索引越界");
        let at = Instant::now();
        // SAFETY: index 已验证在 GetFrameCount 范围内，所有 COM 只在当前线程使用。
        let frame = unsafe { self.decoder.GetFrame(index) }?;
        // SAFETY: frame 仍由当前栈帧持有，reader 在返回像素前释放。
        let r = unsafe { frame.GetMetadataQueryReader() }?;
        let rect = [
            integer(&r, "/imgdesc/Left")?,
            integer(&r, "/imgdesc/Top")?,
            integer(&r, "/imgdesc/Width")?,
            integer(&r, "/imgdesc/Height")?,
        ];
        ensure!(
            rect[2] > 0
                && rect[3] > 0
                && rect[0]
                    .checked_add(rect[2])
                    .is_some_and(|x| x <= self.size[0])
                && rect[1]
                    .checked_add(rect[3])
                    .is_some_and(|y| y <= self.size[1]),
            "Frame Rect 越界"
        );
        let disposal = integer(&r, "/grctlext/Disposal").unwrap_or(0);
        ensure!(disposal <= 3, "未知 Disposal");
        let delay = integer(&r, "/grctlext/Delay").unwrap_or(0);
        let (mut w, mut h) = (0, 0);
        // SAFETY: 输出槽有效，尺寸与 GIF frame rect 进行独立核对。
        unsafe { frame.GetSize(&mut w, &mut h) }?;
        ensure!([w, h] == [rect[2], rect[3]], "WIC 并非局部帧尺寸");
        // SAFETY: factory/frame 在本线程存活，转换输出与合成器的 PBGRA8 契约相同。
        let converter = unsafe { self.factory.CreateFormatConverter() }?;
        // SAFETY: 源接口有效，输出 GUID 是静态常量，真彩色转换不借用调用方 palette。
        unsafe {
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
        }?;
        let n = usize::try_from(u64::from(w) * u64::from(h) * 4)?;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(n)?;
        pixels.resize(n, 0);
        // SAFETY: 分配和 stride 为已验证的局部矩形宽×高×4；CopyPixels 不访问切片之外。
        unsafe { converter.CopyPixels(std::ptr::null(), w * 4, &mut pixels) }?;
        Ok(Patch {
            rect,
            disposal,
            delay,
            pixels,
            decode: at.elapsed(),
        })
    }
}
