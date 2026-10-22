//! Render Thread 独占的单次同步计时器；暂停/空闲时取消并移出等待集。
use anyhow::Result;
use std::time::{Duration, Instant};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::Threading::{CancelWaitableTimer, CreateWaitableTimerW, SetWaitableTimer},
};

pub(super) struct Timer {
    handle: HANDLE,
    armed: Option<Duration>,
}
impl Timer {
    pub(super) fn new() -> Result<Self> {
        // SAFETY: 无名 synchronization timer，仅由当前 Render Thread 持有和关闭。
        Ok(Self {
            handle: unsafe { CreateWaitableTimerW(None, false, None) }?,
            armed: None,
        })
    }
    pub(super) fn set(
        &mut self,
        epoch: Instant,
        deadline: Option<Duration>,
    ) -> Result<Option<HANDLE>> {
        if self.armed != deadline {
            if let Some(at) = deadline {
                let left = at.saturating_sub(epoch.elapsed());
                let ticks = left.as_nanos().div_ceil(100).clamp(1, i64::MAX as u128) as i64;
                let relative = -ticks;
                // SAFETY: relative 在调用期间有效，负值为相对时间；period=0，不恢复睡眠中的系统。
                unsafe { SetWaitableTimer(self.handle, &relative, 0, None, None, false) }?;
            } else {
                // SAFETY: 有效且独占的 timer，取消后不再将其加入等待集。
                unsafe { CancelWaitableTimer(self.handle) }?;
            }
            self.armed = deadline;
        }
        Ok(deadline.map(|_| self.handle))
    }
    pub(super) fn fired(&mut self) {
        self.armed = None;
    }
}
impl Drop for Timer {
    fn drop(&mut self) {
        // SAFETY: Render Thread 不再等待或访问此句柄，无 APC 回调。
        let _ = unsafe { CloseHandle(self.handle) };
    }
}
