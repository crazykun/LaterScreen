//! lscreen-capture: 截屏封装。
//!
//! 平台策略（服务于「单文件、无动态库依赖」目标）：
//! - Linux X11：x11rb（纯 Rust XCB 协议实现），多显示器走 RandR
//! - Linux Wayland：计划走 xdg-desktop-portal（ashpd，纯 Rust D-Bus），M5 实现
//! - Windows / macOS：xcap（走系统 API，无 C 编译依赖）
//!
//! 刻意不用 xcap 的 Linux 路径：它强制链接 pipewire（C 动态库）。

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(target_os = "linux"))]
mod other;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(not(target_os = "linux"))]
use other as platform;

use std::fmt;

#[derive(Debug)]
pub struct CaptureError(pub String);

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CaptureError {}

pub type Result<T> = std::result::Result<T, CaptureError>;

/// 一次截屏的结果。坐标体系：
/// - `origin` 是该显示器在虚拟桌面上的原点
/// - `width/height` 是物理像素尺寸
/// - `scale` = 物理像素 / 逻辑坐标（X11 下恒为 1.0）
pub struct Screenshot {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub origin: (i32, i32),
    pub scale: f32,
    pub is_primary: bool,
}

impl Screenshot {
    /// 读取物理像素坐标 (x, y) 处的颜色，越界返回 None。
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        self.rgba.get(i..i + 4).map(|s| [s[0], s[1], s[2], s[3]])
    }
}

/// 截主显示器。找不到主显示器时退回第一台。
pub fn capture_primary() -> Result<Screenshot> {
    platform::capture_primary()
}

/// 截包含指定虚拟桌面坐标的显示器。
pub fn capture_at(x: i32, y: i32) -> Result<Screenshot> {
    platform::capture_at(x, y)
}

/// 截所有显示器。
pub fn capture_all() -> Result<Vec<Screenshot>> {
    platform::capture_all()
}
