//! 顺序扫描一次 PNG，构建带边框的原始 PBGRA8 分块金字塔；只有完整校验并原子发布的缓存可复用。

use super::stream::{self, PngSize, StreamBudget};
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, UNIX_EPOCH},
};

const MAGIC: &[u8; 8] = b"PXSTILE1";
const VERSION: u32 = 1;
const MAX_TILES: usize = 262_144;
const MAX_INDEX: u64 = 16 * 1024 * 1024;

/// 磁盘和 Builder 有独立预算；超过 2 GiB 的覆盖值只能在负责人批准后配置。
#[derive(Clone, Copy, Debug)]
pub(crate) struct CacheBudget {
    pub(crate) disk_bytes: u64,
    pub(crate) builder_bytes: usize,
    pub(crate) tile_edge: u32,
}
impl Default for CacheBudget {
    fn default() -> Self {
        Self {
            disk_bytes: 2 * 1024 * 1024 * 1024,
            builder_bytes: 128 * 1024 * 1024,
            tile_edge: 1024,
        }
    }
}

/// 一层的尺寸与索引范围，层 0 为原始像素；相邻层按 ceil(size/2) 缩小。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Level {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) first: usize,
    pub(crate) columns: u32,
    pub(crate) rows: u32,
}

/// 固定索引条目；offset/length 均为 u64，像素包含四边各一个采样边框。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tile {
    pub(crate) level: u32,
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) offset: u64,
    pub(crate) length: u64,
    pub(crate) crc: u32,
}

/// 只含有界索引与几何信息，不含整层像素。
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub(crate) size: PngSize,
    pub(crate) edge: u32,
    pub(crate) levels: Vec<Level>,
    pub(crate) tiles: Vec<Tile>,
    pub(crate) bytes: u64,
}
impl Plan {
    /// 只生成有界索引，检查尺寸、Tile 数和 u64 偏移；不分配图片像素。
    pub(crate) fn new(size: PngSize, edge: u32) -> Result<Self> {
        ensure!([512, 1024, 2048].contains(&edge), "不支持的 Tile 尺寸");
        stream::row_layout(size, StreamBudget::default())?;
        let mut levels = Vec::new();
        let mut tiles = Vec::new();
        let mut bytes = 0u64;
        let (mut width, mut height) = (size.width, size.height);
        loop {
            let columns = width.div_ceil(edge);
            let rows = height.div_ceil(edge);
            let count = u64::from(columns) * u64::from(rows);
            ensure!(
                count + tiles.len() as u64 <= MAX_TILES as u64,
                "Tile 索引超过预算"
            );
            tiles
                .try_reserve(count as usize)
                .context("Tile 索引内存分配失败")?;
            let level = levels.len() as u32;
            levels.push(Level {
                width,
                height,
                first: tiles.len(),
                columns,
                rows,
            });
            for y in 0..rows {
                for x in 0..columns {
                    let w = (width - x * edge).min(edge);
                    let h = (height - y * edge).min(edge);
                    let length = u64::from(w + 2)
                        .checked_mul(u64::from(h + 2))
                        .and_then(|v| v.checked_mul(4))
                        .context("Tile 大小溢出")?;
                    tiles.push(Tile {
                        level,
                        x,
                        y,
                        width: w,
                        height: h,
                        offset: bytes,
                        length,
                        crc: 0,
                    });
                    bytes = bytes.checked_add(length).context("缓存大小溢出")?;
                }
            }
            if width == 1 && height == 1 {
                break;
            }
            width = width.div_ceil(2);
            height = height.div_ceil(2);
        }
        Ok(Self {
            size,
            edge,
            levels,
            tiles,
            bytes,
        })
    }
    pub(crate) fn tile_id(&self, level: usize, x: u32, y: u32) -> Option<usize> {
        let l = self.levels.get(level)?;
        (x < l.columns && y < l.rows).then_some(l.first + (y * l.columns + x) as usize)
    }
}

/// 比长度/时间戳多检查文件首、中、尾各 64 KiB；仍不是完整内容哈希。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    path: Vec<u8>,
    len: u64,
    modified: u128,
    samples: [u32; 3],
}
impl Stamp {
    fn read(path: &Path) -> Result<Self> {
        ensure_local(path)?;
        let p = fs::canonicalize(path)?;
        let m = fs::metadata(&p)?;
        let path = p
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect();
        let len = m.len();
        let modified = m.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
        let mut file = File::open(p)?;
        let mut samples = [0; 3];
        let mut buf = [0u8; 65536];
        for (i, offset) in [0, len.saturating_sub(65536) / 2, len.saturating_sub(65536)]
            .into_iter()
            .enumerate()
        {
            file.seek(SeekFrom::Start(offset))?;
            let count = usize::try_from((len - offset).min(65536))?;
            file.read_exact(&mut buf[..count])?;
            samples[i] = crc32fast::hash(&buf[..count]);
        }
        Ok(Self {
            path,
            len,
            modified,
            samples,
        })
    }
    fn key(&self) -> String {
        let mut h = crc32fast::Hasher::new();
        h.update(&self.path);
        h.update(&self.len.to_le_bytes());
        h.update(&self.modified.to_le_bytes());
        for s in self.samples {
            h.update(&s.to_le_bytes());
        }
        format!("cache-{:08x}-{:016x}", h.finalize(), self.len)
    }
}

/// 只查文件属性，拒绝可能触发云端回调取回的占位文件；不请求下载或改变驻留状态。
pub(crate) fn ensure_local(path: &Path) -> Result<()> {
    let attrs = fs::metadata(path)?.file_attributes();
    check_local_attributes(attrs)
}

/// 属性预检不打开像素流，供源身份检查在 canonicalize 之前共用。
pub(crate) fn check_local_attributes(attrs: u32) -> Result<()> {
    ensure!(
        attrs & (0x1000 | 0x40000 | 0x400000) == 0,
        "PNG 不在本地或需要云端取回；请先自行确认下载，不会自动读取"
    );
    Ok(())
}

/// 已发布缓存持有只读 File，拒绝其他实例删除该数据文件；Mutex 只在 Tile I/O 线程使用。
#[derive(Debug)]
pub(crate) struct DiskCache {
    pub(crate) plan: Plan,
    file: Mutex<File>,
    pub(crate) directory: PathBuf,
}

#[derive(Debug)]
pub(crate) struct TilePixels {
    pub(crate) id: usize,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bytes: Vec<u8>,
}

impl DiskCache {
    /// 在后台读取一块并验证 CRC；返回独立拥有的带边框像素，禁止在 Render/UI Thread 调用。
    pub(crate) fn read_tile(&self, id: usize) -> Result<TilePixels> {
        self.read_tile_profiled(id).map(|(pixels, _)| pixels)
    }
    /// 分离分配、文件锁/Seek/Read 和 CRC；文件阶段仍包含 OS 缓存与调度等待。
    pub(crate) fn read_tile_profiled(&self, id: usize) -> Result<(TilePixels, ReadProfile)> {
        let t = self.plan.tiles.get(id).context("Tile 索引越界")?;
        let at = Instant::now();
        let mut bytes = stream::allocated(usize::try_from(t.length)?)?;
        let allocation = at.elapsed();
        let at = Instant::now();
        #[cfg(test)]
        if let Some(delay) = std::env::var("PIXSTRIDE_TILE_TEST_READ_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            // 只供独立 Release 诊断模拟慢存储，不存在于正式可执行文件，也不冒充真实磁盘耗时。
            std::thread::sleep(Duration::from_millis(delay.min(100)));
        }
        let mut file = self
            .file
            .lock()
            .map_err(|_| anyhow::anyhow!("Tile 文件锁已损坏"))?;
        file.seek(SeekFrom::Start(t.offset))?;
        file.read_exact(&mut bytes)?;
        drop(file);
        let file_read = at.elapsed();
        let at = Instant::now();
        ensure!(
            crc32fast::hash(&bytes) == t.crc,
            "Tile CRC 校验失败，请清理该图片缓存后重建"
        );
        let crc = at.elapsed();
        Ok((
            TilePixels {
                id,
                width: t.width + 2,
                height: t.height + 2,
                bytes,
            },
            ReadProfile {
                allocation,
                file_read,
                crc,
            },
        ))
    }
}

/// 每块 CPU 读取阶段的耗时；不包含 Worker 排队、答复背压或 GPU 调用。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ReadProfile {
    pub(crate) allocation: Duration,
    pub(crate) file_read: Duration,
    pub(crate) crc: Duration,
}

/// 构建分项为嵌套计时；scan 包含写入，不能把所有字段直接相加。
#[derive(Debug, Default)]
pub(crate) struct BuildProfile {
    pub(crate) reserve: Duration,
    pub(crate) writes: WriteProfile,
    pub(crate) checksum: Duration,
    pub(crate) data_sync: Duration,
    pub(crate) index_sync: Duration,
    pub(crate) publish: Duration,
}

/// 逐行写入的调用次数、字节和 Seek/Write 累计耗时；不含最终 sync_all。
#[derive(Debug, Default)]
pub(crate) struct WriteProfile {
    pub(crate) calls: u64,
    pub(crate) bytes: u64,
    pub(crate) elapsed: Duration,
}

/// 借用单个输出流，统计每行 Seek/Write；不增加缓存或改变持久化语义。
struct RowWriter<'a, W> {
    output: &'a mut W,
    profile: WriteProfile,
}

/// 本次构建或复用的应用侧测量。cache_hit 不表示所有 Tile 已载入 RAM/VRAM。
#[derive(Debug)]
pub(crate) struct CacheReport {
    pub(crate) cache_hit: bool,
    pub(crate) elapsed: Duration,
    pub(crate) scan: Option<stream::StreamReport>,
    pub(crate) builder_owned_bytes: usize,
    pub(crate) phases: BuildProfile,
}

fn encode(stamp: &Stamp, plan: &Plan) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    b.extend_from_slice(MAGIC);
    for n in [
        VERSION,
        plan.edge,
        plan.size.width,
        plan.size.height,
        plan.tiles.len() as u32,
        stamp.path.len() as u32,
    ] {
        b.extend_from_slice(&n.to_le_bytes());
    }
    b.extend_from_slice(&stamp.len.to_le_bytes());
    b.extend_from_slice(&stamp.modified.to_le_bytes());
    for n in stamp.samples {
        b.extend_from_slice(&n.to_le_bytes());
    }
    b.extend_from_slice(&stamp.path);
    for t in &plan.tiles {
        for n in [t.level, t.x, t.y, t.width, t.height] {
            b.extend_from_slice(&n.to_le_bytes());
        }
        b.extend_from_slice(&t.offset.to_le_bytes());
        b.extend_from_slice(&t.length.to_le_bytes());
        b.extend_from_slice(&t.crc.to_le_bytes());
    }
    ensure!(b.len() as u64 <= MAX_INDEX, "缓存索引过大");
    let crc = crc32fast::hash(&b);
    b.extend_from_slice(&crc.to_le_bytes());
    Ok(b)
}

struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(self.0.len() >= n, "缓存索引截断");
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
}

fn open_cache(directory: &Path, expected: &Stamp) -> Result<DiskCache> {
    let meta = directory.join("index.bin");
    ensure!(fs::metadata(&meta)?.len() <= MAX_INDEX, "缓存索引超过上限");
    let b = fs::read(meta)?;
    ensure!(b.len() >= 4, "缓存索引截断");
    let end = b.len() - 4;
    ensure!(
        crc32fast::hash(&b[..end]) == u32::from_le_bytes(b[end..].try_into()?),
        "缓存索引 CRC 错误"
    );
    let mut c = Cursor(&b[..end]);
    ensure!(
        c.take(8)? == MAGIC && c.u32()? == VERSION,
        "缓存格式版本不兼容"
    );
    let edge = c.u32()?;
    let size = PngSize {
        width: c.u32()?,
        height: c.u32()?,
    };
    let count = c.u32()? as usize;
    let path_len = c.u32()? as usize;
    ensure!(path_len <= 65536, "缓存源路径过长");
    let len = c.u64()?;
    let modified = u128::from_le_bytes(c.take(16)?.try_into()?);
    let samples = [c.u32()?, c.u32()?, c.u32()?];
    let path = c.take(path_len)?.to_vec();
    ensure!(
        Stamp {
            path,
            len,
            modified,
            samples
        } == *expected,
        "缓存源修订已失效"
    );
    let mut plan = Plan::new(size, edge)?;
    ensure!(count == plan.tiles.len(), "缓存 Tile 数量不符");
    for t in &mut plan.tiles {
        ensure!(
            [c.u32()?, c.u32()?, c.u32()?, c.u32()?, c.u32()?]
                == [t.level, t.x, t.y, t.width, t.height]
                && c.u64()? == t.offset
                && c.u64()? == t.length,
            "缓存 Tile 几何或偏移非法"
        );
        t.crc = c.u32()?;
    }
    ensure!(c.0.is_empty(), "缓存索引存在尾部数据");
    let file = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(directory.join("pixels.bin"))?;
    ensure!(file.metadata()?.len() == plan.bytes, "缓存数据长度不符");
    Ok(DiskCache {
        plan,
        file: Mutex::new(file),
        directory: directory.to_owned(),
    })
}

/// 返回该缓存所在卷的可用字节；只查询空间，不修改文件或系统设置。
pub(crate) fn free_bytes(path: &Path) -> Result<u64> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0;
    // SAFETY: 路径以 NUL 结束并在同步调用期间存活；free 是有效的 u64 输出槽。
    unsafe {
        windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            windows::core::PCWSTR(wide.as_ptr()),
            Some(&mut free),
            None,
            None,
        )
    }?;
    Ok(free)
}

struct PartialBuild(PathBuf);
impl Drop for PartialBuild {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn remove_inactive(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.file_attributes() & 0x400 == 0,
        "拒绝清理重解析目录"
    );
    // 调用者持有整个缓存根的独占 lease，其他实例不能在检查后打开新缓存。
    // 活动读者的只读句柄拒绝写共享，只有没有读者时才能先独占检查，再释放并删除。
    let exclusive = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(path.join("pixels.bin"))?;
    drop(exclusive);
    fs::remove_dir_all(path)?;
    Ok(())
}

fn clean_for_build(root: &Path, needed: u64, cap: u64) -> Result<()> {
    let mut entries = Vec::new();
    let mut used = 0u64;
    for (scanned, entry) in fs::read_dir(root)?.enumerate() {
        ensure!(scanned < 4096, "专用缓存根目录条目超过清理上限");
        let entry = entry?;
        if !entry.file_type()?.is_dir() || entry.metadata()?.file_attributes() & 0x400 != 0 {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".build-")
            && fs::read(entry.path().join("owner")).ok().as_deref() == Some(MAGIC)
        {
            fs::remove_dir_all(entry.path())?;
            continue;
        }
        if name.starts_with("cache-") && entry.path().join("index.bin").is_file() {
            let mut header = [0u8; 8];
            if File::open(entry.path().join("index.bin"))?
                .read_exact(&mut header)
                .is_err()
                || &header != MAGIC
            {
                continue;
            }
            let n = fs::metadata(entry.path().join("pixels.bin"))?
                .len()
                .checked_add(fs::metadata(entry.path().join("index.bin"))?.len())
                .context("缓存用量溢出")?;
            used = used.checked_add(n).context("缓存用量溢出")?;
            entries.push((entry.metadata()?.modified()?, entry.path(), n));
        }
    }
    entries.sort_by_key(|e| e.0);
    for (_, path, n) in entries {
        if used.checked_add(needed).is_some_and(|v| v <= cap) {
            break;
        }
        // 活动缓存的像素文件拒绝删除；清理失败时保留它并继续尝试其他旧条目。
        if remove_inactive(&path).is_ok() {
            used -= n;
        }
    }
    ensure!(
        used.checked_add(needed).is_some_and(|v| v <= cap),
        "磁盘缓存容量不足，活动缓存无法淘汰"
    );
    Ok(())
}

/// 一次源扫描建立所有层，最后复核源修订、同步文件并原子改名；失败仅清理本次临时目录。
#[cfg(test)]
pub(crate) fn build_or_open(
    source: &Path,
    root: &Path,
    budget: CacheBudget,
    valid: impl Fn() -> bool,
) -> Result<(DiskCache, CacheReport)> {
    build_or_open_with_progress(source, root, budget, valid, |_| {})
}

/// 通知只描述已经到达的阶段；不改变缓存格式、文件发布顺序及取消点。
pub(crate) fn build_or_open_with_progress(
    source: &Path,
    root: &Path,
    budget: CacheBudget,
    valid: impl Fn() -> bool,
    progress: impl FnMut(super::progress::Stage),
) -> Result<(DiskCache, CacheReport)> {
    build_reporting(source, root, budget, valid, free_bytes, progress)
}

#[cfg(test)]
fn build_with_space(
    source: &Path,
    root: &Path,
    budget: CacheBudget,
    valid: impl Fn() -> bool,
    available: impl Fn(&Path) -> Result<u64>,
) -> Result<(DiskCache, CacheReport)> {
    build_reporting(source, root, budget, valid, available, |_| {})
}

fn build_reporting(
    source: &Path,
    root: &Path,
    budget: CacheBudget,
    valid: impl Fn() -> bool,
    available: impl Fn(&Path) -> Result<u64>,
    progress: impl FnMut(super::progress::Stage),
) -> Result<(DiskCache, CacheReport)> {
    use super::progress::{Reporter, Stage};
    let mut progress = Reporter::new(progress);
    let started = Instant::now();
    ensure!(valid(), "大图任务已取消");
    progress.report(Stage::Checking);
    let stamp = Stamp::read(source)?;
    let size = stream::inspect(source)?;
    let mut plan = Plan::new(size, budget.tile_edge)?;
    fs::create_dir_all(root)?;
    let root = fs::canonicalize(root)?;
    ensure!(
        !fs::canonicalize(source)?.starts_with(&root),
        "原图不能存放在专用缓存目录中"
    );
    let target = root.join(stamp.key());
    // 打开缓存也短暂取得 lease，防止与清理检查发生竞态；崩溃时 OS 自动释放句柄。
    let _lease = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(root.join("builder.lock"))
        .context("缓存 Builder 正忙，请稍后重试")?;
    if let Ok(cache) = open_cache(&target, &stamp) {
        ensure!(
            cache.plan.edge == budget.tile_edge && cache.plan.bytes <= budget.disk_bytes,
            "缓存配置超过当前预算"
        );
        return Ok((
            cache,
            CacheReport {
                cache_hit: true,
                elapsed: started.elapsed(),
                scan: None,
                builder_owned_bytes: 0,
                phases: BuildProfile::default(),
            },
        ));
    }
    let index_bytes = encode(&stamp, &plan)?.len() as u64;
    let needed = plan
        .bytes
        .checked_add(index_bytes)
        .context("缓存大小溢出")?;
    ensure!(
        needed <= budget.disk_bytes,
        "预计缓存需要 {needed} 字节，超过磁盘预算 {}；大于 2 GiB 的首次缓存须负责人批准",
        budget.disk_bytes
    );
    let owned_rows: usize = plan.levels.iter().map(|l| l.width as usize * 4 * 8).sum();
    let max_tile = plan
        .tiles
        .iter()
        .map(|t| t.length as usize)
        .max()
        .unwrap_or(0);
    let builder_owned = owned_rows
        .checked_add(max_tile)
        .and_then(|v| v.checked_add(index_bytes as usize))
        .and_then(|v| {
            v.checked_add(
                plan.tiles
                    .capacity()
                    .checked_mul(std::mem::size_of::<Tile>() * 2)?,
            )
        })
        .and_then(|v| {
            v.checked_add(
                plan.levels
                    .capacity()
                    .checked_mul(std::mem::size_of::<Level>() * 2)?,
            )
        })
        .context("Builder 内存溢出")?;
    ensure!(
        builder_owned
            .checked_add(StreamBudget::default().decoder_bytes)
            .is_some_and(|n| n <= budget.builder_bytes),
        "Builder 内存预算不足"
    );
    clean_for_build(&root, needed, budget.disk_bytes)?;
    ensure!(
        available(&root)?
            > needed
                .checked_add(1024 * 1024 * 1024)
                .context("空间计算溢出")?,
        "缓存卷可用空间不足，需另保留 1 GiB 余量"
    );
    if target.exists() {
        remove_inactive(&target).context("无效缓存正在使用，不能替换")?;
    }
    let temporary = root.join(format!(
        ".build-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_nanos()
    ));
    fs::create_dir(&temporary)?;
    let guard = PartialBuild(temporary.clone());
    fs::write(temporary.join("owner"), MAGIC)?;
    // 原图只读，在整个扫描期间拒绝写入/删除，避免构建中途被替换；作用域结束释放共享限制。
    let _source_guard = OpenOptions::new().read(true).share_mode(1).open(source)?;
    let mut data = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(temporary.join("pixels.bin"))?;
    let mut phases = BuildProfile::default();
    let at = Instant::now();
    progress.report(Stage::Reserving);
    data.set_len(plan.bytes)?;
    phases.reserve = at.elapsed();
    let mut previous: Vec<Option<Vec<u8>>> = vec![None; plan.levels.len()];
    let mut writer = RowWriter {
        output: &mut data,
        profile: WriteProfile::default(),
    };
    progress.report(Stage::Scanning {
        rows: 0,
        total: size.height,
    });
    let report = stream::scan(source, StreamBudget::default(), &valid, |y, row, _| {
        push_row(&plan, &mut writer, &mut previous, 0, y, row, &valid)?;
        progress.report(Stage::Scanning {
            rows: y + 1,
            total: size.height,
        });
        Ok(())
    })?;
    phases.writes = writer.profile;
    let at = Instant::now();
    let total = plan.tiles.len();
    progress.report(Stage::Checksumming { tiles: 0, total });
    for (i, t) in plan.tiles.iter_mut().enumerate() {
        ensure!(valid(), "大图任务已取消");
        let mut bytes = stream::allocated(t.length as usize)?;
        data.seek(SeekFrom::Start(t.offset))?;
        data.read_exact(&mut bytes)?;
        t.crc = crc32fast::hash(&bytes);
        progress.report(Stage::Checksumming {
            tiles: i + 1,
            total,
        });
    }
    phases.checksum = at.elapsed();
    ensure!(Stamp::read(source)? == stamp, "原图在缓存构建期间发生变化");
    ensure!(valid(), "大图任务已取消");
    let at = Instant::now();
    progress.report(Stage::Syncing);
    data.sync_all()?;
    phases.data_sync = at.elapsed();
    drop(data);
    let at = Instant::now();
    progress.report(Stage::Publishing);
    let mut index = File::create(temporary.join("index.bin"))?;
    index.write_all(&encode(&stamp, &plan)?)?;
    index.sync_all()?;
    drop(index);
    phases.index_sync = at.elapsed();
    let at = Instant::now();
    fs::rename(&temporary, &target).context("原子发布 Tile Cache 失败")?;
    drop(guard);
    let cache = open_cache(&target, &stamp)?;
    phases.publish = at.elapsed();
    Ok((
        cache,
        CacheReport {
            cache_hit: false,
            elapsed: started.elapsed(),
            scan: Some(report),
            builder_owned_bytes: builder_owned,
            phases,
        },
    ))
}

fn push_row(
    plan: &Plan,
    data: &mut RowWriter<'_, impl Write + Seek>,
    previous: &mut [Option<Vec<u8>>],
    level: usize,
    y: u32,
    row: &[u8],
    valid: &impl Fn() -> bool,
) -> Result<()> {
    ensure!(valid(), "大图任务已取消");
    let l = &plan.levels[level];
    let ty = y / plan.edge;
    for x in 0..l.columns {
        let id = plan.tile_id(level, x, ty).context("Tile 坐标越界")?;
        let tile = &plan.tiles[id];
        let mut padded = stream::allocated((tile.width as usize + 2) * 4)?;
        for px in 0..tile.width + 2 {
            let global = (i64::from(x * plan.edge) + i64::from(px) - 1)
                .clamp(0, i64::from(l.width) - 1) as usize;
            padded[px as usize * 4..px as usize * 4 + 4]
                .copy_from_slice(&row[global * 4..global * 4 + 4]);
        }
        let local = y % plan.edge;
        write_tile_row(data, tile, local + 1, &padded)?;
        if local == 0 {
            if ty == 0 {
                write_tile_row(data, tile, 0, &padded)?;
            } else {
                let above =
                    &plan.tiles[plan.tile_id(level, x, ty - 1).context("上邻 Tile 越界")?];
                write_tile_row(data, above, above.height + 1, &padded)?;
            }
        }
        if local == tile.height - 1 {
            if ty + 1 == l.rows {
                write_tile_row(data, tile, tile.height + 1, &padded)?;
            } else {
                let below =
                    &plan.tiles[plan.tile_id(level, x, ty + 1).context("下邻 Tile 越界")?];
                write_tile_row(data, below, 0, &padded)?;
            }
        }
    }
    if level + 1 == plan.levels.len() {
        return Ok(());
    }
    if y % 2 == 0 && y + 1 < l.height {
        let mut saved = stream::allocated(row.len())?;
        saved.copy_from_slice(row);
        previous[level] = Some(saved);
        return Ok(());
    }
    let top = previous[level].take();
    let top = top.as_deref().unwrap_or(row);
    let next = &plan.levels[level + 1];
    let mut smaller = stream::allocated(next.width as usize * 4)?;
    for x in 0..next.width as usize {
        let left = x * 2;
        let right = (left + 1).min(l.width as usize - 1);
        for c in 0..4 {
            let sum = u32::from(top[left * 4 + c])
                + u32::from(top[right * 4 + c])
                + u32::from(row[left * 4 + c])
                + u32::from(row[right * 4 + c]);
            smaller[x * 4 + c] = ((sum + 2) / 4) as u8;
        }
    }
    push_row(plan, data, previous, level + 1, y / 2, &smaller, valid)
}

fn write_tile_row(
    data: &mut RowWriter<'_, impl Write + Seek>,
    tile: &Tile,
    y: u32,
    row: &[u8],
) -> Result<()> {
    ensure!(
        row.len() == (tile.width as usize + 2) * 4 && y < tile.height + 2,
        "Tile 行布局错误"
    );
    let at = Instant::now();
    data.output.seek(SeekFrom::Start(
        tile.offset + u64::from(y) * row.len() as u64,
    ))?;
    data.output.write_all(row)?;
    data.profile.elapsed += at.elapsed();
    data.profile.calls += 1;
    data.profile.bytes += row.len() as u64;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::stream::tests::{generate, root};
    use super::*;

    #[test]
    fn build_progress_reports_real_phases_and_cache_hits_skip_scanning() -> Result<()> {
        use super::super::progress::Stage;
        let root = root("progress");
        let image = root.join("own.png");
        let cache_root = root.join("tiles");
        generate(&image, 67, 35)?;
        let mut stages = Vec::new();
        let (cache, report) = build_or_open_with_progress(
            &image,
            &cache_root,
            CacheBudget::default(),
            || true,
            |s| stages.push(s),
        )?;
        assert!(!report.cache_hit);
        assert_eq!(stages.first(), Some(&Stage::Checking));
        assert!(stages.contains(&Stage::Reserving));
        assert!(stages.contains(&Stage::Scanning { rows: 0, total: 35 }));
        assert!(stages.contains(&Stage::Scanning {
            rows: 35,
            total: 35
        }));
        assert!(stages.contains(&Stage::Checksumming {
            tiles: cache.plan.tiles.len(),
            total: cache.plan.tiles.len()
        }));
        assert_eq!(
            &stages[stages.len() - 2..],
            &[Stage::Syncing, Stage::Publishing]
        );
        // Builder 完成不代表 GPU 预览已接受，不得提前报告 Ready。
        assert!(!stages.contains(&Stage::Ready));
        drop(cache);
        stages.clear();
        let (cache, report) = build_or_open_with_progress(
            &image,
            &cache_root,
            CacheBudget::default(),
            || true,
            |s| stages.push(s),
        )?;
        assert!(report.cache_hit);
        assert_eq!(stages, [Stage::Checking]);
        drop(cache);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn cancelling_from_progress_cleans_partial_cache_without_publishing() -> Result<()> {
        use super::super::progress::Stage;
        let root = root("progress-cancel");
        let image = root.join("own.png");
        let cache_root = root.join("tiles");
        generate(&image, 67, 35)?;
        let active = std::cell::Cell::new(true);
        let mut stages = Vec::new();
        let result = build_or_open_with_progress(
            &image,
            &cache_root,
            CacheBudget::default(),
            || active.get(),
            |s| {
                stages.push(s);
                if matches!(s, Stage::Scanning { .. }) {
                    active.set(false);
                }
            },
        );
        assert!(
            result
                .err()
                .context("应取消扫描")?
                .to_string()
                .contains("取消")
        );
        assert!(
            !stages
                .iter()
                .any(|s| matches!(s, Stage::Syncing | Stage::Publishing | Stage::Ready))
        );
        assert!(
            !fs::read_dir(&cache_root)?
                .filter_map(|e| e.ok())
                .any(|e| e.path().is_dir())
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn free_space_refusal_preserves_existing_cache_and_unknown_directories() -> Result<()> {
        let root = root("space");
        let image = root.join("own.png");
        generate(&image, 33, 31)?;
        let c = root.join("tiles");
        let (cache, _) = build_or_open(&image, &c, CacheBudget::default(), || true)?;
        let old = cache.directory.clone();
        let unknown = c.join(".build-unowned");
        fs::create_dir(&unknown)?;
        fs::write(unknown.join("keep"), b"unrelated")?;
        generate(&image, 34, 32)?;
        let error = build_with_space(&image, &c, CacheBudget::default(), || true, |_| Ok(0))
            .err()
            .context("应拒绝空间不足")?;
        assert!(error.to_string().contains("可用空间不足"));
        assert!(old.exists());
        assert!(cache.read_tile(0).is_ok());
        assert!(unknown.join("keep").exists());
        assert!(!fs::read_dir(&c)?.filter_map(|e| e.ok()).any(|e| {
            e.file_name().to_string_lossy().starts_with(".build-") && e.path() != unknown
        }));
        drop(cache);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn write_failure_propagates_and_partial_guard_releases_directory() -> Result<()> {
        struct Fails;
        impl Write for Fails {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from_raw_os_error(112))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl Seek for Fails {
            fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
                Ok(0)
            }
        }
        let root = root("write-failure");
        let temporary = root.join(".build-test");
        fs::create_dir(&temporary)?;
        let plan = Plan::new(
            PngSize {
                width: 3,
                height: 3,
            },
            512,
        )?;
        {
            let _guard = PartialBuild(temporary.clone());
            let mut sink = Fails;
            let mut writer = RowWriter {
                output: &mut sink,
                profile: WriteProfile::default(),
            };
            let error = push_row(
                &plan,
                &mut writer,
                &mut vec![None; plan.levels.len()],
                0,
                0,
                &[0; 12],
                &|| true,
            )
            .unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<std::io::Error>()
                    .and_then(|e| e.raw_os_error()),
                Some(112)
            );
        }
        assert!(!temporary.exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    /// 合成对照：相同 PNG/金字塔回调输出到文件或丢弃流；不读取外部样本。
    #[test]
    #[ignore = "受控 Release 构建分项诊断，使用仓库外 PIXSTRIDE_TEST_ROOT"]
    fn release_build_phase_probe() -> Result<()> {
        struct Discard;
        impl Write for Discard {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl Seek for Discard {
            fn seek(&mut self, at: SeekFrom) -> std::io::Result<u64> {
                match at {
                    SeekFrom::Start(n) => Ok(n),
                    _ => unreachable!(),
                }
            }
        }
        let root = root("phases");
        for (width, height) in [(1025, 769), (4097, 2049)] {
            let source = root.join(format!("{width}.png"));
            generate(&source, width, height)?;
            let plan = Plan::new(PngSize { width, height }, 1024)?;
            for round in 0..3 {
                // 交替顺序，减少顺序造成的缓存/调度偏差；不声称清除了 OS 缓存。
                for disk in if round % 2 == 0 {
                    [true, false]
                } else {
                    [false, true]
                } {
                    if disk {
                        let (cache, report) = build_or_open(
                            &source,
                            &root.join(format!("c-{width}-{round}")),
                            CacheBudget::default(),
                            || true,
                        )?;
                        let scan = report.scan.as_ref().context("应为新构建")?;
                        println!(
                            "BUILD size={width}x{height} round={round} disk=true total_us={} scan_us={} decode_us={} convert_us={} visitor_us={} phases={:?}",
                            report.elapsed.as_micros(),
                            scan.elapsed.as_micros(),
                            scan.decode_rows.as_micros(),
                            scan.convert_rows.as_micros(),
                            scan.visitor.as_micros(),
                            report.phases
                        );
                        assert_eq!(report.phases.writes.bytes, cache.plan.bytes);
                        for id in 0..cache.plan.tiles.len() {
                            cache.read_tile(id)?;
                        }
                        drop(cache);
                    } else {
                        let mut sink = Discard;
                        let mut writer = RowWriter {
                            output: &mut sink,
                            profile: WriteProfile::default(),
                        };
                        let mut previous = vec![None; plan.levels.len()];
                        let scan = stream::scan(
                            &source,
                            StreamBudget::default(),
                            || true,
                            |y, row, _| {
                                push_row(&plan, &mut writer, &mut previous, 0, y, row, &|| true)
                            },
                        )?;
                        println!(
                            "BUILD size={width}x{height} round={round} disk=false scan_us={} decode_us={} convert_us={} visitor_us={} writes={:?}",
                            scan.elapsed.as_micros(),
                            scan.decode_rows.as_micros(),
                            scan.convert_rows.as_micros(),
                            scan.visitor.as_micros(),
                            writer.profile
                        );
                    }
                }
            }
        }
        // 固定 32 MiB 数据、相同文件和 sync_all，隔离小写调用数量；不代表金字塔的随机偏移模式。
        for round in 0..3 {
            for block in if round % 2 == 0 {
                [4096, 262144]
            } else {
                [262144, 4096]
            } {
                let mut file = File::create(root.join(format!("writes-{round}-{block}")))?;
                let data = vec![73u8; block];
                let count = 32 * 1024 * 1024 / block;
                let at = Instant::now();
                for _ in 0..count {
                    file.write_all(&data)?;
                }
                let writes = at.elapsed();
                let at = Instant::now();
                file.sync_all()?;
                println!(
                    "WRITE round={round} block={block} calls={count} write_us={} sync_us={}",
                    writes.as_micros(),
                    at.elapsed().as_micros()
                );
            }
        }
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn cloud_recall_attributes_are_rejected_before_opening_a_stream() {
        assert!(check_local_attributes(0x20).is_ok());
        for attrs in [0x1000, 0x40000, 0x400000, 0x20 | 0x1000] {
            assert!(check_local_attributes(attrs).is_err());
        }
    }
    #[test]
    fn pyramid_pixels_gutters_reuse_revision_and_corruption() -> Result<()> {
        let root = root("cache");
        let image = root.join("own.png");
        generate(&image, 513, 517)?;
        let cache_root = root.join("tiles");
        let budget = CacheBudget {
            tile_edge: 512,
            ..Default::default()
        };
        let (cache, report) = build_or_open(&image, &cache_root, budget, || true)?;
        assert!(!report.cache_hit);
        let mut original = Vec::new();
        stream::scan(
            &image,
            StreamBudget::default(),
            || true,
            |_, r, _| {
                original.extend_from_slice(r);
                Ok(())
            },
        )?;
        for t in cache.plan.tiles.iter().filter(|t| t.level == 0) {
            let id = cache.plan.tile_id(0, t.x, t.y).unwrap();
            let tile = cache.read_tile(id)?;
            assert_eq!(tile.id, id);
            for y in 0..tile.height {
                for x in 0..tile.width {
                    let ox = (i64::from(t.x * cache.plan.edge + x) - 1).clamp(0, 512) as usize;
                    let oy = (i64::from(t.y * cache.plan.edge + y) - 1).clamp(0, 516) as usize;
                    assert_eq!(
                        &tile.bytes[(y * tile.width + x) as usize * 4
                            ..(y * tile.width + x) as usize * 4 + 4],
                        &original[(oy * 513 + ox) * 4..(oy * 513 + ox) * 4 + 4]
                    );
                }
            }
        }
        let mut reference = original;
        let (mut w, mut h) = (513usize, 517usize);
        for (level, l) in cache.plan.levels.iter().enumerate() {
            for ty in 0..l.rows {
                for tx in 0..l.columns {
                    let id = cache.plan.tile_id(level, tx, ty).unwrap();
                    let tile = cache.read_tile(id)?;
                    let t = &cache.plan.tiles[id];
                    for y in 0..t.height as usize {
                        for x in 0..t.width as usize {
                            let offset = ((y + 1) * tile.width as usize + x + 1) * 4;
                            let global = ((ty as usize * cache.plan.edge as usize + y) * w
                                + tx as usize * cache.plan.edge as usize
                                + x)
                                * 4;
                            assert_eq!(
                                &tile.bytes[offset..offset + 4],
                                &reference[global..global + 4]
                            );
                        }
                    }
                }
            }
            let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
            let mut next = vec![0u8; nw * nh * 4];
            for y in 0..nh {
                for x in 0..nw {
                    for c in 0..4 {
                        let mut sum = 0u32;
                        for dy in 0..2 {
                            for dx in 0..2 {
                                sum += u32::from(
                                    reference[((y * 2 + dy).min(h - 1) * w
                                        + (x * 2 + dx).min(w - 1))
                                        * 4
                                        + c],
                                );
                            }
                        }
                        next[(y * nw + x) * 4 + c] = ((sum + 2) / 4) as u8;
                    }
                }
            }
            reference = next;
            w = nw;
            h = nh;
        }
        assert!(
            remove_inactive(&cache.directory).is_err(),
            "活动缓存必须拒绝清理"
        );
        assert!(cache.directory.join("index.bin").is_file());
        let directory = cache.directory.clone();
        drop(cache);
        let (cache, r) = build_or_open(&image, &cache_root, budget, || true)?;
        assert!(r.cache_hit);
        drop(cache);
        let mut f = OpenOptions::new()
            .write(true)
            .open(directory.join("pixels.bin"))?;
        f.write_all(&[99])?;
        drop(f);
        let (cache, _) = build_or_open(&image, &cache_root, budget, || true)?;
        assert!(cache.read_tile(0).is_err());
        drop(cache);
        generate(&image, 17, 19)?;
        let (cache, r) = build_or_open(&image, &cache_root, budget, || true)?;
        assert!(!r.cache_hit);
        assert_eq!(cache.plan.size.width, 17);
        drop(cache);
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn rejects_limits_and_cancellation_cleans_partial_build() -> Result<()> {
        let root = root("cancel");
        let image = root.join("own.png");
        generate(&image, 65, 129)?;
        let c = root.join("tiles");
        assert!(
            build_or_open(
                &image,
                &c,
                CacheBudget {
                    disk_bytes: 1024,
                    ..Default::default()
                },
                || true
            )
            .is_err()
        );
        let count = std::cell::Cell::new(0);
        assert!(
            build_or_open(&image, &c, CacheBudget::default(), || {
                let n = count.get() + 1;
                count.set(n);
                n < 20
            })
            .is_err()
        );
        assert!(
            !fs::read_dir(&c)?
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().starts_with(".build-"))
        );
        let (cache, _) = build_or_open(&image, &c, CacheBudget::default(), || true)?;
        drop(cache);
        let incomplete = c.join(".build-0-old");
        fs::create_dir(&incomplete)?;
        fs::write(incomplete.join("owner"), MAGIC)?;
        generate(&image, 66, 130)?;
        let (cache, _) = build_or_open(&image, &c, CacheBudget::default(), || true)?;
        assert!(!incomplete.exists());
        drop(cache);
        assert!(
            Plan::new(
                PngSize {
                    width: u32::MAX,
                    height: u32::MAX
                },
                1024
            )
            .is_err()
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn sampled_content_changes_and_index_versions_do_not_reuse_stale_data() -> Result<()> {
        let root = root("revision");
        let image = root.join("own.png");
        generate(&image, 17, 19)?;
        let c = root.join("tiles");
        let (cache, _) = build_or_open(&image, &c, CacheBudget::default(), || true)?;
        let index = cache.directory.join("index.bin");
        drop(cache);
        let mut bytes = fs::read(&index)?;
        bytes[8] = 2;
        let end = bytes.len() - 4;
        let crc = crc32fast::hash(&bytes[..end]);
        bytes[end..].copy_from_slice(&crc.to_le_bytes());
        fs::write(&index, bytes)?;
        let (cache, r) = build_or_open(&image, &c, CacheBudget::default(), || true)?;
        assert!(!r.cache_hit);
        drop(cache);
        let before = Stamp::read(&image)?;
        let m = fs::metadata(&image)?.modified()?;
        let mut image_bytes = fs::read(&image)?;
        let at = image_bytes.windows(4).position(|v| v == b"IDAT").unwrap();
        image_bytes[at + 5] ^= 1;
        fs::write(&image, image_bytes)?;
        File::options()
            .write(true)
            .open(&image)?
            .set_times(fs::FileTimes::new().set_modified(m))?;
        let after = Stamp::read(&image)?;
        assert_eq!(before.len, after.len);
        assert_eq!(before.modified, after.modified);
        assert_ne!(before.samples, after.samples);
        assert!(build_or_open(&image, &c, CacheBudget::default(), || true).is_err());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    #[ignore = "Release C2 Tile 尺寸诊断，只生成合成小图"]
    fn release_tile_sizes_probe() -> Result<()> {
        let root = root("sizes");
        let image = root.join("own.png");
        generate(&image, 2049, 1537)?;
        for edge in [512, 1024, 2048] {
            let c = root.join(edge.to_string());
            let (cache, r) = build_or_open(
                &image,
                &c,
                CacheBudget {
                    tile_edge: edge,
                    ..Default::default()
                },
                || true,
            )?;
            let time = Instant::now();
            let mut total = 0;
            for i in (0..cache.plan.tiles.len()).rev() {
                total += cache.read_tile(i)?.bytes.len();
            }
            let read = time.elapsed();
            let tiles = cache.plan.tiles.len();
            let bytes = cache.plan.bytes;
            drop(cache);
            let (cache, hit) = build_or_open(
                &image,
                &c,
                CacheBudget {
                    tile_edge: edge,
                    ..Default::default()
                },
                || true,
            )?;
            println!(
                "edge={edge} tiles={tiles} bytes={bytes} builder_owned={} build_us={} scan_us={} cache_hit_us={} reverse_read_us={} read_bytes={total}",
                r.builder_owned_bytes,
                r.elapsed.as_micros(),
                r.scan.as_ref().unwrap().elapsed.as_micros(),
                hit.elapsed.as_micros(),
                read.as_micros()
            );
            // 只比较保留原字节的 PNG 容器压缩成本，不把缓存里的预乘数据作为可发布颜色图片。
            let tile = cache.read_tile(0)?;
            let mut compressed = Vec::new();
            let time = Instant::now();
            {
                let mut encoder = png::Encoder::new(&mut compressed, tile.width, tile.height);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header()?;
                writer.write_image_data(&tile.bytes)?;
                writer.finish()?;
            }
            let encode_us = time.elapsed().as_micros();
            let time = Instant::now();
            let mut reader = png::Decoder::new(std::io::Cursor::new(&compressed)).read_info()?;
            let mut output = stream::allocated(tile.bytes.len())?;
            reader.next_frame(&mut output)?;
            assert_eq!(output, tile.bytes);
            println!(
                "edge={edge} raw_tile_bytes={} png_tile_bytes={} png_encode_us={encode_us} png_decode_us={}",
                tile.bytes.len(),
                compressed.len(),
                time.elapsed().as_micros()
            );
            drop(cache);
        }
        fs::remove_dir_all(root)?;
        Ok(())
    }
    /// 只在 C3 通过且负责人批准的显式本地诊断中读取真实源；不含任何私人默认路径。
    #[test]
    #[ignore = "C4 私人大图诊断，需要明确批准与独立缓存根目录"]
    fn real_large_cache_probe() -> Result<()> {
        ensure!(
            std::env::var("PIXSTRIDE_APPROVED_LARGE_CACHE").as_deref() == Ok("1"),
            "必须先获得超过 2 GiB 缓存批准"
        );
        let image = std::env::var_os("PIXSTRIDE_STRESS_IMAGE")
            .map(PathBuf::from)
            .context("缺少 PIXSTRIDE_STRESS_IMAGE")?;
        let root = std::env::var_os("PIXSTRIDE_TILE_CACHE")
            .map(PathBuf::from)
            .context("缺少 PIXSTRIDE_TILE_CACHE")?;
        ensure_local(&image)?;
        let before = fs::metadata(&image)?;
        let budget = CacheBudget {
            disk_bytes: 10 * 1024 * 1024 * 1024,
            ..Default::default()
        };
        let size = stream::inspect(&image)?;
        let plan = Plan::new(size, budget.tile_edge)?;
        println!(
            "REAL preflight width={} height={} source_bytes={} planned_data_bytes={} disk_cap={} free_bytes={}",
            size.width,
            size.height,
            before.len(),
            plan.bytes,
            budget.disk_bytes,
            free_bytes(root.parent().context("缺少缓存父目录")?)?
        );
        let (cache, r) = build_or_open(&image, &root, budget, || true)?;
        println!(
            "REAL build hit={} elapsed_us={} scan_us={:?} row_owned_bytes={:?} builder_owned_estimate={} data_bytes={} tiles={} index_bytes={}",
            r.cache_hit,
            r.elapsed.as_micros(),
            r.scan.as_ref().map(|s| s.elapsed.as_micros()),
            r.scan.as_ref().map(|s| s.owned_row_bytes),
            r.builder_owned_bytes,
            cache.plan.bytes,
            cache.plan.tiles.len(),
            fs::metadata(cache.directory.join("index.bin"))?.len()
        );
        let l = &cache.plan.levels[0];
        let mut reads = Vec::new();
        for (x, y) in [
            (0, 0),
            (l.columns - 1, 0),
            (0, l.rows - 1),
            (l.columns - 1, l.rows - 1),
            (l.columns / 2, l.rows / 2),
        ] {
            let id = cache.plan.tile_id(0, x, y).context("Tile 坐标错误")?;
            let at = Instant::now();
            let tile = cache.read_tile(id)?;
            reads.push(at.elapsed().as_micros());
            ensure!(
                tile.bytes
                    .chunks_exact(4)
                    .all(|p| p[..3].iter().all(|&v| v <= p[3])),
                "预乘通道大于 Alpha"
            );
        }
        println!("REAL corner_center_tile_read_us={reads:?}");
        drop(cache);
        let mut reopen = Vec::new();
        for _ in 0..20 {
            let (cache, r) = build_or_open(&image, &root, budget, || true)?;
            ensure!(r.cache_hit, "缓存未命中");
            reopen.push(r.elapsed.as_micros());
            drop(cache);
        }
        reopen.sort_unstable();
        println!(
            "REAL warm_reopen n=20 min_us={} p50_us={} p95_us={} p99_us={} max_us={}; OS cache warm, tail sample insufficient",
            reopen[0], reopen[9], reopen[18], reopen[19], reopen[19]
        );
        let after = fs::metadata(&image)?;
        ensure!(
            before.len() == after.len() && before.modified()? == after.modified()?,
            "原图元数据变化"
        );
        println!("REAL source length and modification time unchanged; source opened read-only");
        Ok(())
    }
}
