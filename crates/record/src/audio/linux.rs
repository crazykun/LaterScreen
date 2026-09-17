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
//! 对齐/混写/收尾对账见 `super`（跨平台共享）。

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::super::{AudioSource, RecordError};
use super::{run_mixer, AacMeta, Core, ToMixer, SAMPLES_PER_FRAME};

/// 采集读块大小：恰好一帧 AAC 对应的 PCM（对齐误差 ≤ 一帧）
const READ_CHUNK: usize = SAMPLES_PER_FRAME as usize * 4;

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
        _ => None,
    }
}

// ---------------------------------------------------------------- 管线

/// Linux 音频管线：N 个采集子进程 → 混写线程（对齐/混合）→ ffmpeg 编码 →
/// ADTS 拆帧 → AAC 帧队列（record_mp4 主循环非阻塞拉取）
pub(crate) struct Pipeline {
    core: Core,
    captures: Vec<Child>,
    ffmpeg: Option<Child>,
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
                &format!("{}k", super::BITRATE_KBPS),
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

        let (mut core, frame_tx) = Core::new(source);
        let (pcm_tx, pcm_rx) = channel::<ToMixer>();
        let mut workers = Vec::new();

        // ffmpeg stderr 排空（防 64KB 管道堵死；留存首段做诊断）
        {
            let slot = Arc::clone(&core.ffmpeg_stderr);
            workers.push(std::thread::spawn(move || {
                let mut buf = String::new();
                let mut out = ffmpeg_stderr;
                let _ = out.read_to_string(&mut buf);
                if let Ok(mut s) = slot.lock() {
                    s.clear();
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
            let origin = Arc::clone(&core.origin);
            let err_slot = Arc::clone(&core.err);
            let flowed = Arc::clone(&core.flowed);
            let nsrc = captures.len();
            workers.push(std::thread::spawn(move || {
                let mut sink = ffmpeg_stdin;
                run_mixer(
                    pcm_rx,
                    move |out| sink.write_all(out).is_ok(),
                    origin,
                    err_slot,
                    flowed,
                    nsrc,
                    "音频编码器写入失败（ffmpeg 中途退出？）".into(),
                );
                // 线程结束 drop(sink) 关闭 stdin 送 EOF，ffmpeg 冲刷收尾
            }));
        }

        // ffmpeg stdout → ADTS 拆帧 → AAC 帧队列
        {
            let tx = frame_tx;
            let counter = Arc::clone(&core.frame_count);
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

        core.workers = workers;
        Ok(Self {
            core,
            captures,
            ffmpeg: Some(ffmpeg),
        })
    }

    pub(crate) fn source(&self) -> AudioSource {
        self.core.source
    }

    pub(crate) fn set_origin(&self, t0: Instant) {
        self.core.set_origin(t0);
    }

    pub(crate) fn pull(&mut self) -> Vec<(AacMeta, Vec<u8>)> {
        self.core.pull()
    }

    /// 停止采集 → 冲刷编码器 → 回收全部子进程与线程。
    /// 返回 (残余 AAC 帧, 运行期异常)。采集端被 SIGKILL，尾缓冲（毫秒级）
    /// 丢失可忽略。
    pub(crate) fn finish(&mut self) -> (Vec<(AacMeta, Vec<u8>)>, Option<String>) {
        if self.core.finished {
            return (Vec::new(), None);
        }
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
        let (frames, mut warn) = self.core.finalize();
        if !exit_ok && warn.is_none() {
            let stderr = self.core.ffmpeg_stderr.lock().unwrap().clone();
            let head = stderr.lines().next().unwrap_or_default();
            warn = Some(if head.is_empty() {
                "音频编码器异常退出".into()
            } else {
                format!("音频编码器异常退出: {head}")
            });
        }
        (frames, warn)
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        if !self.core.finished {
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
    use std::os::unix::fs::PermissionsExt;

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
        s.push(&adts_frame(&[0xAA; 200], 1, 3, 2));
        let (m, f) = s.pop_frame().unwrap();
        assert_eq!((m.aot, m.freq_index, m.chan_conf), (2, 3, 2));
        assert_eq!(f.len(), 200);
        assert!(s.pop_frame().is_none());
    }

    #[test]
    fn adts_splitter_resync_and_garbage() {
        let mut s = AdtsSplitter::default();
        let f = adts_frame(&[0xAB; 100], 0, 4, 1); // Main/44.1k/单声道
        let mut stream = vec![0x00, 0x12, 0xFF, 0x00, 0x99]; // 垃圾前缀（含 0xFF 但不构成同步）
        stream.extend_from_slice(&f);
        s.push(&stream);
        let (m, d) = s.pop_frame().unwrap();
        assert_eq!((m.aot, m.freq_index, m.chan_conf), (1, 4, 1));
        assert_eq!(d, vec![0xAB; 100]);
        assert!(s.pop_frame().is_none());
    }

    #[test]
    fn find_in_path_requires_exec_bit() {
        let dir = std::env::temp_dir().join("lscreen-audio-test-path");
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
