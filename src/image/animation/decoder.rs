//! 只在动画 MTA 内持有 WIC；复用 Poster 的结构扫描并按需解码一个局部帧。

use super::{Metadata, Pixels};
use crate::image::{self, ImageBudget, ImageContent, SourceIdentity, gif, memory::CpuMemory};
use anyhow::{Result, ensure};
use std::{marker::PhantomData, path::Path, rc::Rc};
use windows::{
    Win32::{
        Foundation::{GENERIC_READ, WINCODEC_ERR_PROPERTYNOTFOUND},
        Graphics::Imaging::*,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    },
    core::PCWSTR,
};

pub(super) struct Decoder {
    decoder: IWICBitmapDecoder,
    factory: IWICImagingFactory,
    pub(super) metadata: Metadata,
    // 句柄最后释放，保证 WIC 仍使用文件时禁止外部新建写入/删除句柄。
    _checked: gif::CheckedGif,
    _thread: PhantomData<Rc<()>>,
    #[cfg(test)]
    pub(super) copy_probe: Option<(
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::sync::mpsc::SyncSender<()>,
    )>,
}
pub(super) struct Patch {
    pub(super) rect: [u32; 4],
    pub(super) disposal: u8,
    pub(super) delay: u16,
    pub(super) pixels: Pixels,
}
fn identity(path: &Path) -> Result<SourceIdentity> {
    let mut source = SourceIdentity::read(path)?;
    ensure!(
        source.content == ImageContent::GifPosterV1,
        "动画 Session 只接受 .gif 入口"
    );
    source.content = ImageContent::GifAnimationV1;
    Ok(source)
}
fn optional(reader: &IWICMetadataQueryReader, name: &str) -> Result<u32> {
    match gif::integer(reader, name) {
        Ok(n) => Ok(n),
        Err(error)
            if error
                .downcast_ref::<windows::core::Error>()
                .is_some_and(|e| e.code() == WINCODEC_ERR_PROPERTYNOTFOUND) =>
        {
            Ok(0)
        }
        Err(error) => Err(error),
    }
}
impl Decoder {
    pub(super) fn open(path: &Path, pool: &CpuMemory, current: &impl Fn() -> bool) -> Result<Self> {
        image::check_decode_current(current)?;
        let source = identity(path)?;
        let checked = gif::check_animation_file(&source.path, ImageBudget::default(), current)?;
        let info = &checked.info;
        let [width, height] = info.size;
        let (_, _, canvas) = image::checked_layout(width, height, ImageBudget::default())?;
        let worst_pixels = canvas
            .checked_mul(3)
            .and_then(|n| n.checked_add(info.max_patch))
            .and_then(|n| n.checked_add(info.max_restore))
            .ok_or_else(|| anyhow::anyhow!("动画工作集溢出"))?;
        ensure!(
            worst_pixels <= pool.animation_limit(),
            "动画最坏像素工作集 {worst_pixels} 字节超过子额度；可继续使用静态 Poster"
        );
        let metadata = Metadata {
            source: source.clone(),
            width,
            height,
            frames: info.frames,
            raw_loop: info.loops,
            logical_background: info.background,
            worst_pixels,
        };
        image::check_decode_current(current)?;
        // SAFETY: 调用者在当前线程持有 ComMta，所有 COM 接口保存在 !Send 的 Decoder 内。
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }?;
        let wide = image::wide_nul(&source.path);
        // SAFETY: 有效的 NUL UTF16 路径，checked 的只读句柄在 Decoder 全生命周期保留。
        let decoder = unsafe {
            factory.CreateDecoderFromFilename(
                PCWSTR(wide.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
        }?;
        // SAFETY: decoder 仅由本 MTA 调用，帧数用于比较而非分配帧列表。
        ensure!(
            unsafe { decoder.GetContainerFormat() }? == GUID_ContainerFormatGif
                && unsafe { decoder.GetFrameCount() }? == metadata.frames,
            "WIC GIF 容器或帧数与预检不符"
        );
        // SAFETY: 返回的 metadata reader 在当前线程使用并及时释放。
        let reader = unsafe { decoder.GetMetadataQueryReader() }?;
        ensure!(
            [
                gif::integer(&reader, "/logscrdesc/Width")?,
                gif::integer(&reader, "/logscrdesc/Height")?
            ] == [width, height],
            "WIC Logical Screen 不符"
        );
        ensure!(
            identity(&source.path)? == source,
            "GIF 在 Session 初始化期间发生修订"
        );
        image::check_decode_current(current)?;
        Ok(Self {
            decoder,
            factory,
            metadata,
            _checked: checked,
            _thread: PhantomData,
            #[cfg(test)]
            copy_probe: None,
        })
    }
    pub(super) fn frame(
        &mut self,
        index: u32,
        pool: &CpuMemory,
        current: &impl Fn() -> bool,
    ) -> Result<Patch> {
        image::check_decode_current(current)?;
        ensure!(index < self.metadata.frames, "动画帧索引越界");
        // SAFETY: index 已与帧数核对，decoder 在当前 MTA 持有；不跨线程传递 frame。
        let frame = unsafe { self.decoder.GetFrame(index) }?;
        // SAFETY: 局部 reader 不超出有效 frame 生命周期。
        let reader = unsafe { frame.GetMetadataQueryReader() }?;
        let rect = [
            gif::integer(&reader, "/imgdesc/Left")?,
            gif::integer(&reader, "/imgdesc/Top")?,
            gif::integer(&reader, "/imgdesc/Width")?,
            gif::integer(&reader, "/imgdesc/Height")?,
        ];
        let [x, y, w, h] = rect;
        ensure!(
            w > 0
                && h > 0
                && x.checked_add(w).is_some_and(|n| n <= self.metadata.width)
                && y.checked_add(h).is_some_and(|n| n <= self.metadata.height),
            "动画 Frame Rect 越界"
        );
        let disposal = u8::try_from(optional(&reader, "/grctlext/Disposal")?)?;
        let delay = u16::try_from(optional(&reader, "/grctlext/Delay")?)?;
        ensure!(disposal <= 3, "动画 Disposal 非法");
        let (mut actual_w, mut actual_h) = (0, 0);
        // SAFETY: 两个 u32 输出槽有效；必须确认 WIC 返回局部帧尺寸。
        unsafe { frame.GetSize(&mut actual_w, &mut actual_h) }?;
        ensure!([actual_w, actual_h] == [w, h], "WIC 局部帧尺寸不符");
        // SAFETY: converter 和 frame 只在本 MTA 内使用，输出契约是 PBGRA8。
        let converter = unsafe { self.factory.CreateFormatConverter() }?;
        // SAFETY: frame 存活且已检查局部尺寸，无外部 palette 或裸像素指针。
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
        let length = usize::try_from(u64::from(w) * u64::from(h) * 4)?;
        let mut pixels = Pixels::new(pool, length)?;
        image::check_decode_current(current)?;
        // SAFETY: slice 长度为 w*h*4，stride 为 w*4，WIC 只能写入该局部帧缓冲。
        #[cfg(test)]
        if let Some((inside, started)) = &self.copy_probe {
            inside.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = started.try_send(());
        }
        // SAFETY: 与上述像素长度/stride 检查相同；同步调用返回之前不释放 pixels。
        let copied = unsafe { converter.CopyPixels(std::ptr::null(), w * 4, &mut pixels.data) };
        #[cfg(test)]
        if let Some((inside, _)) = &self.copy_probe {
            inside.store(false, std::sync::atomic::Ordering::SeqCst);
        }
        copied?;
        image::check_decode_current(current)?;
        Ok(Patch {
            rect,
            disposal,
            delay,
            pixels,
        })
    }
}
