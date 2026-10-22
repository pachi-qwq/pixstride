//! 自行生成小型 GIF89a；LZW 每像素重置字典，参考结果由人工坐标表给出，不依赖合成器。

use anyhow::Result;
use std::path::Path;

/// 固定全局调色板，便于独立计算每个最终像素。
pub const PALETTE: [[u8; 3]; 4] = [[0, 0, 0], [255, 0, 0], [0, 255, 0], [0, 0, 255]];

#[derive(Clone)]
/// 自行生成的描述符；允许显式构造非法元数据供错误测试使用。
pub struct Frame {
    pub rect: [u16; 4],
    pub indices: Vec<u8>,
    pub disposal: u8,
    pub delay: u16,
    pub transparent: bool,
    pub palette: Option<[[u8; 3]; 4]>,
}

/// 在专用目录写 GIF89a；此编码器只服务确定性实验，不是正式产品功能。
pub fn write(path: &Path, size: [u16; 2], frames: &[Frame], loops: Option<u16>) -> Result<()> {
    let mut out = b"GIF89a".to_vec();
    out.extend_from_slice(&size[0].to_le_bytes());
    out.extend_from_slice(&size[1].to_le_bytes());
    out.extend_from_slice(&[0x81, 0, 0]);
    for c in PALETTE {
        out.extend_from_slice(&c);
    }
    if let Some(n) = loops {
        out.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01");
        out.extend_from_slice(&n.to_le_bytes());
        out.push(0);
    }
    for f in frames {
        out.extend_from_slice(&[0x21, 0xf9, 4, (f.disposal << 2) | u8::from(f.transparent)]);
        out.extend_from_slice(&f.delay.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0x2c]);
        for v in f.rect {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.push(if f.palette.is_some() { 0x81 } else { 0 });
        if let Some(p) = f.palette {
            for c in p {
                out.extend_from_slice(&c);
            }
        }
        out.push(2);
        let mut packed = Vec::new();
        let mut bits = 0u32;
        let mut count = 0;
        for code in f.indices.iter().flat_map(|i| [4, *i]).chain([5]) {
            bits |= u32::from(code) << count;
            count += 3;
            while count >= 8 {
                packed.push(bits as u8);
                bits >>= 8;
                count -= 8;
            }
        }
        if count > 0 {
            packed.push(bits as u8);
        }
        for block in packed.chunks(255) {
            out.push(block.len() as u8);
            out.extend_from_slice(block);
        }
        out.push(0);
    }
    out.push(0x3b);
    std::fs::write(path, out)?;
    Ok(())
}

/// 覆盖四种 Disposal、偏移、透明和局部调色板的五帧样本。
pub fn sample() -> Vec<Frame> {
    vec![
        Frame {
            rect: [0, 0, 4, 3],
            indices: vec![1; 12],
            disposal: 0,
            delay: 0,
            transparent: false,
            palette: None,
        },
        Frame {
            rect: [1, 0, 2, 2],
            indices: vec![2, 0, 0, 2],
            disposal: 1,
            delay: 1,
            transparent: true,
            palette: None,
        },
        Frame {
            rect: [0, 1, 2, 1],
            indices: vec![3, 3],
            disposal: 2,
            delay: 2,
            transparent: false,
            palette: None,
        },
        Frame {
            rect: [3, 0, 1, 3],
            indices: vec![1; 3],
            disposal: 3,
            delay: 10,
            transparent: false,
            palette: Some([[0, 0, 0], [255, 255, 0], [0, 255, 0], [0, 0, 255]]),
        },
        Frame {
            rect: [0, 2, 1, 1],
            indices: vec![2],
            disposal: 1,
            delay: 3,
            transparent: false,
            palette: None,
        },
    ]
}

/// 按手工坐标表给出每帧的完整画布，不调用待测合成器。
pub fn oracle() -> Vec<Vec<u8>> {
    // r/g/b/y/k 表示不透明红绿蓝黄黑；矩形偏移及恢复结果直接列出。
    [
        "rrrrrrrrrrrr",
        "rgrrrrgrrrrr",
        "rgrrbbgrrrrr",
        "rgrykkgyrrry",
        "rgrrkkgrgrrr",
    ]
    .iter()
    .map(|s| {
        s.chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(|c| match c {
                'r' => [0, 0, 255, 255],
                'g' => [0, 255, 0, 255],
                'b' => [255, 0, 0, 255],
                'y' => [0, 255, 255, 255],
                _ => [0, 0, 0, 255],
            })
            .collect()
    })
    .collect()
}
