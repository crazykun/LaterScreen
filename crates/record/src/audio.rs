//! Linux 录屏音频（M14 方案 A：子进程全链路，零链接依赖）。
//!
//! 采集端输出统一 raw S16LE/48k/立体声 PCM：
//!
//! - 麦克风：`arecord -D default`（实测起流 ~30ms，首选）→ `parec`
//!   （pipewire-pulse 兼容层，起流实测 ~1.9s，兜底）
//! - 系统声：`parec -d @DEFAULT_MONITOR@`（默认输出设备的回录监视源）
//!
//! 编码端：`ffmpeg` 子进程 PCM → AAC-LC ADTS 裸流（stdout 管道），
//! 混流时剥 ADTS 头、按 1024 样本/帧打时间戳。
//!
//! 与 OCR 的 tesseract 同款模式：系统工具以子进程调用（无链接型依赖，
//! 产物仍是零动态库单文件），工具缺失时启动即报错而不是录完才发现没声。
//!
//! A/V 对齐（关键设计）：PipeWire 下 parec 起流有秒级延迟，arecord 也非
//! 零延迟，因此**不信任采集端的起流时刻**——采集子进程在 armed 阶段就
//! 预热 spawn，混写线程丢弃录制零点之前的 PCM；零点之后的首块到达时若
//! 晚了（工具还没起流），按「首块到达时刻 − 零点」补等长静音，音轨时间 0
//! 恒对齐视频时间 0，误差 ≤ 一个读块（4KB = 21ms = 一帧 AAC）。

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{AudioSource, RecordError};

/// 固定音频参数（整条管线写死，两端工具参数与此一致）
pub(crate) const RATE: u32 = 48_000;
/// AAC-LC 每帧 1024 样本（音轨 timescale = 采样率时，一帧 = 1024 tick）
pub(crate) const SAMPLES_PER_FRAME: u32 = 1024;
/// AAC 目标码率（kbps）
pub(crate) const BITRATE_KBPS: u32 = 128;
/// 采集读块大小：恰好一帧 AAC 对应的 PCM（对齐误差 ≤ 一帧）
const READ_CHUNK: usize = SAMPLES_PER_FRAME as usize * 4;

/// 首帧 ADTS 头解析出的编码参数（mp4 AacConfig 由此构造）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AacMeta {
    /// AudioObjectType（ADTS profile + 1）
    pub aot: u8,
    /// 采样率索引（3 = 48000）
    pub freq_index: u8,
    /// 声道配置（2 = 立体声）
    pub chan_conf: u8,
}

/// ADTS 频率索引 → Hz（13/14 保留、15 显式频率——ffmpeg 不会输出，报错处理）
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
enum ToMixer {
    Pcm(usize, Vec<u8>, Instant),
    Eof(usize),
}

// ---------------------------------------------------------------- ADTS 拆帧

/// 增量 ADTS 流拆帧器：feed 字节流，逐帧取出（剥头）。
/// 失步时按同步字 0xFFF 重扫，假同步/坏长度丢字节继续。
#[derive(Default)]
struct AdtsSplitter {
    buf: Vec<u8>,
}

impl AdtsSplitter {
    fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// 取下一帧：(元信息, 裸 AAC 字节)。数据不足时 None（等待更多输入）
    fn pop_frame(&mut self) -> Option<(AacMeta, Vec<u8>)> {
        loop {
            // 同步字对齐到缓冲区头（丢弃头部的非同步字节，保留尾部可能是
            // 同步字前缀的 ≤6 字节）
            let mut sync = 0;
            while sync + 7 <= self.buf.len()
                && !(self.buf[sync] == 0xFF && self.buf[sync + 1] & 0xF0 == 0xF0)
            {
                sync += 1;
            }
            if sync > 0 {
                self.buf.drain(..sync);
            }
            if self.buf.len() < 7 {
                return None;
            }
            let b = &self.buf;
            let header_len = if b[1] & 1 == 1 { 7 } else { 9 }; // protection_absent
            let frame_len =
                (((b[3] & 0x03) as usize) << 11) | ((b[4] as usize) << 3) | (b[5] as usize >> 5);
            // AAC-LC 48k 立体声 128kbps 单帧 ≈ 350B；上限放宽到 8KB 拦假同步
            if frame_len < header_len + 1 || frame_len > 8192 {
                self.buf.remove(0);
                continue;
            }
            if self.buf.len() < frame_len {
                return None;
            }
            let meta = AacMeta {
                aot: ((b[2] >> 6) & 0x3) + 1,
                freq_index: (b[2] >> 2) & 0xF,
                chan_conf: ((b[2] & 1) << 2) | ((b[3] >> 6) & 0x3),
            };
            let frame = self.buf[header_len..frame_len].to_vec();
            self.buf.drain(..frame_len);
            return Some((meta, frame));
        }
    }
}

// ---------------------------------------------------------------- PCM 混合

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

// ---------------------------------------------------------------- 工具探测

/// 在指定 PATH 里找可执行文件（is_file + 任一执行位）
fn find_in_path(path_var: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let cand = dir.join(name);
        if cand.is_file()
            && std::fs::metadata(&cand)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        {
            return Some(cand);
        }
    }
    None
}

fn in_path(name: &str) -> bool {
    find_in_path(&std::env::var_os("PATH").unwrap_or_default(), name).is_some()
}

/// 麦克风采集命令（按可靠性排序，前者起不来自动降级后者）
fn mic_cmds() -> Vec<Command> {
    let mut cmds = Vec::new();
    // arecord 实测起流 ~30ms（pipewire-alsa 插件路由），首选
    if in_path("arecord") {
        let mut c = Command::new("arecord");
        c.args([
            "-D", "default", "-f", "S16_LE", "-r", "48000", "-c", "2", "-t", "raw",
        ]);
        cmds.push(c);
    }
    // parec：pipewire-pulse 兼容层；起流有秒级延迟但由 armed 预热 + 静音
    // 补齐兜底，且覆盖 arecord 缺失/独占失败的纯 pulse 系统
    if in_path("parec") {
        let mut c = Command::new("parec");
        c.args(["--raw", "--format=s16le", "--rate=48000", "--channels=2"]);
        cmds.push(c);
    }
    cmds
}

/// 系统声采集命令：默认输出设备的 monitor 回录源
fn system_cmds() -> Vec<Command> {
    if !in_path("parec") {
        return Vec::new();
    }
    let mut c = Command::new("parec");
    c.args([
        "--raw",
        "--format=s16le",
        "--rate=48000",
        "--channels=2",
        "-d",
        "@DEFAULT_MONITOR@",
    ]);
    vec![c]
}

/// spawn 并确认子进程真的活过 250ms（设备被占/参数被拒会立刻退出，
/// 存活检查通过才视为该候选可用，否则降级下一个）
fn spawn_alive(mut cmd: Command) -> Option<Child> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    std::thread::sleep(Duration::from_millis(250));
    match child.try_wait() {
        Ok(None) => Some(child),
        // 已退出（失败）：让 Drop 收尸
        _ => None,
    }
}

// ---------------------------------------------------------------- 管线

/// 音频管线：N 个采集子进程 → 混写线程（对齐/混合）→ ffmpeg 编码 →
/// ADTS 拆帧 → AAC 帧队列（record_mp4 主循环非阻塞拉取）
pub(crate) struct Pipeline {
    /// 本管线的采集源（armed 阶段配置热改后比对是否需要重开）
    pub(crate) source: AudioSource,
    captures: Vec<Child>,
    ffmpeg: Option<Child>,
    frames_rx: Receiver<(AacMeta, Vec<u8>)>,
    /// 录制零点（视频时间 0）；armed 预热期间为 None，混写线程丢弃 PCM
    origin: Arc<Mutex<Option<Instant>>>,
    /// 运行期首个错误（编码器写失败/读失败）
    err: Arc<Mutex<Option<String>>>,
    /// ffmpeg stderr 首段（失败诊断用）
    ffmpeg_stderr: Arc<Mutex<String>>,
    /// 零点后是否有 PCM 进过编码器（录得比起流延迟还短 = 零音轨，需提示）
    flowed: Arc<AtomicBool>,
    /// stdout 线程累计送出的 AAC 帧数（收尾时与视频时长对账）
    frame_count: Arc<AtomicUsize>,
    workers: Vec<std::thread::JoinHandle<()>>,
    finished: bool,
}

impl Pipeline {
    /// 探测并启动全部子进程。armed 阶段调用（预热），录制零点之后才有
    /// 数据进编码器。任一必需工具缺失/启动失败 = 硬错误（快速失败，
    /// 不让用户录完才发现没声）。
    pub(crate) fn start(source: AudioSource) -> Result<Self, RecordError> {
        if !in_path("ffmpeg") {
            return Err(RecordError(
                "音频录制需要系统安装 ffmpeg（未在 PATH 找到）".into(),
            ));
        }
        let candidates: Vec<Vec<Command>> = match source {
            AudioSource::Mic => vec![mic_cmds()],
            AudioSource::System => vec![system_cmds()],
            AudioSource::Both => vec![mic_cmds(), system_cmds()],
        };
        // 逐源 spawn（源内候选降级）；全部失败才报错
        let mut captures = Vec::new();
        for (i, cmds) in candidates.into_iter().enumerate() {
            let what = if i == 0 || source == AudioSource::Mic {
                "麦克风"
            } else {
                "系统声"
            };
            let mut ok = None;
            for cmd in cmds {
                if let Some(child) = spawn_alive(cmd) {
                    ok = Some(child);
                    break;
                }
            }
            captures.push(ok.ok_or_else(|| {
                RecordError(format!(
                    "启动{what}采集失败（需要 arecord/parec，且音频服务在运行）"
                ))
            })?);
        }

        let mut ffmpeg = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "s16le",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-i",
                "pipe:0",
                "-c:a",
                "aac",
                "-b:a",
                &format!("{}k", BITRATE_KBPS),
                "-f",
                "adts",
                "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RecordError(format!("启动 ffmpeg 失败: {e}")))?;
        // ffmpeg 立即失败（参数/环境问题）也快速报错
        std::thread::sleep(Duration::from_millis(150));
        if let Ok(Some(_)) = ffmpeg.try_wait() {
            return Err(RecordError("ffmpeg 启动后立即退出（编码环境异常）".into()));
        }
        let ffmpeg_stdin = ffmpeg.stdin.take().expect("stdin piped");
        let ffmpeg_stdout = ffmpeg.stdout.take().expect("stdout piped");
        let ffmpeg_stderr = ffmpeg.stderr.take().expect("stderr piped");

        let origin = Arc::new(Mutex::new(None));
        let err = Arc::new(Mutex::new(None));
        let ffmpeg_stderr_txt = Arc::new(Mutex::new(String::new()));
        let flowed = Arc::new(AtomicBool::new(false));
        let frame_count = Arc::new(AtomicUsize::new(0));
        let (pcm_tx, pcm_rx) = channel::<ToMixer>();
        let (frame_tx, frame_rx) = channel::<(AacMeta, Vec<u8>)>();
        let mut workers = Vec::new();

        // ffmpeg stderr 排空（防 64KB 管道堵死；留存首段做诊断）
        {
            let slot = Arc::clone(&ffmpeg_stderr_txt);
            workers.push(std::thread::spawn(move || {
                let mut buf = String::new();
                let mut out = ffmpeg_stderr;
                let _ = out.read_to_string(&mut buf);
                if let Ok(mut s) = slot.lock() {
                    s.truncate(0);
                    s.push_str(&buf[..buf.len().min(400)]);
                }
            }));
        }

        // 采集读线程 ×N：读块打到达时间戳，EOF/读失败发 Eof
        for (i, child) in captures.iter_mut().enumerate() {
            let mut out = child.stdout.take().expect("stdout piped");
            let tx = pcm_tx.clone();
            workers.push(std::thread::spawn(move || {
                let mut buf = [0u8; READ_CHUNK];
                loop {
                    match out.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx
                                .send(ToMixer::Pcm(i, buf[..n].to_vec(), Instant::now()))
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                let _ = tx.send(ToMixer::Eof(i));
            }));
        }
        drop(pcm_tx);

        // 混写线程：对齐（零点前丢弃/晚起流补静音）+ 双源饱和混合 → ffmpeg
        {
            let origin = Arc::clone(&origin);
            let err_slot = Arc::clone(&err);
            let flowed = Arc::clone(&flowed);
            let nsrc = captures.len();
            workers.push(std::thread::spawn(move || {
                let mut sink = ffmpeg_stdin;
                // 每源状态：false = 尚未见到零点后的首个 PCM 块
                let mut flowing = vec![false; nsrc];
                let mut pending: Vec<VecDeque<u8>> = (0..nsrc).map(|_| VecDeque::new()).collect();
                let mut alive = vec![true; nsrc];
                let mut broken = false;
                while let Ok(msg) = pcm_rx.recv() {
                    match msg {
                        ToMixer::Pcm(i, data, at) => {
                            let origin = *origin.lock().unwrap();
                            match origin {
                                // 零点未定（armed 预热）：丢弃
                                None => continue,
                                Some(t0) if !flowing[i] => {
                                    flowing[i] = true;
                                    flowed.store(true, Ordering::Relaxed);
                                    // 起流晚于零点：补等长静音，让该源内容
                                    // 从零点起占位（钳 60s 防时钟异常撑爆内存）
                                    let gap = at.saturating_duration_since(t0);
                                    let secs = gap.as_secs_f64().min(60.0);
                                    if secs > 0.02 {
                                        let bytes = (secs * RATE as f64) as usize * 4;
                                        pending[i].extend(std::iter::repeat_n(0u8, bytes));
                                    }
                                    pending[i].extend(data.iter().copied());
                                }
                                Some(_) => pending[i].extend(data.iter().copied()),
                            }
                        }
                        ToMixer::Eof(i) => {
                            alive[i] = false;
                            if nsrc == 2 {
                                // 双源模式：把残留与对方现存数据混合到帧边界，
                                // 剩下对不齐的尾巴（< 一帧）丢弃
                                let mut out = Vec::new();
                                let (a, b) = pending.split_at_mut(1);
                                mix_pending(&mut a[0], &mut b[0], &mut out);
                                if !broken && sink.write_all(&out).is_err() {
                                    broken = true;
                                }
                                pending[i].clear();
                            }
                        }
                    }
                    if broken {
                        // 编码器已死：继续排空通道丢弃 PCM（防无界积压），
                        // 细节错误已在 err 槽记录
                        for p in &mut pending {
                            p.clear();
                        }
                        continue;
                    }
                    let mut out = Vec::new();
                    if nsrc == 1 {
                        out.extend(pending[0].drain(..));
                    } else {
                        let (a, b) = pending.split_at_mut(1);
                        mix_pending(&mut a[0], &mut b[0], &mut out);
                        // 一方结束后另一方直通
                        if !alive[0] {
                            out.extend(pending[1].drain(..));
                        }
                        if !alive[1] {
                            out.extend(pending[0].drain(..));
                        }
                    }
                    if !out.is_empty() && sink.write_all(&out).is_err() {
                        let mut slot = err_slot.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some("音频编码器写入失败（ffmpeg 中途退出？）".into());
                        }
                        broken = true;
                    }
                }
                // 全部采集端结束：drop(sink) 关闭 stdin 送 EOF，ffmpeg 冲刷收尾
            }));
        }

        // ffmpeg stdout → ADTS 拆帧 → AAC 帧队列
        {
            let tx = frame_tx;
            let counter = Arc::clone(&frame_count);
            workers.push(std::thread::spawn(move || {
                let mut splitter = AdtsSplitter::default();
                let mut buf = [0u8; 16384];
                let mut src = ffmpeg_stdout;
                loop {
                    match src.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            splitter.push(&buf[..n]);
                            while let Some(frame) = splitter.pop_frame() {
                                counter.fetch_add(1, Ordering::Relaxed);
                                if tx.send(frame).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            }));
        }

        Ok(Self {
            source,
            captures,
            ffmpeg: Some(ffmpeg),
            frames_rx: frame_rx,
            origin,
            err,
            ffmpeg_stderr: ffmpeg_stderr_txt,
            flowed,
            frame_count,
            workers,
            finished: false,
        })
    }

    /// 设定录制零点（视频时间 0）。此前的 PCM 丢弃；此后首块晚到补静音。
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

    /// 停止采集 → 冲刷编码器 → 回收全部子进程与线程。
    /// 返回 (残余 AAC 帧, 运行期异常)。采集端被 SIGKILL，尾缓冲（毫秒级）
    /// 丢失可忽略。
    pub(crate) fn finish(&mut self) -> (Vec<(AacMeta, Vec<u8>)>, Option<String>) {
        if self.finished {
            return (Vec::new(), None);
        }
        self.finished = true;
        for c in &mut self.captures {
            let _ = c.kill();
            let _ = c.wait();
        }
        // stdin EOF（混写线程退出时 drop）后 ffmpeg 冲刷编码器并退出；
        // 10s 看门狗兜底防挂死拖住收尾
        let mut exit_ok = true;
        if let Some(ff) = self.ffmpeg.as_mut() {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match ff.try_wait() {
                    Ok(Some(status)) => {
                        exit_ok = status.success();
                        break;
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    _ => {
                        let _ = ff.kill();
                        let _ = ff.wait();
                        exit_ok = false;
                        break;
                    }
                }
            }
        }
        let frames = self.pull();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
        let mut warn = self.err.lock().unwrap().take();
        if !exit_ok && warn.is_none() {
            let stderr = self.ffmpeg_stderr.lock().unwrap().clone();
            let head = stderr.lines().next().unwrap_or_default();
            warn = Some(if head.is_empty() {
                "音频编码器异常退出".into()
            } else {
                format!("音频编码器异常退出: {head}")
            });
        }
        // 音轨时长对账：预期 = 零点至今，实际 = 已编码帧数。零数据
        // （录制短于采集起流延迟）或中途断流导致的大缺口都提示用户，
        // 视频本体不受影响。
        if warn.is_none() {
            if let Some(expected) = self.origin.lock().unwrap().map(|t0| t0.elapsed()) {
                let actual = Duration::from_secs_f64(
                    self.frame_count.load(Ordering::Relaxed) as f64 * SAMPLES_PER_FRAME as f64
                        / RATE as f64,
                );
                if !self.flowed.load(Ordering::Relaxed) {
                    warn = Some(
                        "未采集到音频数据（录制时长短于采集设备起流时间？）已按无音轨保存".into(),
                    );
                } else if expected.saturating_sub(actual) > Duration::from_millis(1500) {
                    let deficit = (expected - actual).as_secs_f32();
                    warn = Some(format!("音频轨比视频短约 {deficit:.1}s（采集中断？）"));
                }
            }
        }
        (frames, warn)
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        if !self.finished {
            // panic/异常路径：杀掉全部子进程防孤儿录音；不 join 线程
            //（随子进程 EOF 自行退出，进程收尾时一并消失）
            for c in &mut self.captures {
                let _ = c.kill();
                let _ = c.wait();
            }
            if let Some(ff) = self.ffmpeg.as_mut() {
                let _ = ff.kill();
                let _ = ff.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个 ADTS 帧（无 CRC）
    fn adts_frame(payload: &[u8], profile: u8, freq: u8, chan: u8) -> Vec<u8> {
        let frame_len = payload.len() + 7;
        let mut h = vec![0xFF, 0xF1]; // syncword + MPEG-4 + layer0 + protection_absent
        h.push((profile << 6) | (freq << 2) | (chan >> 2));
        h.push(((chan & 0x3) << 6) | ((frame_len >> 11) as u8));
        h.push((frame_len >> 3) as u8);
        h.push(((frame_len & 0x7) as u8) << 5);
        h.push(0x1F); // buffer fullness 低 6 位 + raw block 数
        h.extend_from_slice(payload);
        h
    }

    #[test]
    fn adts_splitter_basic() {
        let mut s = AdtsSplitter::default();
        let f1 = adts_frame(&[0x11; 300], 1, 3, 2); // LC/48k/立体声
        let f2 = adts_frame(&[0x22; 20], 1, 3, 2);
        let mut stream = f1.clone();
        stream.extend_from_slice(&f2);
        // 分三次喂入（切碎边界）
        s.push(&stream[..100]);
        assert!(s.pop_frame().is_none(), "数据不足时等待");
        s.push(&stream[100..250]);
        s.push(&stream[250..]);
        let (m1, d1) = s.pop_frame().unwrap();
        assert_eq!((m1.aot, m1.freq_index, m1.chan_conf), (2, 3, 2));
        assert_eq!(d1, vec![0x11; 300]);
        let (m2, d2) = s.pop_frame().unwrap();
        assert_eq!((m2.aot, m2.freq_index, m2.chan_conf), (2, 3, 2));
        assert_eq!(d2, vec![0x22; 20]);
        assert!(s.pop_frame().is_none());
    }

    #[test]
    fn adts_splitter_resync_and_garbage() {
        let mut s = AdtsSplitter::default();
        let f = adts_frame(&[0xAB; 100], 0, 4, 1); // Main/44.1k/单声道
        let mut stream = vec![0x00, 0x12, 0xFF, 0x00, 0x99]; // 垃圾前缀（含假同步 0xFF 0x00）
        stream.extend_from_slice(&f);
        s.push(&stream);
        let (m, d) = s.pop_frame().unwrap();
        assert_eq!((m.aot, m.freq_index, m.chan_conf), (1, 4, 1));
        assert_eq!(d, vec![0xAB; 100]);
        assert!(s.pop_frame().is_none());
    }

    #[test]
    fn mix_saturates_and_aligns() {
        // 饱和：MAX+MAX=MAX，MIN+MIN=MIN
        let mk = |l: i16, r: i16| -> VecDeque<u8> {
            let mut d = VecDeque::new();
            for v in [l, r, l, r] {
                d.extend(v.to_le_bytes());
            }
            d
        };
        let mut a = mk(i16::MAX, i16::MIN);
        let mut b = mk(i16::MAX, i16::MIN);
        let mut out = Vec::new();
        mix_pending(&mut a, &mut b, &mut out);
        let samples: Vec<i16> = out
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(samples, vec![i16::MAX, i16::MIN, i16::MAX, i16::MIN]);
        // 非饱和：正常相加
        let mut a = mk(100, -5);
        let mut b = mk(50, -3);
        let mut out = Vec::new();
        mix_pending(&mut a, &mut b, &mut out);
        let samples: Vec<i16> = out
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(samples, vec![150, -8, 150, -8]);
        // 长度不齐：只混公共部分（4 字节帧对齐），尾巴留在长的一方
        let mut a = mk(1, 1);
        let mut b = mk(2, 2);
        b.extend(9i16.to_le_bytes()); // 半帧尾巴
        let mut out = Vec::new();
        mix_pending(&mut a, &mut b, &mut out);
        assert_eq!(out.len(), 8);
        assert_eq!(a.len(), 0);
        assert_eq!(b.len(), 2, "半帧尾巴保留");
    }

    #[test]
    fn find_in_path_requires_exec_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("lscreen-audio-path-test");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("fakecap");
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        let mut perm = std::fs::metadata(&exe).unwrap().permissions();
        perm.set_mode(0o644);
        std::fs::set_permissions(&exe, perm).unwrap();
        let path = std::ffi::OsString::from(dir.clone());
        assert!(find_in_path(&path, "fakecap").is_none(), "无可执行位不算");
        let mut perm = std::fs::metadata(&exe).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&exe, perm).unwrap();
        assert!(find_in_path(&path, "fakecap").is_some());
        assert!(find_in_path(&path, "nope").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 真机全链路（默认输出设备回录 = 静音，无隐私/无干扰）：
    /// `LSCREEN_TEST_AUDIO=1 cargo test -p lscreen-record -- --ignored audio_e2e`
    #[ignore = "需要本机音频服务与 ffmpeg（LSCREEN_TEST_AUDIO=1 显式开启）"]
    #[test]
    fn audio_e2e_system_silence() {
        if std::env::var("LSCREEN_TEST_AUDIO").ok().as_deref() != Some("1") {
            return;
        }
        let mut p = Pipeline::start(AudioSource::System).unwrap();
        p.set_origin(Instant::now());
        // 3.5s：parec 监视源起流实测 ~1.9s，须留出起流 + 首块触发补静音的时间，
        // 否则整个录制窗口内 PCM 从未到达、零帧（真实缺陷回归）
        std::thread::sleep(Duration::from_millis(3500));
        let (frames, warn) = p.finish();
        assert!(warn.is_none(), "警告: {warn:?}");
        // 3.5s ≈ 164 帧；下限留足启动余量
        assert!(frames.len() >= 100, "帧数过少: {}", frames.len());
        let (m, _) = &frames[0];
        assert_eq!((m.aot, m.freq_index, m.chan_conf), (2, 3, 2));
        println!("{} 帧 AAC, 元信息 {m:?}", frames.len());
    }
}
