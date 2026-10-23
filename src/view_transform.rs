//! 统一图像边界坐标与客户区物理像素；不持有窗口、纹理或解码资源。
//!
//! 先按 Fit/100% 求基础倍率，再应用用户 Zoom，最后加客户区像素单位的 Pan。
//! 像素中心位于 (x + 0.5, y + 0.5)，原图边界为 [0, width] × [0, height]。

/// 用户附加倍率的范围；不限制 Fit 自身为适配客户区而采用的基础倍率。
pub(crate) const MIN_ZOOM: f64 = 0.01;
pub(crate) const MAX_ZOOM: f64 = 64.0;
// 足以覆盖 u32 尺寸在最大 Zoom 下的四角；只防止无限累加，不限制可见边缘。
const MAX_PAN: f64 = 1e15;

/// 模式只决定基础倍率；切图保留模式，F/1 重置额外 Zoom/Pan。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewMode {
    Fit,
    NativePixels,
}

/// 经过有限值校验的一次几何快照。所有位置和偏移均以物理像素表示。
#[derive(Clone, Copy, Debug)]
pub(crate) struct ViewTransform {
    image: (f64, f64),
    client: (f64, f64),
    pub(crate) scale: f64,
    pub(crate) offset: (f64, f64),
}

impl ViewTransform {
    /// 零尺寸、非法倍率或非有限 Pan 返回 None，调用方应跳过图像绘制。
    pub(crate) fn new(
        image: (u32, u32),
        client: (u32, u32),
        mode: ViewMode,
        zoom: f64,
        pan: (f64, f64),
    ) -> Option<Self> {
        if image.0 == 0
            || image.1 == 0
            || client.0 == 0
            || client.1 == 0
            || !zoom.is_finite()
            || !(MIN_ZOOM..=MAX_ZOOM).contains(&zoom)
            || !valid_pan(pan)
        {
            return None;
        }
        let image = (f64::from(image.0), f64::from(image.1));
        let client = (f64::from(client.0), f64::from(client.1));
        let base = match mode {
            ViewMode::Fit => (client.0 / image.0).min(client.1 / image.1),
            ViewMode::NativePixels => 1.0,
        };
        let scale = base * zoom;
        let center = match mode {
            ViewMode::Fit => (client.0 * 0.5, client.1 * 0.5),
            // 奇数像素余量留在右/下方。100%、Pan=0 时边界落在整数像素，
            // 避免把一个源像素的中心置于两个屏幕像素中心之间。
            ViewMode::NativePixels => (
                ((client.0 - image.0) * 0.5).floor() + image.0 * 0.5,
                ((client.1 - image.1) * 0.5).floor() + image.1 * 0.5,
            ),
        };
        Some(Self {
            image,
            client,
            scale,
            offset: (
                center.0 - image.0 * scale * 0.5 + pan.0,
                center.1 - image.1 * scale * 0.5 + pan.1,
            ),
        })
    }

    /// 原图边界坐标映射到客户区；允许图像外的点，用于留白处的缩放锚点。
    pub(crate) fn image_to_client(&self, point: (f64, f64)) -> (f64, f64) {
        (
            self.offset.0 + point.0 * self.scale,
            self.offset.1 + point.1 * self.scale,
        )
    }

    /// 客户区物理像素映射到原图；倍率已由构造函数保证为有限正数。
    pub(crate) fn client_to_image(&self, point: (f64, f64)) -> (f64, f64) {
        (
            (point.0 - self.offset.0) / self.scale,
            (point.1 - self.offset.1) / self.scale,
        )
    }

    /// 返回与客户区相交的原图范围；图像完全移出视口时返回 None。
    pub(crate) fn visible_image_rect(&self) -> Option<[f64; 4]> {
        let a = self.client_to_image((0.0, 0.0));
        let b = self.client_to_image(self.client);
        let rect = [
            a.0.clamp(0.0, self.image.0),
            a.1.clamp(0.0, self.image.1),
            b.0.clamp(0.0, self.image.0),
            b.1.clamp(0.0, self.image.1),
        ];
        (rect[2] > rect[0] && rect[3] > rect[1]).then_some(rect)
    }

    /// 将已校验的原图矩形映射为 D3D NDC；Y 轴由向下转为向上。
    pub(crate) fn ndc_rect(&self, rect: [f64; 4]) -> [f32; 4] {
        let a = self.image_to_client((rect[0], rect[1]));
        let b = self.image_to_client((rect[2], rect[3]));
        [
            (2.0 * a.0 / self.client.0 - 1.0) as f32,
            (1.0 - 2.0 * a.1 / self.client.1) as f32,
            (2.0 * b.0 / self.client.0 - 1.0) as f32,
            (1.0 - 2.0 * b.1 / self.client.1) as f32,
        ]
    }

    /// 普通图只提交可见 Quad 和对应 UV，避免巨大/负偏移成为非法 D3D11 Viewport。
    pub(crate) fn quad(&self) -> Option<[[f32; 4]; 2]> {
        let r = self.visible_image_rect()?;
        Some([
            self.ndc_rect(r),
            [
                (r[0] / self.image.0) as f32,
                (r[1] / self.image.1) as f32,
                (r[2] / self.image.0) as f32,
                (r[3] / self.image.1) as f32,
            ],
        ])
    }
}

fn valid_pan(pan: (f64, f64)) -> bool {
    pan.0.is_finite() && pan.1.is_finite() && pan.0.abs() <= MAX_PAN && pan.1.abs() <= MAX_PAN
}

/// 平移量以客户区物理像素相加；仅做数值保护，不把图像边缘夹回窗口。
pub(crate) fn translated_pan(pan: (f64, f64), delta: (f64, f64)) -> Option<(f64, f64)> {
    if !valid_pan(pan) || !delta.0.is_finite() || !delta.1.is_finite() {
        return None;
    }
    Some((
        (pan.0 + delta.0).clamp(-MAX_PAN, MAX_PAN),
        (pan.1 + delta.1).clamp(-MAX_PAN, MAX_PAN),
    ))
}

/// 先求锚点下的原图坐标 u=(p-offset)/scale，再令新偏移为 p-u*new_scale。
/// 限幅使用实际新倍率；达到上/下限后不会继续移动 Pan。
pub(crate) fn zoom_at(
    image: (u32, u32),
    client: (u32, u32),
    mode: ViewMode,
    zoom: f64,
    pan: (f64, f64),
    factor: f64,
    point: (f64, f64),
) -> Option<(f64, (f64, f64))> {
    if !factor.is_finite() || factor <= 0.0 || !point.0.is_finite() || !point.1.is_finite() {
        return None;
    }
    let old = ViewTransform::new(image, client, mode, zoom, pan)?;
    let next = (zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    if next == zoom {
        return Some((zoom, pan));
    }
    let base = ViewTransform::new(image, client, mode, next, (0.0, 0.0))?;
    let u = old.client_to_image(point);
    let mapped = base.image_to_client(u);
    let pan = translated_pan((0.0, 0.0), (point.0 - mapped.0, point.1 - mapped.1))?;
    Some((next, pan))
}

/// Resize 将原客户区中心下的图像位置保持在新中心；最小化由调用者保留最后非零尺寸。
pub(crate) fn resized_pan(
    image: (u32, u32),
    old: (u32, u32),
    new: (u32, u32),
    mode: ViewMode,
    zoom: f64,
    pan: (f64, f64),
) -> Option<(f64, f64)> {
    let from = ViewTransform::new(image, old, mode, zoom, pan)?;
    // 最小化后恢复到同一尺寸无需逆变换，避免重复恢复积累浮点舍入。
    if old == new {
        return Some(pan);
    }
    let to = ViewTransform::new(image, new, mode, zoom, (0.0, 0.0))?;
    let point = from.client_to_image((f64::from(old.0) * 0.5, f64::from(old.1) * 0.5));
    let mapped = to.image_to_client(point);
    translated_pan(
        (0.0, 0.0),
        (
            f64::from(new.0) * 0.5 - mapped.0,
            f64::from(new.1) * 0.5 - mapped.1,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // f64 在本测试的百万级坐标下 ULP 小于 1e-9；1e-7 像素容纳运算次序误差，
    // 仍远小于像素采样容差。循环测试单独给出累计误差上限。
    fn near(a: (f64, f64), b: (f64, f64)) {
        assert!(
            (a.0 - b.0).abs() < 1e-7 && (a.1 - b.1).abs() < 1e-7,
            "{a:?} != {b:?}"
        );
    }
    #[test]
    fn fit_native_corners_and_oriented_dimensions() {
        let fit =
            ViewTransform::new((400, 200), (1000, 600), ViewMode::Fit, 1.0, (0.0, 0.0)).unwrap();
        assert_eq!(fit.scale, 2.5);
        near(fit.image_to_client((0.0, 0.0)), (0.0, 50.0));
        near(fit.image_to_client((400.0, 200.0)), (1000.0, 550.0));
        let native = ViewTransform::new(
            (400, 200),
            (1001, 601),
            ViewMode::NativePixels,
            1.0,
            (0.0, 0.0),
        )
        .unwrap();
        assert_eq!(native.scale, 1.0);
        near(native.image_to_client((0.5, 0.5)), (300.5, 200.5));
        let oriented =
            ViewTransform::new((200, 400), (1000, 600), ViewMode::Fit, 1.0, (0.0, 0.0)).unwrap();
        assert_eq!(oriented.scale, 1.5);
        near(oriented.offset, (350.0, 0.0));
    }
    #[test]
    fn anchors_inside_outside_and_fractional_wheel_deltas() {
        for mode in [ViewMode::Fit, ViewMode::NativePixels] {
            for p in [(318.0, 177.0), (-500.0, -37.0), (1500.0, 800.0)] {
                let old = ViewTransform::new((48000, 32000), (1101, 761), mode, 1.0, (23.0, -47.0))
                    .unwrap();
                let u = old.client_to_image(p);
                let factor = 1.2f64.powf(15.0 / 120.0);
                let (z, pan) = zoom_at(
                    (48000, 32000),
                    (1101, 761),
                    mode,
                    1.0,
                    (23.0, -47.0),
                    factor,
                    p,
                )
                .unwrap();
                assert!(z > 1.0 && z < 1.2);
                let next = ViewTransform::new((48000, 32000), (1101, 761), mode, z, pan).unwrap();
                near(next.image_to_client(u), p);
            }
        }
    }
    #[test]
    fn round_trips_limits_and_pan_are_stable() {
        let mut zoom = 1.0;
        let mut pan = (42.0, -55.0);
        for _ in 0..2000 {
            (zoom, pan) = zoom_at(
                (4000, 3000),
                (1100, 760),
                ViewMode::Fit,
                zoom,
                pan,
                1.2,
                (113.0, 219.0),
            )
            .unwrap();
            (zoom, pan) = zoom_at(
                (4000, 3000),
                (1100, 760),
                ViewMode::Fit,
                zoom,
                pan,
                1.0 / 1.2,
                (113.0, 219.0),
            )
            .unwrap();
        }
        near(pan, (42.0, -55.0));
        assert!((zoom - 1.0).abs() < 1e-12);
        for z in [MIN_ZOOM, MAX_ZOOM] {
            let f = if z == MIN_ZOOM { 0.1 } else { 10.0 };
            assert_eq!(
                zoom_at((4, 3), (1001, 601), ViewMode::Fit, z, pan, f, (2.0, 3.0)).unwrap(),
                (z, pan)
            );
        }
        near(translated_pan(pan, (13.0, -29.0)).unwrap(), (55.0, -84.0));
    }
    #[test]
    fn resize_keeps_center_source_and_restores_pan() {
        for mode in [ViewMode::Fit, ViewMode::NativePixels] {
            let pan = (133.0, -47.0);
            let next = resized_pan((701, 1400), (1100, 760), (1501, 900), mode, 3.0, pan).unwrap();
            let a = ViewTransform::new((701, 1400), (1100, 760), mode, 3.0, pan).unwrap();
            let b = ViewTransform::new((701, 1400), (1501, 900), mode, 3.0, next).unwrap();
            near(
                a.client_to_image((550.0, 380.0)),
                b.client_to_image((750.5, 450.0)),
            );
            near(
                resized_pan((701, 1400), (1501, 900), (1100, 760), mode, 3.0, next).unwrap(),
                pan,
            );
        }
    }
    #[test]
    fn invalid_geometry_and_extreme_sizes_never_reach_gpu() {
        for z in [0.0, -1.0, f64::NAN, f64::INFINITY, 65.0] {
            assert!(ViewTransform::new((4, 3), (100, 80), ViewMode::Fit, z, (0.0, 0.0)).is_none());
        }
        for (image, client) in [((0, 3), (100, 80)), ((4, 3), (0, 0))] {
            assert!(ViewTransform::new(image, client, ViewMode::Fit, 1.0, (0.0, 0.0)).is_none());
        }
        assert!(translated_pan((0.0, 0.0), (f64::NAN, 0.0)).is_none());
        assert!(
            ViewTransform::new((1, 1), (1, 1), ViewMode::Fit, 1.0, (f64::INFINITY, 0.0)).is_none()
        );
        for image in [(u32::MAX, 1), (1, u32::MAX), (1, 1)] {
            for client in [(1, 1), (u32::MAX, u32::MAX)] {
                let v =
                    ViewTransform::new(image, client, ViewMode::Fit, MIN_ZOOM, (0.0, 0.0)).unwrap();
                assert!(v.scale.is_finite() && v.scale > 0.0);
                assert!(v.quad().unwrap().into_iter().flatten().all(f32::is_finite));
            }
        }
        let v = ViewTransform::new((4, 3), (100, 80), ViewMode::Fit, 1.0, (1e10, 1e10)).unwrap();
        assert!(v.quad().is_none());
    }
}
