//! 截图历史（M11）：`~/.config/lscreen/history/` 下存全尺寸 PNG 副本 +
//! `index.toml` 索引。托盘「历史」菜单项打开一个无边框自绘面板窗（Snipaste
//! 同款思路：原生菜单画不了缩略图，用自绘窗口展示缩略图网格）。
//!
//! 设计取舍：
//! - 存副本而非只记路径：历史要「再贴图 / 复制」必须能读到原图，而源文件
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
use egui::{Pos2, Rect, Vec2};
use serde::{Deserialize, Serialize};

use crate::export;

/// 历史条目来源类型，决定面板里单击的默认动作：
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
    items.sort_by_key(|a| std::cmp::Reverse(a.timestamp));
    items
}

/// 某条记录对应的副本文件路径（history 目录内）。
pub fn file_path(item: &Item) -> PathBuf {
    history_dir().join(&item.filename)
}

/// 历史索引 `index.toml` 的 mtime：面板轮询据此检测新截图、刷新列表。
pub fn index_mtime() -> Option<std::time::SystemTime> {
    index_path().metadata().ok().and_then(|m| m.modified().ok())
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
    index.items.sort_by_key(|a| std::cmp::Reverse(a.timestamp));
    let removed = index.items.split_off(max);
    let dir = history_dir();
    for item in removed {
        let _ = std::fs::remove_file(dir.join(&item.filename));
    }
}

// ---------------------------------------------------------------- 历史面板窗

/// 缩略图边长上限（长边缩到 256，等比缩放）。
const THUMB: u32 = 256;

/// 历史面板（Snipaste 式自绘弹窗）：无边框浮窗 + 缩略图网格，悬停 Tooltip +
/// 右键菜单（贴图/打开目录/删除），单击按类型复制或定位。
pub struct HistoryApp {
    items: Vec<Item>,
    /// 文件名 → 纹理缓存（缩略图，首次显示时加载）
    thumbs: HashMap<String, egui::TextureHandle>,
    /// 已关闭（点右上角 X）：下一帧发 Close
    close: bool,
    /// 上次读到索引 mtime（每小时/每次变更才重载）
    last_mtime: Option<std::time::SystemTime>,
}

impl HistoryApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::apply_window_class(cc);
        // 独立进程必须自己挂中文字体，否则按钮/toast 中文显示为方框。
        // 与 pin.rs 同一套校验再安装。
        if let Some(bytes) = crate::font::load_system_font() {
            if lscreen_core::render::Renderer::new(Some(bytes.clone())).has_font() {
                crate::font::setup_egui_fonts(&cc.egui_ctx, bytes);
            }
        }
        Self {
            items: list(),
            thumbs: HashMap::new(),
            close: false,
            last_mtime: index_mtime(),
        }
    }

    /// 索引 mtime 变化才重载列表（新截图/贴图落盘后面板自动出现新项）。
    fn refresh_if_changed(&mut self) {
        let m = index_mtime();
        if m != self.last_mtime {
            self.last_mtime = m;
            self.items = list();
            self.thumbs.clear();
        }
    }

    /// 缩略图纹理（懒加载 + 缓存）。
    fn thumb(&mut self, ctx: &egui::Context, item: &Item) -> egui::TextureHandle {
        if let Some(t) = self.thumbs.get(&item.filename) {
            return t.clone();
        }
        let path = file_path(item);
        let color = match image::open(&path).ok() {
            Some(img) => {
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

    fn copy_item(&self, item: &Item) {
        if let Ok(img) = image::open(file_path(item)) {
            let img = img.into_rgba8();
            let (w, h) = (img.width(), img.height());
            let _ = export::copy_to_clipboard(img.as_raw(), w, h);
        }
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

    fn pin_item(&self, item: &Item) {
        // spawn 独立贴图进程（与托盘同款：用完即退，脱离本窗口生命周期）
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let _ = std::process::Command::new(exe)
            .args(["pin", "-i"])
            .arg(file_path(item))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

impl eframe::App for HistoryApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.refresh_if_changed();
        let ctx = ui.ctx().clone();

        // 顶栏：标题 + 计数（左）· 关闭（右，Painter 手绘 X——✕ 字符在 egui 内置
        // 字体里无字形会渲染成方框，和 CJK 缺失同源）。整条可拖动（无系统标题栏）。
        egui::Panel::top(egui::Id::new("history-top"))
            .exact_size(30.0)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 0)))
            .show(ui, |ui| {
                let avail = ui.available_width();
                let btn = Rect::from_center_size(
                    Pos2::new(ui.max_rect().right() - 12.0, ui.max_rect().center().y),
                    Vec2::splat(24.0),
                );
                let btn_resp =
                    ui.interact(btn, egui::Id::new("history-close"), egui::Sense::click());
                let hovered = btn_resp.hovered();
                let p = ui.painter();
                p.rect_filled(
                    btn,
                    6.0,
                    if hovered {
                        egui::Color32::from_rgb(0x3a, 0x3a, 0x44)
                    } else {
                        egui::Color32::TRANSPARENT
                    },
                );
                // 手绘 X：两条对角线
                let c = if hovered {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_rgb(0x9e, 0x9e, 0xaa)
                };
                let (x0, x1) = (
                    btn.center() - Vec2::splat(4.0),
                    btn.center() + Vec2::splat(4.0),
                );
                p.line_segment([x0, x1], egui::Stroke::new(1.6, c));
                p.line_segment(
                    [Pos2::new(x0.x, x1.y), Pos2::new(x1.x, x0.y)],
                    egui::Stroke::new(1.6, c),
                );
                if btn_resp.clicked() {
                    self.close = true;
                }
                // 标题 + 计数（左对齐，避开右侧关闭按钮）
                p.text(
                    Pos2::new(ui.min_rect().left(), ui.max_rect().center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("历史 · {} 张", self.items.len()),
                    egui::FontId::proportional(14.0),
                    egui::Color32::from_rgb(0xec, 0xec, 0xf0),
                );
                // 其余区域拖动 = 移窗。只在 drag_started 帧发一次 StartDrag，
                // 否则 WM 交互式移动反复被重启，窗口停不下来（见 record_ui 同款坑）
                let drag_area = Rect::from_min_size(
                    ui.min_rect().left_top(),
                    Vec2::new((avail - 28.0).max(0.0), 30.0),
                );
                let drag_resp = ui.interact(
                    drag_area,
                    egui::Id::new("history-drag"),
                    egui::Sense::drag(),
                );
                if drag_resp
                    .clone()
                    .drag_started_by(egui::PointerButton::Primary)
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                drag_resp.on_hover_cursor(egui::CursorIcon::Grab);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 6)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // take 出 items：循环体要可变借用 self（thumb 缓存）与读列表，
                    // 二者都用 self 会借用冲突，先取出再放回
                    let mut items = std::mem::take(&mut self.items);
                    if items.is_empty() {
                        ui.centered_and_justified(|ui| {
                            ui.label("暂无历史：保存截图或贴图后会出现在这里");
                        });
                    }
                    let mut remove: Option<String> = None;
                    for item in &items {
                        let thumb = self.thumb(&ctx, item);
                        let size = thumb.size_vec2();
                        let mut click_resp = None;
                        ui.horizontal(|ui| {
                            let resp = ui.add(egui::Image::from_texture(
                                egui::load::SizedTexture::new(&thumb, size),
                            ));
                            click_resp = Some(resp);
                            ui.vertical(|ui| {
                                ui.label(fmt_time(item.timestamp));
                                ui.label(
                                    egui::RichText::new(if item.kind == Kind::Record {
                                        "录屏"
                                    } else if item.kind == Kind::Pin {
                                        "贴图"
                                    } else {
                                        "截图"
                                    })
                                    .size(11.0)
                                    .weak(),
                                );
                            });
                        });
                        if let Some(resp) = click_resp {
                            // 单击：按类型复制 / 定位
                            if resp.clicked() {
                                match item.kind {
                                    Kind::Record => self.open_item(item),
                                    Kind::Shot | Kind::Pin => self.copy_item(item),
                                }
                            }
                            // 右键菜单：贴图 / 打开目录 / 删除
                            resp.context_menu(|ui| {
                                if ui.button("贴图").clicked() {
                                    self.pin_item(item);
                                    ui.close();
                                }
                                if ui.button("打开目录").clicked() {
                                    self.open_item(item);
                                    ui.close();
                                }
                                if ui.button("删除").clicked() {
                                    remove = Some(item.filename.clone());
                                    ui.close();
                                }
                            });
                        }
                    }
                    if let Some(name) = remove {
                        if let Some(idx) = items.iter().position(|i| i.filename == name) {
                            let item = items.remove(idx);
                            let _ = std::fs::remove_file(file_path(&item));
                            self.thumbs.remove(&name);
                            // 同步索引
                            let mut index = load_index();
                            index.items.retain(|i| i.filename != name);
                            save_index(&index);
                        }
                    }
                    self.items = items;
                });
            });

        // Esc 或点「✕」关闭
        if self.close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
