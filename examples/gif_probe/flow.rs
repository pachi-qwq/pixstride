//! 独立实验的计费与可取消 Credit；没有连接 Viewer 的正式缓存。
use anyhow::{Result, ensure};
use std::sync::{Arc, Condvar, Mutex};
const MIB: usize = 1024 * 1024;
/// 为不透明的 WIC/allocator 临时开销保留的实验余量；计费预留不能强制限制系统 codec。
pub const CODEC_ALLOWANCE: usize = 64 * MIB;
/// 五份画布等价像素的合计上限，剩余 64 MiB 不借给像素输出。
pub const PIXEL_LIMIT: usize = 192 * MIB;
#[derive(Default)]
struct State {
    ordinary_cpu: usize,
    animation_cpu: usize,
    ordinary_gpu: usize,
    animation_gpu: usize,
    credits: usize,
    session: u64,
    paused: bool,
    stopped: bool,
    peak: usize,
    peak_credits: usize,
    active: bool,
}
/// Mutex 只保护计费和控制状态；等待 Credit 时 Condvar 释放锁，不阻塞消费者释放像素。
pub struct Gate {
    state: Mutex<State>,
    changed: Condvar,
}
#[derive(Clone, Copy)]
/// 动画子额度与普通缓存共用 512 MiB 池；GPU 只进行预留计费，不分配真实纹理。
pub enum Class {
    OrdinaryCpu,
    AnimationCpu,
    OrdinaryGpu,
    AnimationGpu,
}
/// 字节预留的 RAII 所有者，最后释放时归还额度。
pub struct Charge {
    gate: Arc<Gate>,
    class: Class,
    bytes: usize,
}
/// 一个输出名额；包括消费者仍然持有的像素，不能只数通道里的元素。
pub struct Credit {
    gate: Arc<Gate>,
    session: u64,
}
impl Gate {
    /// 创建尚未启动的实验控制器。
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        })
    }
    /// 必须先停止并 join 旧 Worker、归还输出，才能开始新 Session。
    pub fn start(&self, session: u64) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        ensure!(
            !s.active && s.animation_cpu == 0 && s.credits == 0,
            "旧 Session 尚未释放"
        );
        s.session = session;
        s.stopped = false;
        s.paused = false;
        s.active = true;
        Ok(())
    }
    /// 分配前检查池与子额度；失败不改变计数。
    pub fn charge(self: &Arc<Self>, class: Class, bytes: usize) -> Result<Charge> {
        let mut s = self.state.lock().unwrap();
        let (own, total, limit) = match class {
            Class::OrdinaryCpu => (s.ordinary_cpu, s.ordinary_cpu + s.animation_cpu, 512 * MIB),
            Class::AnimationCpu => (s.animation_cpu, s.ordinary_cpu + s.animation_cpu, 256 * MIB),
            Class::OrdinaryGpu => (s.ordinary_gpu, s.ordinary_gpu + s.animation_gpu, 512 * MIB),
            Class::AnimationGpu => (s.animation_gpu, s.ordinary_gpu + s.animation_gpu, 128 * MIB),
        };
        ensure!(
            own.checked_add(bytes).is_some_and(|n| n <= limit)
                && total.checked_add(bytes).is_some_and(|n| n <= 512 * MIB),
            "资源预算不足"
        );
        match class {
            Class::OrdinaryCpu => s.ordinary_cpu += bytes,
            Class::AnimationCpu => s.animation_cpu += bytes,
            Class::OrdinaryGpu => s.ordinary_gpu += bytes,
            Class::AnimationGpu => s.animation_gpu += bytes,
        }
        s.peak = s.peak.max(s.animation_cpu);
        Ok(Charge {
            gate: Arc::clone(self),
            class,
            bytes,
        })
    }
    /// 可取消的阻塞等待；暂停或两个名额占满时不继续解码，失效时返回 None。
    pub fn credit(self: &Arc<Self>, session: u64) -> Option<Credit> {
        let mut s = self.state.lock().unwrap();
        while s.active && !s.stopped && s.session == session && (s.paused || s.credits == 2) {
            s = self.changed.wait(s).unwrap();
        }
        if !s.active || s.stopped || s.session != session {
            return None;
        }
        s.credits += 1;
        s.peak_credits = s.peak_credits.max(s.credits);
        Some(Credit {
            gate: Arc::clone(self),
            session,
        })
    }
    /// 同步 API 返回后重新确认是否仍允许发布输出。
    pub fn valid(&self, session: u64) -> bool {
        let s = self.state.lock().unwrap();
        s.active && !s.stopped && s.session == session
    }
    /// 暂停生产，不主动释放消费者持有的像素。
    pub fn pause(&self, pause: bool) {
        self.state.lock().unwrap().paused = pause;
        self.changed.notify_all();
    }
    /// 唤醒所有等待者；同步 WIC 调用仍需等待其返回后才能 join。
    pub fn stop(&self) {
        let mut s = self.state.lock().unwrap();
        s.stopped = true;
        s.active = false;
        self.changed.notify_all();
    }
    /// 只使旧 Session 失效，不启动另一个 Worker。
    pub fn switch(&self, session: u64) {
        self.state.lock().unwrap().session = session;
        self.changed.notify_all();
    }
    /// 返回动画 CPU 当前字节、未归还名额及两者峰值。
    pub fn stats(&self) -> (usize, usize, usize, usize) {
        let s = self.state.lock().unwrap();
        (s.animation_cpu, s.credits, s.peak, s.peak_credits)
    }
}
impl Drop for Charge {
    fn drop(&mut self) {
        let mut s = self.gate.state.lock().unwrap();
        match self.class {
            Class::OrdinaryCpu => s.ordinary_cpu -= self.bytes,
            Class::AnimationCpu => s.animation_cpu -= self.bytes,
            Class::OrdinaryGpu => s.ordinary_gpu -= self.bytes,
            Class::AnimationGpu => s.animation_gpu -= self.bytes,
        }
        self.gate.changed.notify_all();
    }
}
impl Drop for Credit {
    fn drop(&mut self) {
        self.gate.state.lock().unwrap().credits -= 1;
        self.gate.changed.notify_all();
    }
}
/// Arc 克隆不增加计费；最后一个像素所有者释放时归还字节及 Credit。
pub struct Output {
    pub pixels: Vec<u8>,
    pub session: u64,
    pub frame_index: u32,
    pub delay_ms: u64,
    _charge: Charge,
    _credit: Credit,
}
impl Output {
    /// 先计费再复制画布；Arc 的最后一个引用同时归还像素字节与输出名额。
    pub fn copy(
        gate: &Arc<Gate>,
        credit: Credit,
        pixels: &[u8],
        frame_index: u32,
        delay_ms: u64,
    ) -> Result<Arc<Self>> {
        let charge = gate.charge(Class::AnimationCpu, pixels.len())?;
        let mut owned = Vec::new();
        owned.try_reserve_exact(pixels.len())?;
        owned.extend_from_slice(pixels);
        Ok(Arc::new(Self {
            session: credit.session,
            frame_index,
            delay_ms,
            pixels: owned,
            _charge: charge,
            _credit: credit,
        }))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    #[test]
    fn shared_pools_and_last_reference() -> Result<()> {
        let g = Gate::new();
        g.start(1)?;
        assert!(g.start(2).is_err());
        let a = g.charge(Class::OrdinaryCpu, 300 * MIB)?;
        assert!(g.charge(Class::AnimationCpu, 220 * MIB).is_err());
        drop(a);
        let b = g.charge(Class::OrdinaryGpu, 400 * MIB)?;
        assert!(g.charge(Class::AnimationGpu, 128 * MIB).is_err());
        drop(b);
        let b = g.charge(Class::AnimationGpu, 128 * MIB)?;
        assert!(g.charge(Class::AnimationGpu, 1).is_err());
        drop(b);
        let out = Output::copy(&g, g.credit(1).unwrap(), &[0; 32], 0, 20)?;
        let other = Arc::clone(&out);
        drop(out);
        assert_eq!(g.stats().0, 32);
        drop(other);
        assert_eq!(g.stats().0, 0);
        assert_eq!(g.stats().1, 0);
        Ok(())
    }
    #[test]
    fn stop_switch_pause_wake_credit_waiter() -> Result<()> {
        for scenario in ["stop", "switch", "pause"] {
            let g = Gate::new();
            g.start(1)?;
            let first = g.credit(1).unwrap();
            let mut second = Some(g.credit(1).unwrap());
            let (tx, rx) = mpsc::sync_channel(1);
            let clone = Arc::clone(&g);
            let worker = std::thread::spawn(move || tx.send(clone.credit(1).is_some()).unwrap());
            assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
            let at = Instant::now();
            match scenario {
                "switch" => g.switch(2),
                "pause" => {
                    g.pause(true);
                    second.take();
                    assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
                    g.pause(false);
                }
                _ => g.stop(),
            }
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(2))?,
                scenario == "pause"
            );
            worker.join().unwrap();
            assert_eq!(g.stats().3, 2);
            println!(
                "credit scenario={scenario} unblock_us={}",
                at.elapsed().as_micros()
            );
            drop(first);
        }
        Ok(())
    }

    #[test]
    fn cancellation_during_nonpreemptible_call_drops_result() -> Result<()> {
        let g = Gate::new();
        g.start(1)?;
        let (entered, signal) = mpsc::sync_channel(0);
        let (resume, release) = mpsc::sync_channel(0);
        let worker_gate = Arc::clone(&g);
        let worker = std::thread::spawn(move || {
            let _credit = worker_gate.credit(1).unwrap();
            entered.send(()).unwrap();
            // 用屏障模拟不可抢占调用；不把它的阻塞时间当作真实 WIC 延迟。
            release.recv().unwrap();
            worker_gate.valid(1)
        });
        signal.recv_timeout(Duration::from_secs(2))?;
        g.stop();
        assert!(g.start(2).is_err(), "旧调用仍持有输出名额");
        resume.send(())?;
        assert!(!worker.join().unwrap());
        assert_eq!(g.stats().1, 0);
        g.start(2)?;
        g.stop();
        Ok(())
    }
}
