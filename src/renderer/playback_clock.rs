//! 纯播放时钟：正常播放沿单调时间轴推进；Underflow 重建截止时间，暂停保存余量。

use std::time::Duration;

/// 浏览器兼容策略独立于原始 GIF 元数据；0/10 ms 用 100 ms，其余按百分之一秒换算。
pub(super) fn delay(raw_cs: u16) -> Duration {
    Duration::from_millis(if raw_cs <= 1 {
        100
    } else {
        u64::from(raw_cs) * 10
    })
}

/// 缺失扩展播放一轮，0 无限，正整数表示首次播放后的重复次数。
pub(super) fn passes(raw_loop: Option<u16>) -> Option<u32> {
    match raw_loop {
        Some(0) => None,
        Some(n) => Some(u32::from(n) + 1),
        None => Some(1),
    }
}

/// deadline=None 且未暂停/完成时表示 Underflow，只等新帧，不安排空转计时。
pub(super) struct Clock {
    deadline: Option<Duration>,
    late_deadline: Option<Duration>,
    remaining: Duration,
    paused: bool,
    final_frame: bool,
    finished: bool,
}
impl Clock {
    pub(super) fn new(now: Duration, raw_delay: u16) -> Self {
        Self {
            deadline: Some(now.saturating_add(delay(raw_delay))),
            late_deadline: None,
            remaining: delay(raw_delay),
            paused: false,
            final_frame: false,
            finished: false,
        }
    }
    /// 多种暂停原因先由控制端合并；重复设置不消耗余量。
    pub(super) fn pause(&mut self, now: Duration, paused: bool) {
        if paused == self.paused || self.finished {
            return;
        }
        if paused {
            self.remaining = self
                .deadline
                .take()
                .map(|d| d.saturating_sub(now))
                .unwrap_or_default();
        } else {
            self.deadline = Some(now.saturating_add(self.remaining));
        }
        self.late_deadline = None;
        self.paused = paused;
    }
    pub(super) fn deadline(&self) -> Option<Duration> {
        self.deadline.filter(|_| !self.paused && !self.finished)
    }
    pub(super) fn due(&self, now: Duration) -> bool {
        !self.paused && !self.finished && self.deadline.is_none_or(|d| now >= d)
    }
    /// 到期而无帧时只统计一次；下一份数据由 Worker 事件唤醒。
    pub(super) fn underflow(&mut self) -> bool {
        let previous = self.deadline.take();
        if previous.is_some() {
            self.late_deadline = previous;
        }
        previous.is_some()
    }
    pub(super) fn expected_deadline(&self) -> Option<Duration> {
        self.deadline.or(self.late_deadline)
    }
    pub(super) fn final_due(&mut self, now: Duration) -> bool {
        if self.final_frame && self.due(now) {
            self.finished = true;
            self.deadline = None;
            return true;
        }
        false
    }
    /// Present 返回是应用侧提交边界，不代表实际显示完成。
    pub(super) fn presented(&mut self, now: Duration, raw_delay: u16, final_frame: bool) {
        self.remaining = delay(raw_delay);
        // 普通唤醒抖动不逐帧累加。若已落后整段展示时间则重新起算，最多消费一帧后再等待，
        // 不以零间隔连续提交追赶。最后一帧始终从本次提交起保留完整 Delay。
        let next = self.deadline.map(|at| at.saturating_add(self.remaining));
        self.deadline = if !final_frame && !self.paused {
            next.filter(|at| *at > now)
                .or_else(|| Some(now.saturating_add(self.remaining)))
        } else {
            Some(now.saturating_add(self.remaining))
        };
        self.late_deadline = None;
        self.final_frame = final_frame;
    }
    pub(super) fn finished(&self) -> bool {
        self.finished
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }
    #[test]
    fn delay_and_loop_mapping_preserve_n_plus_one() {
        assert_eq!(
            [0, 1, 2, 3, 10].map(|n| delay(n).as_millis()),
            [100, 100, 20, 30, 100]
        );
        assert_eq!(passes(None), Some(1));
        assert_eq!(passes(Some(0)), None);
        assert_eq!(passes(Some(1)), Some(2));
        assert_eq!(passes(Some(u16::MAX)), Some(65536));
    }
    #[test]
    fn pause_saves_remaining_and_does_not_catch_up() {
        let mut c = Clock::new(ms(0), 10);
        c.pause(ms(40), true);
        c.pause(ms(90), true);
        assert!(!c.due(ms(5000)));
        assert_eq!(c.deadline(), None);
        c.pause(ms(5000), false);
        assert_eq!(c.deadline(), Some(ms(5060)));
        assert!(!c.due(ms(5059)));
        assert!(c.due(ms(5060)));
        assert!(c.underflow());
        assert!(!c.underflow());
        assert_eq!(c.deadline(), None);
        c.presented(ms(8000), 3, false);
        assert_eq!(c.deadline(), Some(ms(8030)));
    }
    #[test]
    fn last_frame_holds_its_delay_then_stops_all_deadlines() {
        let mut c = Clock::new(ms(0), 2);
        c.presented(ms(30), 10, true);
        assert!(!c.final_due(ms(129)));
        c.pause(ms(80), true);
        c.pause(ms(500), false);
        assert_eq!(c.deadline(), Some(ms(550)));
        assert!(c.final_due(ms(550)));
        assert!(c.finished());
        assert_eq!(c.deadline(), None);
        assert!(!c.due(ms(10000)));
    }
    #[test]
    fn underflow_has_no_timer_and_pause_reasons_preserve_user_intent() {
        let mut c = Clock::new(ms(0), 0);
        assert!(!c.due(ms(99)));
        assert!(c.due(ms(100)));
        assert!(c.underflow());
        for _ in 0..100 {
            assert_eq!(c.deadline(), None);
            assert!(!c.underflow());
        }
        let user_pause = true;
        for minimized in [false, true, false] {
            c.pause(ms(200), user_pause || minimized);
            assert!(!c.due(ms(10000)));
            assert_eq!(c.deadline(), None);
        }
        c.pause(ms(10000), false);
        assert!(c.due(ms(10000)));
        c.presented(ms(10000), 2, false);
        assert_eq!(c.deadline(), Some(ms(10020)));
    }
    #[test]
    fn normal_jitter_does_not_accumulate_but_a_whole_late_interval_rebases() {
        let mut c = Clock::new(ms(0), 3);
        c.presented(ms(35), 3, false);
        assert_eq!(c.deadline(), Some(ms(60)));
        c.presented(ms(65), 3, false);
        assert_eq!(c.deadline(), Some(ms(90)));
        c.presented(ms(180), 3, false);
        assert_eq!(c.deadline(), Some(ms(210)));
        assert!(!c.due(ms(180)));
        c.presented(ms(215), 3, true);
        assert_eq!(c.deadline(), Some(ms(245)));
    }
}
