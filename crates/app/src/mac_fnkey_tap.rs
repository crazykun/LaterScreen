//! mac 裸 F1–F12 热键兜底层：CGEventTap 主动拦截（Snipaste 同机制）。
//!
//! 背景：macOS 默认（fnState=0）把裸 F1–F12 翻译成媒体/系统键（亮度/
//! Spotlight/听写），翻译发生在 Carbon RegisterEventHotKey 匹配之前，
//! 走 global-hotkey（Carbon）注册的裸 F 键「注册成功但永不触发」。持有
//! 辅助功能权限后可在 kCGHIDEventTap 装**主动** tap：媒体翻译之前看到
//! keyDown（虚拟键码 0x7A…）并消费（回调返回 NULL），亮度等系统动作
//! 不再发生，同时把热键 id 经注入的 sink 送回 winit 事件循环（与
//! GlobalHotKeyEvent 同一分发路径）。
//!
//! 约束与边界：
//! - 只拦**已注册为热键且无修饰键**的 F 键并过滤自动重复；其余 F 键
//!   （及其修饰组合）一律透传，亮度/媒体功能照常
//! - 标准功能键模式（fnState=1）不消费：让位 Carbon，防双触发。每次
//!   按键实时探测（CFPreferences 进程内，无子进程，3s 缓存），系统
//!   设置中途切换也能跟上
//! - 未授权辅助功能：active tap 建不起来（CGEventTapCreate 返回
//!   NULL），降级为既有行为（fn+F1 / 标准功能键开关），并弹一次系统
//!   授权引导；授权后由托盘 tick 重同步自动装上（无需重启）
//! - tap 源加在主线程 RunLoop（winit 事件循环在主线程跑），回调只做
//!   查表 + 转发；被系统按超时禁用时就地重新 enable
//! - HID 层假设：媒体键模式下 F1 的 keyDown 仍以虚拟键码出现在
//!   kCGHIDEventTap（翻译发生在其后）。若真机点验推翻此假设，备用
//!   方案是 kCGSessionEventTap + NX_SYSDEFINED 解码（见 docs/VERIFY.md
//!   mac 拦截套件的点验项）

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- FFI

type CGEventRef = *mut c_void;
type CGEventTapProxy = *const c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *const c_void;
type CFStringRef = *const c_void;
type CFTypeRef = *const c_void;
type CFBooleanRef = *const c_void;
type CFNumberRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFAllocatorRef = *const c_void;

/// kCGEventKeyDown
const EVENT_KEY_DOWN: u32 = 10;
/// kCGEventTapDisabledByTimeout（通知型事件，event 参数为 NULL）
const EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
/// kCGKeyboardEventKeycode（事件字段序号，core-graphics 0.23 EventField 同源）
const FIELD_KEYCODE: u32 = 9;
/// kCGKeyboardEventAutorepeat：非 0 = 按住产生的重复事件
const FIELD_AUTOREPEAT: u32 = 8;
/// 修饰键掩码（CGEventFlags）。仅这四个参与「裸键」判定：fn 本身忽略
/// （fn+F1 在媒体键模式下也应对该键生效），capslock/小键盘锁无关
const MASK_CTRL: u64 = 1 << 12;
const MASK_SHIFT: u64 = 1 << 17;
const MASK_ALT: u64 = 1 << 19;
const MASK_CMD: u64 = 1 << 20;
/// kCFStringEncodingUTF8
const UTF8: u32 = 0x0800_0100;
/// kCFNumberSInt32Type
const NUMBER_SINT32: isize = 3;

#[repr(C)]
struct CFDictionaryKeyCallBacks {
    _opaque: [usize; 5],
}

#[repr(C)]
struct CFDictionaryValueCallBacks {
    _opaque: [usize; 5],
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap_point: u32,
        placement: u32,
        options: u32,
        event_mask: u64,
        callback: unsafe extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(port: CFMachPortRef, enable: u8);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    static kCFPreferencesAnyApplication: CFStringRef;
    static kCFBooleanTrue: CFBooleanRef;
    static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
    fn CFRunLoopGetMain() -> CFRunLoopRef;
    fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, src: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRemoveSource(rl: CFRunLoopRef, src: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFMachPortInvalidate(port: CFMachPortRef);
    fn CFRelease(cf: CFTypeRef);
    fn CFStringCreateWithCString(
        allocator: CFAllocatorRef,
        c_str: *const std::os::raw::c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFPreferencesCopyAppValue(key: CFStringRef, app_id: CFStringRef) -> CFTypeRef;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFBooleanGetValue(boolean: CFBooleanRef) -> u8;
    fn CFNumberGetValue(number: CFNumberRef, the_type: isize, value_ptr: *mut c_void) -> u8;
    fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: isize,
        key_callbacks: *const CFDictionaryKeyCallBacks,
        value_callbacks: *const CFDictionaryValueCallBacks,
    ) -> CFDictionaryRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    /// 返回 Boolean（u8）；无辅助功能权限 = 0。注意：调用本身无副作用，
    /// 弹窗版是 AXIsProcessTrustedWithOptions
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
}

// ---------------------------------------------------------------- 状态探测

/// fnState（「将 F1、F2 等键用作标准功能键」开关）：Some(true) = 标准功能
/// 键模式；Some(false) = 媒体键模式（键不存在即系统默认）；None = 值类型
/// 异常，不动手猜。3s 缓存：CFPreferences 未命中会走 cfprefsd 往返，
/// 按键级探测太密，3s 足够跟随系统设置切换（tick 重同步兜底）
fn fn_keys_standard() -> Option<bool> {
    static CACHE: Mutex<Option<(Instant, Option<bool>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, v)) = *cache {
        if at.elapsed() < Duration::from_secs(3) {
            return v;
        }
    }
    let v = probe_fn_state();
    *cache = Some((Instant::now(), v));
    v
}

fn probe_fn_state() -> Option<bool> {
    unsafe {
        let v = CFPreferencesCopyAppValue(fn_state_key(), kCFPreferencesAnyApplication);
        if v.is_null() {
            // 键不存在 = 系统默认媒体键模式
            return Some(false);
        }
        let tid = CFGetTypeID(v);
        let val = if tid == CFBooleanGetTypeID() {
            CFBooleanGetValue(v as CFBooleanRef) != 0
        } else if tid == CFNumberGetTypeID() {
            // 手写 defaults write -int 1 的情况
            let mut i: i32 = 0;
            if CFNumberGetValue(
                v as CFNumberRef,
                NUMBER_SINT32,
                &mut i as *mut i32 as *mut c_void,
            ) == 0
            {
                CFRelease(v);
                return None;
            }
            i != 0
        } else {
            CFRelease(v);
            return None;
        };
        CFRelease(v);
        Some(val)
    }
}

/// com.apple.keyboard.fnState 的 CFString（进程存活期复用，不释放）
fn fn_state_key() -> CFStringRef {
    static KEY: OnceLock<usize> = OnceLock::new();
    *KEY.get_or_init(|| unsafe {
        CFStringCreateWithCString(
            std::ptr::null(),
            c"com.apple.keyboard.fnState".as_ptr(),
            UTF8,
        ) as usize
    }) as CFStringRef
}

fn ax_trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

/// 弹一次系统辅助功能授权引导（打开 系统设置▸隐私与安全性▸辅助功能）。
/// 用户添加 lscreen 后由托盘 tick 重同步（探测 3s 缓存）自动装上 tap，
/// 无需重启
fn ax_prompt_once() {
    static PROMPTED: AtomicBool = AtomicBool::new(false);
    if PROMPTED.swap(true, Ordering::Relaxed) {
        return;
    }
    unsafe {
        let key = CFStringCreateWithCString(
            std::ptr::null(),
            c"AXTrustedCheckOptionPrompt".as_ptr(),
            UTF8,
        );
        if key.is_null() {
            return;
        }
        let val = kCFBooleanTrue;
        let dict = CFDictionaryCreate(
            std::ptr::null(),
            [key].as_ptr(),
            [val].as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        if !dict.is_null() {
            AXIsProcessTrustedWithOptions(dict);
            CFRelease(dict);
        }
        CFRelease(key);
    }
}

// ---------------------------------------------------------------- tap 本体

/// 回调送回通道：把命中的热键 id 发回 winit 事件循环（与
/// GlobalHotKeyEvent::set_event_handler 同一分发路径）。run_native 在
/// 创建 EventLoopProxy 后注入
pub type HotkeySink = Arc<dyn Fn(u32) + Send + Sync>;

static SINK: OnceLock<HotkeySink> = OnceLock::new();

pub fn set_sink(sink: HotkeySink) {
    let _ = SINK.set(sink);
}

struct TapCtx {
    sink: HotkeySink,
    /// 虚拟键码 → 热键 id（只含已注册的裸 F 键），apply/热重载时整体替换
    map: Mutex<HashMap<i64, u32>>,
}

unsafe extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    ev_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    if ev_type == EVENT_TAP_DISABLED_BY_TIMEOUT {
        // 回调超时被系统禁用：就地重启（此时 event 为 NULL）
        unsafe { CGEventTapEnable(user_info as CFMachPortRef, 1) };
        return std::ptr::null_mut();
    }
    if ev_type != EVENT_KEY_DOWN {
        return event;
    }
    let ctx = unsafe { &*(user_info as *const TapCtx) };
    let vk = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYCODE) };
    let Some(&hotkey_id) = ctx.map.lock().unwrap_or_else(|p| p.into_inner()).get(&vk) else {
        return event;
    };
    let flags = unsafe { CGEventGetFlags(event) };
    if flags & (MASK_CTRL | MASK_SHIFT | MASK_ALT | MASK_CMD) != 0 {
        // 修饰组合键：Carbon 本就收得到（媒体翻译只吃裸键），透传
        return event;
    }
    if unsafe { CGEventGetIntegerValueField(event, FIELD_AUTOREPEAT) } != 0 {
        // 按住自动重复：消费（不让亮度触发）但不重复派发动作
        return std::ptr::null_mut();
    }
    if fn_keys_standard() == Some(true) {
        // 标准功能键模式：让位 Carbon，防双触发
        return event;
    }
    (ctx.sink)(hotkey_id);
    // 消费：媒体键翻译（亮度/Spotlight/听写）不再发生
    std::ptr::null_mut()
}

/// 已装的拦截层。Drop 先摘 RunLoop 源并失效端口（此后不再有回调），
/// 再回收 userdata——回调与 Drop 同在主线程，顺序保证无 use-after-free
pub struct FnKeyTap {
    port: CFMachPortRef,
    source: CFRunLoopSourceRef,
    /// 传给 CGEventTapCreate 的 Arc 原始指针（Drop 回收）
    ctx_raw: *const TapCtx,
    ctx: Arc<TapCtx>,
}

/// 一次 sync 的结论，调用方据此决定是否提示
pub enum TapStatus {
    /// tap 已装好/映射已更新：裸 F 键由拦截层接管
    Installed,
    /// 无裸 F 键热键，或标准功能键模式（Carbon 直达）：无需 tap
    NotNeeded,
    /// 媒体键模式但无辅助功能权限：已弹授权引导，降级为旧行为
    NoTrust,
}

/// 按当前注册的裸 F 键集合装卸/更新 tap。`bare` 为空即卸载。
/// 探测有 3s 缓存，供托盘 tick 周期重同步空转调用
pub fn sync(tap: &mut Option<FnKeyTap>, bare: &[(i64, u32)]) -> TapStatus {
    if bare.is_empty() || fn_keys_standard() == Some(true) {
        *tap = None;
        return TapStatus::NotNeeded;
    }
    if !ax_trusted() {
        ax_prompt_once();
        *tap = None;
        return TapStatus::NoTrust;
    }
    let Some(sink) = SINK.get().cloned() else {
        // 理论不可达：set_sink 在 run_native 创建 proxy 后、首次 apply 前调用
        return TapStatus::NoTrust;
    };
    let map: HashMap<i64, u32> = bare.iter().copied().collect();
    if let Some(t) = tap {
        *t.ctx.map.lock().unwrap_or_else(|p| p.into_inner()) = map;
        return TapStatus::Installed;
    }
    match install(map, sink) {
        Some(t) => {
            *tap = Some(t);
            TapStatus::Installed
        }
        // Create 失败基本等于权限未生效（TCC 授权有延迟）：按无权限降级
        None => {
            ax_prompt_once();
            TapStatus::NoTrust
        }
    }
}

fn install(map: HashMap<i64, u32>, sink: HotkeySink) -> Option<FnKeyTap> {
    let ctx = Arc::new(TapCtx {
        sink,
        map: Mutex::new(map),
    });
    let ctx_raw = Arc::into_raw(ctx.clone());
    // C 的 CGEventMaskBit 对通知型常量 0xFFFFFFFE 按硬件移位截断（&63=62），
    // 与 macOS 真机各 tap 实现的实际行为一致
    let mask = mask_bit(EVENT_KEY_DOWN) | mask_bit(EVENT_TAP_DISABLED_BY_TIMEOUT);
    unsafe {
        // kCGHIDEventTap=0 / kCGHeadInsertEventTap=0 / kCGEventTapOptionDefault=0（active）
        let port = CGEventTapCreate(0, 0, 0, mask, tap_callback, ctx_raw as *mut c_void);
        if port.is_null() {
            drop(Arc::from_raw(ctx_raw));
            return None;
        }
        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
        if source.is_null() {
            CFMachPortInvalidate(port);
            CFRelease(port as CFTypeRef);
            drop(Arc::from_raw(ctx_raw));
            return None;
        }
        CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopCommonModes);
        CGEventTapEnable(port, 1);
        Some(FnKeyTap {
            port,
            source,
            ctx_raw,
            ctx,
        })
    }
}

fn mask_bit(ev_type: u32) -> u64 {
    1u64 << (ev_type & 63)
}

impl Drop for FnKeyTap {
    fn drop(&mut self) {
        unsafe {
            CFRunLoopRemoveSource(CFRunLoopGetMain(), self.source, kCFRunLoopCommonModes);
            CFRelease(self.source as CFTypeRef);
            CFMachPortInvalidate(self.port);
            CFRelease(self.port as CFTypeRef);
            drop(Arc::from_raw(self.ctx_raw));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 测试专用 FFI：构造合成 CGEvent 直调 tap 回调（这些声明只在测试编译）
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            virtual_key: i64,
            key_down: bool,
        ) -> CGEventRef;
        fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
        fn CGEventSetFlags(event: CGEventRef, flags: u64);
    }

    /// 真机语义测试（M17 风格，ignored）：直接以合成 CGEvent 调 tap 回调，
    /// 覆盖四个分支——命中消费/透传、自动重复、修饰组合、未注册键。
    /// 同时报告本机 fnState 模式与测试二进制的辅助功能授权状态（真
    /// 托盘路径能否装上 tap 的判定依据）。
    ///   LSCREEN_TEST_E2E=1 cargo test -p lscreen -- --ignored --nocapture fnkey
    #[test]
    #[ignore = "真机：读系统偏好（fnState/AX），需 LSCREEN_TEST_E2E=1"]
    fn fnkey_tap_semantics() {
        if std::env::var("LSCREEN_TEST_E2E").as_deref() != Ok("1") {
            eprintln!("跳过：需 LSCREEN_TEST_E2E=1");
            return;
        }
        let standard = fn_keys_standard();
        let trusted = ax_trusted();
        println!("本机 fnState: {standard:?}（Some(false)=媒体键模式）、AX 授权（测试二进制）: {trusted}");

        let fired = Arc::new(Mutex::new(Vec::<u32>::new()));
        let f = fired.clone();
        let ctx = TapCtx {
            sink: Arc::new(move |id| f.lock().unwrap().push(id)),
            map: Mutex::new(HashMap::from([(0x7A, 7u32)])), // F1 → id 7
        };
        let info = &ctx as *const TapCtx as *mut c_void;
        let mk = |vk: i64| unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), vk, true) };
        let fired_ids = || fired.lock().unwrap().clone();

        // 1) 已注册裸 F1：媒体键模式 → 消费 + 派发；标准功能键模式 → 透传
        //    （让位 Carbon 防双触发），都不派发
        let ev = mk(0x7A);
        let ret = unsafe { tap_callback(std::ptr::null(), EVENT_KEY_DOWN, ev, info) };
        match standard {
            Some(false) => {
                assert!(ret.is_null(), "媒体键模式应消费事件");
                assert_eq!(fired_ids(), vec![7], "媒体键模式应派发热键");
            }
            _ => {
                assert!(!ret.is_null(), "标准功能键模式应透传");
                assert!(fired_ids().is_empty(), "标准功能键模式不应派发");
            }
        }
        unsafe { CFRelease(ev) };

        // 2) 按住自动重复：消费（亮度不再触发）但不重复派发——两种模式同判
        fired.lock().unwrap().clear();
        let ev = mk(0x7A);
        unsafe { CGEventSetIntegerValueField(ev, FIELD_AUTOREPEAT, 1) };
        let ret = unsafe { tap_callback(std::ptr::null(), EVENT_KEY_DOWN, ev, info) };
        assert!(ret.is_null(), "自动重复应消费");
        assert!(fired_ids().is_empty(), "自动重复不应派发");
        unsafe { CFRelease(ev) };

        // 3) 带修饰键（Cmd+F1）：Carbon 本就收得到，透传且不派发
        let ev = mk(0x7A);
        unsafe { CGEventSetFlags(ev, MASK_CMD) };
        let ret = unsafe { tap_callback(std::ptr::null(), EVENT_KEY_DOWN, ev, info) };
        assert!(!ret.is_null(), "修饰组合应透传");
        assert!(fired_ids().is_empty(), "修饰组合不应派发");
        unsafe { CFRelease(ev) };

        // 4) 未注册的 F5：透传且不派发
        let ev = mk(0x60);
        let ret = unsafe { tap_callback(std::ptr::null(), EVENT_KEY_DOWN, ev, info) };
        assert!(!ret.is_null(), "未注册键应透传");
        assert!(fired_ids().is_empty(), "未注册键不应派发");
        unsafe { CFRelease(ev) };

        println!("四分支语义全部符合预期");
    }
}
