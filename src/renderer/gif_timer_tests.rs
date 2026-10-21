//! 隔离验证 Waitable Timer 与真实 Swap Chain 等待句柄；不改变正式渲染线程。
use super::*;
use std::time::Duration;
use windows::{
    Win32::{
        Foundation::{CloseHandle, WAIT_TIMEOUT},
        System::Threading::{
            CancelWaitableTimer, CreateWaitableTimerW, SetEvent, SetWaitableTimer,
        },
        UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW},
    },
    core::w,
};

struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: 该隐藏测试窗口由当前测试线程创建，Renderer 在窗口 guard 前析构。
        let _ = unsafe { DestroyWindow(self.0) };
    }
}
struct Timer(HANDLE);
impl Drop for Timer {
    fn drop(&mut self) {
        // SAFETY: 本测试独占 timer，等待结束后关闭，无其他线程保留句柄。
        let _ = unsafe { CloseHandle(self.0) };
    }
}
#[test]
#[ignore = "隔离 Windows/D3D11 Release 时钟诊断"]
fn release_gif_timer_waitset() -> Result<()> {
    let (shared, _wake) = crate::app::tests::test_shared()?;
    // SAFETY: STATIC 是系统窗口类，名称为静态UTF16；窗口仅由当前线程创建/销毁且不显示。
    let window = Window(unsafe {
        CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("Pixstride timer probe"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            64,
            64,
            None,
            None,
            None,
            None,
        )
    }?);
    let renderer = Renderer::new(window.0, &shared, Instant::now())?;
    // SAFETY: 无名称 synchronization timer，当前测试独占其生命周期，不使用 APC。
    let timer = Timer(unsafe { CreateWaitableTimerW(None, false, None) }?);
    let wake = HANDLE(shared.wake_event() as _);
    // SAFETY: 三个句柄存活覆盖等待；先置位工作通知验证优先处理 UI，而非等待动画。
    unsafe { SetEvent(wake) }?;
    // SAFETY: timer 有效，负数代表相对10ms，period=0只触发一次。
    unsafe { SetWaitableTimer(timer.0, &-100000, 0, None, None, false) }?;
    // SAFETY: wake/timer/SwapChain句柄均有效，最大等待100ms。
    assert_eq!(
        unsafe {
            WaitForMultipleObjects(&[wake, renderer.frame_latency_handle, timer.0], false, 100)
        },
        WAIT_OBJECT_0
    );
    // 单独消费 timer；真正静态空闲时必须像现有 Renderer 那样移除 frame-latency 项。
    let mut samples = Vec::new();
    for _ in 0..40 {
        let at = Instant::now();
        // SAFETY: 同一同步timer重新设置为一次10ms，当前线程拥有句柄且输出无外部引用。
        unsafe { SetWaitableTimer(timer.0, &-100000, 0, None, None, false) }?;
        // SAFETY: 等待同一个有效timer，无其他消费者；超时用于实验失败检测。
        assert_eq!(
            unsafe { WaitForMultipleObjects(&[wake, timer.0], false, 1000) },
            WAIT_EVENT(WAIT_OBJECT_0.0 + 1)
        );
        samples.push(at.elapsed().as_micros());
    }
    // SAFETY: 取消尚未到期的一次timer，随后没有任何周期源；测试不会读取失效句柄。
    unsafe {
        SetWaitableTimer(timer.0, &-1000000, 0, None, None, false)?;
        CancelWaitableTimer(timer.0)?;
    }
    let idle = Instant::now();
    // SAFETY: 两个有效句柄，100ms窗口检测是否有停止后的意外唤醒。
    assert_eq!(
        unsafe { WaitForMultipleObjects(&[wake, timer.0], false, 100) },
        WAIT_TIMEOUT
    );
    samples.sort_unstable();
    println!(
        "timer n={} p50_us={} p95_us={} p99_us={} max_us={} idle_wait_us={} idle_unexpected_wakes=0",
        samples.len(),
        samples[19],
        samples[37],
        samples[39],
        samples[39],
        idle.elapsed().as_micros()
    );
    assert!(idle.elapsed() >= Duration::from_millis(80));
    drop(renderer);
    drop(window);
    Ok(())
}
