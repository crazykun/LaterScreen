//! 截图历史（M11）：`~/.config/lscreen/history/` 下存全尺寸 PNG 副本 +
//! `index.toml` 索引，托盘「历史」子菜单据此回看最近截图/贴图/录屏。
//!
//! 设计取舍：
//! - 存副本而非只记路径：历史要「再贴图 / 复制」必须能读到原图，而源文件
//!   可能被用户移动或删除；自包含副本保证历史永远可点开。
//! - 副本统一 PNG：录屏（GIF/MP4）本身无法进剪贴板/贴图，录制时另存一张首帧
//!   PNG（poster）作为副本，原文件路径存 `source` 供「打开目录并选中」定位。
//! - 索引用 TOML（项目已有 toml 依赖），避免为一份小索引引入 serde_json。
//! - 上限 `history_max`（默认 10，钳 1-50），追加后裁最旧；副本文件名按
//!   unix 毫秒时间戳，天然排序且无需计数器。

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::export;

/// 历史条目来源类型，决定托盘子菜单里单击的默认动作：
/// 截图/贴图 = 复制，录屏 = 打开目录并选中。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Shot,
    Pin,
    Record,
}

/// 一条历史记录。
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Item {
    /// 全尺寸 PNG 副本文件名（位于 history 目录内，不含路径）。
    pub filename: String,
    /// unix 秒时间戳，用于排序与显示。
    pub timestamp: u64,
    pub kind: Kind,
    /// 副本图像宽高（像素）。
    pub width: u32,
    pub height: u32,
    /// 源文件路径（录屏=实际 GIF/MP4 文件；截图/贴图=原保存路径）。
    /// 空 = 无独立源文件，「打开目录」指向副本自身。
    pub source: String,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    items: Vec<Item>,
}

fn history_dir() -> PathBuf {
    crate::config::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("history")
}

fn index_path() -> PathBuf {
    history_dir().join("index.toml")
}

fn now() -> (u64, u64) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_millis() as u64, d.as_secs())
}

fn load_index() -> Index {
    let path = index_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_default(),
        Err(_) => Index::default(),
    }
}

fn save_index(index: &Index) {
    let path = index_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = toml::to_string_pretty(index) {
        let _ = std::fs::write(&path, text);
    }
}

/// 读回全部历史条目，按时间倒序（最新在前）。
pub fn list() -> Vec<Item> {
    let mut items = load_index().items;
    items.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    items
}

/// 某条记录对应的副本文件路径（history 目录内）。
pub fn file_path(item: &Item) -> PathBuf {
    history_dir().join(&item.filename)
}

/// 记录一张已编码为 RGBA 的图片到历史。`source` 为可选源文件路径
/// （录屏传实际视频文件，其余传 None 即用副本自身）。
pub fn record_rgba(rgba: &[u8], w: u32, h: u32, kind: Kind, source: Option<&std::path::Path>) {
    let Some(img) = image::RgbaImage::from_raw(w, h, rgba.to_vec()) else {
        return;
    };
    let mut png = Vec::new();
    if img
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .is_err()
    {
        return;
    }
    record_png(&png, kind, source, w, h);
}

/// 记录一个已落盘的图片文件（PNG）到历史：解码 → 复制为 PNG 副本。
/// `source` 为源文件路径（录屏传实际 GIF/MP4 文件）；MP4 无法解码，
/// 调用方应传录制时另存的首帧 poster PNG。
pub fn record_file(path: &std::path::Path, kind: Kind, source: Option<&std::path::Path>) {
    let Ok(img) = image::open(path) else {
        return;
    };
    let img = img.into_rgba8();
    let (w, h) = (img.width(), img.height());
    let mut png = Vec::new();
    if img
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .is_err()
    {
        return;
    }
    record_png(&png, kind, source, w, h);
}

/// 追加一条历史记录并裁到 `history_max` 上限。png 为副本字节。
fn record_png(png: &[u8], kind: Kind, source: Option<&std::path::Path>, w: u32, h: u32) {
    let dir = history_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let (millis, secs) = now();
    // 同毫秒撞名时追加序号，绝不清掉旧副本（同秒两次保存的既有语义）
    let mut filename = format!("{millis}.png");
    let mut n = 1u32;
    while dir.join(&filename).exists() {
        filename = format!("{millis}_{n}.png");
        n += 1;
    }
    if std::fs::write(dir.join(&filename), png).is_err() {
        return;
    }

    let mut index = load_index();
    index.items.push(Item {
        filename,
        timestamp: secs,
        kind,
        width: w,
        height: h,
        source: source.map(|p| p.display().to_string()).unwrap_or_default(),
    });
    trim(&mut index);
    save_index(&index);
}

/// 裁到 history_max：按时间戳倒序保留前 N，删除被裁条目的副本文件。
fn trim(index: &mut Index) {
    let max = crate::config::Config::load().history_max();
    if index.items.len() <= max {
        return;
    }
    index.items.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    let removed = index.items.split_off(max);
    let dir = history_dir();
    for item in removed {
        let _ = std::fs::remove_file(dir.join(&item.filename));
    }
}

// ---------------------------------------------------------------- 托盘子菜单

impl Item {
    /// 托盘子菜单里该条目的文案。录屏用「[录]」前缀与图片区分，
    /// 单击录屏是「打开目录并选中」，图片是「复制」。
    pub fn label(&self) -> String {
        match self.kind {
            Kind::Record => format!("[录] {}", fmt_time(self.timestamp)),
            Kind::Shot | Kind::Pin => format!(
                "{} · {}×{}",
                fmt_time(self.timestamp),
                self.width,
                self.height
            ),
        }
    }
}

/// 把某条历史复制进剪贴板（读副本 PNG）。X11 走 clipd 守护。
pub fn copy_item(item: &Item) {
    if let Ok(img) = image::open(file_path(item)) {
        let img = img.into_rgba8();
        let (w, h) = (img.width(), img.height());
        let _ = export::copy_to_clipboard(img.as_raw(), w, h);
    }
}

/// 在文件管理器中定位并选中某条历史：源文件优先（录屏=实际 GIF/MP4），
/// 无源或已不存在则回退副本自身。
pub fn open_item(item: &Item) {
    if !item.source.is_empty() {
        let p = PathBuf::from(&item.source);
        if p.exists() {
            export::open_and_select(&p);
            return;
        }
    }
    export::open_and_select(&file_path(item));
}

/// unix 秒 → 本地 "MM-DD HH:MM"。
fn fmt_time(secs: u64) -> String {
    #[cfg(unix)]
    {
        let t = secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return format!(
                "{:02}-{:02} {:02}:{:02}",
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min
            );
        }
    }
    // 非 unix / 转换失败：退化为秒数
    format!("{secs}s")
}
