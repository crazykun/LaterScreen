//! 滚动长截图拼接（M4）：帧间位移估计 + 底部追加。
//!
//! 算法（v2，2026-09-19 真机重写；v1「尾部块匹配」在平坦内容上结构性
//! 失灵——尾块几十行里往往只有一两行文字，对齐真实位置（带 AA 残差）
//! 反而输给纯平坦位置，轻则保守 NoChange 丢失跟踪、重则 Mismatch）：
//! 1. 帧签名：每行 4px 列跨步亮度 + [1,2,1] 垂直平滑（吸收亚像素滚动的
//!    AA 逐帧微变，见 `frame_signature`）；
//! 2. 锚行：自拼接图**倒数第二行**向上找有纹理的行（行 MAD ≥ FLAT_MAD；
//!    平坦行在任何位移下都「匹配」，没有信息量；末行是帧缝行——其平滑
//!    用边界复制而下一帧同内容行带真实下邻，两处必然不同，排除）；
//! 3. 位移假设：锚行 i 与帧中纹理行 p 构成 S = i − p（S ≥ 0，≤ 预算）；
//! 4. 假设验证：全幅重叠（帧行 r ↔ 拼接行 r+S）只累计两侧都有纹理的行
//!    的 SAD/px，取最小者（并列取更小 S，保守少拼）；
//! 5. S=0 → NoChange；拟追加的 S 行（视口底部新进入的内容）若全为平坦
//!    行——粘性底栏/纯空白带滚入——同样 NoChange，不把固定镶边重复拼
//!    进长图；否则追加帧的最后 S 行。
//!
//! 已知限制：
//! - 悬浮表头/固定侧栏/大面积动画：验证不过 → Mismatch，由调用方停止
//!   （拿到已拼接部分，不失败）；选区底部粘性镶边 → 恒 NoChange；
//! - 选区底部全平坦（无可跟踪特征）→ NoChange。

use crate::{RecordError, Result};

/// 匹配块行数：区域高度的 1/6，钳在 [16, 96]——太小易误匹配，
/// 太大浪费比对且平滑滚动中块内变化过大
fn block_rows(h: u32) -> usize {
    (h as usize / 6).clamp(16, 96)
}

/// 匹配验证 SAD/像素 阈值：只对**有纹理的行**取平均，亚像素 AA 残差
/// 集中在文字行上（~10/255 量级，真机探针实测），比整块平均更宽松
const MATCH_TOL: u32 = 16;
/// 锚行/验证的行纹理门槛（行签名的平均绝对偏差）：平坦行（纯色背景、
/// 空隙）≈0，文字/控件行几十。低于它的行不作锚、不参与验证
const FLAT_MAD: u32 = 6;
/// 验证可信的最低纹理行数：重叠中可对齐的纹理行少于此值时该位移假设
/// 不可信（大面积平坦内容上瞎猜的位移也能局部吻合）
const MIN_VERIFY: u64 = 8;
/// 锚行数量上限：自底部向上取最深的若干个有纹理行。注意拼接图长过帧
/// （len > h）后，最深几行的真实位置可能已滚出帧视口（p > h−1），
/// 可用锚在更深处的窗口里——数量要留够
const MAX_ANCHORS: usize = 24;
/// 签名行预筛阈值（单行比整幅验证更「脆」，放宽）
const ROW_TOL: u32 = 10;
/// 列降采样步长（像素）
const COL_STRIDE: usize = 4;

/// 一帧的降采样亮度签名：每行按 COL_STRIDE 跨步取亮度字节（覆盖整行宽度），
/// 行间做 [1,2,1]/4 垂直平滑。平滑是必须的：浏览器平滑滚动经常停在**亚像素**
/// 偏移上，同一内容行的文字边缘抗锯齿逐帧微变，精确行匹配时灵时不灵
/// （真机探针 2026-09-19：同一选区同一格数，尾块落点不同分别得到
/// Appended(168)/Appended(16)/Mismatch 三种结果）；±1px 的 AA 漂移被
/// 平滑吸收后，块级 SAD 才能稳定锁定真实位置。首末行复制边界。
fn frame_signature(rgba: &[u8], w: u32, h: u32) -> Vec<Vec<u8>> {
    let w = w as usize;
    let raw: Vec<Vec<u8>> = rgba
        .chunks_exact(w * 4)
        .map(|row_px| {
            row_px
                .as_chunks::<4>()
                .0
                .iter()
                .step_by(COL_STRIDE)
                .map(|px| ((px[0] as u32 * 3 + px[1] as u32 * 6 + px[2] as u32) / 10) as u8)
                .collect()
        })
        .collect();
    let mut rows = Vec::with_capacity(h as usize);
    for r in 0..raw.len() {
        let (up, down) = (
            if r > 0 { &raw[r - 1] } else { &raw[r] },
            if r + 1 < raw.len() {
                &raw[r + 1]
            } else {
                &raw[r]
            },
        );
        rows.push(
            up.iter()
                .zip(raw[r].iter())
                .zip(down.iter())
                .map(|((a, b), c)| ((*a as u32 + 2 * *b as u32 + *c as u32) / 4) as u8)
                .collect(),
        );
    }
    rows
}

fn sad_row(a: &[u8], b: &[u8]) -> u32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs())
        .sum()
}

/// 行签名纹理度：平均绝对偏差（MAD）。平坦行 ≈0，文字/控件行几十
fn row_mad(row: &[u8]) -> u32 {
    if row.is_empty() {
        return 0;
    }
    let n = row.len() as u64;
    let mean = row.iter().map(|&b| b as u64).sum::<u64>() / n;
    (row.iter()
        .map(|&b| (b as i64 - mean as i64).unsigned_abs())
        .sum::<u64>()
        / n) as u32
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
    /// 每行的降采样亮度签名（与 image 同步增长，位移估计用）
    sig: Vec<Vec<u8>>,
    /// 与 sig 同步的每行纹理度（row_mad）
    mad: Vec<u32>,
    block: usize,
    /// 首帧高度（max_shift 用；block 也按它取）
    first_h: u32,
}

impl ScrollStitcher {
    /// 用首帧初始化
    pub fn new(rgba: &[u8], w: u32, h: u32) -> Result<Self> {
        // 高度下限 20：匹配块最小 16 行 + 锚行取倒数第二行需要余量
        if w == 0 || h < 20 || rgba.len() != (w * h * 4) as usize {
            return Err(RecordError("首帧尺寸/数据无效（高度需 ≥20px）".into()));
        }
        let sig = frame_signature(rgba, w, h);
        let mad = sig.iter().map(|r| row_mad(r)).collect();
        Ok(Self {
            width: w,
            image: rgba.to_vec(),
            sig,
            mad,
            block: block_rows(h),
            first_h: h,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    /// 帧间可匹配的最大内容位移（px）：超过它时上一帧的尾块整块滚出
    /// 新帧 → Mismatch。滚动调用方据此控制每步滚轮格数（闭环校准）。
    pub fn max_shift(&self) -> u32 {
        self.first_h.saturating_sub(self.block as u32)
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
        let frame_mad: Vec<u32> = frame_sig.iter().map(|r| row_mad(r)).collect();
        let len_prev = self.sig.len();
        if frame_sig.first().is_none_or(|r| r.is_empty()) {
            return Ok(ScrollOutcome::Mismatch);
        }
        let cols = frame_sig[0].len();

        // 锚行：除帧缝行（末行，其平滑用边界复制）外，把有纹理的行**按
        // 深度均匀采样**最多 MAX_ANCHORS 个——只取最深的几个会漏掉大位移
        // 的真匹配（拼接图长过帧后，深处的锚才能见证大位移），均匀采样
        // 对小/大位移都有覆盖。位移不设硬上限：正确性由全幅重叠验证
        // （纹理行数 ≥ MIN_VERIFY 且 SAD/px ≤ MATCH_TOL）保证。
        let mut textured: Vec<usize> = Vec::new();
        for i in (0..self.sig.len().saturating_sub(1)).rev() {
            if self.mad[i] >= FLAT_MAD {
                textured.push(i);
            }
        }
        let anchors: Vec<usize> = if textured.len() <= MAX_ANCHORS {
            textured
        } else {
            let n = textured.len();
            (0..MAX_ANCHORS)
                .map(|k| textured[k * n / MAX_ANCHORS])
                .collect()
        };
        if anchors.is_empty() {
            // 拼接图全平坦：无可跟踪特征（选区压着纯色内容）
            return Ok(ScrollOutcome::NoChange);
        }

        // 位移假设 × 全幅重叠验证（只计两侧都有纹理的行）
        let mut best: Option<(usize, u64, u64)> = None; // (S, ΣSAD, 纹理行数)
        for &i in &anchors {
            let anchor = &self.sig[i];
            for (p, frow) in frame_sig.iter().enumerate() {
                if p > i {
                    continue; // 仅接受向下滚动（S = i−p ≥ 0）
                }
                if frame_mad[p] < FLAT_MAD {
                    continue;
                }
                if sad_row(&anchor[..cols], &frow[..cols]) / cols as u32 > ROW_TOL {
                    continue;
                }
                let s = i - p;
                let mut sad = 0u64;
                let mut rows = 0u64;
                for r in 0..h as usize {
                    let j = r + s;
                    if j >= self.sig.len() {
                        break;
                    }
                    if frame_mad[r] < FLAT_MAD || self.mad[j] < FLAT_MAD {
                        continue;
                    }
                    sad += sad_row(&frame_sig[r][..cols], &self.sig[j][..cols]) as u64;
                    rows += 1;
                }
                if rows < MIN_VERIFY {
                    continue;
                }
                // 验证 SAD/px 最小者；并列取更小 S（保守少拼）
                if best.is_none_or(|(bs, bsad, brows)| {
                    let per = sad / (rows * cols as u64);
                    let bper = bsad / (brows * cols as u64);
                    per < bper || (per == bper && s < bs)
                }) {
                    best = Some((s, sad, rows));
                }
            }
        }
        let Some((s, sad, rows)) = best else {
            return Ok(ScrollOutcome::Mismatch);
        };
        if sad / (rows * cols as u64) > MATCH_TOL as u64 {
            return Ok(ScrollOutcome::Mismatch);
        }
        // s = 本帧视口顶在拼接图坐标中的位置（帧行 r ↔ 拼接行 r+s）。
        // 视口底（拼接图末行 len-1）在本帧中的行号 = len-1-s，其后才是
        // 新进入视口的内容：追加行数 = h − (len−s)，s 本身不是追加量
        // （首帧 len=h 时两者相等，拼接图长过帧后不同）
        let new_start = len_prev.saturating_sub(s);
        // 视口底前进量 = h + s − len（s < len−h 时视口顶未越过上次位置，
        // 前进量为 0 → NoChange）
        let new_rows = (h as usize + s).saturating_sub(len_prev);
        if new_rows == 0 {
            return Ok(ScrollOutcome::NoChange);
        }
        // 拟追加的 new_rows 行若与拼接图底部**同视口位置**的已有行一致，
        // 说明视口底没有前进（粘性镶边 / 内容未实际进入）：不追加，避免
        // 把固定镶边逐帧重复拼进长图。正常滚动时两段内容不同，不受影响。
        let mut band_static = true;
        for r in 0..new_rows {
            let frow = &frame_sig[new_start + r];
            let grow = &self.sig[len_prev - new_rows + r];
            if sad_row(&frow[..cols], &grow[..cols]) / cols as u32 > MATCH_TOL {
                band_static = false;
                break;
            }
        }
        if band_static {
            return Ok(ScrollOutcome::NoChange);
        }
        let byte_off = new_start * self.width as usize * 4;
        self.image.extend_from_slice(&rgba[byte_off..]);
        self.sig.extend_from_slice(&frame_sig[new_start..]);
        self.mad.extend_from_slice(&frame_mad[new_start..]);
        Ok(ScrollOutcome::Appended(new_rows as u32))
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

    /// 选区底部压着**纯色**固定镶边（输入条/工具栏底衬）时：内容滚动、
    /// 镶边不动——位移估计仍能锁定（纯色行不参与验证），但拟追加的行
    /// 全是镶边（band-static）→ 恒 NoChange 不污染长图。文档化该已知
    /// 限制（上层表现为「画面在滚动但拼接无进展」提示）
    #[test]
    fn flat_footer_no_progress() {
        let (w, h) = (64usize, 120usize);
        const FOOTER: usize = 40; // ≥ 匹配块 k = 120/6 = 20
                                  // 底部 FOOTER 行纯色（页脚），上方内容行随 top 偏移滚动
        let compose = |top: usize| -> Vec<u8> {
            let mut f = Vec::with_capacity(w * h * 4);
            for y in 0..h - FOOTER {
                f.extend_from_slice(&source_line(top + y, w));
            }
            for _ in 0..FOOTER {
                for _ in 0..w {
                    f.extend_from_slice(&[77, 77, 77, 255]);
                }
            }
            f
        };
        let mut st = ScrollStitcher::new(&compose(0), w as u32, h as u32).unwrap();
        assert_eq!(
            st.push(&compose(10), w as u32, h as u32).unwrap(),
            ScrollOutcome::NoChange
        );
        assert_eq!(
            st.push(&compose(30), w as u32, h as u32).unwrap(),
            ScrollOutcome::NoChange
        );
        // 内容确实滚动了，但拼接高度纹丝不动
        assert_eq!(st.height(), h as u32);
    }

    /// 底部固定镶边**带纹理**（横向渐变模拟文字/图标）时：镶边行的纹理
    /// 度高、会参与位移假设，但内容滚动 + 镶边不动仍被 band-static 拦截
    /// → NoChange 不污染长图（与纯色镶边同结局、不同内部路径）
    #[test]
    fn textured_footer_no_progress() {
        let (w, h) = (64usize, 120usize);
        let frame = |top: usize| -> Vec<u8> {
            let mut f = Vec::with_capacity(w * h * 4);
            for y in 0..80 {
                f.extend_from_slice(&source_line(top + y, w));
            }
            for _ in 0..40 {
                f.extend_from_slice(&source_line(10_000, w));
            }
            f
        };
        let mut st = ScrollStitcher::new(&frame(0), w as u32, h as u32).unwrap();
        // 结局在 NoChange/Mismatch 间取决于位移对齐细节——「内容滚动 +
        // 镶边不动」下刚性位移模型本无稳定解。产品保证的不变量 = 镶边
        // 绝不被当新内容拼入长图（高度不增长），且逐帧优雅停止
        for top in [15usize, 30] {
            if let ScrollOutcome::Appended(n) = st.push(&frame(top), w as u32, h as u32).unwrap() {
                panic!("固定镶边被当作新内容拼入（{n} 行）");
            }
        }
        assert_eq!(st.height(), h as u32);
    }
}
