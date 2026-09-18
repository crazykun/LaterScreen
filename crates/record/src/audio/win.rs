//! Windows 录屏音频：WASAPI 采集 + Media Foundation AAC 编码（全系统 API）。
//!
//! - 麦克风：共享模式 IAudioClient（eCapture/eConsole 默认设备）
//! - 系统声：默认渲染设备 + `AUDCLNT_STREAMFLAGS_LOOPBACK` 回录。真机
//!   实测（2026-09-17）：**无渲染流时 loopback 完全不产包**（并非持续
//!   静音包）——静默桌面录完收尾对账走「零数据」告警按无音轨保存，
//!   有播放流后每包正常（无声时段才是静音包）
//! - 编码：MFT 的微软 AAC 编码器（同步 MFT），输入 s16/48k/立体声 PCM，
//!   输出裸 AAC 帧（1024 样本/帧，与混流层假设一致）
//!
//! 采集在共享模式 mix format（f32、原生采样率/声道数）下进行，用共享的
//! [`ToStereo48`](super::ToStereo48) 转换到管线固定格式——避免格式协商
//! 失败（共享模式对非 mix format 的支持不保证）。
//!
//! 采集线程轮询（10ms）取包而非事件回调：停止延迟 ≤20ms，远小于一帧 AAC
//! （21ms）的对齐容差，代码路径更短。
//!
//! **盲写说明**：本文件按 Windows 文档盲写，以交叉编译 + Windows CI 为
//! 验证基线，真机行为待 Windows 实机点验（PLAN M14）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaType, IMFSample, IMFTransform, MFAudioFormat_AAC, MFAudioFormat_PCM,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Audio, MFShutdown,
    MFStartup, MFTEnumEx, MFSTARTUP_LITE, MFT_CATEGORY_AUDIO_ENCODER, MFT_ENUM_FLAG_SYNCMFT,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_NUM_CHANNELS,
    MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_VERSION,
};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};

use super::super::{AudioSource, RecordError};
use super::{run_mixer, AacMeta, Core, ToMixer, ToStereo48, FIXED_META, RATE, SAMPLES_PER_FRAME};

/// MFT 输入块大小：一帧 AAC 对应的 PCM（s16 立体声）
const PCM_CHUNK: usize = SAMPLES_PER_FRAME as usize * 4;

/// 采集线程初始化结果（start() 等待全部就绪或失败，实现快速失败语义）
enum Ready {
    Ok,
    Fail(String),
}

pub(crate) struct Pipeline {
    core: Core,
    stop: Arc<AtomicBool>,
}

impl Pipeline {
    pub(crate) fn start(source: AudioSource) -> Result<Self, RecordError> {
        // (数据流方向, 是否 loopback)：麦克风 = 采集端；系统声 = 渲染端回录
        let sources: Vec<(windows::Win32::Media::Audio::EDataFlow, bool)> = match source {
            AudioSource::Mic => vec![(eCapture, false)],
            AudioSource::System => vec![(eRender, true)],
            AudioSource::Both => vec![(eCapture, false), (eRender, true)],
        };
        let (mut core, frame_tx) = Core::new(source);
        let (pcm_tx, pcm_rx) = channel::<ToMixer>();
        let (ready_tx, ready_rx) = channel::<Ready>();
        let stop = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::new();

        for (i, (flow, loopback)) in sources.iter().enumerate() {
            let tx = pcm_tx.clone();
            let rtx = ready_tx.clone();
            let stop = Arc::clone(&stop);
            let flow = *flow;
            let loopback = *loopback;
            workers.push(std::thread::spawn(move || {
                // 就绪信号（Ready::Ok）由 run_capture 在起流成功时发出，
                // 这里只兜失败（对齐 mac.rs 的快速失败语义）
                if let Err(e) = capture_thread(flow, loopback, &stop, &tx, &rtx, i) {
                    let _ = rtx.send(Ready::Fail(e));
                }
                let _ = tx.send(ToMixer::Eof(i));
            }));
        }
        drop(ready_tx);
        drop(pcm_tx);

        // 等全部采集源初始化完成（设备查询秒级内；5s 兜底）
        for (_, loopback) in sources.iter() {
            let what = if *loopback { "系统声" } else { "麦克风" };
            match ready_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ready::Ok) => {}
                Ok(Ready::Fail(e)) => {
                    stop.store(true, Ordering::Relaxed);
                    return Err(RecordError(format!("启动{what}采集失败: {e}")));
                }
                Err(_) => {
                    stop.store(true, Ordering::Relaxed);
                    return Err(RecordError(format!("启动{what}采集超时（音频设备无响应）")));
                }
            }
        }

        // 混写 → 编码线程
        let (enc_tx, enc_rx) = channel::<Vec<u8>>();
        {
            let origin = Arc::clone(&core.origin);
            let err = Arc::clone(&core.err);
            let flowed = Arc::clone(&core.flowed);
            let nsrc = sources.len();
            workers.push(std::thread::spawn(move || {
                run_mixer(
                    pcm_rx,
                    move |out| enc_tx.send(out.to_vec()).is_ok(),
                    origin,
                    err,
                    flowed,
                    nsrc,
                    "音频编码失败（AAC 编码器退出？）".into(),
                );
            }));
        }
        {
            let counter = Arc::clone(&core.frame_count);
            let err = Arc::clone(&core.err);
            workers.push(std::thread::spawn(move || {
                encoder_thread(enc_rx, &frame_tx, &counter, &err);
            }));
        }

        core.workers = workers;
        Ok(Self { core, stop })
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

    /// 置停止位：采集线程退出（发 Eof）→ 混写线程结束（drop 通道）→
    /// 编码线程冲刷 MFT 后发残余帧 → join + 对账。
    pub(crate) fn finish(&mut self) -> (Vec<(AacMeta, Vec<u8>)>, Option<String>) {
        self.stop.store(true, Ordering::Relaxed);
        self.core.finalize()
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        if !self.core.finished {
            // panic/异常路径：停止位让全部线程自行退出（COM/MFT 随线程清理）
            self.stop.store(true, Ordering::Relaxed);
        }
    }
}

// ---------------------------------------------------------------- WASAPI 采集

/// 单个采集源的一生（独立线程 + 独立 COM 单元）
fn capture_thread(
    flow: windows::Win32::Media::Audio::EDataFlow,
    loopback: bool,
    stop: &AtomicBool,
    tx: &Sender<ToMixer>,
    rtx: &Sender<Ready>,
    idx: usize,
) -> Result<(), String> {
    unsafe {
        let _com = ComGuard::new()?;
        run_capture(flow, loopback, stop, tx, rtx, idx)
    }
}

/// CoInitializeEx/CoUninitialize 配对守卫（new 失败不构造，无需记结果）
struct ComGuard;

impl ComGuard {
    unsafe fn new() -> Result<Self, String> {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        if hr.is_err() {
            return Err(format!("COM 初始化失败: {hr}"));
        }
        Ok(Self)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

unsafe fn run_capture(
    flow: windows::Win32::Media::Audio::EDataFlow,
    loopback: bool,
    stop: &AtomicBool,
    tx: &Sender<ToMixer>,
    rtx: &Sender<Ready>,
    idx: usize,
) -> Result<(), String> {
    let what = if loopback {
        "系统输出"
    } else {
        "麦克风"
    };
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        .map_err(|e| format!("设备枚举器创建失败: {e}"))?;
    let device = enumerator
        .GetDefaultAudioEndpoint(flow, eConsole)
        .map_err(|e| format!("{what}设备不可用（设备未连接/被禁用？）: {e}"))?;
    let client: IAudioClient = device
        .Activate(CLSCTX_ALL, None)
        .map_err(|e| format!("音频客户端激活失败: {e}"))?;
    let fmt = client
        .GetMixFormat()
        .map_err(|e| format!("读取设备混音格式失败: {e}"))?;
    let _free_fmt = FormatGuard(fmt);
    let fmt_val = std::ptr::read_unaligned(fmt);
    let (ch, rate) = parse_mix_format(&fmt_val).ok_or_else(|| {
        // packed 结构字段先拷到局部再格式化（直接引用触发 E0793）
        let (ch, rate) = (fmt_val.nChannels, fmt_val.nSamplesPerSec);
        format!("{what}设备格式不受支持（共享模式混音格式应为 f32 交错：ch={ch}, rate={rate}）")
    })?;

    // 共享模式直接用 mix format 初始化（引擎保证接受）；loopback 标志把
    // 渲染端点的播放混音导给采集端。缓冲 200ms，轮询 10ms 取包。
    let flags = if loopback {
        AUDCLNT_STREAMFLAGS_LOOPBACK
    } else {
        0
    };
    client
        .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, fmt, None)
        .map_err(|e| format!("音频客户端初始化失败: {e}"))?;
    let capture: IAudioCaptureClient = client
        .GetService()
        .map_err(|e| format!("采集接口获取失败: {e}"))?;
    client.Start().map_err(|e| format!("采集启动失败: {e}"))?;
    // 起流成功即报告就绪（start() 的 5s 等待解除），此后进入采集循环
    let _ = rtx.send(Ready::Ok);

    let mut conv = ToStereo48::new(rate, ch as usize);
    let silent_flag = AUDCLNT_BUFFERFLAGS_SILENT.0 as u32;
    'outer: while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
        loop {
            let packets = capture.GetNextPacketSize().unwrap_or(0);
            if packets == 0 {
                break;
            }
            let mut data = std::ptr::null_mut::<u8>();
            let mut frames = 0u32;
            let mut buf_flags = 0u32;
            if capture
                .GetBuffer(&mut data, &mut frames, &mut buf_flags, None, None)
                .is_err()
            {
                break;
            }
            let pcm = if buf_flags & silent_flag != 0 || data.is_null() {
                conv.silence(frames as usize)
            } else {
                let floats =
                    std::slice::from_raw_parts(data as *const f32, frames as usize * ch as usize);
                conv.push(floats)
            };
            let downstream_alive =
                !pcm.is_empty() && tx.send(ToMixer::Pcm(idx, pcm, Instant::now())).is_ok();
            if capture.ReleaseBuffer(frames).is_err() || !downstream_alive {
                // 下游已死/设备异常：退出（stop 位稍后由外部置起）
                break 'outer;
            }
        }
    }
    client.Stop().ok();
    Ok(())
}

/// GetMixFormat 缓冲释放守卫
struct FormatGuard(*mut WAVEFORMATEX);

impl Drop for FormatGuard {
    fn drop(&mut self) {
        unsafe {
            CoTaskMemFree(Some(self.0 as *const core::ffi::c_void));
        }
    }
}

/// 解析 mix format：返回 (声道数, 采样率)。仅接受 f32（共享模式混音格式
/// 恒为 IEEE float；扩展头里带 SubFormat）。传入按值副本（WAVEFORMATEX
/// 为 packed 结构，调用侧先 read_unaligned 拷出）
fn parse_mix_format(w: &WAVEFORMATEX) -> Option<(u16, u32)> {
    if w.nChannels == 0 || w.nSamplesPerSec == 0 || w.wBitsPerSample != 32 {
        return None;
    }
    let is_float = if w.wFormatTag as u32 == 0xFFFE {
        // WAVE_FORMAT_EXTENSIBLE：整结构按未对齐读出（packed），取尾部 GUID
        let ext = unsafe {
            std::ptr::read_unaligned(w as *const WAVEFORMATEX as *const WAVEFORMATEXTENSIBLE)
        };
        let sub = ext.SubFormat;
        sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
    } else {
        w.wFormatTag as u32 == WAVE_FORMAT_IEEE_FLOAT
    };
    if !is_float {
        return None;
    }
    Some((w.nChannels, w.nSamplesPerSec))
}

// ---------------------------------------------------------------- MFT AAC

/// Media Foundation AAC 编码器封装（同步 MFT；1024 样本/帧进，裸 AAC 出）
struct AacMft {
    transform: IMFTransform,
    /// 编码器不自带输出样本时自备（GetOutputStreamInfo 决定）
    out_sample: Option<IMFSample>,
    /// 已送入编码器的样本总数（打时间戳用，100ns 单位换算）
    samples_in: i64,
}

impl AacMft {
    unsafe fn new() -> windows::core::Result<Self> {
        let in_type = audio_mt(MFAudioFormat_PCM)?;
        let out_type = audio_mt(MFAudioFormat_AAC)?;
        // 128kbps：用 AVG_BYTES_PER_SECOND 指定码率（单位字节/秒须 /8，
        // 超出编码器支持范围会报 MF_E_INVALIDMEDIATYPE）
        out_type.SetUINT32(
            &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
            super::BITRATE_KBPS * 1000 / 8,
        )?;

        let transform = enum_aac_encoder()?;
        transform.SetInputType(0, &in_type, 0)?;
        transform.SetOutputType(0, &out_type, 0)?;

        let info = transform.GetOutputStreamInfo(0)?;
        let out_sample = if info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) == 0 {
            let sample: IMFSample = MFCreateSample()?;
            let buffer = MFCreateMemoryBuffer(info.cbSize.max(8192))?;
            sample.AddBuffer(&buffer)?;
            Some(sample)
        } else {
            None
        };

        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        Ok(Self {
            transform,
            out_sample,
            samples_in: 0,
        })
    }

    /// 送入恰好一帧 PCM（4096 字节），取回产出的 AAC 帧（可能 0..n 帧）
    unsafe fn push(&mut self, pcm: &[u8]) -> windows::core::Result<Vec<Vec<u8>>> {
        let sample: IMFSample = MFCreateSample()?;
        let buffer = MFCreateMemoryBuffer(pcm.len() as u32)?;
        let mut dst = std::ptr::null_mut::<u8>();
        let mut cap = 0u32;
        buffer.Lock(&mut dst, None, Some(&mut cap))?;
        if cap as usize >= pcm.len() {
            std::ptr::copy_nonoverlapping(pcm.as_ptr(), dst, pcm.len());
        }
        buffer.Unlock()?;
        buffer.SetCurrentLength(pcm.len() as u32)?;
        sample.AddBuffer(&buffer)?;
        // REFERENCE_TIME = 100ns 单位；样本数 × 10^7 / 采样率
        sample.SetSampleTime(self.samples_in * 10_000_000 / RATE as i64)?;
        sample.SetSampleDuration(SAMPLES_PER_FRAME as i64 * 10_000_000 / RATE as i64)?;
        self.transform.ProcessInput(0, &sample, 0)?;
        self.samples_in += SAMPLES_PER_FRAME as i64;
        self.collect()
    }

    /// 冲刷：编码器排空后返回残余帧
    unsafe fn drain(&mut self) -> windows::core::Result<Vec<Vec<u8>>> {
        self.transform
            .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
        let frames = self.collect()?;
        self.transform
            .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
        Ok(frames)
    }

    /// 反复 ProcessOutput 直到 NEED_MORE_INPUT
    unsafe fn collect(&mut self) -> windows::core::Result<Vec<Vec<u8>>> {
        let mut frames = Vec::new();
        loop {
            let mut ob = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(self.out_sample.clone()),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            };
            let mut status = 0u32;
            let hr = self
                .transform
                .ProcessOutput(0, std::slice::from_mut(&mut ob), &mut status);
            if let Err(e) = hr {
                if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                    return Ok(frames);
                }
                return Err(e);
            }
            if let Some(sample) = ob.pSample.as_ref() {
                let buffer = sample.ConvertToContiguousBuffer()?;
                let mut ptr = std::ptr::null_mut::<u8>();
                let mut len = 0u32;
                buffer.Lock(&mut ptr, None, Some(&mut len))?;
                if len > 0 {
                    frames.push(std::slice::from_raw_parts(ptr, len as usize).to_vec());
                }
                buffer.Unlock()?;
            }
        }
    }
}

/// Audio/子类型/16bit/48k/立体声 的输入输出 MediaType
unsafe fn audio_mt(subtype: windows::core::GUID) -> windows::core::Result<IMFMediaType> {
    let mt: IMFMediaType = MFCreateMediaType()?;
    mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
    mt.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
    mt.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
    mt.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE)?;
    mt.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
    Ok(mt)
}

/// MFTEnumEx 找同步 AAC 编码器并激活（取第一个：微软内置编码器优先注册）
unsafe fn enum_aac_encoder() -> windows::core::Result<IMFTransform> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Audio,
        guidSubtype: MFAudioFormat_PCM,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Audio,
        guidSubtype: MFAudioFormat_AAC,
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    MFTEnumEx(
        MFT_CATEGORY_AUDIO_ENCODER,
        MFT_ENUM_FLAG_SYNCMFT,
        Some(&input),
        Some(&output),
        &mut activates,
        &mut count,
    )?;
    if count == 0 || activates.is_null() {
        return Err(windows::core::Error::new(
            windows::core::HRESULT(0x80004005u32 as i32), // E_FAIL
            "系统未找到 AAC 编码器（Media Foundation）",
        ));
    }
    let list = std::slice::from_raw_parts(activates, count as usize);
    let activate = list[0].clone();
    // 释放数组内全部接口引用 + 数组本体（MFTEnumEx 约定）
    for i in 0..count as usize {
        std::ptr::drop_in_place(activates.add(i));
    }
    CoTaskMemFree(Some(activates as *const core::ffi::c_void));
    let transform: IMFTransform = activate
        .ok_or_else(|| {
            windows::core::Error::new(
                windows::core::HRESULT(0x80004005u32 as i32),
                "AAC 编码器激活失败",
            )
        })?
        .ActivateObject()?;
    Ok(transform)
}

/// 编码线程：PCM 块 → 4096 字节整块 → MFT → AAC 帧队列。
/// 通道关闭（采集全停）后冲刷编码器再退出
fn encoder_thread(
    rx: Receiver<Vec<u8>>,
    tx: &Sender<(AacMeta, Vec<u8>)>,
    counter: &Arc<std::sync::atomic::AtomicUsize>,
    err: &Arc<Mutex<Option<String>>>,
) {
    unsafe {
        // MFT/激活器是 COM 对象：MFStartup 前须初始化本线程 COM 单元
        // （真机点验：不初始化时 MFTEnumEx 激活的编码器 SetOutputType
        // 报 MF_E_INVALIDMEDIATYPE）
        let _com = match ComGuard::new() {
            Ok(g) => g,
            Err(e) => {
                *err.lock().unwrap() = Some(format!("COM 初始化失败: {e}"));
                return;
            }
        };
        if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_LITE) {
            *err.lock().unwrap() = Some(format!("Media Foundation 启动失败: {e}"));
            return;
        }
        let mut enc = match AacMft::new() {
            Ok(e) => e,
            Err(e) => {
                *err.lock().unwrap() = Some(format!("AAC 编码器初始化失败: {e}"));
                MFShutdown().ok();
                return;
            }
        };
        let mut pending: Vec<u8> = Vec::new();
        while let Ok(pcm) = rx.recv() {
            pending.extend_from_slice(&pcm);
            while pending.len() >= PCM_CHUNK {
                let chunk: Vec<u8> = pending.drain(..PCM_CHUNK).collect();
                match enc.push(&chunk) {
                    Ok(frames) => send_frames(&frames, tx, counter),
                    Err(e) => {
                        note_err(err, &format!("AAC 编码失败: {e}"));
                        MFShutdown().ok();
                        return;
                    }
                }
            }
        }
        // 冲刷（不足一帧的尾巴 <21ms 丢弃）
        match enc.drain() {
            Ok(frames) => send_frames(&frames, tx, counter),
            Err(e) => note_err(err, &format!("AAC 编码冲刷失败: {e}")),
        }
        MFShutdown().ok();
    }
}

unsafe fn send_frames(
    frames: &[Vec<u8>],
    tx: &Sender<(AacMeta, Vec<u8>)>,
    counter: &Arc<std::sync::atomic::AtomicUsize>,
) {
    for f in frames {
        counter.fetch_add(1, Ordering::Relaxed);
        if tx.send((FIXED_META, f.clone())).is_err() {
            return;
        }
    }
}

fn note_err(err: &Arc<Mutex<Option<String>>>, msg: &str) {
    let mut slot = err.lock().unwrap();
    if slot.is_none() {
        *slot = Some(msg.into());
    }
}
