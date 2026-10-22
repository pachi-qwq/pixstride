//! 收集 Stage A 的低开销计时样本，并在程序退出时汇总。

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const MAX_SAMPLES_PER_METRIC: usize = 4096;
const MAX_SLOW_IMAGE_TASKS: usize = 16;

/// 最多保留 16 条关联记录，包含最近 8 条及更早记录中最慢的 8 条；只在退出时输出。
#[derive(Default)]
pub(crate) struct ImageTaskTrace {
    pub(crate) generation: u64,
    pub(crate) file_name: String,
    pub(crate) preload: bool,
    pub(crate) queued_behind_current: Option<u64>,
    pub(crate) queued_behind_preload: bool,
    pub(crate) queue: Duration,
    pub(crate) service: Duration,
    pub(crate) still_current: bool,
    pub(crate) outcome: String,
    pub(crate) stages: Vec<(Metric, Duration)>,
}

struct NavigationRelease {
    generation: u64,
    at: Instant,
    status: &'static str,
    delay: Option<Duration>,
}

/// 可单独统计的 Stage A 计时边界。
#[derive(Clone, Copy, Debug)]
pub(crate) enum Metric {
    Startup,
    D3dInitialization,
    DeviceCreation,
    SwapChainCreation,
    ShaderInitialization,
    TextureUpload,
    FirstPresent,
    InputHandling,
    RequestToRender,
    RequestToRenderInitial,
    RequestToRenderInput,
    RequestToRenderResize,
    RequestToRenderMinimize,
    RequestToRenderRestore,
    RequestToRenderPaint,
    RenderCommands,
    Present,
    Resize,
    Shutdown,
    DirectoryScan,
    WicOpen,
    PixelFormatConversion,
    WicCopyPixels,
    ImageTextureUpload,
    ImageRequestToRender,
    ImageFirstPresent,
    ImageDecodeTotal,
    PreloadDecode,
    ImageWorkerQueueWait,
    ImageWorkerService,
    ImageRequestToWorker,
    CpuCacheLookup,
    CurrentGpuHitToPresent,
    CurrentGpuMissToPresent,
    IdentityQueueWait,
    SourceIdentityRead,
    SourceIdentityVerify,
    ImageLayout,
    PixelAllocation,
    QueueBehindPreload,
    CurrentWicTotal,
    PreloadWicTotal,
    CurrentCpuHitToPresent,
    QueueBehindCurrent,
    WicFactoryCreation,
    WicDecoderOpen,
    WicFrameMetadata,
    KeyReleaseToPresent,
    WicGetFrame,
    WicExif,
    ImageSourceBudget,
    TileRead,
    TileUpload,
    LargePreview,
    TileViewport,
    TileCpuHit,
    TileDiskRead,
    TileAllocation,
    TileFileRead,
    TileCrc,
    TileWorkerQueue,
    TileReplyBackpressure,
    TileReplyToRender,
    TileViewDispatch,
    GifStructure,
    GifPoster,
    AnimationInit,
    AnimationDecode,
    AnimationCompose,
    AnimationOutput,
    AnimationBackpressure,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Counter {
    CpuCacheHit,
    CpuCacheMiss,
    GpuCacheHit,
    GpuCacheMiss,
    PreloadDecoded,
    CpuCurrentHit,
    CpuCurrentMiss,
    CpuPreloadHit,
    CpuPreloadMiss,
    GpuCurrentHit,
    GpuCurrentMiss,
    GpuPreloadHit,
    GpuPreloadMiss,
    NavigationTargetChanged,
    ImageSelectionPresented,
    ImageDecodeFailed,
    ImageLoadFailed,
    DecodeCancelled,
    BudgetRejectionHit,
    NavigationProgressPresented,
}

impl Counter {
    const ALL: [Self; 20] = [
        Self::CpuCacheHit,
        Self::CpuCacheMiss,
        Self::GpuCacheHit,
        Self::GpuCacheMiss,
        Self::PreloadDecoded,
        Self::CpuCurrentHit,
        Self::CpuCurrentMiss,
        Self::CpuPreloadHit,
        Self::CpuPreloadMiss,
        Self::GpuCurrentHit,
        Self::GpuCurrentMiss,
        Self::GpuPreloadHit,
        Self::GpuPreloadMiss,
        Self::NavigationTargetChanged,
        Self::ImageSelectionPresented,
        Self::ImageDecodeFailed,
        Self::ImageLoadFailed,
        Self::DecodeCancelled,
        Self::BudgetRejectionHit,
        Self::NavigationProgressPresented,
    ];

    fn index(self) -> usize {
        match self {
            Self::CpuCacheHit => 0,
            Self::CpuCacheMiss => 1,
            Self::GpuCacheHit => 2,
            Self::GpuCacheMiss => 3,
            Self::PreloadDecoded => 4,
            Self::CpuCurrentHit => 5,
            Self::CpuCurrentMiss => 6,
            Self::CpuPreloadHit => 7,
            Self::CpuPreloadMiss => 8,
            Self::GpuCurrentHit => 9,
            Self::GpuCurrentMiss => 10,
            Self::GpuPreloadHit => 11,
            Self::GpuPreloadMiss => 12,
            Self::NavigationTargetChanged => 13,
            Self::ImageSelectionPresented => 14,
            Self::ImageDecodeFailed => 15,
            Self::ImageLoadFailed => 16,
            Self::DecodeCancelled => 17,
            Self::BudgetRejectionHit => 18,
            Self::NavigationProgressPresented => 19,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::CpuCacheHit => "CPU 缓存命中",
            Self::CpuCacheMiss => "CPU 缓存未命中",
            Self::GpuCacheHit => "GPU 缓存命中",
            Self::GpuCacheMiss => "GPU 缓存未命中",
            Self::PreloadDecoded => "预加载完成",
            Self::CpuCurrentHit => "当前选择 CPU 缓存命中",
            Self::CpuCurrentMiss => "当前选择 CPU 缓存未命中",
            Self::CpuPreloadHit => "预加载 CPU 缓存命中",
            Self::CpuPreloadMiss => "预加载 CPU 缓存未命中",
            Self::GpuCurrentHit => "当前选择 GPU 缓存命中",
            Self::GpuCurrentMiss => "当前选择 GPU 缓存未命中",
            Self::GpuPreloadHit => "预加载 GPU 缓存命中",
            Self::GpuPreloadMiss => "预加载 GPU 缓存未命中",
            Self::NavigationTargetChanged => "导航目标索引变化次数",
            Self::ImageSelectionPresented => "图片选择 Present 成功返回次数",
            Self::ImageDecodeFailed => "WIC 解码失败（含预算拒绝）",
            Self::ImageLoadFailed => "当前图片读取或解码失败提示",
            Self::DecodeCancelled => "阶段间提前取消的过期图片任务",
            Self::BudgetRejectionHit => "同一源修订的预算拒绝命中",
            Self::NavigationProgressPresented => "长按进度图片 Present 成功返回次数",
        }
    }
}

impl Metric {
    const ALL: [Self; 71] = [
        Self::Startup,
        Self::D3dInitialization,
        Self::DeviceCreation,
        Self::SwapChainCreation,
        Self::ShaderInitialization,
        Self::TextureUpload,
        Self::FirstPresent,
        Self::InputHandling,
        Self::RequestToRender,
        Self::RequestToRenderInitial,
        Self::RequestToRenderInput,
        Self::RequestToRenderResize,
        Self::RequestToRenderMinimize,
        Self::RequestToRenderRestore,
        Self::RequestToRenderPaint,
        Self::RenderCommands,
        Self::Present,
        Self::Resize,
        Self::Shutdown,
        Self::DirectoryScan,
        Self::WicOpen,
        Self::PixelFormatConversion,
        Self::WicCopyPixels,
        Self::ImageTextureUpload,
        Self::ImageRequestToRender,
        Self::ImageFirstPresent,
        Self::ImageDecodeTotal,
        Self::PreloadDecode,
        Self::ImageWorkerQueueWait,
        Self::ImageWorkerService,
        Self::ImageRequestToWorker,
        Self::CpuCacheLookup,
        Self::CurrentGpuHitToPresent,
        Self::CurrentGpuMissToPresent,
        Self::IdentityQueueWait,
        Self::SourceIdentityRead,
        Self::SourceIdentityVerify,
        Self::ImageLayout,
        Self::PixelAllocation,
        Self::QueueBehindPreload,
        Self::CurrentWicTotal,
        Self::PreloadWicTotal,
        Self::CurrentCpuHitToPresent,
        Self::QueueBehindCurrent,
        Self::WicFactoryCreation,
        Self::WicDecoderOpen,
        Self::WicFrameMetadata,
        Self::KeyReleaseToPresent,
        Self::WicGetFrame,
        Self::WicExif,
        Self::ImageSourceBudget,
        Self::TileRead,
        Self::TileUpload,
        Self::LargePreview,
        Self::TileViewport,
        Self::TileCpuHit,
        Self::TileDiskRead,
        Self::TileAllocation,
        Self::TileFileRead,
        Self::TileCrc,
        Self::TileWorkerQueue,
        Self::TileReplyBackpressure,
        Self::TileReplyToRender,
        Self::TileViewDispatch,
        Self::GifStructure,
        Self::GifPoster,
        Self::AnimationInit,
        Self::AnimationDecode,
        Self::AnimationCompose,
        Self::AnimationOutput,
        Self::AnimationBackpressure,
    ];

    fn index(self) -> usize {
        match self {
            Self::Startup => 0,
            Self::D3dInitialization => 1,
            Self::DeviceCreation => 2,
            Self::SwapChainCreation => 3,
            Self::ShaderInitialization => 4,
            Self::TextureUpload => 5,
            Self::FirstPresent => 6,
            Self::InputHandling => 7,
            Self::RequestToRender => 8,
            Self::RequestToRenderInitial => 9,
            Self::RequestToRenderInput => 10,
            Self::RequestToRenderResize => 11,
            Self::RequestToRenderMinimize => 12,
            Self::RequestToRenderRestore => 13,
            Self::RequestToRenderPaint => 14,
            Self::RenderCommands => 15,
            Self::Present => 16,
            Self::Resize => 17,
            Self::Shutdown => 18,
            Self::DirectoryScan => 19,
            Self::WicOpen => 20,
            Self::PixelFormatConversion => 21,
            Self::WicCopyPixels => 22,
            Self::ImageTextureUpload => 23,
            Self::ImageRequestToRender => 24,
            Self::ImageFirstPresent => 25,
            Self::ImageDecodeTotal => 26,
            Self::PreloadDecode => 27,
            Self::ImageWorkerQueueWait => 28,
            Self::ImageWorkerService => 29,
            Self::ImageRequestToWorker => 30,
            Self::CpuCacheLookup => 31,
            Self::CurrentGpuHitToPresent => 32,
            Self::CurrentGpuMissToPresent => 33,
            Self::IdentityQueueWait => 34,
            Self::SourceIdentityRead => 35,
            Self::SourceIdentityVerify => 36,
            Self::ImageLayout => 37,
            Self::PixelAllocation => 38,
            Self::QueueBehindPreload => 39,
            Self::CurrentWicTotal => 40,
            Self::PreloadWicTotal => 41,
            Self::CurrentCpuHitToPresent => 42,
            Self::QueueBehindCurrent => 43,
            Self::WicFactoryCreation => 44,
            Self::WicDecoderOpen => 45,
            Self::WicFrameMetadata => 46,
            Self::KeyReleaseToPresent => 47,
            Self::WicGetFrame => 48,
            Self::WicExif => 49,
            Self::ImageSourceBudget => 50,
            Self::TileRead => 51,
            Self::TileUpload => 52,
            Self::LargePreview => 53,
            Self::TileViewport => 54,
            Self::TileCpuHit => 55,
            Self::TileDiskRead => 56,
            Self::TileAllocation => 57,
            Self::TileFileRead => 58,
            Self::TileCrc => 59,
            Self::TileWorkerQueue => 60,
            Self::TileReplyBackpressure => 61,
            Self::TileReplyToRender => 62,
            Self::TileViewDispatch => 63,
            Self::GifStructure => 64,
            Self::GifPoster => 65,
            Self::AnimationInit => 66,
            Self::AnimationDecode => 67,
            Self::AnimationCompose => 68,
            Self::AnimationOutput => 69,
            Self::AnimationBackpressure => 70,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::GifStructure => "GIF 完整块结构预检（不校验 LZW 内容）",
            Self::GifPoster => "GIF WIC 首帧与透明画布合成",
            Self::AnimationInit => "动画 Session 预检与 WIC 初始化",
            Self::AnimationDecode => "动画 WIC 局部帧解码与分配",
            Self::AnimationCompose => "动画 CPU Disposal 与合成",
            Self::AnimationOutput => "动画完整输出分配与复制",
            Self::AnimationBackpressure => "动画 Credit 等待（不含暂停）",
            Self::TileRead => "Tile CPU 查询或磁盘读取与 CRC 校验",
            Self::TileUpload => "Tile GPU 创建与上传提交",
            Self::LargePreview => "大图选择到预览 Present 返回",
            Self::TileViewport => "视口发布到本次最后所需 Tile Present 返回",
            Self::TileCpuHit => "Tile CPU 缓存命中查询",
            Self::TileDiskRead => "Tile 磁盘读取与 CRC 校验",
            Self::TileAllocation => "Tile 缓冲区分配与清零",
            Self::TileFileRead => "Tile 文件锁、Seek 与 Read（含 OS 缓存）",
            Self::TileCrc => "Tile CRC 计算",
            Self::TileWorkerQueue => "视口入队到该 Tile 开始（含前序 Tile 处理）",
            Self::TileReplyBackpressure => "Tile 答复队列背压等待",
            Self::TileReplyToRender => "Tile 答复发布到渲染线程接收",
            Self::TileViewDispatch => "视口状态发布到 Tile 请求入队",

            Self::Startup => "启动到 CreateWindowExW 返回",
            Self::D3dInitialization => "D3D11 初始化",
            Self::DeviceCreation => "D3D11 Device 创建",
            Self::SwapChainCreation => "DXGI Swap Chain 创建和帧延迟设置",
            Self::ShaderInitialization => "Shader 与渲染状态初始化",
            Self::TextureUpload => "测试纹理 CPU 准备与 GPU 资源创建",
            Self::FirstPresent => "渲染线程启动到首次 Present 成功返回",
            Self::InputHandling => "Win32 输入处理到状态发布",
            Self::RequestToRender => "最新状态发布到渲染线程开始处理（全部来源）",
            Self::RequestToRenderInitial => "首次窗口尺寸请求到渲染线程处理",
            Self::RequestToRenderInput => "方向键请求到渲染线程处理",
            Self::RequestToRenderResize => "窗口 Resize 请求到渲染线程处理",
            Self::RequestToRenderMinimize => "最小化请求到渲染线程处理",
            Self::RequestToRenderRestore => "恢复请求到渲染线程处理",
            Self::RequestToRenderPaint => "WM_PAINT 请求到渲染线程处理",
            Self::RenderCommands => "渲染命令准备",
            Self::Present => "Present 调用耗时",
            Self::Resize => "Swap Chain Resize 处理",
            Self::Shutdown => "关闭请求到渲染线程退出",
            Self::DirectoryScan => "图片目录扫描",
            Self::WicOpen => "WIC 打开源文件和读取元数据",
            Self::PixelFormatConversion => "WIC Pixel Format Converter 初始化",
            Self::WicCopyPixels => "WIC CopyPixels 解码与格式转换",
            Self::ImageTextureUpload => "D3D11 图片纹理创建与上传提交",
            Self::ImageRequestToRender => "图片请求到渲染线程处理",
            Self::ImageFirstPresent => "首张图片 Present 调用返回",
            Self::ImageDecodeTotal => "WIC 打开与解码耗时（含失败路径）",
            Self::PreloadDecode => "相邻图片预加载耗时",
            Self::ImageWorkerQueueWait => "当前图片请求在解码 Worker 队列中的等待时间",
            Self::ImageWorkerService => "解码 Worker 处理当前图片请求耗时（含缓存查询和 WIC）",
            Self::ImageRequestToWorker => "当前图片请求到解码线程结果",
            Self::CpuCacheLookup => "CPU 缓存查询（不含文件身份检查）",
            Self::CurrentGpuHitToPresent => "当前 GPU 命中选择到 Present 返回",
            Self::CurrentGpuMissToPresent => "当前 GPU 未命中选择到 Present 返回",
            Self::IdentityQueueWait => "文件身份校验线程队列等待时间",
            Self::SourceIdentityRead => "SourceIdentity 文件规范化与元数据读取",
            Self::SourceIdentityVerify => "解码后 SourceIdentity 复核",
            Self::ImageLayout => "WIC 方向转换与尺寸预算检查",
            Self::PixelAllocation => "像素缓冲区分配与清零",
            Self::QueueBehindPreload => "入队时已有预加载运行的当前请求队列等待",
            Self::CurrentWicTotal => "当前图片 WIC 处理（含失败路径）",
            Self::PreloadWicTotal => "预加载 WIC 处理（含失败路径）",
            Self::CurrentCpuHitToPresent => {
                "当前 CPU 命中选择到 Present 返回（含 Worker 和 GPU 处理）"
            }
            Self::QueueBehindCurrent => "入队时已有当前图片运行的队列等待",
            Self::WicFactoryCreation => "WIC Imaging Factory 创建",
            Self::WicDecoderOpen => "WIC CreateDecoderFromFilename",
            Self::WicFrameMetadata => "WIC GetFrame 与 EXIF 元数据查询",
            Self::KeyReleaseToPresent => "方向键松开到该目标 Present 返回（仅松键时未完成的目标）",
            Self::WicGetFrame => "WIC GetFrame",
            Self::WicExif => "EXIF Orientation 查询",
            Self::ImageSourceBudget => "原始尺寸预算预检（EXIF 前）",
        }
    }
}

/// 固定容量的最近样本集合，避免长时间运行时统计数据无界增长。
#[derive(Default)]
struct Samples {
    values_us: VecDeque<u64>,
    discarded: u64,
}

impl Samples {
    fn record(&mut self, duration: Duration) {
        if self.values_us.len() == MAX_SAMPLES_PER_METRIC {
            self.values_us.pop_front();
            self.discarded += 1;
        }
        self.values_us
            .push_back(duration.as_micros().min(u64::MAX as u128) as u64);
    }

    fn summary(&self) -> Option<Summary> {
        if self.values_us.is_empty() {
            return None;
        }

        let mut sorted: Vec<_> = self.values_us.iter().copied().collect();
        sorted.sort_unstable();
        Some(Summary {
            count: sorted.len(),
            min: sorted[0],
            max: sorted[sorted.len() - 1],
            p50: percentile(&sorted, 50),
            p95: percentile(&sorted, 95),
            p99: percentile(&sorted, 99),
            discarded: self.discarded,
        })
    }
}

struct Summary {
    count: usize,
    min: u64,
    max: u64,
    p50: u64,
    p95: u64,
    p99: u64,
    discarded: u64,
}

/// 记录渲染与窗口生命周期中的计时样本。
pub(crate) struct Metrics {
    samples: Vec<Samples>,
    counters: [u64; 20],
    slow_image_tasks: VecDeque<ImageTaskTrace>,
    slow_tile_views: VecDeque<String>,
    cpu_cache_bytes: u64,
    gpu_cache_bytes: u64,
    peak_cpu_cache_bytes: u64,
    peak_gpu_cache_bytes: u64,
    target_generation: u64,
    presented_generation: u64,
    last_navigation_release: Option<NavigationRelease>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            samples: std::iter::repeat_with(Samples::default)
                .take(Metric::ALL.len())
                .collect(),
            counters: [0; 20],
            slow_image_tasks: VecDeque::new(),
            slow_tile_views: VecDeque::new(),
            cpu_cache_bytes: 0,
            gpu_cache_bytes: 0,
            peak_cpu_cache_bytes: 0,
            peak_gpu_cache_bytes: 0,
            target_generation: 0,
            presented_generation: 0,
            last_navigation_release: None,
        }
    }
}

impl Metrics {
    /// 只保留最近 16 个长尾视口的关联分项，退出时统一打印。
    pub(crate) fn record_tile_view(&mut self, trace: String) {
        if self.slow_tile_views.len() == 16 {
            self.slow_tile_views.pop_front();
        }
        self.slow_tile_views.push_back(trace);
    }
    /// 记录方向键松开时的目标；已经提交的图片不混入后续等待分布。
    pub(crate) fn record_navigation_release(&mut self, generation: u64, at: Instant) {
        self.last_navigation_release = Some(NavigationRelease {
            generation,
            at,
            delay: None,
            status: if self.presented_generation == generation {
                "already-presented"
            } else {
                "awaiting-present"
            },
        });
    }
    /// 有界保留最近记录和更早的最慢记录；调用者先筛选慢任务或真实错误。
    pub(crate) fn record_image_task(&mut self, trace: ImageTaskTrace) {
        self.slow_image_tasks.push_back(trace);
        if self.slow_image_tasks.len() > MAX_SLOW_IMAGE_TASKS {
            // 重复的快速预算拒绝不能挤掉最有价值的秒级长尾。最近一半无条件保留，其余按工作/等待总耗时保留。
            let older = self.slow_image_tasks.len() - MAX_SLOW_IMAGE_TASKS / 2;
            let least_useful = self
                .slow_image_tasks
                .iter()
                .take(older)
                .enumerate()
                .min_by_key(|(_, task)| {
                    task.service.saturating_add(if task.preload {
                        Duration::ZERO
                    } else {
                        task.queue
                    })
                })
                .map(|(index, _)| index)
                .unwrap_or(0);
            self.slow_image_tasks.remove(least_useful);
        }
    }
    pub(crate) fn set_target_generation(&mut self, generation: u64) {
        self.target_generation = generation;
        if let Some(release) = self.last_navigation_release.as_mut() {
            if release.generation != generation && release.status == "awaiting-present" {
                release.status = "superseded-by-new-selection";
            }
        }
    }

    pub(crate) fn record_presented_generation(&mut self, generation: u64) {
        self.presented_generation = generation;
        self.increment(Counter::ImageSelectionPresented);
        let delay = self.last_navigation_release.as_mut().and_then(|release| {
            if release.generation == generation && release.status == "awaiting-present" {
                let delay = release.at.elapsed();
                release.delay = Some(delay);
                release.status = "presented-after-release";
                Some(delay)
            } else {
                None
            }
        });
        if let Some(delay) = delay {
            self.record(Metric::KeyReleaseToPresent, delay);
        }
    }

    #[cfg(test)]
    pub(crate) fn presented_generation(&self) -> u64 {
        self.presented_generation
    }
    #[cfg(test)]
    pub(crate) fn counter(&self, counter: Counter) -> u64 {
        self.counters[counter.index()]
    }
    pub(crate) fn record(&mut self, metric: Metric, duration: Duration) {
        self.samples[metric.index()].record(duration);
    }

    #[cfg(test)]
    pub(crate) fn sample_count(&self, metric: Metric) -> usize {
        self.samples[metric.index()].values_us.len()
    }

    #[cfg(test)]
    pub(crate) fn last_sample_us(&self, metric: Metric) -> Option<u64> {
        self.samples[metric.index()].values_us.back().copied()
    }

    pub(crate) fn increment(&mut self, counter: Counter) {
        self.counters[counter.index()] = self.counters[counter.index()].saturating_add(1);
    }

    pub(crate) fn set_cpu_cache_bytes(&mut self, cpu: u64) {
        self.cpu_cache_bytes = cpu;
        self.peak_cpu_cache_bytes = self.peak_cpu_cache_bytes.max(cpu);
    }
    pub(crate) fn set_gpu_cache_bytes(&mut self, gpu: u64) {
        self.gpu_cache_bytes = gpu;
        self.peak_gpu_cache_bytes = self.peak_gpu_cache_bytes.max(gpu);
    }

    pub(crate) fn report(&self) -> String {
        let mut report = String::from("Pixstride timings (microseconds; app-side events)\n");
        for metric in Metric::ALL {
            if let Some(summary) = self.samples[metric.index()].summary() {
                let _ = std::fmt::Write::write_fmt(
                    &mut report,
                    format_args!(
                        "{}: n={} min={} p50={} p95={} p99={} max={}{}\n",
                        metric.name(),
                        summary.count,
                        summary.min,
                        summary.p50,
                        summary.p95,
                        summary.p99,
                        summary.max,
                        if summary.discarded == 0 {
                            String::new()
                        } else {
                            format!(" older_samples_omitted={}", summary.discarded)
                        }
                    ),
                );
            } else {
                let _ = std::fmt::Write::write_fmt(
                    &mut report,
                    format_args!("{}: n=0 (not measured)\n", metric.name()),
                );
            }
        }
        report.push_str("Cache counters and current resident bytes:\n");
        if let Some(release) = &self.last_navigation_release {
            let _ = std::fmt::Write::write_fmt(
                &mut report,
                format_args!(
                    "Last navigation key release: generation={} status={} delay_us={:?}\n",
                    release.generation,
                    release.status,
                    release.delay.map(|delay| delay.as_micros())
                ),
            );
        }
        let _ = std::fmt::Write::write_fmt(
            &mut report,
            format_args!(
                "Latest target generation={}; last successful image Present generation={}\n",
                self.target_generation, self.presented_generation
            ),
        );
        for counter in Counter::ALL {
            let _ = std::fmt::Write::write_fmt(
                &mut report,
                format_args!("{}: {}\n", counter.name(), self.counters[counter.index()]),
            );
        }
        report_cache_rate(
            &mut report,
            "当前选择 CPU 缓存命中率",
            self.counters[Counter::CpuCurrentHit.index()],
            self.counters[Counter::CpuCurrentMiss.index()],
        );
        report_cache_rate(
            &mut report,
            "预加载 CPU 缓存命中率",
            self.counters[Counter::CpuPreloadHit.index()],
            self.counters[Counter::CpuPreloadMiss.index()],
        );
        report_cache_rate(
            &mut report,
            "当前选择 GPU 缓存命中率",
            self.counters[Counter::GpuCurrentHit.index()],
            self.counters[Counter::GpuCurrentMiss.index()],
        );
        report_cache_rate(
            &mut report,
            "预加载 GPU 缓存命中率",
            self.counters[Counter::GpuPreloadHit.index()],
            self.counters[Counter::GpuPreloadMiss.index()],
        );
        let _ = std::fmt::Write::write_fmt(
            &mut report,
            format_args!(
                "CPU image cache current={} peak={} bytes; GPU texture cache current={} peak={} bytes\n",
                self.cpu_cache_bytes,
                self.peak_cpu_cache_bytes,
                self.gpu_cache_bytes,
                self.peak_gpu_cache_bytes
            ),
        );
        report.push_str(
            "Present duration is not display latency or input-to-photon latency. Use PresentMon/ETW or an external display measurement for displayed-frame timing.\n",
        );
        report.push_str(
            "Slow/error image tasks (at most 16: recent 8 plus slowest older 8; microseconds):\n",
        );
        for trace in &self.slow_image_tasks {
            let _ = std::fmt::Write::write_fmt(
                &mut report,
                format_args!(
                    "generation={} role={} file={:?} queue={} service={} behind_current={:?} behind_preload={} current_at_finish={} outcome={}\n",
                    trace.generation,
                    if trace.preload { "preload" } else { "current" },
                    trace.file_name,
                    trace.queue.as_micros(),
                    trace.service.as_micros(),
                    trace.queued_behind_current,
                    trace.queued_behind_preload,
                    trace.still_current,
                    trace.outcome
                ),
            );
            for (metric, duration) in &trace.stages {
                let _ = std::fmt::Write::write_fmt(
                    &mut report,
                    format_args!("  {}: {}\n", metric.name(), duration.as_micros()),
                );
            }
        }
        report.push_str(
            "Slow Tile viewports (at most 16; microseconds; stage sums overlap pipeline time):\n",
        );
        for trace in &self.slow_tile_views {
            report.push_str(trace);
            report.push('\n');
        }
        report
    }
}

fn report_cache_rate(report: &mut String, name: &str, hits: u64, misses: u64) {
    let total = hits.saturating_add(misses);
    if total == 0 {
        let _ = std::fmt::Write::write_fmt(report, format_args!("{name}: n=0\n"));
    } else {
        let rate = hits as f64 * 100.0 / total as f64;
        let _ = std::fmt::Write::write_fmt(
            report,
            format_args!("{name}: {rate:.1}% ({hits}/{total})\n"),
        );
    }
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let rank = (sorted.len() * percentile).div_ceil(100).max(1);
    sorted[rank - 1]
}

#[cfg(test)]
mod tests {
    use super::{MAX_SAMPLES_PER_METRIC, Metric, Metrics, Samples, percentile};
    use std::time::Duration;

    #[test]
    fn slow_tile_view_traces_are_bounded_and_report_pipeline_units() {
        let mut metrics = super::Metrics::default();
        for i in 0..100 {
            metrics.record_tile_view(format!("tile-view-{i}"));
        }
        assert_eq!(metrics.slow_tile_views.len(), 16);
        let report = metrics.report();
        assert!(report.contains("stage sums overlap pipeline time"));
        assert!(report.contains("tile-view-99\n"));
        assert!(!report.contains("tile-view-83\n"));
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        let values = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&values, 50), 5);
        assert_eq!(percentile(&values, 95), 10);
        assert_eq!(percentile(&values, 99), 10);
    }

    #[test]
    fn samples_keep_a_bounded_recent_window() {
        let mut samples = Samples::default();
        for value in 0..=MAX_SAMPLES_PER_METRIC {
            samples.record(Duration::from_micros(value as u64));
        }

        let summary = samples.summary().expect("已有样本");
        assert_eq!(summary.count, MAX_SAMPLES_PER_METRIC);
        assert_eq!(summary.min, 1);
        assert_eq!(summary.max, MAX_SAMPLES_PER_METRIC as u64);
        assert_eq!(summary.discarded, 1);
    }

    #[test]
    fn report_marks_unmeasured_metrics() {
        let mut metrics = Metrics::default();
        metrics.record(Metric::Startup, Duration::from_micros(12));

        let report = metrics.report();
        assert!(report.contains("启动到 CreateWindowExW 返回: n=1"));
        assert!(report.contains("Present 调用耗时: n=0 (not measured)"));
        assert!(report.contains("DXGI Swap Chain 创建和帧延迟设置: n=0 (not measured)"));
        assert!(report.contains("not display latency"));
        assert!(report.contains("GPU texture cache current=0 peak=0 bytes"));
        assert!(report.contains("CPU 缓存命中: 0"));
        assert!(report.contains("当前选择 CPU 缓存命中率: n=0"));
    }

    #[test]
    fn slow_task_records_are_bounded_and_keep_generation_stage_and_outcome() {
        let mut metrics = Metrics::default();
        for generation in 0..=super::MAX_SLOW_IMAGE_TASKS as u64 {
            metrics.record_image_task(super::ImageTaskTrace {
                generation,
                outcome: "cancelled-stale".to_owned(),
                stages: vec![(Metric::WicOpen, Duration::from_millis(1200))],
                ..Default::default()
            });
        }
        assert_eq!(metrics.slow_image_tasks.len(), super::MAX_SLOW_IMAGE_TASKS);
        let report = metrics.report();
        assert!(!report.contains("generation=0 role="));
        assert!(report.contains("generation=16 role="));
        assert!(report.contains("outcome=cancelled-stale"));
        assert!(report.contains("WIC 打开源文件和读取元数据: 1200000"));
    }

    #[test]
    fn repeated_fast_errors_do_not_evict_the_worst_task() {
        let mut metrics = Metrics::default();
        metrics.record_image_task(super::ImageTaskTrace {
            generation: 1,
            service: Duration::from_secs(2),
            ..Default::default()
        });
        for generation in 2..=60 {
            metrics.record_image_task(super::ImageTaskTrace {
                generation,
                service: Duration::from_millis(1),
                ..Default::default()
            });
        }
        assert_eq!(metrics.slow_image_tasks.len(), super::MAX_SLOW_IMAGE_TASKS);
        assert!(
            metrics
                .slow_image_tasks
                .iter()
                .any(|task| task.generation == 1)
        );
        for generation in 53..=60 {
            assert!(
                metrics
                    .slow_image_tasks
                    .iter()
                    .any(|task| task.generation == generation)
            );
        }
    }

    #[test]
    fn key_release_correlates_only_its_target_and_ignores_superseded_selections() {
        let mut metrics = Metrics::default();
        metrics.set_target_generation(5);
        metrics
            .record_navigation_release(5, std::time::Instant::now() - Duration::from_millis(100));
        metrics.record_presented_generation(4);
        assert!(
            metrics
                .report()
                .contains("generation=5 status=awaiting-present")
        );
        metrics.record_presented_generation(5);
        assert!(
            metrics
                .report()
                .contains("generation=5 status=presented-after-release")
        );
        assert_eq!(
            metrics.samples[Metric::KeyReleaseToPresent.index()]
                .summary()
                .unwrap()
                .count,
            1
        );
        metrics.record_navigation_release(5, std::time::Instant::now());
        assert!(
            metrics
                .report()
                .contains("generation=5 status=already-presented")
        );
        metrics.set_target_generation(6);
        metrics.record_navigation_release(6, std::time::Instant::now());
        metrics.set_target_generation(7);
        metrics.record_presented_generation(6);
        assert!(
            metrics
                .report()
                .contains("generation=6 status=superseded-by-new-selection")
        );
        assert_eq!(
            metrics.samples[Metric::KeyReleaseToPresent.index()]
                .summary()
                .unwrap()
                .count,
            1
        );
    }
}
