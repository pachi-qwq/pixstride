//! 可注入时间的播放状态；最小化、手动暂停和 Session 失效不依赖真实 Sleep。
/// 单调虚拟时间上的顺序播放状态；所有时间单位为毫秒。
pub struct Clock {
    delays: Vec<u64>,
    pub frame: usize,
    loops: Option<u32>,
    completed: u32,
    deadline: u64,
    remaining: u64,
    paused: bool,
    minimized: bool,
    valid: bool,
}
impl Clock {
    /// delays 来自受检帧；缺失扩展播放一次，0 无限循环，正数表示首次播放后的重复次数。
    pub fn new(delays: Vec<u64>, loops: Option<u16>) -> Self {
        // 构造只接受经过 delay_ms 归一化的非空帧表，防止空索引或零时间循环。
        assert!(!delays.is_empty() && delays.iter().all(|d| *d > 0));
        let deadline = delays[0];
        Self {
            delays,
            frame: 0,
            loops: match loops {
                Some(0) => None,
                Some(n) => Some(u32::from(n) + 1),
                None => Some(1),
            },
            completed: 0,
            deadline,
            remaining: 0,
            paused: false,
            minimized: false,
            valid: true,
        }
    }
    /// 返回下一截止时间；暂停、最小化、播放结束或取消后均不安排 timer。
    pub fn due(&self) -> Option<u64> {
        (self.valid && !self.paused && !self.minimized).then_some(self.deadline)
    }
    /// 分开保存手动暂停和最小化，恢复窗口不会取消手动暂停。
    pub fn suspend(&mut self, now: u64, manual: bool, minimized: bool) {
        let was = self.paused || self.minimized;
        let next = manual || minimized;
        if !was && next {
            self.remaining = self.deadline.saturating_sub(now);
        }
        if was && !next {
            self.deadline = now.saturating_add(self.remaining);
        }
        self.paused = manual;
        self.minimized = minimized;
    }
    /// 迟到也按顺序返回帧，调用者可以跳过显示，不能跳过影响后续画面的合成。
    pub fn advance(&mut self, now: u64) -> Option<usize> {
        if self.due().is_none_or(|d| now < d) {
            return None;
        }
        if self.frame + 1 == self.delays.len() {
            self.completed = self.completed.saturating_add(1);
            if self.loops.is_some_and(|n| self.completed >= n) {
                self.valid = false;
                return None;
            }
            self.frame = 0;
        } else {
            self.frame += 1;
        }
        self.deadline = self.deadline.saturating_add(self.delays[self.frame]);
        Some(self.frame)
    }
    /// 使旧 Session 的时钟永久失效。
    pub fn cancel(&mut self) {
        self.valid = false;
    }
}
/// 实验策略：0/10ms 统一限制为 20ms；这是候选产品策略，不是 GIF 格式要求。
pub fn delay_ms(hundredths: u32) -> u64 {
    u64::from(hundredths).saturating_mul(10).max(20)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn intervals_pause_minimize_and_cancel() {
        let mut c = Clock::new(vec![20, 40, 100], None);
        assert_eq!(c.advance(19), None);
        assert_eq!(c.advance(20), Some(1));
        c.suspend(30, true, false);
        assert_eq!(c.due(), None);
        c.suspend(500, true, true);
        c.suspend(600, false, true);
        assert_eq!(c.due(), None);
        c.suspend(1000, false, false);
        assert_eq!(c.due(), Some(1030));
        assert_eq!(c.advance(1030), Some(2));
        assert_eq!(c.advance(1130), None);
        assert_eq!(c.due(), None);
        let mut c = Clock::new(vec![20], Some(0));
        assert_eq!(c.advance(20), Some(0));
        c.cancel();
        assert_eq!(c.advance(1000), None);
    }
    #[test]
    fn late_finite_loop_keeps_composition_order() {
        let mut c = Clock::new(vec![20, 20, 20], Some(1));
        let mut processed = Vec::new();
        while let Some(i) = c.advance(1000) {
            processed.push(i);
        }
        assert_eq!(processed, [1, 2, 0, 1, 2]);
        assert_eq!(c.due(), None);
        assert_eq!([0, 1, 2, 10].map(delay_ms), [20, 20, 20, 100]);
    }
}
