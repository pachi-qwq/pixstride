//! 把视口映射到金字塔层和逻辑 Tile；不访问文件或 GPU，可独立验证边界与内存上限。

use super::cache::Plan;
use crate::{app::RenderSnapshot, view_transform::ViewTransform};

/// 一次视口需求最多 24 块；过多时选择更粗一层，而不是无界扩展工作队列。
pub(crate) const MAX_VISIBLE: usize = 24;

/// 每块只画一次，缺块时用预览对应的 UV，避免透明图片与底层预览重复混合。
pub(crate) struct Piece {
    pub(crate) id: usize,
    pub(crate) rect: [f32; 4],
    pub(crate) preview_uv: [f32; 4],
}

/// 使用已校验的预览索引计算可见区域；零窗口或图像完全移出视口时返回空列表。
pub(crate) fn pieces(plan: &Plan, snapshot: &RenderSnapshot, preview: usize) -> Vec<Piece> {
    let Some(view) = ViewTransform::new(
        (plan.size.width, plan.size.height),
        (snapshot.width, snapshot.height),
        snapshot.view_mode,
        snapshot.zoom,
        snapshot.pan,
    ) else {
        return Vec::new();
    };
    let iw = f64::from(plan.size.width);
    let ih = f64::from(plan.size.height);
    let Some([left, top, right, bottom]) = view.visible_image_rect() else {
        return Vec::new();
    };
    let scale = view.scale;
    let initial = (-scale.log2()).floor().max(0.0) as usize;
    let mut level = initial.min(plan.levels.len() - 1);
    loop {
        let l = &plan.levels[level];
        let step = 2f64.powi(level as i32);
        let span = f64::from(plan.edge) * step;
        let x0 = (left / span).floor() as u32;
        let y0 = (top / span).floor() as u32;
        let x1 = ((right / span).ceil() as u32).min(l.columns);
        let y1 = ((bottom / span).ceil() as u32).min(l.rows);
        if u64::from(x1 - x0) * u64::from(y1 - y0) > MAX_VISIBLE as u64
            && level + 1 < plan.levels.len()
        {
            level += 1;
            continue;
        }
        let p = &plan.tiles[preview];
        let mut result = Vec::new();
        for y in y0..y1 {
            for x in x0..x1 {
                let id = l.first + (y * l.columns + x) as usize;
                let t = &plan.tiles[id];
                let a = f64::from(x) * span;
                let b = f64::from(y) * span;
                let c = (a + f64::from(t.width) * step).min(iw);
                let d = (b + f64::from(t.height) * step).min(ih);
                result.push(Piece {
                    id,
                    rect: view.ndc_rect([a, b, c, d]),
                    preview_uv: [
                        (1.0 + a / iw * f64::from(p.width)) as f32 / (p.width + 2) as f32,
                        (1.0 + b / ih * f64::from(p.height)) as f32 / (p.height + 2) as f32,
                        (1.0 + c / iw * f64::from(p.width)) as f32 / (p.width + 2) as f32,
                        (1.0 + d / ih * f64::from(p.height)) as f32 / (p.height + 2) as f32,
                    ],
                });
            }
        }
        return result;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{RequestOrigin, ViewMode},
        large::stream::PngSize,
    };
    #[test]
    fn viewport_is_bounded_and_native_pan_reaches_edges() {
        let plan = Plan::new(
            PngSize {
                width: 48000,
                height: 32000,
            },
            1024,
        )
        .unwrap();
        let preview = plan
            .levels
            .iter()
            .find(|l| l.columns == 1 && l.rows == 1)
            .unwrap()
            .first;
        let mut s = RenderSnapshot {
            sequence: 0,
            texture_index: 0,
            width: 1100,
            height: 760,
            requested_at: std::time::Instant::now(),
            request_origin: RequestOrigin::Paint,
            view_mode: ViewMode::Fit,
            zoom: 1.0,
            pan: (0.0, 0.0),
        };
        let fit = pieces(&plan, &s, preview);
        assert!(!fit.is_empty() && fit.len() <= MAX_VISIBLE);
        assert!(fit.iter().all(|p| plan.tiles[p.id].level >= 5));
        s.view_mode = ViewMode::NativePixels;
        let center = pieces(&plan, &s, preview);
        assert!(center.iter().all(|p| plan.tiles[p.id].level == 0));
        s.pan = (23450.0, 15620.0);
        assert!(pieces(&plan, &s, preview).iter().any(|p| p.id == 0));
        s.pan = (-23450.0, -15620.0);
        let edge = pieces(&plan, &s, preview);
        assert!(edge.iter().any(|p| {
            let t = &plan.tiles[p.id];
            t.x == 46 && t.y == 31
        }));
        s.width = 0;
        assert!(pieces(&plan, &s, preview).is_empty());
    }

    #[test]
    fn shared_geometry_matches_tile_rects_and_reaches_all_four_corners() {
        let plan = Plan::new(
            PngSize {
                width: 2049,
                height: 1025,
            },
            512,
        )
        .unwrap();
        let preview = plan
            .levels
            .iter()
            .find(|l| l.columns == 1 && l.rows == 1)
            .unwrap()
            .first;
        let mut s = RenderSnapshot {
            sequence: 0,
            texture_index: 0,
            width: 401,
            height: 301,
            requested_at: std::time::Instant::now(),
            request_origin: RequestOrigin::Paint,
            view_mode: ViewMode::NativePixels,
            zoom: 1.0,
            pan: (0.0, 0.0),
        };
        // NativePixels 的独立公式：offset=(-824,-362)+pan；四角分别对齐客户区边缘。
        for (pan, corner) in [
            ((824.0, 362.0), (0, 0)),
            ((-824.0, 362.0), (4, 0)),
            ((824.0, -362.0), (0, 2)),
            ((-824.0, -362.0), (4, 2)),
        ] {
            s.pan = pan;
            let parts = pieces(&plan, &s, preview);
            assert!(!parts.is_empty() && parts.len() <= MAX_VISIBLE);
            assert!(parts.iter().any(|p| {
                let tile = &plan.tiles[p.id];
                tile.level == 0 && (tile.x, tile.y) == corner
            }));
            for p in parts {
                let tile = &plan.tiles[p.id];
                let x = f64::from(tile.x * 512) - 824.0 + pan.0;
                let y = f64::from(tile.y * 512) - 362.0 + pan.1;
                let expected = [
                    (2.0 * x / 401.0 - 1.0) as f32,
                    (1.0 - 2.0 * y / 301.0) as f32,
                    (2.0 * (x + f64::from(tile.width)) / 401.0 - 1.0) as f32,
                    (1.0 - 2.0 * (y + f64::from(tile.height)) / 301.0) as f32,
                ];
                assert_eq!(p.rect, expected);
                assert!(
                    p.preview_uv
                        .iter()
                        .all(|n| n.is_finite() && *n >= 0.0 && *n <= 1.0)
                );
            }
        }
        s.view_mode = ViewMode::Fit;
        s.pan = (0.0, 0.0);
        for z in [0.01, 0.5, 1.0, 2.0, 64.0] {
            s.zoom = z;
            let p = pieces(&plan, &s, preview);
            assert!(!p.is_empty() && p.len() <= MAX_VISIBLE);
            assert!(p.iter().all(|t| t.rect.into_iter().all(f32::is_finite)));
        }
        s.zoom = f64::NAN;
        assert!(pieces(&plan, &s, preview).is_empty());
    }
}
