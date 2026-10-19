//! 收集 Stage A 的低开销计时样本，并在程序退出时汇总。

use std::{collections::VecDeque, time::Duration};

const MAX_SAMPLES_PER_METRIC: usize = 4096;

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
    ImageRequestToWorker,
    CpuCacheLookup,
    CurrentGpuHitToPresent,
    CurrentGpuMissToPresent,
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
}

impl Counter {
    const ALL: [Self; 13] = [
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
        }
    }
}

impl Metric {
    const ALL: [Self; 32] = [
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
        Self::ImageRequestToWorker,
        Self::CpuCacheLookup,
        Self::CurrentGpuHitToPresent,
        Self::CurrentGpuMissToPresent,
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
            Self::ImageRequestToWorker => 28,
            Self::CpuCacheLookup => 29,
            Self::CurrentGpuHitToPresent => 30,
            Self::CurrentGpuMissToPresent => 31,
        }
    }

    fn name(self) -> &'static str {
        match self {
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
            Self::ImageDecodeTotal => "WIC 完整打开与解码耗时（含内部工作）",
            Self::PreloadDecode => "相邻图片预加载耗时",
            Self::ImageRequestToWorker => "当前图片请求到解码线程结果",
            Self::CpuCacheLookup => "文件身份检查与 CPU 缓存查询",
            Self::CurrentGpuHitToPresent => "当前 GPU 命中选择到 Present 返回",
            Self::CurrentGpuMissToPresent => "当前 GPU 未命中选择到 Present 返回",
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
#[derive(Default)]
pub(crate) struct Metrics {
    samples: [Samples; 32],
    counters: [u64; 13],
    cpu_cache_bytes: u64,
    gpu_cache_bytes: u64,
    peak_cpu_cache_bytes: u64,
    peak_gpu_cache_bytes: u64,
}

impl Metrics {
    pub(crate) fn record(&mut self, metric: Metric, duration: Duration) {
        self.samples[metric.index()].record(duration);
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
}
