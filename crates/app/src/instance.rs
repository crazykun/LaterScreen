//! 跨进程单实例控制（v0.11.4）：托盘与配置面板的「多开」治理。
//!
//! 背景（2026-09-29 用户反馈）：合并 PR #1 后实测发现——
//! - `lscreen config` 连开两次会堆叠两个配置窗（各写各的 config.toml，
//!   后保存的覆盖先保存的，未保存改动互相覆盖）；
//! - `lscreen tray --foreground` 双开出现两个托盘图标 + 一串
//!   「热键注册失败」stderr（第二个实例的热键全军覆没却照常驻留）。
//!
//! 方案：复用 history.rs 已验证的 flock/LockFileEx 句柄锁语义（进程退出
//! 自动释放，崩溃不留 stale 锁），抽出**按用途命名**的单例守卫。锁冲突的
//! 第二个实例不再「半死不活地驻留」，而是：
//! - 托盘：提示已驻留后直接退出（托盘没有窗口可唤起）；
//! - 配置面板：留下 raise 信号让活着的窗口跳到前台（连按不再像没反应）。
//!
//! 贴图（`lscreen pin`）**刻意不做单例**：一屏多钉是 Snipaste 式核心用法，
//! 每张贴图独立进程、独立关闭，多开是特性不是 bug。

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 单例用途。锁文件名 = `singleton.<name>.lock`，不同用途互不干扰；
/// 唤起信号 = `singleton.<name>.raise`。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// 常驻托盘进程（裸 `lscreen` / `lscreen tray`）。
    Tray,
    /// 配置面板窗口（`lscreen config`）。
    Settings,
    /// 截图/取色/框选覆盖层（`gui` / `pick` / `record --select` /
    /// `scroll` 框选 / `annotate` 预览）。全屏覆盖层本身就是独占式
    /// 交互——两个热键连按叠出两层覆盖层，第二层截屏时把第一层
    /// 也截进图里。短命（覆盖层 Esc/完成即退），静默让路。
    Overlay,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tray => "tray",
            Self::Settings => "settings",
            Self::Overlay => "overlay",
        }
    }

    /// raise 信号对该用途是否有意义（托盘无窗口可唤；覆盖层短命，
    /// 连按场景「静默让路」比「唤起第一层」更符合直觉——第一层
    /// 本来就在最前，用户看到的是截图界面已经打开）。
    fn can_raise(self) -> bool {
        matches!(self, Self::Settings)
    }

    /// 冲突时第二实例应给用户的话术（None = 静默让路，不刷屏）。
    pub fn conflict_message(self) -> Option<&'static str> {
        match self {
            Self::Tray => Some("LaterScreen 托盘已在运行（热键已生效），本次启动退出"),
            Self::Settings => Some("配置面板已打开，已将其带到前台"),
            Self::Overlay => None,
        }
    }
}

fn lock_path(kind: Kind) -> PathBuf {
    crate::config::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!("singleton.{}.lock", kind.as_str()))
}

fn raise_path(kind: Kind) -> PathBuf {
    crate::config::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!("singleton.{}.raise", kind.as_str()))
}

/// 独占文件锁句柄：句柄存活期间持锁，drop/进程退出即释放。
/// 与 history.rs 的 FileLock 同语义，独立成份以免跨模块共享私有类型。
struct FileLock(std::fs::File);

impl FileLock {
    /// 打开（不存在则创建）并尝试**非阻塞**独占锁定。
    /// `Ok(Some(_))` = 拿到；`Ok(None)` = 被别的进程持有；`Err` = IO 失败。
    fn try_lock(path: &std::path::Path) -> std::io::Result<Option<FileLock>> {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if r != 0 {
                let e = std::io::Error::last_os_error();
                return if e.kind() == std::io::ErrorKind::WouldBlock {
                    Ok(None)
                } else {
                    Err(e)
                };
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
            };
            let mut ov: windows_sys::Win32::System::IO::OVERLAPPED = unsafe { std::mem::zeroed() };
            let r = unsafe {
                LockFileEx(
                    f.as_raw_handle(),
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    u32::MAX,
                    u32::MAX,
                    &mut ov,
                )
            };
            if r == 0 {
                let e = std::io::Error::last_os_error();
                // ERROR_LOCK_VIOLATION(33) = 已被锁定（等价 flock 的 EWOULDBLOCK）
                return if e.raw_os_error() == Some(33) {
                    Ok(None)
                } else {
                    Err(e)
                };
            }
        }
        Ok(Some(FileLock(f)))
    }

    /// 通过持锁句柄写 PID（同句柄读写不受自身锁影响；仅供人工诊断）。
    fn write_pid(&mut self, pid: u32) -> std::io::Result<()> {
        use std::io::{Seek, Write};
        let f = &mut self.0;
        f.rewind()?;
        f.set_len(0)?;
        f.write_all(format!("{pid}\n").as_bytes())?;
        f.flush()
    }
}

/// 本进程持有的单例锁句柄（按用途各存一格，进程存活期间常驻）。
static HELD: Mutex<[Option<FileLock>; 3]> = Mutex::new([None, None, None]);

fn slot_index(kind: Kind) -> usize {
    match kind {
        Kind::Tray => 0,
        Kind::Settings => 1,
        Kind::Overlay => 2,
    }
}

/// 抢单例锁。返回 true = 本进程是唯一实例（或本进程此前已拿过，幂等）；
/// false = 已有活着的持有者——对可唤起的用途已顺手留下 raise 信号，
/// 调用方应按 [`Kind::conflict_message`] 提示（None 则静默）并退出。
///
/// flock 冲突可能是瞬时假冲突（多线程 fd 高速更替下 close 后立刻重开
/// 同一路径偶尔 EWOULDBLOCK，此刻并无真实持有者；history.rs 实测可稳定
/// 复现）：短重试窗口内假冲突微秒级消散，真持有者会持续到超时。
pub fn acquire(kind: Kind) -> bool {
    let lock = lock_path(kind);
    if let Some(dir) = lock.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if HELD.lock().unwrap()[slot_index(kind)].is_some() {
        return true;
    }
    let deadline = Instant::now() + Duration::from_millis(150);
    loop {
        match FileLock::try_lock(&lock) {
            Ok(Some(mut l)) => {
                // 锁内写 PID 供人工诊断（诊断值，不参与存活判断）
                let _ = l.write_pid(std::process::id());
                // 清掉上次会话残留的唤起信号，免得新窗口刚开就自我 Focus
                let _ = std::fs::remove_file(raise_path(kind));
                HELD.lock().unwrap()[slot_index(kind)] = Some(l);
                return true;
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                if kind.can_raise() {
                    let _ = std::fs::write(raise_path(kind), b"1");
                }
                return false;
            }
            Err(_) => return false, // IO 异常：宁可放过也不锁死用户
        }
    }
}

/// 轮询唤起信号：有则消费掉（删文件）并返回 true。可唤起用途的窗口
/// 在自己的心跳里调用（窗口在后台时无输入事件，eframe 不会主动重绘，
/// 必须配合 `request_repaint_after`，见 history::poll_raise 的复盘）。
pub fn take_raise(kind: Kind) -> bool {
    let p = raise_path(kind);
    if p.exists() {
        let _ = std::fs::remove_file(&p);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两个进程抢同一用途：先拿者胜，后者 false 且留下 raise 信号（可唤起
    /// 用途）。测试内用子进程模拟「别的持有者」不现实（锁是进程级语义），
    /// 这里验证的是「已持锁时重复 acquire 幂等」与「未持锁时能拿到」。
    #[test]
    fn acquire_is_idempotent_per_kind_and_kinds_do_not_conflict() {
        assert!(acquire(Kind::Tray));
        // 同用途重复抢：本进程已持锁，幂等返回 true
        assert!(acquire(Kind::Tray));
        // 不同用途不互相干扰（托盘进程本身可能再开覆盖层，两把锁并存）
        assert!(acquire(Kind::Settings));
        assert!(acquire(Kind::Overlay));
    }

    /// raise 信号消费一次即消失。
    #[test]
    fn raise_signal_consumed_once() {
        std::fs::write(raise_path(Kind::Settings), b"1").unwrap();
        assert!(take_raise(Kind::Settings));
        assert!(!take_raise(Kind::Settings));
    }

    /// 托盘/覆盖层无窗口可唤起：锁冲突时不该写 raise 文件（can_raise=false）；
    /// 覆盖层冲突静默让路（无提示文案），托盘/配置有文案。
    #[test]
    fn only_settings_has_raise_and_overlay_is_silent() {
        assert!(!Kind::Tray.can_raise());
        assert!(Kind::Settings.can_raise());
        assert!(!Kind::Overlay.can_raise());
        assert!(Kind::Tray.conflict_message().is_some());
        assert!(Kind::Settings.conflict_message().is_some());
        assert_eq!(Kind::Overlay.conflict_message(), None);
    }
}
