//! 通过真实 D3D11 SRV、staging 回读和 GPU 缓存验证生产 GIF poster。
use super::*;
use crate::image::{
    ComMta, ImageContent,
    gif_tests::{Directory, decode, sample},
};
use windows::{
    Win32::{
        Graphics::Direct3D11::{
            D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_USAGE_STAGING,
        },
        UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW},
    },
    core::w,
};

struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: 当前线程创建 HWND，Renderer 在 guard 前释放，无跨线程使用者。
        let _ = unsafe { DestroyWindow(self.0) };
    }
}
#[test]
#[ignore = "Release GIF GPU 像素与缓存诊断，需要 Windows D3D11"]
fn release_gif_poster_gpu() -> Result<()> {
    let _com = ComMta::initialize()?;
    let dir = Directory::new()?;
    let path = dir.0.join("poster.gif");
    std::fs::write(&path, sample(true))?;
    let image = Arc::new(decode(&path)?);
    let (shared, _event) = crate::app::tests::test_shared()?;
    // SAFETY: 系统 STATIC 类名称和标题为静态 UTF16，窗口由本线程独占且不显示。
    let window = Window(unsafe {
        CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("GIF poster GPU probe"),
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
    let mut r = Renderer::new(window.0, &shared, Instant::now())?;
    r.upload_image(Arc::clone(&image), true, &shared)?;
    let view = r.image_cache[&image.source].0.clone();
    let cost = r.gpu_cache_bytes;
    r.upload_image(Arc::clone(&image), true, &shared)?;
    assert_eq!(view.as_raw(), r.image_cache[&image.source].0.as_raw());
    assert_eq!(cost, r.gpu_cache_bytes);
    let mut wrong_content = image.source.clone();
    wrong_content.content = ImageContent::Static;
    assert!(
        !r.image_cache.contains_key(&wrong_content),
        "poster 输出策略必须参与 GPU 身份"
    );
    // SAFETY: view 是本线程持有的有效 SRV，返回的资源引用由局部 texture 拥有。
    let texture: ID3D11Texture2D = unsafe { view.GetResource() }?.cast()?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: texture 存活，desc 为可写输出。
    unsafe { texture.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    let mut staging = None;
    // SAFETY: staging 与源尺寸/格式一致，初始化无外部数据。
    unsafe { r.device.CreateTexture2D(&desc, None, Some(&mut staging)) }?;
    let staging = staging.context("未返回 staging")?;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: 当前测试线程独占 Context，复制同尺寸同格式 Texture，然后只读 Map。
    unsafe {
        r.context.CopyResource(&staging, &texture);
        r.context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
    }
    let mut pixels = Vec::new();
    for y in 0..3 {
        // SAFETY: Map 成功，RowPitch 至少为 4×4 字节，y 仅取有效的三行。
        pixels.extend_from_slice(unsafe {
            std::slice::from_raw_parts(
                (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                16,
            )
        });
    }
    // SAFETY: 所有行借用均已结束，配对 Unmap；随后不访问 mapped 指针。
    unsafe { r.context.Unmap(&staging, 0) };
    let mut oracle = vec![0; 48];
    oracle[20..24].copy_from_slice(&[0, 0, 255, 255]);
    assert_eq!(pixels, oracle);
    assert_eq!(fit_viewport(4, 3, 1000, 600, ViewMode::Fit), (800.0, 600.0));
    assert_eq!(
        fit_viewport(4, 3, 1000, 600, ViewMode::NativePixels),
        (4.0, 3.0)
    );
    let big = dir.0.join("budget.gif");
    std::fs::write(
        &big,
        crate::image::gif_tests::generated([2048, 1024], [0, 0, 2048, 1024], true, false, false),
    )?;
    let large = Arc::new(decode(&big)?);
    r.upload_image(Arc::clone(&large), true, &shared)?;
    let large_view = r.image_cache[&large.source].0.as_raw();
    let peak = r.gpu_cache_bytes;
    r.upload_image(Arc::clone(&large), true, &shared)?;
    assert_eq!(large_view, r.image_cache[&large.source].0.as_raw());
    assert_eq!(peak, 8 * 1024 * 1024 + 48);
    assert_eq!(peak, r.gpu_cache_bytes);
    println!(
        "GIF GPU poster: 12 pixels exact, SRV reused, peak_payload_bytes={peak} (8 MiB poster plus 48 bytes); Fit/100% passed (not displayed-frame timing)"
    );
    // SAFETY: 解除当前线程的 Context 绑定，再释放 Renderer 和隐藏 HWND。
    unsafe { r.context.ClearState() };
    drop(r);
    Ok(())
}
