//! 用真实 D3D11 纹理回读验证方向像素、GPU 复用及文件修订隔离；不驱动原生 GUI。

use super::*;
use crate::image::{
    ComMta,
    orientation_tests::{Fixture, decode, expected},
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
        // SAFETY: 窗口由本线程创建，Renderer 已在 guard 之前释放，没有其他使用者。
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

/// Map 成功后每条路径均先 Unmap；读回的是 GPU 纹理而非源 Vec。
fn readback(renderer: &Renderer, view: &ID3D11ShaderResourceView) -> Result<Vec<u8>> {
    // SAFETY: SRV 是本线程 image_cache 持有的有效 COM 引用，输出拥有资源引用。
    let source: ID3D11Texture2D = unsafe { view.GetResource() }?.cast()?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: source 存活，desc 为有效可写输出。
    unsafe { source.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    let mut staging = None;
    // SAFETY: staging 尺寸和格式与源一致，创建时无初始数据，输出由本函数独占。
    unsafe {
        renderer
            .device
            .CreateTexture2D(&desc, None, Some(&mut staging))
    }?;
    let staging = staging.context("未返回 staging Texture")?;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: 源和目标为同尺寸同格式纹理；Immediate Context 仅由当前测试线程调用。
    unsafe {
        renderer.context.CopyResource(&staging, &source);
        renderer
            .context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
    }
    let mut pixels = Vec::new();
    for y in 0..desc.Height {
        // SAFETY: Map 已成功，D3D11 保证每行至少 Width×4 字节；严格按 RowPitch 逐行读取。
        let row = unsafe {
            std::slice::from_raw_parts(
                (mapped.pData as *const u8).add(y as usize * mapped.RowPitch as usize),
                desc.Width as usize * 4,
            )
        };
        pixels.extend_from_slice(row);
    }
    // SAFETY: Map 成功且所有切片借用均已结束，在 staging 析构之前配对释放。
    unsafe { renderer.context.Unmap(&staging, 0) };
    Ok(pixels)
}

#[test]
#[ignore = "Windows Release 专项：实际 GPU 回读与缓存，不冒充人工 GUI 验收"]
fn release_exif_gpu_cache_and_layout() -> Result<()> {
    let _com = ComMta::initialize()?;
    let fixture = Fixture::new()?;
    let reference = decode(&fixture.write("reference-none.jpg", None)?)?;
    let (shared, _wake) = crate::app::tests::test_shared()?;
    // SAFETY: STATIC 为系统窗口类，名称是静态 UTF16；隐藏窗口在本线程创建并由 guard 销毁。
    let window = Window(unsafe {
        CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("Pixstride EXIF probe"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            128,
            96,
            None,
            None,
            None,
            None,
        )
    }?);
    let mut renderer = Renderer::new(window.0, &shared, Instant::now())?;
    let mut previous = None;
    for orientation in 1..=8 {
        let path = fixture.write("gpu-selected.jpg", Some((3, orientation)))?;
        // 内容和长度变化，保证 SourceIdentity 不依赖修改时间的精度。
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(&vec![0; orientation as usize])?;
        let image = Arc::new(decode(&path)?);
        if let Some(key) = previous {
            assert_ne!(key, image.source);
        }
        renderer.upload_image(Arc::clone(&image), true, &shared)?;
        let view = renderer.image_cache[&image.source].0.clone();
        let cost = renderer.gpu_cache_bytes;
        renderer.upload_image(Arc::clone(&image), true, &shared)?;
        assert_eq!(
            view.as_raw(),
            renderer.image_cache[&image.source].0.as_raw()
        );
        assert_eq!(renderer.gpu_cache_bytes, cost, "命中不得重复上传或计费");
        assert_eq!(renderer.current_image.as_ref(), Some(&image.source));
        let pixels = readback(&renderer, &view)?;
        let (w, h, oracle) = expected(&reference, orientation);
        assert_eq!(pixels, oracle, "orientation={orientation} GPU 内容不符");
        assert_eq!(renderer.current_dimensions, (w, h));
        assert_eq!(
            fit_viewport(w, h, 1000, 600, ViewMode::NativePixels),
            (w as f32, h as f32)
        );
        let (fw, fh) = fit_viewport(w, h, 1000, 600, ViewMode::Fit);
        assert!(fw <= 1000.001 && fh <= 600.001);
        assert!((fw / fh - w as f32 / h as f32).abs() < 0.0001);
        assert!((fw - 1000.0).abs() < 0.001 || (fh - 600.0).abs() < 0.001);
        println!(
            "EXIF GPU orientation={orientation} pixels={} cache_hit=true revision_distinct=true Fit={fw}x{fh} Native={w}x{h}",
            pixels.len() / 4
        );
        previous = Some(image.source.clone());
    }
    // SAFETY: 当前线程独占 Immediate Context，解除绑定后先释放 Renderer，再销毁窗口。
    unsafe { renderer.context.ClearState() };
    drop(renderer);
    Ok(())
}
