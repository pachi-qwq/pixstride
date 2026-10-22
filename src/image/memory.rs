//! 共同 CPU 像素额度；计费跟随分配所有者，缓存淘汰和 Arc 克隆不改变真实存活量。

use anyhow::{Result, ensure};
use std::sync::{Arc, Mutex};

/// 动画像素从普通 CPU 总额度中使用子额度，不建立额外的全局池。
pub(crate) const ANIMATION_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
/// 同一个 ImageLoader 与其动画控制器共享的账本；克隆只共享状态，不复制额度。
pub(crate) struct CpuMemory(Arc<Inner>);
#[derive(Debug)]
struct Inner {
    total: u64,
    animation_limit: u64,
    state: Mutex<Usage>,
}
/// 应用持有的像素容量；不等于进程内存，也不覆盖 WIC 内部分配。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Usage {
    pub(crate) total: u64,
    pub(crate) animation: u64,
    pub(crate) peak: u64,
    pub(crate) animation_peak: u64,
}

impl CpuMemory {
    /// 总额度由既有 ImageBudget 提供，动画最多使用其中 256 MiB。
    pub(crate) fn new(total: u64) -> Self {
        Self(Arc::new(Inner {
            total,
            animation_limit: total.min(ANIMATION_LIMIT),
            state: Mutex::new(Usage::default()),
        }))
    }
    /// 返回当前容量及历史峰值，不包含 WIC、allocator 元数据、线程栈或有界控制元数据。
    pub(crate) fn usage(&self) -> Usage {
        *self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// 返回共同总额度中的动画上限，不是额外分配量。
    pub(crate) fn animation_limit(&self) -> u64 {
        self.0.animation_limit
    }
    /// 在分配前原子预留；失败不改变账本，返回的唯一票据负责最终归还。
    pub(crate) fn reserve(&self, bytes: u64, animation: bool) -> Result<Charge> {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure!(
            bytes <= self.0.total.saturating_sub(state.total),
            "共同 CPU 像素额度不足"
        );
        if animation {
            ensure!(
                bytes <= self.0.animation_limit.saturating_sub(state.animation),
                "动画 CPU 子额度不足"
            );
        }
        state.total += bytes;
        state.animation += if animation { bytes } else { 0 };
        state.peak = state.peak.max(state.total);
        state.animation_peak = state.animation_peak.max(state.animation);
        Ok(Charge {
            pool: self.clone(),
            bytes,
            animation,
        })
    }
}

/// 非 Clone 的计费票据；必须放在像素字段之后，保证先释放 Vec 再归还额度。
#[derive(Debug)]
pub(crate) struct Charge {
    pool: CpuMemory,
    bytes: u64,
    animation: bool,
}
impl Drop for Charge {
    fn drop(&mut self) {
        let mut state = self
            .pool
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.total -= self.bytes;
        if self.animation {
            state.animation -= self.bytes;
        }
    }
}

/// 先记账再分配；allocator 报告的额外 Vec capacity 也必须计费。
pub(crate) fn allocate(bytes: usize, mut charge: Charge) -> Result<(Vec<u8>, Charge)> {
    ensure!(charge.bytes == bytes as u64, "像素分配与预留字节数不符");
    let mut data = Vec::new();
    data.try_reserve_exact(bytes)?;
    let extra = data.capacity() as u64 - charge.bytes;
    if extra > 0 {
        let mut additional = charge.pool.reserve(extra, charge.animation)?;
        charge.bytes += additional.bytes;
        // 把新增票据合入原票据；只转移计费所有权，不提前归还。
        additional.bytes = 0;
    }
    data.resize(bytes, 0);
    Ok((data, charge))
}

/// 动画工作缓冲。字段顺序保证像素先释放、额度后归还。
pub(crate) struct Pixels {
    pub(crate) data: Vec<u8>,
    _charge: Charge,
}
impl Pixels {
    /// 分配清零的动画缓冲；申请或额度失败通过 Result 返回，不绕过共同总额度。
    pub(crate) fn new(pool: &CpuMemory, bytes: usize) -> Result<Self> {
        let (data, charge) = allocate(bytes, pool.reserve(bytes as u64, true)?)?;
        Ok(Self {
            data,
            _charge: charge,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_pool_counts_last_owner_and_rejects_overlap() -> Result<()> {
        let pool = CpuMemory::new(32);
        let (pixels, charge) = allocate(20, pool.reserve(20, false)?)?;
        let owner = Arc::new((pixels, charge));
        let other = Arc::clone(&owner);
        assert!(pool.reserve(13, true).is_err());
        let animation = Pixels::new(&pool, 12)?;
        assert_eq!(pool.usage().total, 32);
        drop(owner);
        assert_eq!(pool.usage().total, 32);
        drop(other);
        assert_eq!(pool.usage().total, 12);
        drop(animation);
        assert_eq!(pool.usage().total, 0);
        Ok(())
    }
    #[test]
    fn animation_subquota_is_part_of_512_mib_not_added_to_it() -> Result<()> {
        let mib = 1024 * 1024;
        let pool = CpuMemory::new(512 * mib);
        // 此例只验证预留拒绝，不实际分配数百 MiB；性能诊断另按真实 Vec 计费。
        assert!(pool.reserve(256 * mib + 1, true).is_err());
        let ordinary = pool.reserve(300 * mib, false)?;
        assert!(pool.reserve(220 * mib, true).is_err());
        let animation = pool.reserve(200 * mib, true)?;
        assert_eq!(pool.usage().total, 500 * mib);
        drop(ordinary);
        drop(animation);
        assert_eq!(pool.usage().total, 0);
        Ok(())
    }
}
