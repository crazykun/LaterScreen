//! macOS 录屏音频：CoreAudio HAL 麦克风 + ScreenCaptureKit 系统声 +
//! AudioToolbox AAC 编码。
//!
//! - 麦克风：默认输入设备 + IOProc 回调（原生格式恒为 f32 交错），回调内
//!   仅做 memcpy 入队，转换（[`ToStereo48`](super::ToStereo48)）与发送在
//!   转储线程完成，避免在实时线程上做过长的工作
//! - 系统声：ScreenCaptureKit 的 SCStream 音频输出（**macOS 13.0+**；
//!   sampleRate/channelCount 配置项 12.x 没有）。音频-only 流（capturesVideo
//!   = false）+ 自建串行 dispatch 队列收 CMSampleBuffer，回调里展平成
//!   交错 f32 入队，转储线程与麦克风同款转换路径。进程需有「屏幕录制」
//!   TCC 权限（与截图共用同一权限，正常用户已授予）
//! - 编码：AudioConverter（PCM s16 → AAC-LC 48k 立体声 128kbps），
//!   `AudioConverterFillComplexBuffer` 拉取式喂入，每包 = 一帧裸 AAC
//!   （1024 样本，与混流层假设一致）
//!
//! AAC 编码器固有的 priming（~2112 样本 ≈ 44ms）会带来等量解码延迟，
//! mp4 侧无 edit list 修剪，A/V 偏差远低于人眼可感，记录在案。
//!
//! **真机点验**：✅ 2026-09-19（macOS 15.3.1）麦克风（CoreAudio+AAC，
//! A/V 0.063s）与系统声（SCK，A/V 0.241s）e2e 全过；点验修正的盲写
//! 缺陷见 `K_AUDIO_CONVERTER_ENCODE_BITRATE` 与 `sck_extract` 注释。
//! ScreenCaptureKit 为强链接框架，产物最低系统要求随本功能升至
//! macOS 12.3（系统声 13+）；cargo 测试进程的 TCC 归属终端 App，
//! 首跑授权步骤见 docs/VERIFY.md mac 节。

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass};
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
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSArray, NSError, NSInteger};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType, SCWindow,
};

use super::super::{AudioSource, RecordError};
use super::{run_mixer, AacMeta, Core, ToMixer, ToStereo48, FIXED_META, RATE, SAMPLES_PER_FRAME};

/// 一帧 AAC 对应的 PCM 大小（s16 立体声）
const PCM_CHUNK: usize = SAMPLES_PER_FRAME as usize * 4;

/// kAudioConverterEncodeBitRate = 'brat'（objc2-audio-toolbox 未生成此常量）。
/// 真机点验修正：曾盲写成 'brte'——不存在的属性 ID，SetProperty 报
/// 'prop'（PropertyNotSupported），编码线程启动即死、音轨整条丢失
const K_AUDIO_CONVERTER_ENCODE_BITRATE: u32 = 0x6272_6174;

/// 采集源就绪等待上限。默认 5s；**首次运行** macOS 会同步弹 TCC 权限框
/// （麦克风/屏幕录制），用户应答前底层 CoreAudio/SCK 调用一直阻塞——
/// 5s 内点不完就会以「音频服务无响应」误报。用
/// `LSCREEN_AUDIO_READY_TIMEOUT_MS` 放宽（真机首跑/自动化场景，
/// 1s–300s 钳位，非法值回落默认）
fn ready_timeout() -> Duration {
    const DEFAULT_MS: u64 = 5_000;
    let ms = std::env::var("LSCREEN_AUDIO_READY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_MS)
        .clamp(1_000, 300_000);
    Duration::from_millis(ms)
}

/// 采集线程初始化结果（start() 等待全部就绪或失败，实现快速失败语义）
enum Ready {
    Ok,
    Fail(String),
}

/// 采集源类型：麦克风 = CoreAudio HAL；系统声 = ScreenCaptureKit
#[derive(Clone, Copy)]
enum SourceKind {
    Hal,
    Sck,
}

pub(crate) struct Pipeline {
    core: Core,
    stop: Arc<AtomicBool>,
}

impl Pipeline {
    pub(crate) fn start(source: AudioSource) -> Result<Self, RecordError> {
        let sources: Vec<(SourceKind, &'static str)> = match source {
            AudioSource::Mic => vec![(SourceKind::Hal, "麦克风")],
            AudioSource::System => vec![(SourceKind::Sck, "系统声")],
            AudioSource::Both => vec![(SourceKind::Hal, "麦克风"), (SourceKind::Sck, "系统声")],
        };
        let (mut core, frame_tx) = Core::new(source);
        let (pcm_tx, pcm_rx) = channel::<ToMixer>();
        let (ready_tx, ready_rx) = channel::<Ready>();
        let stop = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::new();

        // 采集线程 × N：HAL 建立起流 / SCK startCapture 成功即报告就绪
        for (i, &(kind, _)) in sources.iter().enumerate() {
            let tx = pcm_tx.clone();
            let rtx = ready_tx.clone();
            let stop = Arc::clone(&stop);
            workers.push(std::thread::spawn(move || {
                let r = match kind {
                    SourceKind::Hal => hal_capture_thread(&stop, &tx, &rtx, i),
                    SourceKind::Sck => sck_capture_thread(&stop, &tx, &rtx, i),
                };
                if let Err(e) = r {
                    // 开录前 = 快速失败；开录后 = 运行期故障，收尾对账兜底
                    let _ = rtx.send(Ready::Fail(e));
                }
                let _ = tx.send(ToMixer::Eof(i));
            }));
        }
        drop(ready_tx);
        drop(pcm_tx);

        // 等全部采集源初始化完成（SCK 内容枚举百毫秒级；首次运行 TCC
        // 权限框等待用户应答，可用环境变量放宽，见 ready_timeout）
        for &(_, what) in sources.iter() {
            match ready_rx.recv_timeout(ready_timeout()) {
                Ok(Ready::Ok) => {}
                Ok(Ready::Fail(e)) => {
                    stop.store(true, Ordering::Relaxed);
                    return Err(RecordError(format!("启动{what}采集失败: {e}")));
                }
                Err(_) => {
                    stop.store(true, Ordering::Relaxed);
                    return Err(RecordError(format!(
                        "启动{what}采集超时（音频服务无响应；若首次运行，请查看系统是否正在请求 麦克风/屏幕录制 权限）"
                    )));
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

// ---------------------------------------------------------------- HAL 麦克风

/// 回调/转储线程共享的采集上下文（user data 指向它）
struct CaptureShared {
    queue: Mutex<Vec<u8>>,
}

/// 麦克风源的一生（独立线程）；起流成功后经 rtx 报告就绪（快速失败）
fn hal_capture_thread(
    stop: &AtomicBool,
    tx: &Sender<ToMixer>,
    rtx: &Sender<Ready>,
    idx: usize,
) -> Result<(), String> {
    unsafe { run_capture(stop, tx, rtx, idx) }
}

unsafe fn run_capture(
    stop: &AtomicBool,
    tx: &Sender<ToMixer>,
    rtx: &Sender<Ready>,
    idx: usize,
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
        // mBuffers 是变长结构的 [AudioBuffer; 1] 建模，必须按 mNumberBuffers
        // 展开成切片再访问——直接 [i] 索引在多缓冲布局下越界 panic，
        // panic=abort 会带走整个录制进程
        let bufs =
            std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers.min(8) as usize);
        for b in bufs {
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
        if !pcm.is_empty() && tx.send(ToMixer::Pcm(idx, pcm, Instant::now())).is_err() {
            break;
        }
    }
    let _ = AudioDeviceStop(devid, proc_id);
    let _ = AudioDeviceDestroyIOProcID(devid, proc_id);
    Ok(())
}

// ---------------------------------------------------------------- ScreenCaptureKit 系统声

/// SCK 音频回调与转储线程的共享状态
struct SckState {
    /// 回调展平后的交错 f32 帧（转储线程排空）
    pcm: Mutex<Vec<f32>>,
    /// 首帧数据的声道数（0 = 尚未见到数据，转换器按它建立）
    ch: AtomicUsize,
    /// 首个提取错误（运行期降级：转储线程发现后退出，收尾对账提示）
    err: Mutex<Option<String>>,
}

define_class!(
    // SAFETY:
    // - The superclass NSObject does not have any subclassing requirements.
    // - `SckOutput` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[ivars = Arc<SckState>]
    struct SckOutput;

    unsafe impl NSObjectProtocol for SckOutput {}

    unsafe impl SCStreamOutput for SckOutput {
        // 音频 CMSampleBuffer 到达（挂载在自建串行队列上，非实时线程，
        // 做提取与入队是安全的；实时约束只存在于 CoreAudio IOProc）
        // 方法名与 SCStreamOutput 协议声明保持一致（非 snake_case）
        #[allow(non_snake_case)]
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_didOutputSampleBuffer_ofType(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            r#type: SCStreamOutputType,
        ) {
            if r#type != SCStreamOutputType::Audio {
                return;
            }
            let state = self.ivars();
            match unsafe { sck_extract(sample_buffer) } {
                Ok((frames, ch)) => {
                    if ch > 0 {
                        state.ch.store(ch, Ordering::Relaxed);
                    }
                    if !frames.is_empty() {
                        state.pcm.lock().unwrap().extend(frames);
                    }
                }
                Err(e) => {
                    let mut slot = state.err.lock().unwrap();
                    if slot.is_none() {
                        *slot = Some(e);
                    }
                }
            }
        }
    }
);

impl SckOutput {
    fn new(state: Arc<SckState>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(state);
        unsafe { msg_send![super(this), init] }
    }
}

/// AudioBuffer 的数据体（空缓冲返回空切片）
unsafe fn ab_bytes(b: &AudioBuffer) -> &[u8] {
    if b.mData.is_null() || b.mDataByteSize == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(b.mData as *const u8, b.mDataByteSize as usize)
    }
}

/// 从 CMSampleBuffer 提取交错 f32 帧，返回 (样本, 声道数)。
/// 采样率按配置恒为 48k（SCStreamConfiguration 契约：音频格式由
/// sampleRate/channelCount 决定）；声道布局以实际 AudioBufferList 为准：
/// 单缓冲多声道 = 交错，多缓冲单声道 = 非交错（手工交织）
///
/// 真机点验修正（2026-09-19）：两次实测认知——
/// 1. `bufferListSize` 必须**精确等于**首查返回的 `needed`（实测定长 40
///    字节的列表传 512 字节容量反而报 -12737 ArrayTooSmall，超大同样拒绝）
/// 2. `block_buffer_out` 必须给真指针并保留到复制完成：列表里的 mData
///    指向返回的 CMBlockBuffer 内部，不持有它数据指针随时可能失效
unsafe fn sck_extract(sbuf: &CMSampleBuffer) -> Result<(Vec<f32>, usize), String> {
    // 变长结构 AudioBufferList：先探需要多少字节，再取进对齐的栈缓冲
    let mut needed: usize = 0;
    let st = sbuf.audio_buffer_list_with_retained_block_buffer(
        &mut needed,
        std::ptr::null_mut(),
        0,
        None,
        None,
        0,
        std::ptr::null_mut(),
    );
    if st != 0 || needed == 0 {
        return Err(format!("读取音频缓冲失败: {st}"));
    }
    // 头 8 字节 + 每缓冲 16 字节；立体声 40 字节。异常大的请求直接拒绝
    const MAX_LIST: usize = 512;
    if needed > MAX_LIST {
        return Err(format!("音频声道布局异常（AudioBufferList {needed} 字节）"));
    }
    let mut storage = [0u64; MAX_LIST / 8];
    let list_ptr = storage.as_mut_ptr() as *mut AudioBufferList;
    // 数据体生命周期挂在返回的 CMBlockBuffer 上：持有它直到复制完成
    let mut bb: *mut objc2_core_media::CMBlockBuffer = std::ptr::null_mut();
    let st = sbuf.audio_buffer_list_with_retained_block_buffer(
        std::ptr::null_mut(),
        list_ptr,
        needed,
        None,
        None,
        0,
        &mut bb,
    );
    if st != 0 {
        return Err(format!("提取音频缓冲失败: {st}"));
    }
    // 持有返回的 CMBlockBuffer 直到解析完成（from_raw 接管 +1 引用，
    // drop 时 release；null 时无持有）
    let bb_guard = objc2::rc::Retained::from_raw(bb);
    let r = sck_parse_list(&*list_ptr);
    drop(bb_guard);
    r
}

/// 解析 AudioBufferList 为交错 f32（数据已在栈上复制完成，无生命周期顾虑）
unsafe fn sck_parse_list(list: &AudioBufferList) -> Result<(Vec<f32>, usize), String> {
    let f32s = |b: &[u8]| -> Vec<f32> {
        // 字节级读取：mData 不保证 f32 对齐（16 字节对齐需显式传标志）
        b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect::<Vec<f32>>()
    };
    let nbuf = list.mNumberBuffers.min(8) as usize;
    if nbuf == 0 {
        return Ok((Vec::new(), 0));
    }
    if nbuf == 1 {
        let b = &list.mBuffers[0];
        let ch = b.mNumberChannels.max(1) as usize;
        return Ok((f32s(unsafe { ab_bytes(b) }), ch));
    }
    // 非交错：每缓冲一声道，按最短缓冲对齐帧数后交织。mBuffers 是
    // [AudioBuffer; 1] 建模的变长结构，先按 nbuf 展开切片（直接 [i] 索引
    // 越界 panic，见 HAL ioproc 同款注释）
    let bufs: Vec<&[u8]> = std::slice::from_raw_parts(list.mBuffers.as_ptr(), nbuf)
        .iter()
        .map(|b| unsafe { ab_bytes(b) })
        .collect();
    let frames = bufs.iter().map(|b| b.len() / 4).min().unwrap_or(0);
    let mut out = Vec::with_capacity(frames * nbuf);
    for f in 0..frames {
        for b in &bufs {
            out.push(f32::from_le_bytes([
                b[f * 4],
                b[f * 4 + 1],
                b[f * 4 + 2],
                b[f * 4 + 3],
            ]));
        }
    }
    Ok((out, nbuf))
}

/// 系统声源的一生（独立线程）：SCK 音频-only 流建立 → 就绪 → 转储循环 →
/// 停机拆卸。macOS 13+（版本门在 run 内做）；需「屏幕录制」TCC 权限
fn sck_capture_thread(
    stop: &AtomicBool,
    tx: &Sender<ToMixer>,
    rtx: &Sender<Ready>,
    idx: usize,
) -> Result<(), String> {
    unsafe { run_sck_capture(stop, tx, rtx, idx) }
}

unsafe fn run_sck_capture(
    stop: &AtomicBool,
    tx: &Sender<ToMixer>,
    rtx: &Sender<Ready>,
    idx: usize,
) -> Result<(), String> {
    // 版本门：setSampleRate:/setChannelCount: 是 macOS 13 API，12.x 上调用
    // 是未识别选择子（直接崩溃），必须先探测
    let probe = SCStreamConfiguration::new();
    if !probe.respondsToSelector(sel!(setSampleRate:)) {
        return Err("系统声内录需 macOS 13.0+（12.x 无 ScreenCaptureKit 音频输出）".into());
    }
    drop(probe);

    // 枚举可捕获内容（异步 block → 通道等待）。无「屏幕录制」权限时这里
    // 拿不到 display 或直接报错
    let (ct_tx, ct_rx) = channel::<Result<Retained<SCShareableContent>, String>>();
    let ct_block: RcBlock<dyn Fn(*mut SCShareableContent, *mut NSError)> = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let r = if let Some(e) = unsafe { Retained::retain(error) } {
                Err(e.localizedDescription().to_string())
            } else if let Some(c) = unsafe { Retained::retain(content) } {
                Ok(c)
            } else {
                Err("无法获取屏幕内容（系统设置 → 隐私与安全性 → 屏幕录制 未授权？）".into())
            };
            let _ = ct_tx.send(r);
        },
    );
    SCShareableContent::getShareableContentWithCompletionHandler(&ct_block);
    let content = ct_rx
        .recv_timeout(ready_timeout())
        .map_err(|_| "枚举屏幕内容超时（屏幕录制权限未授予或系统忙）".to_string())??;
    // 音频与显示器无关（系统级混音），任取一块即可；空列表 = 权限被拒
    let display = content
        .displays()
        .firstObject()
        .ok_or_else(|| "无可捕获的显示器（屏幕录制权限被拒？）".to_string())?;

    // 系统声：SCStream 无「关视频」开关（视频是流的默认产物），把尺寸压到
    // 2×2 让合成开销可忽略，只挂音频输出（视频帧无接收方即丢弃）；
    // 48k/立体声与管线固定格式一致（音频格式契约由 sampleRate/channelCount 决定）
    let excluded = NSArray::<SCWindow>::array();
    let filter = SCContentFilter::initWithDisplay_excludingWindows(
        SCContentFilter::alloc(),
        &display,
        &excluded,
    );
    let cfg = SCStreamConfiguration::new();
    cfg.setWidth(2);
    cfg.setHeight(2);
    cfg.setCapturesAudio(true);
    cfg.setSampleRate(RATE as NSInteger);
    cfg.setChannelCount(2);
    let stream =
        SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &cfg, None);

    // 输出挂载：回调类 + 自建串行队列（不传队列可能落到主队列，CLI/测试
    // 场景主线程不跑 runloop 会永远收不到回调）
    let state = Arc::new(SckState {
        pcm: Mutex::new(Vec::new()),
        ch: AtomicUsize::new(0),
        err: Mutex::new(None),
    });
    let output = SckOutput::new(Arc::clone(&state));
    let queue = DispatchQueue::new("lscreen.record.sck", None);
    stream
        .addStreamOutput_type_sampleHandlerQueue_error(
            ProtocolObject::from_ref(&*output),
            SCStreamOutputType::Audio,
            Some(&queue),
        )
        .map_err(|e| format!("挂载音频输出失败: {}", e.localizedDescription()))?;

    // 开流（完成回调里的错误在此可见：权限被收回/音频服务不可用）
    let (st_tx, st_rx) = channel::<Option<String>>();
    let st_block: RcBlock<dyn Fn(*mut NSError)> = RcBlock::new(move |error: *mut NSError| {
        let msg = unsafe { Retained::retain(error) }.map(|e| e.localizedDescription().to_string());
        let _ = st_tx.send(msg);
    });
    stream.startCaptureWithCompletionHandler(Some(&st_block));
    match st_rx.recv_timeout(ready_timeout()) {
        Ok(None) => {}
        Ok(Some(e)) => return Err(format!("系统声采集开启失败: {e}")),
        Err(_) => return Err("等待系统声采集开启超时".into()),
    }
    let _ = rtx.send(Ready::Ok);

    // 转储循环：回调入队的交错 f32 → ToStereo48 → PCM 通道（麦克风同款）
    let mut conv: Option<ToStereo48> = None;
    while !stop.load(Ordering::Relaxed) {
        if let Some(e) = state.err.lock().unwrap().take() {
            // 运行期故障：退出线程走对账告警，不毁视频
            return Err(format!("系统声数据异常: {e}"));
        }
        std::thread::sleep(Duration::from_millis(10));
        let chunk = std::mem::take(&mut *state.pcm.lock().unwrap());
        if chunk.is_empty() {
            continue;
        }
        if conv.is_none() {
            let ch = state.ch.load(Ordering::Relaxed);
            if ch == 0 {
                continue; // 声道数未定（理论到不了这里：有数据必有 ch）
            }
            conv = Some(ToStereo48::new(RATE, ch));
        }
        let pcm = conv.as_mut().unwrap().push(&chunk);
        if !pcm.is_empty() && tx.send(ToMixer::Pcm(idx, pcm, Instant::now())).is_err() {
            break;
        }
    }

    // 收尾：停流等完成 → 摘输出 → Retained 释放（RAII）。停流不干净时
    // 2s 后也继续走 Drop（SCStream 析构会再停一次）
    let (sp_tx, sp_rx) = channel::<()>();
    let sp_block: RcBlock<dyn Fn(*mut NSError)> = RcBlock::new(move |_error: *mut NSError| {
        let _ = sp_tx.send(());
    });
    stream.stopCaptureWithCompletionHandler(Some(&sp_block));
    let _ = sp_rx.recv_timeout(Duration::from_secs(2));
    let _ = stream.removeStreamOutput_type_error(
        ProtocolObject::from_ref(&*output),
        SCStreamOutputType::Audio,
    );
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
