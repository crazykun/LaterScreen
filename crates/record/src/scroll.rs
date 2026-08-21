//! 滚动长截图拼接（M4）：帧间特征匹配。
//!
//! 算法（经典尾部块匹配）：
//! 1. 已拼接图像 A 取尾部 K 行（匹配块），对每行做 4px 降采样亮度签名；
//! 2. 在新帧 F 的签名行里定位 A 尾部最后一行的出现位置（候选 dy，两阶段
//!    省掉逐候选全块比对）；
//! 3. 对候选做整块 SAD 校验，取最小者；SAD/像素低于阈值视为同一内容
//!    （容许抗锯齿/亚像素平滑滚动带来的轻微行变化）；
//! 4. F 中位于匹配块之后的部分 [dy+K, F.h) 即新内容，追加到 A。
//!
//! 已知限制（对齐计划「先支持等速滚动的简单场景」）：
//! - 悬浮表头/固定侧栏：块匹配找不到会返回 Mismatch，由调用方停止
//!   （拿到已拼接部分，不失败）；
//! - 区域内动画内容（视频/闪烁光标附近）：光标是小面积、阈值容忍；
//!   大面积动画会误判，建议选区避开。

use crate::{RecordError, Result};

/// 匹配块行数：区域高度的 1/6，钳在 [16, 96]——太小易误匹配，
/// 太大浪费比对且平滑滚动中块内变化过大
fn block_rows(h: u32) -> usize {
    (h as usize / 6).clamp(16, 96)
}

/// SAD/像素 阈值：抗锯齿+平滑滚动下同内容行有轻微差异，8/255 足够宽
const MATCH_TOL: u32 = 8;
/// 签名行预筛阈值（单行比整块更「脆」，放宽）
const ROW_TOL: u32 = 10;
/// 列降采样步长（像素）
const COL_STRIDE: usize = 4;

/// 一帧的降采样亮度签名：每行按 COL_STRIDE 跨步取亮度字节（覆盖整行宽度）
fn frame_signature(rgba: &[u8], w: u32, h: u32) -> Vec<Vec<u8>> {
    let w = w as usize;
    let cols = w.div_ceil(COL_STRIDE);
    let mut rows = Vec::with_capacity(h as usize);
    for row_px in rgba.chunks_exact(w * 4) {
        let mut row = Vec::with_capacity(cols);
        for px in row_px.as_chunks::<4>().0.iter().step_by(COL_STRIDE) {
            let y = (px[0] as u32 * 3 + px[1] as u32 * 6 + px[2] as u32) / 10;
            row.push(y as u8);
        }
        rows.push(row);
    }
    rows
}

fn sad_row(a: &[u8], b: &[u8]) -> u32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs())
        .sum()
}

/// 一次 push 的结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollOutcome {
    /// 追加了 n 行新内容
    Appended(u32),
    /// 与已有内容完全一致（无滚动/已到底）
    NoChange,
    /// 尾部块在新帧中找不到匹配（内容突变/悬浮元素），建议停止
    Mismatch,
}

pub struct ScrollStitcher {
    width: u32,
    /// 拼接图像（RGBA，宽度固定为区域宽）
    image: Vec<u8>,
    /// 每行的降采样亮度签名（与 image 同步增长，块匹配用）
    sig: Vec<Vec<u8>>,
    block: usize,
}

impl ScrollStitcher {
    /// 用首帧初始化
    pub fn new(rgba: &[u8], w: u32, h: u32) -> Result<Self> {
        if w == 0 || h == 0 || rgba.len() != (w * h * 4) as usize {
            return Err(RecordError("首帧尺寸/数据无效".into()));
        }
        Ok(Self {
            width: w,
            image: rgba.to_vec(),
            sig: frame_signature(rgba, w, h),
            block: block_rows(h),
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        (self.image.len() / (self.width as usize * 4)) as u32
    }

    /// 已拼接图像的 RGBA
    pub fn image(&self) -> &[u8] {
        &self.image
    }

    /// 喂入后续帧
    pub fn push(&mut self, rgba: &[u8], w: u32, h: u32) -> Result<ScrollOutcome> {
        if w != self.width {
            return Err(RecordError(format!("帧宽不一致: {} != {}", self.width, w)));
        }
        if rgba.len() != (w * h * 4) as usize {
            return Err(RecordError("帧数据长度与尺寸不符".into()));
        }
        let frame_sig = frame_signature(rgba, w, h);
        let k = self.block.min(self.sig.len()).min(frame_sig.len());
        if k == 0 {
            return Err(RecordError("匹配块为空".into()));
        }

        // 两阶段：签名行（尾部块最后一行）预筛候选块起点。
        // anchor 是块的「末行」，在新帧中出现于 p 时，块起点 = p - (k-1)
        let anchor = &self.sig[self.sig.len() - 1];
        let cols = anchor
            .len()
            .min(frame_sig.iter().map(|r| r.len()).min().unwrap_or(0));
        if cols == 0 {
            return Ok(ScrollOutcome::Mismatch);
        }
        let k1 = k - 1;
        let mut candidates: Vec<usize> = Vec::new();
        for (p, row) in frame_sig.iter().enumerate() {
            let head = &row[..cols.min(row.len())];
            let a = &anchor[..head.len()];
            if sad_row(a, head) / head.len() as u32 <= ROW_TOL && p >= k1 {
                candidates.push(p - k1);
            }
        }
        if candidates.is_empty() {
            return Ok(ScrollOutcome::Mismatch);
        }

        // 整块校验：取 SAD 最小的候选
        let tail = &self.sig[self.sig.len() - k..];
        let mut best: Option<(usize, u32)> = None;
        for dy in candidates {
            if dy + k > frame_sig.len() {
                continue;
            }
            let mut sad = 0u32;
            for (i, trow) in tail.iter().enumerate() {
                let frow = &frame_sig[dy + i];
                let n = trow.len().min(frow.len());
                sad += sad_row(&trow[..n], &frow[..n]);
            }
            let per_px = sad / (k as u32 * cols as u32);
            if best.is_none_or(|(_, b)| per_px < b) {
                best = Some((dy, per_px));
            }
        }
        let Some((dy, per_px)) = best else {
            return Ok(ScrollOutcome::Mismatch);
        };
        if per_px > MATCH_TOL {
            return Ok(ScrollOutcome::Mismatch);
        }

        // F[dy+k, h) 是新内容；dy+k <= h 保证追加快速
        let new_rows = frame_sig.len() - (dy + k);
        match new_rows {
            0 => Ok(ScrollOutcome::NoChange),
            n => {
                let byte_off = (dy + k) * self.width as usize * 4;
                self.image.extend_from_slice(&rgba[byte_off..]);
                self.sig.extend_from_slice(&frame_sig[dy + k..]);
                Ok(ScrollOutcome::Appended(n as u32))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成一张 w×(h) 的竖直长图：每行有确定性的独特纹理
    /// （行号哈希 + 列变化），避免大面积纯色导致的误匹配
    fn source_line(y: usize, w: usize) -> Vec<u8> {
        (0..w)
            .flat_map(|x| {
                let v = (y * 31 + x * 7 + (y / 8) * 13) as u8;
                [v.wrapping_mul(3), v, (255 - v).wrapping_mul(5) / 3, 255]
            })
            .collect()
    }

    fn window_view(src_h: usize, w: usize, h: usize, offset: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h * 4);
        for y in offset..(offset + h).min(src_h) {
            out.extend_from_slice(&source_line(y, w));
        }
        // 到底后补最后一行撑满窗口
        while out.len() < w * h * 4 {
            out.extend_from_slice(&source_line(src_h.saturating_sub(1), w));
        }
        out
    }

    #[test]
    fn stitch_scrolling_frames() {
        let (w, h, src_h) = (200usize, 120usize, 800usize);
        let mut st = ScrollStitcher::new(&window_view(src_h, w, h, 0), w as u32, h as u32).unwrap();

        // 等速滚动：每次向下 17px（非整除，验证任意步长）
        let mut offset = 0usize;
        let mut appended_total = 0u32;
        while offset + h < src_h {
            offset += 17;
            match st
                .push(&window_view(src_h, w, h, offset), w as u32, h as u32)
                .unwrap()
            {
                ScrollOutcome::Appended(n) => appended_total += n,
                other => panic!("offset={offset}: 意外结果 {other:?}"),
            }
        }
        // 拼接高度 = 首帧 + 全部新增内容（末尾补齐行造成的少量误差允许 2px）
        let expected = src_h as u32;
        let got = st.height();
        assert!(
            (got as i64 - expected as i64).abs() <= 2,
            "拼接高度 {got} != 预期 ~{expected}"
        );
        assert!(appended_total > 0);
        // 内容逐行校验（首 800 行应与源完全一致）
        for y in 0..src_h {
            let row = &st.image()[y * w * 4..(y + 1) * w * 4];
            assert_eq!(row, &source_line(y, w)[..], "第 {y} 行内容不一致");
        }
    }

    #[test]
    fn no_change_and_mismatch() {
        let (w, h) = (100usize, 80usize);
        let frame = window_view(10_000, w, h, 0);
        let mut st = ScrollStitcher::new(&frame, w as u32, h as u32).unwrap();
        // 同一帧再推 → NoChange
        assert_eq!(
            st.push(&frame, w as u32, h as u32).unwrap(),
            ScrollOutcome::NoChange
        );
        // 完全无关内容（全随机底噪会误配？用结构性反转图案确保行签名不匹配）
        let mut other = vec![0u8; w * h * 4];
        for (i, b) in other.iter_mut().enumerate() {
            *b = if (i / 4) % 2 == 0 { 250 } else { 0 };
        }
        assert_eq!(
            st.push(&other, w as u32, h as u32).unwrap(),
            ScrollOutcome::Mismatch
        );
    }

    /// 抗噪能力：滚动内容带轻微亮度抖动（模拟抗锯齿/渲染差异）仍应匹配
    #[test]
    fn tolerates_render_noise() {
        let (w, h, src_h) = (160usize, 90usize, 400usize);
        let mut st = ScrollStitcher::new(&window_view(src_h, w, h, 0), w as u32, h as u32).unwrap();
        let mut offset = 0;
        while offset + h < src_h {
            offset += 13;
            let mut frame = window_view(src_h, w, h, offset);
            // 每帧施加 ±3 亮度噪声
            for b in frame.iter_mut().step_by(4) {
                *b = b.saturating_add(3);
            }
            match st.push(&frame, w as u32, h as u32).unwrap() {
                ScrollOutcome::Appended(_) => {}
                other => panic!("噪声帧匹配失败: {other:?}"),
            }
        }
        assert!((st.height() as i64 - src_h as i64).abs() <= 2);
    }
}
