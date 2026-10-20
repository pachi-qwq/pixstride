//! 逐行读取非隔行静态 PNG，输出借用的 BGRA8 预乘像素；调用方不得将它当作完整图片缓冲区。

use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    time::{Duration, Instant},
};

/// 解码前已经验证的 PNG 几何信息；不携带整个文件或 COM 接口。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PngSize {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// 解码器内部限制和调用方行缓冲限制分开计费；不是整个进程的硬内存上限。
#[derive(Clone, Copy, Debug)]
pub(crate) struct StreamBudget {
    pub(crate) decoder_bytes: usize,
    pub(crate) row_bytes: usize,
}

impl Default for StreamBudget {
    fn default() -> Self {
        Self {
            decoder_bytes: 64 * 1024 * 1024,
            row_bytes: 4 * 1024 * 1024,
        }
    }
}

/// 单次行扫描的应用侧诊断，不表示屏幕呈现时间。
#[derive(Debug)]
pub(crate) struct StreamReport {
    pub(crate) size: PngSize,
    pub(crate) rows: u32,
    pub(crate) owned_row_bytes: usize,
    pub(crate) elapsed: Duration,
    pub(crate) decode_rows: Duration,
    pub(crate) convert_rows: Duration,
    pub(crate) visitor: Duration,
}

/// 在分配前验证宽度和 64 位总像素计算；限制的是行，不拒绝超大总像素。
pub(crate) fn row_layout(size: PngSize, budget: StreamBudget) -> Result<usize> {
    ensure!(size.width > 0 && size.height > 0, "PNG 尺寸不能为零");
    let row = u64::from(size.width)
        .checked_mul(4)
        .context("PNG 行跨度溢出")?;
    let _total = row
        .checked_mul(u64::from(size.height))
        .context("PNG 总像素字节数溢出")?;
    ensure!(row <= budget.row_bytes as u64, "PNG 行缓冲超过预算");
    usize::try_from(row).context("PNG 行缓冲超过地址范围")
}

/// 只读固定 33 字节头信息，不解码 IDAT。调用前仍须检查云文件驻留属性。
pub(crate) fn inspect(path: &Path) -> Result<PngSize> {
    let mut file = File::open(path).context("打开 PNG 头失败")?;
    let mut h = [0u8; 33];
    file.read_exact(&mut h)?;
    ensure!(
        &h[..8] == b"\x89PNG\r\n\x1a\n"
            && &h[12..16] == b"IHDR"
            && u32::from_be_bytes(h[8..12].try_into()?) == 13,
        "PNG 签名或 IHDR 非法"
    );
    ensure!(
        crc32fast::hash(&h[12..29]) == u32::from_be_bytes(h[29..33].try_into()?),
        "PNG IHDR CRC 错误"
    );
    ensure!(
        h[24] <= 8 && h[28] == 0,
        "超大 PNG 路径暂不支持 16-bit 或 Adam7 隔行"
    );
    let size = PngSize {
        width: u32::from_be_bytes(h[16..20].try_into()?),
        height: u32::from_be_bytes(h[20..24].try_into()?),
    };
    row_layout(size, StreamBudget::default())?;
    Ok(size)
}

/// 为可选元数据设较保守的压缩前上限，避免忽略文本/ICC 仍无法约束其他附加块。
/// 只跳转检查块头，文件偏移使用 u64；这不是对 IDAT 做随机区域解码。
fn check_chunks(file: &mut File, valid: &impl Fn() -> bool) -> Result<()> {
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(8))?;
    let mut ancillary = 0u64;
    for _ in 0..1_000_000 {
        ensure!(valid(), "PNG 扫描已取消");
        let mut h = [0u8; 8];
        file.read_exact(&mut h)?;
        let n = u64::from(u32::from_be_bytes(h[..4].try_into()?));
        ensure!(n <= 0x7fff_ffff, "PNG 块长度非法");
        if h[4] & 32 != 0 {
            ancillary = ancillary.checked_add(n).context("元数据长度溢出")?;
            ensure!(
                n <= 1024 * 1024 && ancillary <= 8 * 1024 * 1024,
                "PNG 附加元数据超过安全预算"
            );
        }
        ensure!(&h[4..8] != b"acTL", "超大图片路径暂不支持 APNG");
        let next = file
            .stream_position()?
            .checked_add(n + 4)
            .context("PNG 块偏移溢出")?;
        ensure!(next <= length, "PNG 块已截断");
        file.seek(SeekFrom::Start(next))?;
        if &h[4..8] == b"IEND" {
            ensure!(n == 0 && next == length, "PNG IEND 或尾部数据非法");
            file.seek(SeekFrom::Start(0))?;
            return Ok(());
        }
    }
    bail!("PNG 块数超过上限")
}

/// 依次借出一行 PBGRA8。行切片只在回调期间有效；取消在行和阶段之间检查，不能中断 OS 文件调用。
pub(crate) fn scan(
    path: &Path,
    budget: StreamBudget,
    valid: impl Fn() -> bool,
    mut visit: impl FnMut(u32, &[u8], PngSize) -> Result<()>,
) -> Result<StreamReport> {
    let started = Instant::now();
    ensure!(valid(), "PNG 扫描已取消");
    let size = inspect(path)?;
    let rgba_len = row_layout(size, budget)?;
    let mut file = File::open(path)?;
    check_chunks(&mut file, &valid)?;
    let mut options = png::DecodeOptions::default();
    options.set_ignore_checksums(false);
    options.set_skip_ancillary_crc_failures(false);
    options.set_ignore_text_chunk(true);
    options.set_ignore_iccp_chunk(true);
    let mut decoder =
        png::Decoder::new_with_options(BufReader::with_capacity(64 * 1024, file), options);
    decoder.set_limits(png::Limits {
        bytes: budget.decoder_bytes,
    });
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info()?;
    ensure!(
        !reader.info().interlaced && reader.info().animation_control.is_none(),
        "不支持隔行/动画 PNG"
    );
    ensure!(
        reader.info().width == size.width && reader.info().height == size.height,
        "PNG 在打开过程中改变"
    );
    let (color, depth) = reader.output_color_type();
    ensure!(depth == png::BitDepth::Eight, "PNG 输出不是 8-bit");
    let raw_len = reader
        .output_line_size(size.width)
        .context("PNG 输出行长度溢出")?;
    ensure!(raw_len <= budget.row_bytes, "PNG 输出行超过预算");
    let mut raw = allocated(raw_len)?;
    let mut bgra = allocated(rgba_len)?;
    let mut rows = 0;
    let mut decode_rows = Duration::ZERO;
    let mut convert_rows = Duration::ZERO;
    let mut visitor = Duration::ZERO;
    for y in 0..size.height {
        ensure!(valid(), "PNG 扫描已取消");
        let at = Instant::now();
        ensure!(reader.read_row(&mut raw)?.is_some(), "PNG 缺少扫描行");
        decode_rows += at.elapsed();
        let at = Instant::now();
        convert_row(&raw, &mut bgra, color)?;
        convert_rows += at.elapsed();
        ensure!(valid(), "PNG 扫描已取消");
        let at = Instant::now();
        visit(y, &bgra, size)?;
        visitor += at.elapsed();
        rows += 1;
    }
    ensure!(reader.next_row()?.is_none(), "PNG 扫描行数量不符");
    reader.finish()?;
    ensure!(valid(), "PNG 扫描已取消");
    Ok(StreamReport {
        size,
        rows,
        owned_row_bytes: raw.capacity() + bgra.capacity(),
        elapsed: started.elapsed(),
        decode_rows,
        convert_rows,
        visitor,
    })
}

/// 使用可返回错误的分配，调用方已验证相应资源预算。
pub(crate) fn allocated(len: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .context("像素/缓存内存分配失败")?;
    bytes.resize(len, 0);
    Ok(bytes)
}

fn convert_row(raw: &[u8], output: &mut [u8], color: png::ColorType) -> Result<()> {
    let channels = match color {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        _ => bail!("PNG 格式展开失败"),
    };
    ensure!(
        raw.len() / channels == output.len() / 4 && raw.len() % channels == 0,
        "PNG 行布局不符"
    );
    for (source, pixel) in raw.chunks_exact(channels).zip(output.chunks_exact_mut(4)) {
        let (r, g, b, a) = match channels {
            4 => (source[0], source[1], source[2], source[3]),
            3 => (source[0], source[1], source[2], 255),
            2 => (source[0], source[0], source[0], source[1]),
            _ => (source[0], source[0], source[0], 255),
        };
        // 和预乘 BGRA 契约一致；在当前编码值域按四舍五入计算，不做 ICC/HDR 转换。
        for (v, c) in pixel[..3].iter_mut().zip([b, g, r]) {
            *v = ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
        }
        pixel[3] = a;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    pub(crate) fn root(name: &str) -> std::path::PathBuf {
        let base = std::env::var_os("PIXSTRIDE_TEST_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let p = base.join(format!(
            "stage-c-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    pub(crate) fn generate(path: &Path, width: u32, height: u32) -> Result<()> {
        let mut e = png::Encoder::new(std::io::BufWriter::new(File::create(path)?), width, height);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let mut writer = e.write_header()?;
        {
            let mut stream = writer.stream_writer()?;
            let mut row = allocated(width as usize * 4)?;
            for y in 0..height {
                for x in 0..width {
                    let p = &mut row[x as usize * 4..x as usize * 4 + 4];
                    p.copy_from_slice(&[
                        (x % 256) as u8,
                        (y % 256) as u8,
                        ((x + y) % 256) as u8,
                        if (x + y) % 3 == 0 { 128 } else { 255 },
                    ]);
                }
                stream.write_all(&row)?;
            }
            stream.finish()?;
        }
        writer.finish()?;
        Ok(())
    }

    /// 显式生成并保留自己的 GUI 样本；只写入唯一测试子目录，供人工验收后清理。
    #[test]
    #[ignore = "人工 GUI 样本生成器，保留仓库外合成文件"]
    fn release_prepare_gui_samples() -> Result<()> {
        let root = root("gui-samples");
        generate(&root.join("ordinary.png"), 97, 73)?;
        generate(&root.join("wide.png"), 20000, 257)?;
        generate(&root.join("build-cancel.png"), 20000, 2049)?;
        std::fs::write(root.join("broken.png"), b"not a png")?;
        println!("Synthetic GUI samples retained at {}", root.display());
        Ok(())
    }

    #[test]
    fn rows_match_wic_and_can_cancel_without_full_bitmap() -> Result<()> {
        let root = root("rows");
        let path = root.join("own.png");
        generate(&path, 37, 19)?;
        let source = crate::image::SourceIdentity::read(&path)?;
        let _com = crate::image::ComMta::initialize()?;
        let wic = crate::image::decode_wic(
            &path,
            &source,
            crate::image::ImageBudget::default(),
            |_, _| {},
            || true,
        )?;
        let report = scan(
            &path,
            StreamBudget::default(),
            || true,
            |y, row, _| {
                let expected = &wic.pixels[y as usize * 37 * 4..(y as usize + 1) * 37 * 4];
                ensure!(
                    row.iter().zip(expected).all(|(a, b)| a.abs_diff(*b) <= 1),
                    "WIC 像素差异超过预乘舍入误差"
                );
                Ok(())
            },
        )?;
        assert_eq!(report.rows, 19);
        assert!(report.owned_row_bytes < wic.pixels.len());
        assert_eq!(report.size.width, 37);
        let active = std::cell::Cell::new(true);
        assert!(
            scan(
                &path,
                StreamBudget::default(),
                || active.get(),
                |y, _, _| {
                    if y == 2 {
                        active.set(false);
                    }
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(
            row_layout(
                PngSize {
                    width: u32::MAX,
                    height: u32::MAX
                },
                StreamBudget::default()
            )
            .is_err()
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn rejects_crc_truncation_and_metadata_bombs() -> Result<()> {
        let root = root("malformed");
        let path = root.join("own.png");
        generate(&path, 8, 8)?;
        let good = std::fs::read(&path)?;
        let at = good
            .windows(4)
            .position(|b| b == b"IDAT")
            .context("缺少 IDAT")?;
        let mut bad = good.clone();
        bad[at + 5] ^= 1;
        std::fs::write(&path, bad)?;
        assert!(scan(&path, StreamBudget::default(), || true, |_, _, _| Ok(())).is_err());
        std::fs::write(&path, &good[..good.len() - 6])?;
        assert!(scan(&path, StreamBudget::default(), || true, |_, _, _| Ok(())).is_err());
        let mut bomb = good[..33].to_vec();
        bomb.extend_from_slice(&(2 * 1024 * 1024u32).to_be_bytes());
        bomb.extend_from_slice(b"tEXt");
        bomb.resize(bomb.len() + 2 * 1024 * 1024 + 4, 0);
        bomb.extend_from_slice(&good[33..]);
        std::fs::write(&path, bomb)?;
        assert!(
            scan(&path, StreamBudget::default(), || true, |_, _, _| Ok(()))
                .unwrap_err()
                .to_string()
                .contains("元数据")
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn expands_rgb_gray_palette_and_rejects_unsupported_headers() -> Result<()> {
        let root = root("colors");
        for (name, color, raw, expected) in [
            (
                "rgb",
                png::ColorType::Rgb,
                vec![20, 40, 60],
                vec![60, 40, 20, 255],
            ),
            (
                "gray",
                png::ColorType::Grayscale,
                vec![80],
                vec![80, 80, 80, 255],
            ),
            (
                "gray-alpha",
                png::ColorType::GrayscaleAlpha,
                vec![80, 128],
                vec![40, 40, 40, 128],
            ),
            (
                "palette",
                png::ColorType::Indexed,
                vec![0],
                vec![30, 20, 10, 128],
            ),
        ] {
            let path = root.join(format!("{name}.png"));
            let mut e = png::Encoder::new(File::create(&path)?, 1, 1);
            e.set_color(color);
            e.set_depth(png::BitDepth::Eight);
            if color == png::ColorType::Indexed {
                e.set_palette(vec![20, 40, 60]);
                e.set_trns(vec![128]);
            }
            let mut w = e.write_header()?;
            w.write_image_data(&raw)?;
            w.finish()?;
            scan(
                &path,
                StreamBudget::default(),
                || true,
                |_, row, _| {
                    assert_eq!(row, expected);
                    Ok(())
                },
            )?;
        }
        let path = root.join("header.png");
        generate(&path, 8, 8)?;
        let original = std::fs::read(&path)?;
        for (byte, value) in [(24, 16), (28, 1)] {
            let mut b = original.clone();
            b[byte] = value;
            let crc = crc32fast::hash(&b[12..29]);
            b[29..33].copy_from_slice(&crc.to_be_bytes());
            std::fs::write(&path, b)?;
            assert!(inspect(&path).is_err());
        }
        let mut apng = original[..33].to_vec();
        apng.extend_from_slice(&8u32.to_be_bytes());
        let mut chunk = b"acTL".to_vec();
        chunk.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0]);
        apng.extend_from_slice(&chunk);
        apng.extend_from_slice(&crc32fast::hash(&chunk).to_be_bytes());
        apng.extend_from_slice(&original[33..]);
        std::fs::write(&path, apng)?;
        assert!(scan(&path, StreamBudget::default(), || true, |_, _, _| Ok(())).is_err());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    /// 在独立进程中扫描，外部读取进程 PeakWorkingSet64；结果不含 OS 文件缓存。
    #[test]
    #[ignore = "独立行内存诊断，由父诊断指定合成 PNG"]
    fn stream_memory_child() -> Result<()> {
        let path = std::env::var_os("PIXSTRIDE_STREAM_PROBE").context("缺少合成样本路径")?;
        let r = scan(
            Path::new(&path),
            StreamBudget::default(),
            || true,
            |_, _, _| Ok(()),
        )?;
        println!(
            "STREAM rows={} width={} owned_row_bytes={} elapsed_us={}",
            r.rows,
            r.size.width,
            r.owned_row_bytes,
            r.elapsed.as_micros()
        );
        Ok(())
    }

    #[test]
    #[ignore = "Release C1 内存诊断，生成自己的小型压缩 PNG，不读取私人图片"]
    fn release_streaming_memory_probe() -> Result<()> {
        let root = root("memory");
        let mut peaks = Vec::new();
        for height in [256, 8192] {
            let path = root.join(format!("own-{height}.png"));
            generate(&path, 8192, height)?;
            let script = format!(
                "$p = Start-Process -FilePath '{}' -ArgumentList 'stream_memory_child --ignored --nocapture' -PassThru -WindowStyle Hidden -RedirectStandardOutput '{}' -RedirectStandardError '{}'; $peak=0L; while (!$p.HasExited) {{ $p.Refresh(); $peak=[Math]::Max($peak,$p.PeakWorkingSet64); Start-Sleep -Milliseconds 5 }}; Write-Output ('PEAK_WORKING_SET_BYTES='+$peak); exit $p.ExitCode",
                std::env::current_exe()?
                    .to_string_lossy()
                    .replace('\'', "''"),
                root.join("child.txt").to_string_lossy().replace('\'', "''"),
                root.join("error.txt").to_string_lossy().replace('\'', "''")
            );
            let output = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", &script])
                .env("PIXSTRIDE_STREAM_PROBE", &path)
                .output()?;
            ensure!(
                output.status.success(),
                "内存子进程失败：{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            let peak: u64 = stdout
                .lines()
                .find_map(|line| line.trim().strip_prefix("PEAK_WORKING_SET_BYTES="))
                .context("缺少进程内存数据")?
                .parse()?;
            ensure!(
                peak > 0 && peak < 64 * 1024 * 1024,
                "行解码进程工作集超过诊断上限"
            );
            peaks.push(peak);
            println!(
                "height={height} compressed_bytes={} {} {}",
                std::fs::metadata(path)?.len(),
                String::from_utf8_lossy(&output.stdout),
                std::fs::read_to_string(root.join("child.txt"))?
            );
        }
        ensure!(
            peaks[0].abs_diff(peaks[1]) < 8 * 1024 * 1024,
            "增加高度导致进程工作集过度增长"
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
