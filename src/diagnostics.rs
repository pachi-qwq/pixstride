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
}

impl Metric {
    const ALL: [Self; 19] = [
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
    samples: [Samples; 19],
}

impl Metrics {
    pub(crate) fn record(&mut self, metric: Metric, duration: Duration) {
        self.samples[metric.index()].record(duration);
    }

    pub(crate) fn report(&self) -> String {
        let mut report =
            String::from("Pixstride Stage A timings (microseconds; app-side events)\n");
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
        report.push_str(
            "Present duration is not display latency or input-to-photon latency. Use PresentMon/ETW or an external display measurement for displayed-frame timing.\n",
        );
        report
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
    }
}
