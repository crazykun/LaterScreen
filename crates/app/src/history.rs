//! 截图历史（M11）：`~/.config/lscreen/history/` 下存全尺寸 PNG 副本 +
//! `index.toml` 索引，托盘「历史」窗口据此回看最近截图/贴图/录屏。
//!
//! 设计取舍：
//! - 存副本而非只记路径：历史窗口要「再贴图 / 复制」必须能读到原图，而源文件
//!   可能被用户移动或删除；自包含副本保证历史永远可点开。
//! - 副本统一 PNG：录屏（GIF/MP4）本身无法进剪贴板/贴图，录制时另存一张首帧
//!   PNG（poster）作为副本，原文件路径存 `source` 供「打开目录并选中」定位。
//! - 索引用 TOML（项目已有 toml 依赖），避免为一份小索引引入 serde_json。
//! - 上限 `history_max`（默认 10，钳 1-50），追加后裁最旧；副本文件名按
//!   unix 毫秒时间戳，天然排序且无需计数器。

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::export;

/// 历史条目来源类型，决定窗口里单击的默认动作：
/// 截图/贴图 = 复制，录屏 = 打开目录并选中。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Shot,
    Pin,
    Record,
}

/// 一条历史记录。
#[derive(Clone, Debug, Serialize, Deserialize)]
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

/// 删除一条记录（索引 + 副本文件），不动源文件。
pub fn remove(item: &Item) {
    let mut index = load_index();
    index.items.retain(|i| i.filename != item.filename);
    save_index(&index);
    let _ = std::fs::remove_file(history_dir().join(&item.filename));
}

/// 清空历史：删除索引与全部副本文件。
pub fn clear() {
    let dir = history_dir();
    let _ = std::fs::remove_dir_all(&dir);
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

// ---------------------------------------------------------------- 历史窗口

/// 缩略图边长上限（长边缩到 256，等比缩放）。
const THUMB: u32 = 256;

/// 截图历史窗口（M11）：网格展示最近 N 项，单击按类型分动作，
/// 右键菜单贴图/打开目录/删除。
pub struct HistoryApp {
    items: Vec<Item>,
    /// 文件名 → 纹理缓存（缩略图，首次显示时加载）
    thumbs: HashMap<String, egui::TextureHandle>,
    /// 单帧 toast（复制/删除反馈），下一帧清空
    toast: Option<String>,
    /// 待删除项（帧末统一刷新，避免遍历中改列表）
    pending_remove: Option<String>,
    pending_clear: bool,
}

impl HistoryApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::apply_window_class(cc);
        // 历史窗口是独立进程，必须自己挂中文字体，否则按钮/toast 中文会显示
        // 为方框（egui 内置字体无 CJK）。与 pin.rs 同一套校验再安装。
        if let Some(bytes) = crate::font::load_system_font() {
            if lscreen_core::render::Renderer::new(Some(bytes.clone())).has_font() {
                crate::font::setup_egui_fonts(&cc.egui_ctx, bytes);
            }
        }
        Self {
            items: list(),
            thumbs: HashMap::new(),
            toast: None,
            pending_remove: None,
            pending_clear: false,
        }
    }

    fn refresh(&mut self) {
        self.items = list();
        self.thumbs.clear();
    }

    /// 缩略图纹理（懒加载 + 缓存）。
    fn thumb(&mut self, ctx: &egui::Context, item: &Item) -> egui::TextureHandle {
        if let Some(t) = self.thumbs.get(&item.filename) {
            return t.clone();
        }
        let path = file_path(item);
        let color = match image::open(&path).ok() {
            Some(img) => {
                // DynamicImage 的 GenericImageView 像素即 Rgba<u8>，
                // thumbnail 直接回 ImageBuffer<Rgba<u8>>，无需 to_rgba8
                let img = image::imageops::thumbnail(&img, THUMB, THUMB);
                let (w, h) = (img.width() as usize, img.height() as usize);
                egui::ColorImage::from_rgba_unmultiplied([w, h], img.as_raw())
            }
            None => egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0x44, 0x44, 0x44, 0xff]),
        };
        let tex = ctx.load_texture(&item.filename, color, Default::default());
        self.thumbs.insert(item.filename.clone(), tex.clone());
        tex
    }

    fn copy_item(&mut self, item: &Item) {
        match image::open(file_path(item)) {
            Ok(img) => {
                let img = img.into_rgba8();
                let (w, h) = (img.width(), img.height());
                match export::copy_to_clipboard(img.as_raw(), w, h) {
                    Ok(()) => self.toast = Some("已复制".to_string()),
                    Err(e) => self.toast = Some(format!("复制失败: {e}")),
                }
            }
            Err(e) => self.toast = Some(format!("读取失败: {e}")),
        }
    }

    fn pin_item(&self, item: &Item) {
        // spawn 独立贴图进程（与托盘同款：用完即退，脱离本窗口生命周期）
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(_) => return,
        };
        let _ = std::process::Command::new(exe)
            .args(["pin", "-i"])
            .arg(file_path(item))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    fn open_item(&self, item: &Item) {
        // 源文件优先（录屏指向实际 GIF/MP4）；无源或已不存在则回退副本
        if !item.source.is_empty() {
            let p = PathBuf::from(&item.source);
            if p.exists() {
                export::open_and_select(&p);
                return;
            }
        }
        export::open_and_select(&file_path(item));
    }
}

impl eframe::App for HistoryApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // 顶栏：标题 + 清空/打开历史目录（Panel::top，eframe 0.35 新 API）
        egui::Panel::top(egui::Id::new("history_top"))
            .exact_size(40.0)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 0)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(format!("最近 {} 张", self.items.len()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("清空").clicked() {
                            self.pending_clear = true;
                        }
                        if ui.button("打开历史目录").clicked() {
                            export::open_in_file_manager(&history_dir());
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::same(12)))
            .show(ui, |ui| {
                if self.items.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label("暂无历史：保存截图或贴图后会出现在这里");
                    });
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // 每行 3 个，不足靠左。先物化一行（克隆）：循环体要调
                    // self.thumb（可变借用），与 items 的切片借用冲突
                    let mut idx = 0;
                    while idx < self.items.len() {
                        let row: Vec<Item> = self.items[idx..].iter().take(3).cloned().collect();
                        ui.horizontal(|ui| {
                            for item in row {
                                ui.vertical(|ui| {
                                    let thumb = self.thumb(&ctx, &item);
                                    let size = thumb.size_vec2();
                                    let resp = ui.add(egui::Image::from_texture(
                                        egui::load::SizedTexture::new(&thumb, size),
                                    ));
                                    // 单击按类型分动作
                                    if resp.clicked() {
                                        match item.kind {
                                            Kind::Record => self.open_item(&item),
                                            Kind::Shot | Kind::Pin => self.copy_item(&item),
                                        }
                                    }
                                    // 右键菜单：贴图 / 打开目录 / 删除
                                    resp.context_menu(|ui| {
                                        if ui.button("贴图").clicked() {
                                            self.pin_item(&item);
                                            ui.close();
                                        }
                                        if ui.button("打开目录").clicked() {
                                            self.open_item(&item);
                                            ui.close();
                                        }
                                        if ui.button("删除").clicked() {
                                            self.pending_remove = Some(item.filename.clone());
                                            ui.close();
                                        }
                                    });
                                    ui.label(format!(
                                        "{} · {}×{}",
                                        fmt_time(item.timestamp),
                                        item.width,
                                        item.height
                                    ));
                                });
                            }
                        });
                        idx += 3;
                    }
                });
            });

        // 帧末统一处理删除/清空，避免遍历中改列表
        if let Some(name) = self.pending_remove.take() {
            if let Some(item) = self.items.iter().find(|i| i.filename == name).cloned() {
                remove(&item);
                self.refresh();
                self.toast = Some("已删除".to_string());
            }
        }
        if self.pending_clear {
            self.pending_clear = false;
            clear();
            self.refresh();
            self.toast = Some("已清空历史".to_string());
        }

        // toast：每次动作后展示一帧（置于 bottom area）
        if let Some(t) = self.toast.take() {
            egui::Area::new(egui::Id::new("history_toast"))
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -8.0])
                .show(&ctx, |ui| {
                    ui.label(egui::RichText::new(t).size(13.0));
                });
        }
    }
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
