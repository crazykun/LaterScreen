//! lscreen-record: 屏幕录制编码。
//!
//! GIF：gifski（纯 Rust，帧差分 + 高质量调色板）。
//! 滚动长截图：帧间尾部块匹配拼接（scroll 模块）。
//! MP4：Linux 走 openh264 静态链接（M4）；Win/mac 系统编码器待实现。
//!
//! 帧源以闭包注入（返回 RGBA 帧），本 crate 不依赖截屏实现，便于单测与复用。

pub mod scroll;

pub(crate) mod audio;

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use imgref::ImgVec;
use rgb::RGBA8;

#[derive(Debug)]
pub struct RecordError(pub String);

/// 记录底层写失败的 writer 包装。mp4-rust 的 `Mp4Writer` 无法取回内部
/// writer，收尾只能靠 Drop 时 `BufWriter` 的静默 flush——ENOSPC/EIO 会被
/// 吞掉，moov 截断仍报成功。本包装把首个错误存入共享标志，收尾后显式
/// 检查并并入错误路径（触发半成品清理）。
struct TrackedWriter {
    inner: File,
    err: Arc<Mutex<Option<String>>>,
}

impl TrackedWriter {
    fn note(&self, e: &std::io::Error) {
        let mut slot = self.err.lock().unwrap();
        if slot.is_none() {
            *slot = Some(e.to_string());
        }
    }
}

impl Write for TrackedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf).inspect_err(|e| self.note(e))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush().inspect_err(|e| self.note(e))
    }
}

impl std::io::Seek for TrackedWriter {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos).inspect_err(|e| self.note(e))
    }
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RecordError {}

pub type Result<T> = std::result::Result<T, RecordError>;

fn err<E: std::fmt::Display>(e: E) -> RecordError {
    RecordError(e.to_string())
}

pub struct GifOptions {
    /// 帧率，录屏合理范围 5..=30
    pub fps: u32,
    /// 1-100，gifski 推荐 50-100
    pub quality: u8,
}

impl Default for GifOptions {
    fn default() -> Self {
        Self {
            fps: 10,
            quality: 90,
        }
    }
}

/// 按 fps 采帧编码 GIF，直到时长耗尽或 stop 置位。返回帧数。
///
/// grab_frame 返回 (rgba, width, height)；尺寸必须每帧一致（gifski 会缩放不一致帧，
/// 但录屏场景尺寸不一致说明上游有 bug，这里直接报错）。
pub fn record_gif(
    mut grab_frame: impl FnMut() -> Result<(Vec<u8>, u32, u32)>,
    opts: &GifOptions,
    max_duration: Duration,
    stop: &AtomicBool,
    out_path: &Path,
) -> Result<usize> {
    let fps = opts.fps.clamp(1, 30);
    let settings = gifski::Settings {
        width: None,
        height: None,
        quality: opts.quality.clamp(1, 100),
        fast: false,
        repeat: gifski::Repeat::Infinite,
    };
    let (collector, writer) = gifski::new(settings).map_err(err)?;

    let mut file = BufWriter::new(File::create(out_path).map_err(err)?);
    // 编码在独立线程消费帧队列，采帧循环不被编码耗时拖慢
    let encode_thread = std::thread::spawn(move || -> Result<()> {
        // 以 &mut 传入保留所有权：gifski 的 write 会按值吃掉 W 并在其内部
        // drop，BufWriter 落盘失败（ENOSPC/EIO）会被静默吞掉、截断文件仍报 Ok。
        // 写完显式 flush，把尾盘失败并入错误路径，触发半成品清理
        writer
            .write(&mut file, &mut gifski::progress::NoProgress {})
            .map_err(err)?;
        file.flush().map_err(err)
    });

    let interval = Duration::from_secs_f64(1.0 / fps as f64);
    let start = Instant::now();
    let mut frame_size: Option<(u32, u32)> = None;
    let mut count = 0usize;
    // 采帧中途失败（截屏报错/帧尺寸漂移）不直接 return：直接 return 会把编码
    // 线程句柄丢在半路（线程分离、产物文件不确定），先记下错误走统一收尾
    let mut abort: Option<RecordError> = None;

    while abort.is_none() && !stop.load(Ordering::Relaxed) && start.elapsed() < max_duration {
        let tick = Instant::now();
        match grab_frame() {
            Ok((rgba, w, h)) => {
                // 长度与声明尺寸不符的帧喂进 gifski 会触发其内部硬断言
                // （panic=abort 下整进程崩溃、不走半成品清理），先在此拦截
                if w == 0 || h == 0 || rgba.len() != (w * h * 4) as usize {
                    abort = Some(RecordError("帧尺寸/数据无效".into()));
                    break;
                }
                match frame_size {
                    None => frame_size = Some((w, h)),
                    Some(size) if size != (w, h) => {
                        abort = Some(RecordError(format!(
                            "帧尺寸不一致: {size:?} -> {:?}",
                            (w, h)
                        )));
                        break;
                    }
                    _ => {}
                }
                let pixels: Vec<RGBA8> = rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| RGBA8::new(p[0], p[1], p[2], 255))
                    .collect();
                let pts = start.elapsed().as_secs_f64();
                if let Err(e) = collector.add_frame_rgba(
                    count,
                    ImgVec::new(pixels, w as usize, h as usize),
                    pts,
                ) {
                    abort = Some(err(e));
                    break;
                }
                count += 1;
            }
            Err(e) => {
                abort = Some(e);
            }
        }

        if let Some(rest) = interval.checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }

    drop(collector); // 关闭帧队列，编码线程随之收尾
    let encode = encode_thread
        .join()
        .map_err(|_| RecordError("编码线程崩溃".into()))
        .and_then(|r| r);

    // 失败不留半成品文件（哪怕编码本身成功，内容也是残缺的）
    if abort.is_some() || encode.is_err() || count == 0 {
        let _ = std::fs::remove_file(out_path);
        if let Some(e) = abort {
            return Err(e);
        }
        encode?;
        return Err(RecordError("未采集到任何帧".into()));
    }
    Ok(count)
}

// ---------------------------------------------------------------- MP4（M4）

/// 录屏音频源（M14：Linux 子进程链 / Win WASAPI+MFT / mac CoreAudio）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSource {
    /// 麦克风（默认输入源）
    Mic,
    /// 系统声（默认输出设备回录）
    System,
    /// 麦克风 + 系统声（PCM 饱和混合为单音轨）
    Both,
}

/// 已启动的音频管线句柄。armed 阶段经 [`start_audio`] 预热（采集端起流
/// 有延迟），录制零点由 record_mp4 对齐；传给 `record_mp4` 混流。
/// Drop 即停止全部采集/编码。
pub struct AudioHandle(audio::Pipeline);

impl AudioHandle {
    /// 本管线采集的源（armed 阶段经配置面板热改音频默认后，比对是否重开）
    pub fn source(&self) -> AudioSource {
        self.0.source()
    }
}

/// 启动音频管线：探测系统采集/编码设施，任一必需项缺失直接报错（快速
/// 失败，不让用户录完才发现没声）。macOS 无系统声内录公开 API，显式
/// 请求 System/Both 在此报错（app 层的配置默认会先降级为麦克风）。
pub fn start_audio(source: AudioSource) -> Result<AudioHandle> {
    audio::Pipeline::start(source).map(AudioHandle)
}

pub struct Mp4Options {
    pub fps: u32,
    /// 目标码率（kbps）
    pub bitrate_kbps: u32,
}

impl Default for Mp4Options {
    fn default() -> Self {
        Self {
            fps: 15,
            bitrate_kbps: 4000,
        }
    }
}

/// AnnexB 码流里的一个 NAL 单元（不含起始码）
struct Nal<'a> {
    kind: u8,
    data: &'a [u8],
}

/// 切分 AnnexB 码流为 NAL 列表（容忍 3/4 字节起始码）
fn split_annexb(data: &[u8]) -> Vec<Nal<'_>> {
    let mut nals = Vec::new();
    // (起始码起点, NAL 体起点)：NAL 体结束于「下一个起始码起点」，
    // 若取下一个体起点会把起始码字节算进本 NAL，污染 avcC/样本数据
    let mut marks: Vec<(usize, usize)> = Vec::new();
    let mut pos = 0usize;
    while let Some((sc, body)) = find_start_code(data, pos) {
        marks.push((sc, body));
        pos = body;
    }
    for (i, &(_, body)) in marks.iter().enumerate() {
        let end = marks.get(i + 1).map(|&(sc, _)| sc).unwrap_or(data.len());
        if end > body {
            nals.push(Nal {
                kind: data[body] & 0x1f,
                data: &data[body..end],
            });
        }
    }
    nals
}

/// 找下一个起始码，返回 (起始码起点, NAL 体起点)。
/// 起始码起点已含 4 字节起始码（00 00 00 01）的前导 0
fn find_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 < data.len() {
        if data[i..].starts_with(&[0, 0, 1]) {
            let sc = if i > from && data[i - 1] == 0 {
                i - 1
            } else {
                i
            };
            return Some((sc, i + 3));
        }
        i += 1;
    }
    None
}

/// 把 AnnexB 帧转成 AVCC（每 NAL 4 字节大端长度前缀），只保留 VCL NAL（1/5），
/// 返回 (avcc, 是否关键帧)
fn annexb_to_avcc(data: &[u8]) -> (Vec<u8>, bool) {
    let mut out = Vec::with_capacity(data.len());
    let mut sync = false;
    for nal in split_annexb(data) {
        match nal.kind {
            1 => {}
            5 => sync = true,
            // SPS/PPS/SEI 等非 VCL NAL 不进 sample（avcC 已携带参数集）
            _ => continue,
        }
        out.extend_from_slice(&(nal.data.len() as u32).to_be_bytes());
        out.extend_from_slice(nal.data);
    }
    (out, sync)
}

/// 按 fps 采帧编码 H.264/MP4，直到时长耗尽或 stop 置位。返回帧数。
///
/// Linux 走 openh264（静态链接 C++ 源）；Win/mac 系统编码器（MF/VideoToolbox）
/// 待 M4 后续——当前在那些平台调用直接返回「不支持」错误。
///
/// `audio`：armed 预热的音频管线（[`start_audio`]），Some 则并行采集编码，
/// 收尾时作为第二条轨道混入（AAC-LC，48k 立体声，1024 样本/帧）。
/// 音轨时间 0 与视频时间 0 对齐（音频侧按首块到达时刻补静音，见 audio 模块）。
pub fn record_mp4(
    mut grab_frame: impl FnMut() -> Result<(Vec<u8>, u32, u32)>,
    opts: &Mp4Options,
    max_duration: Duration,
    stop: &AtomicBool,
    out_path: &Path,
    audio: Option<AudioHandle>,
) -> Result<usize> {
    let fps = opts.fps.clamp(1, 60);
    let bitrate = opts.bitrate_kbps.clamp(200, 50_000) * 1000;
    let write_err: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut file = Some(BufWriter::new(TrackedWriter {
        inner: File::create(out_path).map_err(err)?,
        err: Arc::clone(&write_err),
    }));
    let mut muxer: Option<mp4::Mp4Writer<_>> = None;
    let mut encoder: Option<openh264::encoder::Encoder> = None;
    // 90000 可被 24/25/30/50/60 fps 整除，帧时长不因整除截断漂移
    // （timescale=1000 时 fps=60 每帧少 0.67ms，播放偏快 ~4%）
    let config_tsc = 90_000u32;

    let interval = Duration::from_secs_f64(1.0 / fps as f64);
    let start = Instant::now();
    // 音频管线对齐零点 = 视频 PTS 零点（此后采集端晚到的首块自动补静音）
    let mut audio = audio.map(|h| {
        h.0.set_origin(start);
        h.0
    });
    let mut count = 0usize;
    let mut frame_size: Option<(u32, u32)> = None;
    let mut abort: Option<RecordError> = None;
    let mut audio_added = false;

    while abort.is_none() && !stop.load(Ordering::Relaxed) && start.elapsed() < max_duration {
        let tick = Instant::now();
        match grab_frame() {
            Ok((rgba, w, h)) => {
                if w == 0 || h == 0 || rgba.len() != (w * h * 4) as usize {
                    abort = Some(RecordError("帧尺寸/数据无效".into()));
                    break;
                }
                // H.264 宏块要求偶数尺寸；openh264 的 RGB→YUV 转换对奇数维
                // 直接 assert（panic=abort 下整进程崩溃，状态窗/边框全消失）。
                // 奇数宽/高裁掉最右列/最下行 1px（每帧一次行拷贝，小区域开销可忽略）
                let (rgba, w, h) = if w % 2 == 1 || h % 2 == 1 {
                    let (cw, ch) = (w & !1, h & !1);
                    let mut buf = Vec::with_capacity((cw * ch * 4) as usize);
                    for row in 0..ch as usize {
                        let src = row * w as usize * 4;
                        buf.extend_from_slice(&rgba[src..src + cw as usize * 4]);
                    }
                    (buf, cw, ch)
                } else {
                    (rgba, w, h)
                };
                if let Some(size) = frame_size {
                    if size != (w, h) {
                        abort = Some(RecordError(format!(
                            "帧尺寸不一致: {size:?} -> {w:?}×{h:?}"
                        )));
                        break;
                    }
                }
                let step: std::result::Result<(), RecordError> = (|| {
                    // 惰性初始化：拿到首帧尺寸才能建 encoder/muxer
                    if encoder.is_none() {
                        let cfg = openh264::encoder::EncoderConfig::new()
                            .max_frame_rate(openh264::encoder::FrameRate::from_hz(fps as f32))
                            .bitrate(openh264::encoder::BitRate::from_bps(bitrate))
                            .rate_control_mode(openh264::encoder::RateControlMode::Bitrate);
                        encoder = Some(
                            openh264::encoder::Encoder::with_api_config(
                                openh264::OpenH264API::from_source(),
                                cfg,
                            )
                            .map_err(err)?,
                        );
                    }
                    let enc = encoder.as_mut().unwrap();
                    // RGBA → I420（openh264 内置转换）→ 编码 → AnnexB
                    let yuv = openh264::formats::YUVBuffer::from_rgba8_source(
                        openh264::formats::RgbaSliceU8::new(&rgba, (w as usize, h as usize)),
                    );
                    let bits = enc.encode(&yuv).map_err(err)?;
                    let annexb = bits.to_vec();

                    if muxer.is_none() {
                        // 首帧一定含 SPS(7)/PPS(8)；取出建 avcC
                        let sps = split_annexb(&annexb)
                            .iter()
                            .find(|n| n.kind == 7)
                            .map(|n| n.data.to_vec())
                            .ok_or_else(|| RecordError("码流缺少 SPS".into()))?;
                        let pps = split_annexb(&annexb)
                            .iter()
                            .find(|n| n.kind == 8)
                            .map(|n| n.data.to_vec())
                            .ok_or_else(|| RecordError("码流缺少 PPS".into()))?;
                        let mut m = mp4::Mp4Writer::write_start(
                            file.take().ok_or_else(|| RecordError("内部错误".into()))?,
                            &mp4::Mp4Config {
                                major_brand: "isom".parse().unwrap(),
                                minor_version: 512,
                                compatible_brands: vec![
                                    "isom".parse().unwrap(),
                                    "iso2".parse().unwrap(),
                                    "avc1".parse().unwrap(),
                                    "mp41".parse().unwrap(),
                                ],
                                timescale: config_tsc,
                            },
                        )
                        .map_err(err)?;
                        m.add_track(&mp4::TrackConfig {
                            track_type: mp4::TrackType::Video,
                            timescale: config_tsc,
                            language: "und".into(),
                            media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
                                width: w as u16,
                                height: h as u16,
                                seq_param_set: sps,
                                pic_param_set: pps,
                            }),
                        })
                        .map_err(err)?;
                        muxer = Some(m);
                    }
                    let (avcc, sync) = annexb_to_avcc(&annexb);
                    if avcc.is_empty() {
                        return Err(RecordError("编码输出空帧".into()));
                    }
                    let sample = mp4::Mp4Sample {
                        // mp4-rust 0.14 的 write_sample 只累计 duration，
                        // start_time 被忽略（时间戳由 stts 表推导），恒置 0
                        start_time: 0,
                        duration: (config_tsc / fps).max(1),
                        rendering_offset: 0,
                        is_sync: sync,
                        bytes: bytes::Bytes::from(avcc),
                    };
                    muxer
                        .as_mut()
                        .unwrap()
                        .write_sample(1, &sample)
                        .map_err(err)?;
                    Ok(())
                })();
                if let Err(e) = step {
                    abort = Some(e);
                    break;
                }
                // 音频：非阻塞拉取已编码完成的 AAC 帧入轨。muxer 必已建立
                // （视频步骤在同一次迭代里先跑，首帧即建轨 1）
                if let Some(pipe) = audio.as_mut() {
                    let frames = pipe.pull();
                    if !frames.is_empty() {
                        if let Err(e) =
                            write_audio_frames(muxer.as_mut().unwrap(), &mut audio_added, &frames)
                        {
                            abort = Some(e);
                            break;
                        }
                    }
                }
                frame_size = Some((w, h));
                count += 1;
            }
            Err(e) => abort = Some(e),
        }

        if let Some(rest) = interval.checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }

    // 音频收尾：停采集 → 冲刷编码器 → 残余帧入轨（必须在 write_end 之前）。
    // abort 路径同样要 finish（回收子进程/线程），帧丢弃（文件即将清理）
    {
        let (tail, warn) = match audio.as_mut() {
            Some(pipe) => pipe.finish(),
            None => (Vec::new(), None),
        };
        if !tail.is_empty() && abort.is_none() {
            if let Some(m) = muxer.as_mut() {
                if let Err(e) = write_audio_frames(m, &mut audio_added, &tail) {
                    abort = Some(e);
                }
            }
        }
        if let Some(w) = warn {
            // 运行期音频故障但视频完好：保留可用部分而非毁掉整段录制，
            // stderr 提示让用户知情（record crate 唯一的直接打印路径）
            eprintln!("lscreen: 音频录制异常（已保留可用部分）: {w}");
        }
    }

    // 收尾：flush muxer（moov 盒在 write_end 写出，失败同样清理半成品）。
    // muxer 在 match 内 drop，其中 BufWriter 尾盘 flush 的失败经共享标志带出
    let finalize: std::result::Result<(), RecordError> = match muxer.take() {
        Some(mut m) => m.write_end().map_err(err),
        None => Ok(()),
    };
    let write_failed = write_err.lock().unwrap().take();
    if abort.is_some() || finalize.is_err() || count == 0 || write_failed.is_some() {
        let _ = std::fs::remove_file(out_path);
        if let Some(e) = abort {
            return Err(e);
        }
        finalize?;
        if let Some(e) = write_failed {
            return Err(RecordError(format!("写入文件失败: {e}")));
        }
        return Err(RecordError("未采集到任何帧".into()));
    }
    Ok(count)
}

/// 把一批 AAC 帧写入音轨。首次调用按 ADTS 元信息建轨（轨道 id 恒为 2：
/// 视频轨先建）；每帧 1024 样本，音轨 timescale = 采样率，即一帧 1024 tick。
fn write_audio_frames(
    muxer: &mut mp4::Mp4Writer<BufWriter<TrackedWriter>>,
    added: &mut bool,
    frames: &[(audio::AacMeta, Vec<u8>)],
) -> Result<()> {
    if !*added {
        let m = frames[0].0;
        let freq = audio::freq_hz(m.freq_index)
            .ok_or_else(|| RecordError(format!("音频采样率索引无效: {}", m.freq_index)))?;
        muxer
            .add_track(&mp4::TrackConfig {
                track_type: mp4::TrackType::Audio,
                timescale: freq,
                language: "und".into(),
                media_conf: mp4::MediaConfig::AacConfig(mp4::AacConfig {
                    bitrate: audio::BITRATE_KBPS * 1000,
                    profile: mp4::AudioObjectType::try_from(m.aot).map_err(err)?,
                    freq_index: mp4::SampleFreqIndex::try_from(m.freq_index).map_err(err)?,
                    chan_conf: mp4::ChannelConfig::try_from(m.chan_conf).map_err(err)?,
                }),
            })
            .map_err(err)?;
        *added = true;
    }
    for (_, bytes) in frames {
        muxer
            .write_sample(
                2,
                &mp4::Mp4Sample {
                    start_time: 0,
                    duration: audio::SAMPLES_PER_FRAME,
                    rendering_offset: 0,
                    is_sync: true,
                    bytes: bytes::Bytes::from(bytes.clone()),
                },
            )
            .map_err(err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// split_annexb / annexb_to_avcc 的纯逻辑测试
    #[test]
    fn annexb_parsing() {
        // 两个 NAL：SPS(0x67 & 0x1f = 7) 与 IDR(0x65 & 0x1f = 5)
        let annexb = [
            0, 0, 0, 1, 0x67, 1, 2, // SPS
            0, 0, 0, 1, 0x65, 9, 9, 9, // IDR
        ];
        let nals = split_annexb(&annexb);
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0].kind, 7);
        // 非末尾 NAL 不得把下一个起始码（00 00 00 01）算进自身数据
        assert_eq!(nals[0].data, [0x67, 1, 2]);
        assert_eq!(nals[1].kind, 5);
        let (avcc, sync) = annexb_to_avcc(&annexb);
        assert!(sync, "IDR 帧应标记为关键帧");
        assert_eq!(avcc, [0, 0, 0, 4, 0x65, 9, 9, 9], "只保留 VCL NAL");

        // 3 字节起始码 + 非 VCL 过滤
        let annexb2 = [0, 0, 1, 0x68, 7, 0, 0, 1, 0x41, 5, 5];
        let nals2 = split_annexb(&annexb2);
        assert_eq!(nals2[0].data, [0x68, 7], "3 字节起始码边界");
        let (avcc2, sync2) = annexb_to_avcc(&annexb2);
        assert!(!sync2);
        assert_eq!(avcc2, [0, 0, 0, 3, 0x41, 5, 5]);

        // 连续多个 VCL NAL（多 slice 帧）：每个长度前缀都不含起始码字节
        let annexb3 = [0, 0, 0, 1, 0x41, 1, 2, 0, 0, 0, 1, 0x41, 3];
        let (avcc3, _) = annexb_to_avcc(&annexb3);
        assert_eq!(avcc3, [0, 0, 0, 3, 0x41, 1, 2, 0, 0, 0, 2, 0x41, 3]);
    }

    /// 合成帧 → GIF 全链路：渐变色动画，验证产物是合法 GIF。
    #[test]
    fn synthetic_frames_to_gif() {
        let dir = std::env::temp_dir().join("lscreen-record-test.gif");
        let mut tick = 0u8;
        let stop = AtomicBool::new(false);
        let n = record_gif(
            move || {
                tick = tick.wrapping_add(40);
                let (w, h) = (64u32, 48u32);
                let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                for y in 0..h {
                    for x in 0..w {
                        rgba.extend_from_slice(&[(x * 4) as u8, (y * 5) as u8, tick, 255]);
                    }
                }
                Ok((rgba, w, h))
            },
            &GifOptions {
                fps: 30,
                quality: 60,
            },
            Duration::from_millis(200),
            &stop,
            &dir,
        )
        .unwrap();
        assert!(n >= 2, "帧数过少: {n}");
        let data = std::fs::read(&dir).unwrap();
        assert_eq!(&data[..6], b"GIF89a");
        std::fs::remove_file(&dir).ok();
    }

    /// 采帧中途失败：报错且不留半成品文件（回归：曾直接 return，
    /// 丢下分离的编码线程和残缺产物）
    #[test]
    fn grab_failure_cleans_up_partial_file() {
        let dir = std::env::temp_dir().join("lscreen-record-fail-test.gif");
        let stop = AtomicBool::new(false);
        let mut calls = 0;
        let err = record_gif(
            move || {
                calls += 1;
                if calls == 1 {
                    Ok((vec![255u8; 64 * 48 * 4], 64, 48))
                } else {
                    Err(RecordError("采帧失败".into()))
                }
            },
            &GifOptions {
                fps: 30,
                quality: 60,
            },
            Duration::from_secs(10),
            &stop,
            &dir,
        )
        .unwrap_err();
        assert_eq!(err.0, "采帧失败");
        assert!(!dir.exists(), "失败后不应残留半成品文件");
    }

    /// 帧尺寸漂移：同样要清理半成品并报错
    #[test]
    fn frame_size_mismatch_cleans_up() {
        let dir = std::env::temp_dir().join("lscreen-record-size-test.gif");
        let stop = AtomicBool::new(false);
        let mut calls = 0;
        let err = record_gif(
            move || {
                calls += 1;
                let (w, h) = if calls == 1 { (64, 48) } else { (32, 48) };
                Ok((vec![0u8; (w * h * 4) as usize], w, h))
            },
            &GifOptions {
                fps: 30,
                quality: 60,
            },
            Duration::from_secs(10),
            &stop,
            &dir,
        )
        .unwrap_err();
        assert!(err.0.contains("帧尺寸不一致"), "{}", err.0);
        assert!(!dir.exists());
    }

    /// 合成帧 → MP4 全链路：验证产物可被 mp4 解析器读回、轨道为 H.264、
    /// 帧数与时长正确（openh264 无 asm 下编码 64×48 小图，测试秒级完成）。
    /// 仅 Linux 运行：openh264 无 asm 在 mac CI 上太慢，wall-clock 断言会
    /// 误报（视频编码三平台同为 openh264，Linux 覆盖即代表路径正确）。
    #[cfg(target_os = "linux")]
    #[test]
    fn synthetic_frames_to_mp4() {
        let dir = std::env::temp_dir().join("lscreen-record-test.mp4");
        let mut tick = 0u8;
        let stop = AtomicBool::new(false);
        let n = record_mp4(
            move || {
                tick = tick.wrapping_add(40);
                let (w, h) = (64u32, 48u32);
                let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                for y in 0..h {
                    for x in 0..w {
                        rgba.extend_from_slice(&[(x * 4) as u8, (y * 5) as u8, tick, 255]);
                    }
                }
                Ok((rgba, w, h))
            },
            &Mp4Options {
                fps: 10,
                bitrate_kbps: 500,
            },
            Duration::from_millis(450),
            &stop,
            &dir,
            None,
        )
        .unwrap();
        assert!(n >= 3, "帧数过少: {n}");

        // 读回验证：H.264 轨道、尺寸、帧数
        let f = std::fs::File::open(&dir).unwrap();
        let reader = mp4::read_mp4(f).unwrap();
        assert_eq!(reader.tracks().len(), 1);
        let track = reader.tracks().values().next().unwrap();
        assert_eq!(track.track_id(), 1);
        assert_eq!(track.width(), 64);
        assert_eq!(track.height(), 48);
        assert_eq!(track.sample_count() as usize, n);
        std::fs::remove_file(&dir).ok();
    }

    /// 奇数宽高的帧不得让 openh264 崩溃（RGB→YUV 转换断言偶数维；
    /// panic=abort 下整进程退出，状态窗/边框全消失——真实缺陷回归）。
    /// 裁偶后产物尺寸 = 原尺寸各减 1。
    #[cfg(target_os = "linux")]
    #[test]
    fn odd_frames_to_mp4() {
        let dir = std::env::temp_dir().join("lscreen-record-test-odd.mp4");
        let stop = AtomicBool::new(false);
        let n = record_mp4(
            move || {
                let (w, h) = (65u32, 49u32);
                Ok((vec![0x80u8; (w * h * 4) as usize], w, h))
            },
            &Mp4Options {
                fps: 10,
                bitrate_kbps: 500,
            },
            Duration::from_millis(350),
            &stop,
            &dir,
            None,
        )
        .unwrap();
        assert!(n >= 2, "帧数过少: {n}");
        let f = std::fs::File::open(&dir).unwrap();
        let reader = mp4::read_mp4(f).unwrap();
        let track = reader.tracks().values().next().unwrap();
        assert_eq!(track.width(), 64);
        assert_eq!(track.height(), 48);
        std::fs::remove_file(&dir).ok();
    }

    /// 真机端到端：合成视频帧 + 系统声回录（默认输出静音，无隐私/无干扰）
    /// → 双轨 MP4。验证音频管线、混流与读回。
    /// `LSCREEN_TEST_AUDIO=1 cargo test -p lscreen-record -- --ignored`
    #[cfg(target_os = "linux")]
    #[ignore = "需要本机音频服务与 ffmpeg（LSCREEN_TEST_AUDIO=1 显式开启）"]
    #[test]
    fn synthetic_mp4_with_audio() {
        if std::env::var("LSCREEN_TEST_AUDIO").ok().as_deref() != Some("1") {
            return;
        }
        let dir = std::env::temp_dir().join("lscreen-record-audio-test.mp4");
        let handle = start_audio(AudioSource::System).unwrap();
        let stop = AtomicBool::new(false);
        let mut tick = 0u8;
        let n = record_mp4(
            move || {
                tick = tick.wrapping_add(40);
                let (w, h) = (64u32, 48u32);
                let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                for y in 0..h {
                    for x in 0..w {
                        rgba.extend_from_slice(&[(x * 4) as u8, (y * 5) as u8, tick, 255]);
                    }
                }
                Ok((rgba, w, h))
            },
            &Mp4Options {
                fps: 10,
                bitrate_kbps: 500,
            },
            Duration::from_millis(3500),
            &stop,
            &dir,
            Some(handle),
        )
        .unwrap();
        assert!(n >= 25, "帧数过少: {n}");

        // 读回：双轨，轨道 2 为音频，样本数与时长合理（3.5s ≈ 164 帧 AAC）
        let f = std::fs::File::open(&dir).unwrap();
        let reader = mp4::read_mp4(f).unwrap();
        assert_eq!(reader.tracks().len(), 2, "应含视频+音频双轨");
        let audio = reader.tracks().get(&2).unwrap();
        assert_eq!(audio.track_type().unwrap(), mp4::TrackType::Audio);
        let samples = audio.sample_count();
        assert!((100..250).contains(&samples), "音频帧数异常: {samples}");
        let dur = samples as f64 * 1024.0 / 48000.0;
        let video_dur = n as f64 / 10.0;
        let drift = (dur - video_dur).abs();
        assert!(
            drift < 0.5,
            "A/V 时长偏差过大: 音频{dur:.2}s vs 视频{video_dur:.2}s"
        );
        println!("{n} 视频帧 + {samples} 音频帧（{dur:.2}s），A/V 偏差 {drift:.3}s");

        if std::env::var("LSCREEN_TEST_AUDIO_KEEP").is_ok() {
            println!("保留产物供人工检查: {}", dir.display());
        } else {
            std::fs::remove_file(&dir).ok();
        }
    }
}
