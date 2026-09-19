//! 录屏音频：平台采集/编码 + 共享的对齐混写与收尾对账。
//!
//! 三平台同一外部接口（[`Pipeline`]）：
//!
//! - Linux（`linux.rs`）：子进程全链路（arecord/parec 采集 + ffmpeg AAC），
//!   零链接依赖（M14 方案 A）
//! - Windows（`win.rs`）：WASAPI 采集（麦克风 + loopback 系统声）+
//!   Media Foundation AAC 编码，全部系统 API
//! - macOS（`mac.rs`）：CoreAudio HAL 麦克风 + ScreenCaptureKit 系统声
//!   （macOS 13+）+ AudioToolbox AAC，全部系统框架
//!
//! 共享语义（各平台必须一致）：
//!
//! - 固定输出 AAC-LC 48kHz 立体声、128kbps、1024 样本/帧（音轨 timescale
//!   = 采样率，一帧 = 1024 tick，与 mp4 混流层的打点假设一致）
//! - **A/V 对齐**：不信任采集端起流时刻——armed 阶段预热，零点前 PCM
//!   丢弃；零点后首块晚到按到达时刻补等长静音，音轨时间 0 恒对齐视频
//!   时间 0（[`AlignMixer`] 承载，细节见其文档）
//! - 工具/设备初始化失败 = 开录前硬错误；运行中故障只告警不毁视频
//! - 收尾对账（[`Core::finalize`]）：零数据 / 音轨比视频短 >1.5s 提示

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::AudioSource;

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod mac;
#[cfg(target_os = "windows")]
pub(crate) mod win;

#[cfg(target_os = "linux")]
pub(crate) use linux::Pipeline;
#[cfg(target_os = "macos")]
pub(crate) use mac::Pipeline;
#[cfg(target_os = "windows")]
pub(crate) use win::Pipeline;

/// 固定音频参数（整条管线写死，两端格式与此一致）
pub(crate) const RATE: u32 = 48_000;
/// AAC-LC 每帧 1024 样本（音轨 timescale = 采样率时，一帧 = 1024 tick）
pub(crate) const SAMPLES_PER_FRAME: u32 = 1024;
/// AAC 目标码率（kbps）
pub(crate) const BITRATE_KBPS: u32 = 128;
/// 每秒 PCM 字节数（s16 立体声）：静音补齐换算用
pub(crate) const BYTES_PER_SEC: usize = RATE as usize * 2 /* 声道 */ * 2 /* s16 */;

/// AAC 帧编码参数（mp4 AacConfig 由此构造；各平台编码器输出一致）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AacMeta {
    /// AudioObjectType（AAC-LC = 2）
    pub aot: u8,
    /// 采样率索引（3 = 48000）
    pub freq_index: u8,
    /// 声道配置（2 = 立体声）
    pub chan_conf: u8,
}

/// 全管线的固定编码参数（AAC-LC 48k 立体声）。Linux 走 ffmpeg ADTS 路径
/// （元信息解析自帧头）不用它，仅 Win/mac 编码线程使用
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub(crate) const FIXED_META: AacMeta = AacMeta {
    aot: 2,
    freq_index: 3,
    chan_conf: 2,
};

/// ADTS 采样率索引 → Hz（mp4 音轨 timescale 用；Linux ffmpeg 路径解析自
/// ADTS 头，Win/mac 编码器参数固定不走此表的非法段）
pub(crate) fn freq_hz(index: u8) -> Option<u32> {
    Some(match index {
        0 => 96_000,
        1 => 88_200,
        2 => 64_000,
        3 => 48_000,
        4 => 44_100,
        5 => 32_000,
        6 => 24_000,
        7 => 22_050,
        8 => 16_000,
        9 => 12_000,
        10 => 11_025,
        11 => 8_000,
        12 => 7_350,
        _ => return None,
    })
}

/// 混写线程消息：PCM 块（带采集端到达时刻）或某源结束
pub(crate) enum ToMixer {
    Pcm(usize, Vec<u8>, Instant),
    Eof(usize),
}

// ---------------------------------------------------------------- 对齐混写

/// 双源按立体声采样（4 字节）饱和混合公共前缀，写入 out
fn mix_pending(a: &mut VecDeque<u8>, b: &mut VecDeque<u8>, out: &mut Vec<u8>) {
    while a.len() >= 4 && b.len() >= 4 {
        let l = i16::from_le_bytes([a[0], a[1]]).saturating_add(i16::from_le_bytes([b[0], b[1]]));
        let r = i16::from_le_bytes([a[2], a[3]]).saturating_add(i16::from_le_bytes([b[2], b[3]]));
        a.drain(..4);
        b.drain(..4);
        out.extend_from_slice(&l.to_le_bytes());
        out.extend_from_slice(&r.to_le_bytes());
    }
}

/// 混写线程主体（三平台共用）：消费采集线程的 PCM 消息，经 [`AlignMixer`]
/// 对齐/混合后交给 sink（Linux = ffmpeg stdin 写入；Win/mac = 编码线程通道）。
/// sink 返回 false 表示编码端已死：继续排空通道丢弃 PCM 防无界积压，
/// 错误记入 err 槽（沿用 Linux 语义）。
pub(crate) fn run_mixer(
    pcm_rx: Receiver<ToMixer>,
    mut sink: impl FnMut(&[u8]) -> bool,
    origin: Arc<Mutex<Option<Instant>>>,
    err: Arc<Mutex<Option<String>>>,
    flowed: Arc<AtomicBool>,
    nsrc: usize,
    err_text: String,
) {
    let mut mixer = AlignMixer::new(nsrc, BYTES_PER_SEC);
    let mut broken = false;
    while let Ok(msg) = pcm_rx.recv() {
        let out = match msg {
            ToMixer::Pcm(i, data, at) => {
                let origin = *origin.lock().unwrap();
                mixer.push_pcm(i, &data, at, origin)
            }
            ToMixer::Eof(i) => Some(mixer.eof(i)),
        };
        if mixer.any_flowing() {
            flowed.store(true, Ordering::Relaxed);
        }
        if broken {
            mixer.discard();
            continue;
        }
        if let Some(out) = out {
            if !out.is_empty() && !sink(&out) {
                let mut slot = err.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(err_text.clone());
                }
                broken = true;
            }
        }
    }
    // 采集端全部退出（消息循环结束）：drop(sink) 由闭包析构完成
}

/// 零点对齐 + （可选）双源饱和混合的状态机（跨平台共用）。
///
/// - 零点（origin）未定时 [`push_pcm`] 返回 `None`：armed 预热期的 PCM
///   直接丢弃；
/// - 零点后某源**首块**晚到（设备起流延迟，如 PipeWire parec ~1.9s）：
///   按「到达时刻 − 零点」补等长静音（钳 60s 防时钟异常撑爆内存），
///   使该源内容从零点起占位——音轨时间 0 恒对齐视频时间 0；
/// - 单源：PCM 直通；双源：按立体声采样饱和混合，一方 EOF 后另一方
///   直通，EOF 源的尾巴裁到帧边界（< 一帧的残余丢弃）。
pub(crate) struct AlignMixer {
    nsrc: usize,
    bytes_per_sec: usize,
    /// 每源状态：false = 尚未见到零点后的首个 PCM 块
    flowing: Vec<bool>,
    pending: Vec<VecDeque<u8>>,
    alive: Vec<bool>,
}

impl AlignMixer {
    pub(crate) fn new(nsrc: usize, bytes_per_sec: usize) -> Self {
        Self {
            nsrc,
            bytes_per_sec,
            flowing: vec![false; nsrc],
            pending: (0..nsrc).map(|_| VecDeque::new()).collect(),
            alive: vec![true; nsrc],
        }
    }

    /// 是否已有任何源见到零点后的数据（零数据告警用）
    pub(crate) fn any_flowing(&self) -> bool {
        self.flowing.iter().any(|&f| f)
    }

    /// 编码器已死时的丢弃通道：清空全部待混合数据防无界积压
    pub(crate) fn discard(&mut self) {
        for p in &mut self.pending {
            p.clear();
        }
    }

    /// 处理一块 PCM。零点未定 → `None`（丢弃）；否则并入并返回本轮可送
    /// 编码器的混合 PCM（可能为空）
    pub(crate) fn push_pcm(
        &mut self,
        i: usize,
        data: &[u8],
        at: Instant,
        origin: Option<Instant>,
    ) -> Option<Vec<u8>> {
        let origin = origin?;
        if !self.flowing[i] {
            self.flowing[i] = true;
            // 起流晚于零点：补等长静音让该源内容从零点起占位。
            // **字节数必须向 4 字节（一帧 s16 立体声）取整**：任意长度的
            // pad 会让后续块错位 1-3 字节，整条流变成满幅噪声（真机点验
            // 抓到过 1164139 字节的奇数 mix 输出——v0.10.x 三平台通病，
            // 偶发条件 = pad 字节非 4 倍数（~75% 概率），静音回录 e2e 只
            // 验帧数不验内容故未暴露）
            let gap = at.saturating_duration_since(origin);
            let secs = gap.as_secs_f64().min(60.0);
            if secs > 0.02 {
                let bytes = ((secs * self.bytes_per_sec as f64) as usize) & !3;
                self.pending[i].extend(std::iter::repeat_n(0u8, bytes));
            }
        }
        self.pending[i].extend(data.iter().copied());
        Some(self.drain_mixed())
    }

    /// 某源 EOF：残留与对方现存数据混合到帧边界，对不齐的尾巴丢弃；
    /// 返回本轮可送编码器的 PCM（含幸存者直通部分）
    pub(crate) fn eof(&mut self, i: usize) -> Vec<u8> {
        self.alive[i] = false;
        let mut out = Vec::new();
        if self.nsrc == 2 {
            let (a, b) = self.pending.split_at_mut(1);
            mix_pending(&mut a[0], &mut b[0], &mut out);
            self.pending[i].clear();
            out.extend(self.drain_mixed());
        }
        // 单源：历次 push 已直通排空，无尾巴
        out
    }

    fn drain_mixed(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.nsrc == 1 {
            out.extend(self.pending[0].drain(..));
        } else {
            let (a, b) = self.pending.split_at_mut(1);
            mix_pending(&mut a[0], &mut b[0], &mut out);
            // 一方结束后另一方直通
            if !self.alive[0] {
                out.extend(self.pending[1].drain(..));
            }
            if !self.alive[1] {
                out.extend(self.pending[0].drain(..));
            }
        }
        out
    }
}

// ---------------------------------------------------------------- 采集格式转换

/// f32 [-1,1] → s16（钳位；Win/mac 采集格式转换终点）
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
fn to_s16(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// WASAPI/CoreAudio 的采集格式（f32 交错、原生采样率/声道数）→ 管线固定
/// 格式（s16le/48k/立体声）转换器。纯 Rust、无平台依赖，跨块保持插值
/// 连续（残差携带），Win/mac 共用（含单测，全部平台跑）。
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub(crate) struct ToStereo48 {
    src_ch: usize,
    /// 每产出一帧需要推进的输入帧数（src_rate/48000）
    step: f64,
    /// 残差内的分数读位置（帧）
    pos: f64,
    /// 未消费的输入帧（交错存放；线性插值需要「当前+下一」两帧）
    residual: Vec<f32>,
}

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
impl ToStereo48 {
    pub(crate) fn new(src_rate: u32, src_ch: usize) -> Self {
        Self {
            src_ch,
            step: src_rate as f64 / RATE as f64,
            pos: 0.0,
            residual: Vec::new(),
        }
    }

    /// 喂入一段 f32 交错帧，产出 s16le/48k/立体声字节
    pub(crate) fn push(&mut self, input: &[f32]) -> Vec<u8> {
        self.residual.extend_from_slice(input);
        let frames = self.residual.len() / self.src_ch;
        let mut out = Vec::new();
        while self.pos + 1.0 < frames as f64 {
            let i = self.pos.floor() as usize;
            let f = (self.pos - i as f64) as f32;
            let cur = &self.residual[i * self.src_ch..];
            let nxt = &self.residual[(i + 1) * self.src_ch..];
            let frame: Vec<f32> = (0..self.src_ch)
                .map(|c| cur[c] + (nxt[c] - cur[c]) * f)
                .collect();
            // 降混立体声：单声道复制、多声道取前两个（FL/FR），屏幕录制场景
            // 丢弃其余声道
            let (l, r) = match self.src_ch {
                1 => (frame[0], frame[0]),
                _ => (frame[0], frame[1]),
            };
            out.extend_from_slice(&to_s16(l).to_le_bytes());
            out.extend_from_slice(&to_s16(r).to_le_bytes());
            self.pos += self.step;
        }
        let consumed = self.pos.floor() as usize;
        self.residual.drain(..consumed * self.src_ch);
        self.pos -= consumed as f64;
        out
    }

    /// 静音包（WASAPI 报 SILENT 标志时）：输出 frames 帧零字节。mac 的
    /// IOProc 恒给真实缓冲（静音 = 零样本），不用此路径
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn silence(&self, frames: usize) -> Vec<u8> {
        vec![0u8; frames * 4]
    }
}

// ---------------------------------------------------------------- 管线核心

/// 各平台 [`Pipeline`] 的共享骨架：AAC 帧出口、零点槽、错误槽与收尾对账。
/// 平台层负责采集/编码线程的启动与停止，通过它发帧/报错/对账。
pub(crate) struct Core {
    pub(crate) source: AudioSource,
    pub(crate) frames_rx: Receiver<(AacMeta, Vec<u8>)>,
    /// 录制零点（视频时间 0）；armed 预热期间为 None，混写线程丢弃 PCM
    pub(crate) origin: Arc<Mutex<Option<Instant>>>,
    /// 运行期首个错误（编码器写失败/读失败）
    pub(crate) err: Arc<Mutex<Option<String>>>,
    /// 平台编码器诊断信息槽（Linux = ffmpeg stderr 首段；Win/mac 预留）
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) ffmpeg_stderr: Arc<Mutex<String>>,
    /// 零点后是否有 PCM 进过编码器（录得比起流延迟还短 = 零音轨，需提示）
    pub(crate) flowed: Arc<AtomicBool>,
    /// 编码线程累计送出的 AAC 帧数（收尾时与视频时长对账）
    pub(crate) frame_count: Arc<AtomicUsize>,
    pub(crate) workers: Vec<std::thread::JoinHandle<()>>,
    pub(crate) finished: bool,
}

impl Core {
    /// 建好共享骨架与帧通道（返回核心 + 帧发送端，交给平台编码线程）
    pub(crate) fn new(source: AudioSource) -> (Self, Sender<(AacMeta, Vec<u8>)>) {
        let (frame_tx, frame_rx) = channel();
        (
            Self {
                source,
                frames_rx: frame_rx,
                origin: Arc::new(Mutex::new(None)),
                err: Arc::new(Mutex::new(None)),
                ffmpeg_stderr: Arc::new(Mutex::new(String::new())),
                flowed: Arc::new(AtomicBool::new(false)),
                frame_count: Arc::new(AtomicUsize::new(0)),
                workers: Vec::new(),
                finished: false,
            },
            frame_tx,
        )
    }

    /// 设定录制零点（视频时间 0）。此前的 PCM 丢弃；此后首块晚到补静音
    pub(crate) fn set_origin(&self, t0: Instant) {
        *self.origin.lock().unwrap() = Some(t0);
    }

    /// 非阻塞拉取已编码完成的 AAC 帧
    pub(crate) fn pull(&mut self) -> Vec<(AacMeta, Vec<u8>)> {
        let mut frames = Vec::new();
        while let Ok(f) = self.frames_rx.try_recv() {
            frames.push(f);
        }
        frames
    }

    /// join 全部线程 + 音轨时长对账（平台先完成采集停止与编码器冲刷再调）。
    /// 预期 = 零点至今；实际 = 已编码帧数。零数据或大缺口提示用户，
    /// 视频本体不受影响
    pub(crate) fn finalize(&mut self) -> (Vec<(AacMeta, Vec<u8>)>, Option<String>) {
        self.finished = true;
        let frames = self.pull();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
        let warn = self.err.lock().unwrap().take();
        if let Some(expected) = self.origin.lock().unwrap().map(|t0| t0.elapsed()) {
            let actual = Duration::from_secs_f64(
                self.frame_count.load(Ordering::Relaxed) as f64 * SAMPLES_PER_FRAME as f64
                    / RATE as f64,
            );
            if !self.flowed.load(Ordering::Relaxed) && warn.is_none() {
                return (
                    frames,
                    Some("未采集到音频数据（录制时长短于采集设备起流时间？）已按无音轨保存".into()),
                );
            }
            if expected.saturating_sub(actual) > Duration::from_millis(1500) && warn.is_none() {
                let deficit = (expected - actual).as_secs_f32();
                return (
                    frames,
                    Some(format!("音频轨比视频短约 {deficit:.1}s（采集中断？）")),
                );
            }
        }
        (frames, warn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixer_single_source_passthrough_and_drop_before_origin() {
        let mut m = AlignMixer::new(1, BYTES_PER_SEC);
        let t0 = Instant::now();
        // 零点未定：丢弃
        assert!(m.push_pcm(0, &[1, 2, 3, 4], t0, None).is_none());
        // 零点后首块即刻到达：不补静音，直通
        let out = m.push_pcm(0, &[1, 2, 3, 4], t0, Some(t0)).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4]);
        assert!(m.any_flowing());
        // EOF：单源无尾巴
        assert!(m.eof(0).is_empty());
    }

    #[test]
    fn mixer_late_first_chunk_pads_silence() {
        let mut m = AlignMixer::new(1, BYTES_PER_SEC);
        let t0 = Instant::now();
        let late = t0 + Duration::from_millis(500);
        // 首块晚到 0.5s：补 0.5s 静音 + 数据本体
        let out = m.push_pcm(0, &[9, 9, 9, 9], late, Some(t0)).unwrap();
        // 静音部分（约 48000*4*0.5 字节，Instant 精度内）+ 数据 4 字节
        assert!(out.len() > BYTES_PER_SEC / 4, "应有静音补齐: {}", out.len());
        assert!(out.len() <= BYTES_PER_SEC / 2 + 8192);
        // pad 必须帧对齐（4 字节）：错位会让后续整条流变成满幅噪声
        assert_eq!(out.len() % 4, 0, "静音补齐未按帧对齐: {}", out.len());
        assert_eq!(&out[out.len() - 4..], &[9, 9, 9, 9]);
        // 静音本体：除最后 4 字节外全零
        assert!(out[..out.len() - 4].iter().all(|&b| b == 0));
    }

    #[test]
    fn mixer_pad_is_frame_aligned_for_any_delay() {
        // 任意延迟下 pad 都不能破坏帧对齐（真机曾以奇数字节 pad 引发
        // 整流错位噪声，~75% 概率触发）
        for ms in [21, 33, 100, 555, 1234, 1900] {
            let mut m = AlignMixer::new(1, BYTES_PER_SEC);
            let t0 = Instant::now();
            let out = m
                .push_pcm(0, &[8, 8, 8, 8], t0 + Duration::from_millis(ms), Some(t0))
                .unwrap();
            assert_eq!(out.len() % 4, 0, "{ms}ms 延迟 pad 未对齐: {}", out.len());
            assert_eq!(&out[out.len() - 4..], &[8, 8, 8, 8]);
        }
    }

    #[test]
    fn mixer_two_source_mix_and_survivor_passthrough() {
        let mut m = AlignMixer::new(2, BYTES_PER_SEC);
        let t0 = Instant::now();
        let a = vec![0x10u8; 8]; // 2 采样
        let b = vec![0x20u8; 4]; // 1 采样
        let _ = m.push_pcm(0, &a, t0, Some(t0)).unwrap();
        let out = m.push_pcm(1, &b, t0, Some(t0)).unwrap();
        // 公共前缀 1 采样（4 字节）饱和混合
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], 0x10u8.saturating_add(0x20));
        // 源 0 EOF：其亚帧尾巴（剩余 1 采样）裁掉，本轮无可输出
        assert_eq!(m.eof(0), Vec::<u8>::new());
        // 此后幸存者直通（不再与已死源混合）
        let out = m.push_pcm(1, &[0x30; 8], t0, Some(t0)).unwrap();
        assert_eq!(out, vec![0x30; 8]);
    }

    #[test]
    fn converter_48k_passthrough_mono_dup() {
        // 48k 立体声直通：一次一帧残差（插值需要下一帧），后续每帧产出
        let mut c = ToStereo48::new(RATE, 2);
        let f = |l: f32, r: f32| vec![l, r];
        let out1 = c.push(&f(0.25, -0.5));
        assert_eq!(out1.len(), 0, "单帧无法插值，先缓存");
        let out2 = c.push(&f(0.0, 0.0));
        assert_eq!(out2.len(), 4, "第二帧到达后产出第一帧");
        let l = i16::from_le_bytes([out2[0], out2[1]]);
        let r = i16::from_le_bytes([out2[2], out2[3]]);
        assert!((8190..=8192).contains(&l), "{l}");
        assert!((-16384..=-16382).contains(&r), "{r}");

        // 单声道复制到双声道
        let mut c = ToStereo48::new(RATE, 1);
        let _ = c.push(&[0.5]);
        let out = c.push(&[0.5]);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0..2], out[2..4], "左右声道相同");
    }

    #[test]
    fn converter_resample_ratio() {
        // 24k → 48k：输出帧数 ≈ 输入 × 2
        let mut c = ToStereo48::new(24_000, 2);
        let input: Vec<f32> = (0..240).flat_map(|i| [i as f32 / 1000.0, -0.5]).collect();
        let out = c.push(&input);
        let ratio = out.len() as f64 / 4.0 / 240.0;
        assert!((ratio - 2.0).abs() < 0.05, "比例 {ratio}");
        // 96k → 48k：输出 ≈ 输入 × 0.5
        let mut c = ToStereo48::new(96_000, 1);
        let input: Vec<f32> = (0..960).map(|i| (i % 100) as f32 / 100.0).collect();
        let out = c.push(&input);
        let ratio = out.len() as f64 / 4.0 / 960.0;
        assert!((ratio - 0.5).abs() < 0.05, "比例 {ratio}");
    }

    #[test]
    fn converter_chunk_split_continuity() {
        // 分块喂入与整段喂入输出一致（残差跨块保持插值连续）
        let input: Vec<f32> = (0..500)
            .flat_map(|i| [(i % 7) as f32 / 7.0, -0.3])
            .collect();
        let mut whole = ToStereo48::new(44_100, 2);
        let a = whole.push(&input);
        let mut split = ToStereo48::new(44_100, 2);
        let mut b = Vec::new();
        for chunk in input.chunks(37) {
            b.extend(split.push(chunk));
        }
        assert_eq!(a, b);
    }

    #[test]
    fn converter_silence_length() {
        let c = ToStereo48::new(RATE, 2);
        assert_eq!(c.silence(10).len(), 40);
        assert!(c.silence(10).iter().all(|&x| x == 0));
    }
}
