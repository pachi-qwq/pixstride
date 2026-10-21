//! 单透明初始画布合成；Disposal 只作用于上一帧区域，恢复区按真实像素容量计费。
use super::{Pixels, decoder::Patch};
use crate::image::{check_decode_current, memory::CpuMemory};
use anyhow::{Result, ensure};

pub(super) struct Composer {
    pub(super) pixels: Pixels,
    width: u32,
    height: u32,
    background: [u8; 4],
    previous: Option<([u32; 4], u8, Option<Pixels>)>,
}
impl Composer {
    pub(super) fn new(
        pool: &CpuMemory,
        width: u32,
        height: u32,
        background: [u8; 4],
    ) -> Result<Self> {
        let (_, len, _) = crate::image::checked_layout(width, height, Default::default())?;
        Ok(Self {
            pixels: Pixels::new(pool, len)?,
            width,
            height,
            background,
            previous: None,
        })
    }
    pub(super) fn reset(&mut self) {
        self.previous = None;
        self.pixels.data.fill(0);
    }
    pub(super) fn apply(
        &mut self,
        patch: &Patch,
        pool: &CpuMemory,
        current: &impl Fn() -> bool,
    ) -> Result<()> {
        let [x, y, w, h] = patch.rect;
        ensure!(
            w > 0
                && h > 0
                && x.checked_add(w).is_some_and(|n| n <= self.width)
                && y.checked_add(h).is_some_and(|n| n <= self.height)
                && u64::from(w) * u64::from(h) * 4 == patch.pixels.data.len() as u64
                && patch.disposal <= 3,
            "局部帧布局非法"
        );
        if let Some((rect, disposal, saved)) = self.previous.take() {
            for row in 0..rect[3] {
                check_decode_current(current)?;
                let at = (((rect[1] + row) * self.width + rect[0]) * 4) as usize;
                let target = &mut self.pixels.data[at..at + (rect[2] * 4) as usize];
                match disposal {
                    2 => {
                        for pixel in target.chunks_exact_mut(4) {
                            pixel.copy_from_slice(&self.background);
                        }
                    }
                    3 => {
                        let saved = saved
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("缺少 Restore Previous 区域"))?;
                        let start = (row * rect[2] * 4) as usize;
                        target.copy_from_slice(&saved.data[start..start + target.len()]);
                    }
                    _ => {}
                }
            }
        } // 先释放上一份恢复区，再申请当前恢复区，避免两个恢复 Vec 同时存活。
        let mut saved = if patch.disposal == 3 {
            Some(Pixels::new(pool, patch.pixels.data.len())?)
        } else {
            None
        };
        for row in 0..h {
            check_decode_current(current)?;
            let src = (row * w * 4) as usize;
            let dst = (((y + row) * self.width + x) * 4) as usize;
            let target = &mut self.pixels.data[dst..dst + (w * 4) as usize];
            if let Some(saved) = &mut saved {
                saved.data[src..src + target.len()].copy_from_slice(target);
            }
            for (from, to) in patch.pixels.data[src..src + target.len()]
                .chunks_exact(4)
                .zip(target.chunks_exact_mut(4))
            {
                ensure!([0, 255].contains(&from[3]), "GIF Alpha 非二值");
                if from[3] != 0 {
                    to.copy_from_slice(from);
                }
            }
        }
        self.previous = Some((patch.rect, patch.disposal, saved));
        Ok(())
    }
}
