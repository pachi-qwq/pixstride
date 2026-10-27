//! 构建阶段只保留最新值；行数百分比不代表整个缓存的完成比例，也不预测剩余时间。

use std::time::{Duration, Instant};

/// Builder 的真实工作阶段。Ready 由渲染线程在预览上传并接受当前 Generation 后发布。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Checking,
    Reserving,
    Scanning { rows: u32, total: u32 },
    Checksumming { tiles: usize, total: usize },
    Syncing,
    Publishing,
    Preview,
    Ready,
}

impl Stage {
    /// 标题和控制台使用同一阶段说明；百分比只针对已有总数的扫描或校验阶段。
    pub(crate) fn description(self) -> String {
        match self {
            Self::Checking => "检查源文件和已有缓存".into(),
            Self::Reserving => "准备磁盘缓存空间".into(),
            Self::Scanning { rows, total } => {
                format!(
                    "扫描并生成金字塔：{rows}/{total} 行（{:.1}%）",
                    percent(rows as u64, total as u64)
                )
            }
            Self::Checksumming { tiles, total } => {
                format!(
                    "校验分块：{tiles}/{total}（{:.1}%）",
                    percent(tiles as u64, total as u64)
                )
            }
            Self::Syncing => "同步缓存到磁盘（等待系统 I/O）".into(),
            Self::Publishing => "同步索引并发布完整缓存".into(),
            Self::Preview => "读取预览，等待渲染线程接收".into(),
            Self::Ready => "缓存和预览已就绪".into(),
        }
    }
}

fn percent(done: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        done.min(total) as f64 * 100.0 / total as f64
    }
}

/// 行扫描不等待 UI；同一阶段最多每秒通知一次，阶段变化和该阶段完成立即通知。
/// 没有计时线程或空闲轮询；阻塞的系统 I/O 期间保持最后一个真实阶段。
pub(super) struct Reporter<F> {
    send: F,
    last: Option<(std::mem::Discriminant<Stage>, Instant)>,
}

impl<F: FnMut(Stage)> Reporter<F> {
    pub(super) fn new(send: F) -> Self {
        Self { send, last: None }
    }

    pub(super) fn report(&mut self, stage: Stage) {
        self.at(stage, Instant::now());
    }

    fn at(&mut self, stage: Stage, now: Instant) {
        let kind = std::mem::discriminant(&stage);
        let complete = match stage {
            Stage::Scanning { rows, total } => rows == total,
            Stage::Checksumming { tiles, total } => tiles == total,
            _ => false,
        };
        if complete
            || self.last.is_none_or(|(previous, at)| {
                kind != previous || now.saturating_duration_since(at) >= Duration::from_secs(1)
            })
        {
            (self.send)(stage);
            self.last = Some((kind, now));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_are_throttled_but_phase_changes_and_completion_are_immediate() {
        let mut updates = Vec::new();
        let now = Instant::now();
        let mut reporter = Reporter::new(|s| updates.push(s));
        reporter.at(Stage::Checking, now);
        reporter.at(Stage::Scanning { rows: 0, total: 10 }, now);
        for rows in 1..9 {
            reporter.at(
                Stage::Scanning { rows, total: 10 },
                now + Duration::from_millis(5),
            );
        }
        reporter.at(
            Stage::Scanning { rows: 9, total: 10 },
            now + Duration::from_secs(1),
        );
        reporter.at(
            Stage::Scanning {
                rows: 10,
                total: 10,
            },
            now + Duration::from_secs(1),
        );
        reporter.at(Stage::Syncing, now + Duration::from_secs(1));
        assert_eq!(updates.len(), 5);
        assert_eq!(updates[2], Stage::Scanning { rows: 9, total: 10 });
        assert_eq!(
            updates[3],
            Stage::Scanning {
                rows: 10,
                total: 10
            }
        );
        assert_eq!(updates[4], Stage::Syncing);
        assert!(
            Stage::Scanning { rows: 1, total: 4 }
                .description()
                .contains("25.0%")
        );
        assert_eq!(percent(u64::MAX, 0), 0.0);
        assert_eq!(percent(u64::MAX, u64::MAX), 100.0);
    }
}
