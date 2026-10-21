//! 单画布顺序合成；Restore Previous 仅保存当前矩形，输出是否显示不改变合成步骤。
use super::wic::Patch;
use anyhow::{Context, Result, ensure};
/// 顺序画布和上一帧的 Disposal 状态；最多保留一个恢复区域。
pub struct Composer {
    pub pixels: Vec<u8>,
    size: [u32; 2],
    background: [u8; 4],
    previous: Option<([u32; 4], u32, Vec<u8>)>,
    pub restore_peak: usize,
}
impl Composer {
    /// 创建有界画布；尺寸溢出、零尺寸或分配失败通过 Result 返回。
    pub fn new(size: [u32; 2], background: [u8; 4]) -> Result<Self> {
        let count = u64::from(size[0])
            .checked_mul(u64::from(size[1]))
            .and_then(|n| n.checked_mul(4))
            .context("画布尺寸溢出")?;
        ensure!(
            size.iter().all(|v| *v > 0 && *v <= 16384)
                && count <= super::flow::PIXEL_LIMIT as u64 / 5,
            "画布超过实验工作集预算"
        );
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(usize::try_from(count)?)?;
        for _ in 0..count / 4 {
            pixels.extend_from_slice(&background);
        }
        Ok(Self {
            pixels,
            size,
            background,
            previous: None,
            restore_peak: 0,
        })
    }
    /// 先执行上一帧 Disposal，再保存恢复区并叠加当前局部帧。
    pub fn apply(&mut self, patch: &Patch) -> Result<()> {
        let [x, y, w, h] = patch.rect;
        ensure!(
            w > 0
                && h > 0
                && x.checked_add(w).is_some_and(|v| v <= self.size[0])
                && y.checked_add(h).is_some_and(|v| v <= self.size[1]),
            "局部矩形越界"
        );
        ensure!(
            patch.pixels.len() == (u64::from(w) * u64::from(h) * 4) as usize && patch.disposal <= 3,
            "局部像素布局错误"
        );
        if let Some((rect, disposal, saved)) = self.previous.take() {
            for row in 0..rect[3] {
                for col in 0..rect[2] {
                    let at = (((rect[1] + row) * self.size[0] + rect[0] + col) * 4) as usize;
                    match disposal {
                        2 => self.pixels[at..at + 4].copy_from_slice(&self.background),
                        3 => {
                            let from = ((row * rect[2] + col) * 4) as usize;
                            self.pixels[at..at + 4].copy_from_slice(&saved[from..from + 4]);
                        }
                        _ => {}
                    }
                }
            }
        }
        let mut saved = Vec::new();
        if patch.disposal == 3 {
            saved.try_reserve_exact(patch.pixels.len())?;
            for row in 0..h {
                let at = (((y + row) * self.size[0] + x) * 4) as usize;
                saved.extend_from_slice(&self.pixels[at..at + (w * 4) as usize]);
            }
            self.restore_peak = self.restore_peak.max(saved.len());
        }
        for row in 0..h {
            for col in 0..w {
                let src = ((row * w + col) * 4) as usize;
                let dst = (((y + row) * self.size[0] + x + col) * 4) as usize;
                // GIF Alpha 为 0 或 255；透明索引保持目标像素，不能清除已有画布。
                ensure!(
                    [0, 255].contains(&patch.pixels[src + 3]),
                    "GIF 出现半透明像素"
                );
                if patch.pixels[src + 3] != 0 {
                    self.pixels[dst..dst + 4].copy_from_slice(&patch.pixels[src..src + 4]);
                }
            }
        }
        self.previous = Some((patch.rect, patch.disposal, saved));
        Ok(())
    }
}
