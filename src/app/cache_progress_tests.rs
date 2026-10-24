//! 进度邮箱和实际 HWND 标题验证；不解码私人图片，也不启动 Renderer。
use super::*;
use crate::large::progress::Stage;
use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, GetWindowTextW};

#[test]
fn large_progress_uses_one_slot_restores_title_and_rejects_stale_updates() -> Result<()> {
    struct TestWindow(windows::Win32::Foundation::HWND);
    impl Drop for TestWindow {
        fn drop(&mut self) {
            // SAFETY: 测试独占隐藏 STATIC HWND，没有后台线程使用；在所属 UI 线程销毁。
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }
    // SAFETY: 使用系统 STATIC 类创建本线程的隐藏测试窗口，所有字符串在调用期间有效。
    let window = TestWindow(unsafe {
        CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("progress-test"),
            Default::default(),
            0,
            0,
            100,
            100,
            None,
            None,
            None,
            None,
        )
    }?);
    let title = || {
        let mut text = [0u16; 1024];
        // SAFETY: 自有存活 HWND，输出数组可写且长度由绑定传入。
        let n = unsafe { GetWindowTextW(window.0, &mut text) };
        String::from_utf16_lossy(&text[..n as usize])
    };
    let (shared, _wake) = tests::test_shared()?;
    let mut context = tests::window_context(Arc::clone(&shared));
    shared.set_image_generation(1);
    context.navigation.current_path = Some(PathBuf::from("first.png"));
    let sequence = shared.current_snapshot().sequence;
    shared.publish_large_progress(
        1,
        Stage::Scanning { rows: 1, total: 4 },
        Duration::from_secs(3),
    );
    shared.publish_large_progress(
        1,
        Stage::Checksumming { tiles: 2, total: 4 },
        Duration::from_secs(4),
    );
    process_large_progress(&mut context, window.0);
    assert!(title().contains("校验分块：2/4（50.0%）"));
    assert!(shared.large_progress.lock().unwrap().is_none());
    assert_eq!(shared.current_snapshot().sequence, sequence);
    shared.publish_large_progress(1, Stage::Scanning { rows: 1, total: 4 }, Duration::ZERO);
    shared.set_image_generation(2);
    context.navigation.current_path = Some(PathBuf::from("second.png"));
    set_window_title(window.0, "second.png");
    process_large_progress(&mut context, window.0);
    assert!(title().contains("second.png"));
    assert!(!title().contains("大图缓存"));
    shared.publish_large_progress(2, Stage::Syncing, Duration::ZERO);
    shared.publish_large_progress(1, Stage::Ready, Duration::ZERO);
    process_large_progress(&mut context, window.0);
    assert!(title().contains("同步缓存到磁盘"));
    shared.publish_large_progress(2, Stage::Ready, Duration::ZERO);
    process_large_progress(&mut context, window.0);
    assert!(title().contains("second.png"));
    assert!(!title().contains("大图缓存"));
    shared.request_shutdown();
    shared.publish_large_progress(2, Stage::Scanning { rows: 0, total: 1 }, Duration::ZERO);
    assert!(shared.large_progress.lock().unwrap().is_none());
    Ok(())
}
