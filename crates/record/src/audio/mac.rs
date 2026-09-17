//! macOS 录屏音频：CoreAudio HAL 麦克风采集 + AudioToolbox AAC 编码。
//!
//! - 麦克风：默认输入设备 + IOProc 回调（原生格式恒为 f32 交错），回调内
//!   仅做 memcpy 入队，转换（[`ToStereo48`](super::ToStereo48)）与发送在
//!   转储线程完成，避免在实时线程上做过长的工作
//! - 编码：AudioConverter（PCM s16 → AAC-LC 48k 立体声 128kbps），
//!   `AudioConverterFillComplexBuffer` 拉取式喂入，每包 = 一帧裸 AAC
//!   （1024 样本，与混流层假设一致）
//! - 系统声：**不支持**——无公开 loopback API（需 ScreenCaptureKit 的
//!   SCStream 音频输出，盲写风险过高，见 PLAN M14 的待办）。显式请求
//!   System/Both 直接报错；app 层配置默认会先降级为麦克风
//!
//! AAC 编码器固有的 priming（~2112 样本 ≈ 44ms）会带来等量解码延迟，
//! mp4 侧无 edit list 修剪，A/V 偏差远低于人眼可感，记录在案。
//!
//! **盲写说明**：本文件按 Apple 文档盲写，以交叉编译 + macOS CI 为验证
//! 基线，真机行为待 macOS 实机点验（PLAN M14）。

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use objc2_audio_toolbox::{
    AudioConverterDispose, AudioConverterFillComplexBuffer, AudioConverterNew, AudioConverterRef,
    AudioConverterSetProperty,
};
use objc2_core_audio::{
    kAudioDevicePropertyScopeInput, kAudioDevicePropertyStreamFormat,
    kAudioHardwarePropertyDefaultInputDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, AudioDeviceCreateIOProcID,
    AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioObjectGetPropertyData, AudioObjectPropertyAddress,
};
use objc2_core_audio_types::{
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked, kAudioFormatFlagIsSignedInteger,
    kAudioFormatLinearPCM, kAudioFormatMPEG4AAC, AudioBuffer, AudioBufferList,
    AudioStreamBasicDescription, AudioStreamPacketDescription, AudioTimeStamp,
};

use super::super::{AudioSource, RecordError};
use super::{run_mixer, AacMeta, Core, ToMixer, ToStereo48, FIXED_META, RATE, SAMPLES_PER_FRAME};

/// 一帧 AAC 对应的 PCM 大小（s16 立体声）
const PCM_CHUNK: usize = SAMPLES_PER_FRAME as usize * 4;

/// kAudioConverterEncodeBitrate = 'brte'（objc2-audio-toolbox 未生成此常量）
const K_AUDIO_CONVERTER_ENCODE_BITRATE: u32 = 0x6272_7465;

/// 采集线程初始化结果（start() 等待就绪或失败，实现快速失败语义）
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
        if !matches!(source, AudioSource::Mic) {
            return Err(RecordError(
                "macOS 暂不支持系统声内录（无公开 loopback API，待 ScreenCaptureKit；麦克风可用）"
                    .into(),
            ));
        }
        let (mut core, frame_tx) = Core::new(source);
        let (pcm_tx, pcm_rx) = channel::<ToMixer>();
        let (ready_tx, ready_rx) = channel::<Ready>();
        let stop = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::new();

        // 采集线程：HAL 建立 + IOProc 挂载（起流成功即报告就绪）；停机时拆卸
        {
            let tx = pcm_tx.clone();
            let rtx = ready_tx.clone();
            let stop = Arc::clone(&stop);
            workers.push(std::thread::spawn(move || {
                let r = capture_thread(&stop, &tx, &rtx);
                if r.is_err() {
                    let _ = rtx.send(Ready::Fail(r.unwrap_err()));
                }
                let _ = tx.send(ToMixer::Eof(0));
            }));
        }
        drop(ready_tx);
        drop(pcm_tx);

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ready::Ok) => {}
            Ok(Ready::Fail(e)) => {
                stop.store(true, Ordering::Relaxed);
                return Err(RecordError(format!("启动麦克风采集失败: {e}")));
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                return Err(RecordError("启动麦克风采集超时（音频设备无响应）".into()));
            }
        }

        // 混写 → 编码线程
        let (enc_tx, enc_rx) = channel::<Vec<u8>>();
        {
            let origin = Arc::clone(&core.origin);
            let err = Arc::clone(&core.err);
            let flowed = Arc::clone(&core.flowed);
            workers.push(std::thread::spawn(move || {
                run_mixer(
                    pcm_rx,
                    move |out| enc_tx.send(out.to_vec()).is_ok(),
                    origin,
                    err,
                    flowed,
                    1,
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

    /// 置停止位：采集线程停机（发 Eof）→ 混写结束（drop 通道）→
    /// 编码线程退出 → join + 对账。
    pub(crate) fn finish(&mut self) -> (Vec<(AacMeta, Vec<u8>)>, Option<String>) {
        self.stop.store(true, Ordering::Relaxed);
        self.core.finalize()
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        if !self.core.finished {
            // panic/异常路径：停止位让全部线程自行退出（HAL/转换器随线程清理）
            self.stop.store(true, Ordering::Relaxed);
        }
    }
}

// ---------------------------------------------------------------- HAL 采集

/// 回调/转储线程共享的采集上下文（user data 指向它）
struct CaptureShared {
    queue: Mutex<Vec<u8>>,
}

/// 单源采集的一生（独立线程）；起流成功后经 rtx 报告就绪（快速失败）
fn capture_thread(
    stop: &AtomicBool,
    tx: &Sender<ToMixer>,
    rtx: &Sender<Ready>,
) -> Result<(), String> {
    unsafe { run_capture(stop, tx, rtx) }
}

unsafe fn run_capture(
    stop: &AtomicBool,
    tx: &Sender<ToMixer>,
    rtx: &Sender<Ready>,
) -> Result<(), String> {
    // 默认输入设备
    let mut devid: objc2_core_audio::AudioObjectID = 0;
    let mut size = std::mem::size_of::<objc2_core_audio::AudioObjectID>() as u32;
    let st = AudioObjectGetPropertyData(
        kAudioObjectSystemObject as u32,
        NonNull::from(&AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyDefaultInputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        }),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::from(&mut devid).cast(),
    );
    if st != 0 || devid == 0 {
        return Err(format!("无可用输入设备（麦克风未连接/未授权？）: {st}"));
    }

    // 输入流格式：f32 交错、原生采样率/声道数
    let mut asbd: AudioStreamBasicDescription = std::mem::zeroed();
    let mut size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
    let st = AudioObjectGetPropertyData(
        devid,
        NonNull::from(&AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyStreamFormat,
            mScope: kAudioDevicePropertyScopeInput,
            mElement: kAudioObjectPropertyElementMain,
        }),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::from(&mut asbd).cast(),
    );
    if st != 0 {
        return Err(format!("读取输入流格式失败: {st}"));
    }
    let (ch, rate) = (asbd.mChannelsPerFrame, asbd.mSampleRate);
    if ch == 0
        || rate <= 0.0
        || asbd.mFormatID != kAudioFormatLinearPCM
        || asbd.mBitsPerChannel != 32
        || (asbd.mFormatFlags & kAudioFormatFlagIsFloat) == 0
    {
        return Err(format!(
            "输入设备格式不受支持（应为 f32 交错：ch={ch}, rate={rate:.0}）"
        ));
    }

    // IOProc：回调只 memcpy 入队，转换在转储循环
    let shared = Arc::new(CaptureShared {
        queue: Mutex::new(Vec::new()),
    });
    unsafe extern "C-unwind" fn ioproc(
        _dev: objc2_core_audio::AudioObjectID,
        _now: NonNull<AudioTimeStamp>,
        in_input: NonNull<AudioBufferList>,
        _in_time: NonNull<AudioTimeStamp>,
        _out: NonNull<AudioBufferList>,
        _out_time: NonNull<AudioTimeStamp>,
        user: *mut c_void,
    ) -> i32 {
        let shared = &*(user as *const CaptureShared);
        let list = in_input.as_ref();
        for i in 0..list.mNumberBuffers.min(8) as usize {
            let b = &list.mBuffers[i];
            if b.mDataByteSize > 0 && !b.mData.is_null() {
                let bytes =
                    std::slice::from_raw_parts(b.mData as *const u8, b.mDataByteSize as usize);
                if let Ok(mut q) = shared.queue.lock() {
                    q.extend_from_slice(bytes);
                }
            }
        }
        0
    }
    // 绑定把 AudioDeviceIOProcID 生成为 Option<fn>（按值传递的句柄）
    let mut proc_id: AudioDeviceIOProcID = None;
    let st = AudioDeviceCreateIOProcID(
        devid,
        Some(ioproc),
        Arc::as_ptr(&shared) as *mut c_void,
        NonNull::from(&mut proc_id),
    );
    if st != 0 || proc_id.is_none() {
        return Err(format!("注册采集回调失败: {st}"));
    }
    let st = AudioDeviceStart(devid, proc_id);
    if st != 0 {
        let _ = AudioDeviceDestroyIOProcID(devid, proc_id);
        return Err(format!("启动采集失败（设备被独占？）: {st}"));
    }
    let _ = rtx.send(Ready::Ok);

    // 转储循环：queue → ToStereo48 → PCM 通道
    let mut conv = ToStereo48::new(rate as u32, ch as usize);
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
        let chunk = {
            let Ok(mut q) = shared.queue.lock() else {
                break;
            };
            std::mem::take(&mut *q)
        };
        if chunk.is_empty() {
            continue;
        }
        let floats: Vec<f32> = chunk
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let pcm = conv.push(&floats);
        if !pcm.is_empty() && tx.send(ToMixer::Pcm(0, pcm, Instant::now())).is_err() {
            break;
        }
    }
    let _ = AudioDeviceStop(devid, proc_id);
    let _ = AudioDeviceDestroyIOProcID(devid, proc_id);
    Ok(())
}

// ---------------------------------------------------------------- AAC 转换器

/// AudioToolbox AAC 编码器封装（拉取式：FillComplexBuffer 从输入回调拿 PCM）
struct AacConverter {
    conv: AudioConverterRef,
}

impl AacConverter {
    unsafe fn new() -> Result<Self, String> {
        let input = AudioStreamBasicDescription {
            mSampleRate: RATE as f64,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 4,
            mFramesPerPacket: 1,
            mBytesPerFrame: 4,
            mChannelsPerFrame: 2,
            mBitsPerChannel: 16,
            mReserved: 0,
        };
        let output = AudioStreamBasicDescription {
            mSampleRate: RATE as f64,
            mFormatID: kAudioFormatMPEG4AAC,
            mFormatFlags: 0,
            mBytesPerPacket: 0,
            mFramesPerPacket: SAMPLES_PER_FRAME,
            mBytesPerFrame: 0,
            mChannelsPerFrame: 2,
            mBitsPerChannel: 0,
            mReserved: 0,
        };
        let mut conv: AudioConverterRef = std::ptr::null_mut();
        let st = AudioConverterNew(
            NonNull::from(&input),
            NonNull::from(&output),
            NonNull::from(&mut conv),
        );
        if st != 0 || conv.is_null() {
            return Err(format!("AAC 转换器创建失败: {st}"));
        }
        let bitrate: u32 = super::BITRATE_KBPS * 1000;
        let st = AudioConverterSetProperty(
            conv,
            K_AUDIO_CONVERTER_ENCODE_BITRATE,
            std::mem::size_of::<u32>() as u32,
            NonNull::from(&bitrate).cast(),
        );
        if st != 0 {
            let _ = AudioConverterDispose(conv);
            return Err(format!("AAC 码率设置失败: {st}"));
        }
        Ok(Self { conv })
    }

    /// 编码一帧（消费 pending 头部 4096 字节；不足返回 None）
    unsafe fn encode_frame(&mut self, pending: &mut Vec<u8>) -> Result<Option<Vec<u8>>, String> {
        if pending.len() < PCM_CHUNK {
            return Ok(None);
        }
        // 本帧 PCM：在 FillComplexBuffer 调用期间存活，回调只借用其指针
        let chunk: Vec<u8> = pending.drain(..PCM_CHUNK).collect();
        struct FeedCtx<'a> {
            chunk: &'a [u8],
            used: bool,
        }
        unsafe extern "C-unwind" fn input_proc(
            _conv: AudioConverterRef,
            mut io_packets: NonNull<u32>,
            mut io_data: NonNull<AudioBufferList>,
            _out_descs: *mut *mut AudioStreamPacketDescription,
            user: *mut c_void,
        ) -> i32 {
            let feed = &mut *(user as *mut FeedCtx);
            if feed.used {
                // 一次调用只喂一包（一帧）；再要就报「暂无数据」
                *io_packets.as_mut() = 0;
                return 0;
            }
            feed.used = true;
            *io_packets.as_mut() = 1;
            let list = io_data.as_mut();
            list.mNumberBuffers = 1;
            list.mBuffers[0] = AudioBuffer {
                mNumberChannels: 2,
                mDataByteSize: feed.chunk.len() as u32,
                mData: feed.chunk.as_ptr() as *mut c_void,
            };
            0
        }

        let mut out = vec![0u8; 2048];
        let mut out_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: 2,
                mDataByteSize: out.len() as u32,
                mData: out.as_mut_ptr() as *mut c_void,
            }],
        };
        let mut packets: u32 = 1;
        let mut desc = AudioStreamPacketDescription {
            mStartOffset: 0,
            mVariableFramesInPacket: 0,
            mDataByteSize: 0,
        };
        let mut feed = FeedCtx {
            chunk: &chunk,
            used: false,
        };
        let st = AudioConverterFillComplexBuffer(
            self.conv,
            Some(input_proc),
            &mut feed as *mut FeedCtx as *mut c_void,
            NonNull::from(&mut packets),
            NonNull::from(&mut out_list),
            &mut desc,
        );
        if st != 0 {
            return Err(format!("AAC 编码失败: {st}"));
        }
        if packets == 0 {
            return Ok(None);
        }
        let len = if desc.mDataByteSize > 0 {
            desc.mDataByteSize
        } else {
            out_list.mBuffers[0].mDataByteSize
        } as usize;
        if len == 0 || len > out.len() {
            return Ok(None);
        }
        out.truncate(len);
        Ok(Some(out))
    }
}

impl Drop for AacConverter {
    fn drop(&mut self) {
        unsafe {
            let _ = AudioConverterDispose(self.conv);
        }
    }
}

/// 编码线程：PCM 块 → 4096 字节整块 → AudioConverter → AAC 帧队列。
/// 通道关闭（采集全停）后退出（不足一帧的尾巴 <21ms 丢弃）
fn encoder_thread(
    rx: Receiver<Vec<u8>>,
    tx: &Sender<(AacMeta, Vec<u8>)>,
    counter: &Arc<AtomicUsize>,
    err: &Arc<Mutex<Option<String>>>,
) {
    unsafe {
        let mut enc = match AacConverter::new() {
            Ok(e) => e,
            Err(e) => {
                *err.lock().unwrap() = Some(e);
                return;
            }
        };
        let mut pending: Vec<u8> = Vec::new();
        while let Ok(pcm) = rx.recv() {
            pending.extend_from_slice(&pcm);
            while pending.len() >= PCM_CHUNK {
                match enc.encode_frame(&mut pending) {
                    Ok(Some(frame)) => {
                        counter.fetch_add(1, Ordering::Relaxed);
                        if tx.send((FIXED_META, frame)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let mut slot = err.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(e);
                        }
                        return;
                    }
                }
            }
        }
    }
}
