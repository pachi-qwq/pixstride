//! GIF poster 的有界结构预检与透明首帧合成；不维护播放 Session 或动画时钟。

use super::*;
use anyhow::ensure;
use std::{
    fs::{File, OpenOptions},
    io::{BufReader, Read},
    os::windows::fs::OpenOptionsExt,
};
use windows::Win32::{
    Graphics::Imaging::{
        IWICBitmapDecoder, IWICBitmapFrameDecode, IWICMetadataQueryReader, WICRect,
    },
    System::Com::StructuredStorage::PropVariantToUInt32,
};

const MAX_SCAN: u64 = 64 * 1024 * 1024;
const MAX_METADATA: u64 = 4 * 1024 * 1024;
const MAX_FRAMES: u32 = 10_000;
const MAX_BLOCKS: u32 = 1_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Frame {
    rect: [u32; 4],
    transparent: Option<u8>,
}

/// 文件句柄在 WIC 返回前保持存活，拒绝其他写入/删除句柄；结构结果只保留首帧固定元数据。
pub(super) struct CheckedGif {
    _file: File,
    info: Info,
}

#[derive(Debug)]
struct Info {
    size: [u32; 2],
    first: Frame,
    background: Option<[u8; 4]>,
    frames: u32,
}

/// 只读完整扫描块边界。64 MiB 文件/4 MiB metadata/10,000 帧是本阶段的准入策略。
pub(super) fn check_file(
    path: &Path,
    budget: ImageBudget,
    current: &impl Fn() -> bool,
) -> Result<CheckedGif> {
    // FILE_SHARE_READ=1：预检和 WIC 都可读；此 guard 存活期间不允许更改已预检的文件。
    let file = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(path)
        .context("无法只读锁定 GIF")?;
    ensure!(
        file.metadata()?.len() <= MAX_SCAN,
        "GIF 结构扫描上限为 64 MiB，当前文件过大"
    );
    let info = inspect(BufReader::with_capacity(8192, &file), budget, current)?;
    Ok(CheckedGif { _file: file, info })
}

struct Scan<'a, R, F> {
    reader: R,
    current: &'a F,
    bytes: u64,
    metadata: u64,
    blocks: u32,
}

impl<R: Read, F: Fn() -> bool> Scan<'_, R, F> {
    fn exact(&mut self, bytes: &mut [u8], metadata: bool) -> Result<()> {
        check_decode_current(self.current)?;
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .context("GIF 长度溢出")?;
        ensure!(self.bytes <= MAX_SCAN, "GIF 结构扫描超过 64 MiB");
        if metadata {
            self.metadata += bytes.len() as u64;
            ensure!(self.metadata <= MAX_METADATA, "GIF metadata 超过 4 MiB");
        }
        self.reader
            .read_exact(bytes)
            .context("GIF 结构截断或读取失败")?;
        Ok(())
    }
    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut data = [0; N];
        self.exact(&mut data, true)?;
        Ok(data)
    }
    fn block(&mut self) -> Result<()> {
        self.blocks += 1;
        ensure!(self.blocks <= MAX_BLOCKS, "GIF 块数量超过预检上限");
        Ok(())
    }
    fn subblocks(&mut self, metadata: bool) -> Result<u64> {
        let mut total = 0;
        let mut data = [0; 255];
        loop {
            self.block()?;
            // 像素 sub-block 的长度字节也不计入 metadata；图像数量另受限。
            let mut length = [0];
            self.exact(&mut length, metadata)?;
            if length[0] == 0 {
                return Ok(total);
            }
            self.exact(&mut data[..usize::from(length[0])], metadata)?;
            total += u64::from(length[0]);
        }
    }
    fn palette(&mut self, packed: u8) -> Result<([[u8; 3]; 256], usize)> {
        let count = 1usize << ((packed & 7) + 1);
        let mut table = [[0; 3]; 256];
        for color in &mut table[..count] {
            self.exact(color, true)?;
        }
        Ok((table, count))
    }
}

fn inspect(reader: impl Read, budget: ImageBudget, current: &impl Fn() -> bool) -> Result<Info> {
    let mut s = Scan {
        reader,
        current,
        bytes: 0,
        metadata: 0,
        blocks: 0,
    };
    ensure!(
        [*b"GIF87a", *b"GIF89a"].contains(&s.fixed::<6>()?),
        "不是 GIF87a/GIF89a 文件"
    );
    let screen = s.fixed::<7>()?;
    let word = |b: &[u8]| u32::from(u16::from_le_bytes([b[0], b[1]]));
    let size = [word(&screen[..2]), word(&screen[2..4])];
    checked_layout(size[0], size[1], budget)?;
    let (colors, global_count) = if screen[4] & 0x80 != 0 {
        s.palette(screen[4])?
    } else {
        ([[0; 3]; 256], 0)
    };
    let background = if global_count > 0 {
        let index = usize::from(screen[5]);
        ensure!(index < global_count, "GIF 全局背景索引越界");
        let [r, g, b] = colors[index];
        Some([b, g, r, 255])
    } else {
        None
    };
    let mut first = None;
    let mut transparent = None;
    let mut frames = 0;
    loop {
        s.block()?;
        match s.fixed::<1>()?[0] {
            0x3b => {
                ensure!(frames > 0, "GIF 没有图像帧");
                check_decode_current(current)?;
                let mut extra = [0];
                ensure!(
                    s.reader.read(&mut extra)? == 0,
                    "GIF 结束标记后存在额外数据"
                );
                return Ok(Info {
                    size,
                    first: first.context("GIF 缺少首帧")?,
                    background,
                    frames,
                });
            }
            0x21 => match s.fixed::<1>()?[0] {
                0xf9 => {
                    ensure!(s.fixed::<1>()?[0] == 4, "GIF GCE 长度必须为 4");
                    let gce = s.fixed::<4>()?;
                    ensure!(
                        gce[0] & 0xe0 == 0 && (gce[0] >> 2) & 7 <= 3,
                        "GIF GCE 含非法 Disposal 或保留位"
                    );
                    ensure!(s.fixed::<1>()?[0] == 0, "GIF GCE 缺少结束标记");
                    transparent = (gce[0] & 1 != 0).then_some(gce[3]);
                }
                0xff => {
                    ensure!(
                        s.fixed::<1>()?[0] == 11,
                        "GIF Application Header 长度必须为 11"
                    );
                    s.fixed::<11>()?;
                    s.subblocks(true)?; // Loop 等应用数据只验证边界，不建立播放语义。
                }
                0x01 => bail!("GIF poster 暂不支持 Plain Text Extension"),
                _ => {
                    s.subblocks(true)?;
                }
            },
            0x2c => {
                frames += 1;
                ensure!(frames <= MAX_FRAMES, "GIF 帧数超过 10000");
                let desc = s.fixed::<9>()?;
                ensure!(desc[8] & 0x18 == 0, "GIF Image Descriptor 保留位非零");
                let rect = [
                    word(&desc[..2]),
                    word(&desc[2..4]),
                    word(&desc[4..6]),
                    word(&desc[6..8]),
                ];
                ensure!(
                    rect[2] > 0
                        && rect[3] > 0
                        && rect[0].checked_add(rect[2]).is_some_and(|v| v <= size[0])
                        && rect[1].checked_add(rect[3]).is_some_and(|v| v <= size[1]),
                    "GIF Frame Rect 越界"
                );
                let count = if desc[8] & 0x80 != 0 {
                    s.palette(desc[8])?.1
                } else {
                    global_count
                };
                ensure!(
                    count > 0 && transparent.is_none_or(|i| usize::from(i) < count),
                    "GIF 缺少调色板或透明索引越界"
                );
                ensure!(
                    (2..=8).contains(&s.fixed::<1>()?[0]),
                    "GIF LZW 最小码宽非法"
                );
                ensure!(s.subblocks(false)? > 0, "GIF 缺少压缩像素数据");
                if first.is_none() {
                    first = Some(Frame { rect, transparent });
                }
                transparent = None;
            }
            _ => bail!("GIF 含未知块标记"),
        }
    }
}

/// WIC 值在成功和失败路径均显式清理；接口始终留在调用线程。
fn integer(reader: &IWICMetadataQueryReader, name: &str) -> Result<u32> {
    let wide: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
    let mut value = PROPVARIANT::default();
    // SAFETY: 字符串为有效 NUL UTF16，value 是可写输出；reader 属于当前 MTA。
    let result = unsafe { reader.GetMetadataByName(PCWSTR(wide.as_ptr()), &mut value) };
    let number = result.and_then(|_| {
        // SAFETY: 查询成功填充了 value；转换失败返回错误，不手动读取未检查的 union。
        unsafe { PropVariantToUInt32(&value) }
    });
    // SAFETY: 本函数独占 value，成功与失败路径都清理其中可能存在的 COM 分配。
    let _ = unsafe { PropVariantClear(&mut value) };
    Ok(number?)
}

/// 在调用线程的 WIC MTA 内合成透明 poster；只分配 Logical Screen 画布，拒绝过期结果。
pub(super) fn decode_poster(
    factory: &IWICImagingFactory,
    decoder: &IWICBitmapDecoder,
    identity: &SourceIdentity,
    checked: &CheckedGif,
    budget: ImageBudget,
    record: &impl Fn(crate::diagnostics::Metric, Duration),
    current: &impl Fn() -> bool,
) -> Result<DecodedImage> {
    use crate::diagnostics::Metric;
    let timer = |metric| StageTimer {
        started: Instant::now(),
        record,
        metric,
    };
    let composite_timer = timer(Metric::GifPoster);
    let metadata_timer = timer(Metric::WicFrameMetadata);
    // SAFETY: 预检已确认存在首帧，COM 接口只由当前 MTA 线程持有。
    let global = unsafe { decoder.GetMetadataQueryReader() }?;
    ensure!(
        [
            integer(&global, "/logscrdesc/Width")?,
            integer(&global, "/logscrdesc/Height")?
        ] == checked.info.size,
        "WIC Logical Screen 与预检不符"
    );
    // 背景定义经过结构校验，poster 策略仍固定为透明画布，不隐式铺上 palette 背景。
    if checked.info.background.is_some() {
        integer(&global, "/logscrdesc/BackgroundColorIndex")?;
    }
    let frame_timer = timer(Metric::WicGetFrame);
    // SAFETY: 只解码首个 image frame，不申请其他帧或建立动画缓存。
    let frame: IWICBitmapFrameDecode = unsafe { decoder.GetFrame(0) }?;
    drop(frame_timer);
    check_decode_current(current)?;
    // SAFETY: frame 在当前线程存活，reader 在像素输出前释放。
    let metadata = unsafe { frame.GetMetadataQueryReader() }?;
    let mut actual = [0; 4];
    for (i, name) in [
        "/imgdesc/Left",
        "/imgdesc/Top",
        "/imgdesc/Width",
        "/imgdesc/Height",
    ]
    .iter()
    .enumerate()
    {
        actual[i] = integer(&metadata, name)?;
    }
    ensure!(
        actual == checked.info.first.rect,
        "WIC Frame Rect 与预检不符"
    );
    if let Some(index) = checked.info.first.transparent {
        ensure!(
            integer(&metadata, "/grctlext/TransparentColorIndex")? == u32::from(index),
            "WIC 透明索引与预检不符"
        );
    }
    let (mut w, mut h) = (0, 0);
    // SAFETY: 两个 u32 输出槽有效，frame 生命周期覆盖调用。
    unsafe { frame.GetSize(&mut w, &mut h) }?;
    ensure!(
        [w, h] == [actual[2], actual[3]],
        "WIC 未返回 GIF 局部帧尺寸"
    );
    ensure!(checked.info.frames > 0, "GIF 帧数非法");
    drop(metadata_timer);
    let [width, height] = checked.info.size;
    let (stride, len, _) = checked_layout(width, height, budget)?;
    let converter_timer = timer(Metric::PixelFormatConversion);
    // SAFETY: factory/frame 在同一 MTA 存活，格式是 Renderer 既有的 PBGRA8。
    let converter = unsafe { factory.CreateFormatConverter() }?;
    // SAFETY: converter 保留有效 frame 引用，真彩色输出不使用调用者 palette。
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
    drop(converter_timer);
    check_decode_current(current)?;
    let allocation_timer = timer(Metric::PixelAllocation);
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(len)
        .context("GIF poster 画布分配失败")?;
    pixels.resize(len, 0);
    drop(allocation_timer);
    let copy_timer = timer(Metric::WicCopyPixels);
    if actual == [0, 0, width, height] {
        // SAFETY: 完整帧与画布同尺寸，stride×height 等于切片长度。
        unsafe { converter.CopyPixels(std::ptr::null(), stride, &mut pixels) }?;
    } else {
        for y in 0..h {
            check_decode_current(current)?;
            let offset = u64::from(actual[1] + y) * u64::from(stride) + u64::from(actual[0]) * 4;
            let offset = usize::try_from(offset)?;
            let row_bytes = usize::try_from(u64::from(w) * 4)?;
            let roi = WICRect {
                X: 0,
                Y: i32::try_from(y)?,
                Width: i32::try_from(w)?,
                Height: 1,
            };
            // SAFETY: rect 已预检在 Logical Screen 内；ROI 是局部帧的一行，紧密 stride 与目标切片长度相等。
            // 直接写透明画布避免第二个整帧 Vec；首帧透明像素在 PBGRA 中为零，无需背景副本或 Disposal。
            unsafe { converter.CopyPixels(&roi, w * 4, &mut pixels[offset..offset + row_bytes]) }?;
        }
    }
    drop(copy_timer);
    check_decode_current(current)?;
    let verification_timer = timer(Metric::SourceIdentityVerify);
    ensure!(
        identity == &SourceIdentity::read(&identity.path)?,
        "GIF 在解码期间发生变化，已丢弃结果"
    );
    drop(verification_timer);
    check_decode_current(current)?;
    drop(composite_timer);
    Ok(DecodedImage {
        source: identity.clone(),
        width,
        height,
        format: PixelFormat::Bgra8Premultiplied,
        stride,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scan_rejects_every_truncation_and_checks_cancel() -> Result<()> {
        let bytes = crate::image::gif_tests::sample(false);
        assert!(inspect(&bytes[..], ImageBudget::default(), &|| true).is_ok());
        for n in 0..bytes.len() {
            assert!(
                inspect(&bytes[..n], ImageBudget::default(), &|| true).is_err(),
                "length={n}"
            );
        }
        assert!(
            inspect(&bytes[..], ImageBudget::default(), &|| false)
                .unwrap_err()
                .is::<DecodeCancelled>()
        );
        Ok(())
    }

    #[test]
    fn metadata_and_scan_limits_and_mid_scan_cancel() -> Result<()> {
        use std::cell::Cell;
        let mut bytes = crate::image::gif_tests::sample(false);
        let checks = Cell::new(0);
        assert!(
            inspect(&bytes[..], ImageBudget::default(), &|| {
                checks.set(checks.get() + 1);
                checks.get() < 15
            })
            .unwrap_err()
            .is::<DecodeCancelled>()
        );
        // 合法结构的 Comment 大于 metadata 额度；测试只分配约 4 MiB，不进入 WIC。
        let mut comment = vec![0x21, 0xfe];
        for _ in 0..(MAX_METADATA / 255 + 1) {
            comment.push(255);
            comment.extend_from_slice(&[b'x'; 255]);
        }
        comment.push(0);
        bytes.splice(25..25, comment);
        assert!(
            inspect(&bytes[..], ImageBudget::default(), &|| true)
                .unwrap_err()
                .to_string()
                .contains("metadata")
        );
        let dir = crate::image::gif_tests::Directory::new()?;
        let path = dir.0.join("scan-limit.gif");
        File::create(&path)?.set_len(MAX_SCAN + 1)?;
        assert!(check_file(&path, ImageBudget::default(), &|| true).is_err());
        let small = crate::image::gif_tests::generated([1, 1], [0, 0, 1, 1], false, false, false);
        let mut many = small[..25].to_vec();
        for _ in 0..=MAX_FRAMES {
            many.extend_from_slice(&small[25..small.len() - 1]);
        }
        many.push(0x3b);
        assert!(
            inspect(&many[..], ImageBudget::default(), &|| true)
                .unwrap_err()
                .to_string()
                .contains("10000")
        );
        let mut trailing = crate::image::gif_tests::sample(false);
        trailing.push(0);
        assert!(inspect(&trailing[..], ImageBudget::default(), &|| true).is_err());
        Ok(())
    }
}
