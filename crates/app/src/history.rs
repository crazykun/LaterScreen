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

/// 历史窗口单例锁：`~/.config/lscreen/history.lock` 存本进程 PID。
/// 打开历史面板前先抢锁；已有活着的进程则直接返回 false（该进程应立即退出，
/// 不再开第二个窗口——快捷键/菜单连按会不断 spawn 新进程）。
/// 锁文件含 PID 而非纯存在性：进程崩溃留下的 stale 锁，下一次启动读到死 PID
/// 会覆盖它，不会把用户锁死。
pub fn acquire_single_instance() -> bool {
    let lock = crate::config::config_dir()
        .map(|d| d.join("history.lock"))
        .unwrap_or_else(|| PathBuf::from("history.lock"));
    // 已有锁：先判断是否是活着的进程
    if let Ok(text) = std::fs::read_to_string(&lock) {
        if let Ok(pid) = text.trim().parse::<u32>() {
            if pid_alive(pid) && pid != std::process::id() {
                return false; // 另一个历史面板在跑，别开新的
            }
        }
    }
    // 拿到锁（覆盖 stale 锁）：写出自己的 PID
    let _ = std::fs::write(&lock, format!("{}\n", std::process::id()));
    true
}

/// 释放单例锁（窗口正常关闭时调用）。只删本进程持有的锁——先比对 PID，
/// 避免误删刚被下一个实例重新写入的锁。
pub fn release_single_instance() {
    let lock = crate::config::config_dir()
        .map(|d| d.join("history.lock"))
        .unwrap_or_else(|| PathBuf::from("history.lock"));
    if let Ok(text) = std::fs::read_to_string(&lock) {
        if text.trim().parse::<u32>() == Ok(std::process::id()) {
            let _ = std::fs::remove_file(&lock);
        }
    }
}

/// 该 PID 是否还活着。Linux/mac 走 `kill(pid, 0)`（信号 0 只探测存活，
/// 不投递信号）；Windows 走 OpenProcess 探测。
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    // 0 = 存活；EPERM = 存活但无权投递信号（同样视为活着）；ESRCH = 不存在
    if r == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_INVALID_PARAMETER};
    use windows_sys::Win32::System::Threading::OpenProcess;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if !h.is_null() {
            let _ = CloseHandle(h);
            return true;
        }
        // 进程不存在报 ERROR_INVALID_PARAMETER；存在但权限不足则是别的错误
        GetLastError() != ERROR_INVALID_PARAMETER
    }
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
/// 列表里缩略图的固定高度（逻辑点）：图片按此高成等比缩放，行高一致，
/// 与右侧时间/类型标签对齐。图片是主角，保持较高的固定高度。
const THUMB_H: f32 = 120.0;
/// 右栏宽度上限（类型/时间/分辨率各一行）。实际取「图片实际宽度」与
/// 此上限的较小值：横版图填满行宽时右栏收到上限，竖版窄图时右栏跟着
/// 收窄——图片永远是行内最大元素，文字只是配角。
const META_W: f32 = 76.0;
/// 行与行之间的垂直间距：红框悬停会外扩 2px，无间距时会压到相邻行。
const ROW_GAP: f32 = 12.0;

/// 历史面板（Snipaste 式自绘弹窗）：无边框浮窗 + 缩略图列表，悬停 Tooltip +
/// 右键菜单（贴图/打开目录/删除），单击按类型复制或定位；鼠标可拖拽滚动。
pub struct HistoryApp {
    items: Vec<Item>,
    /// 文件名 → 纹理缓存（缩略图，首次显示时加载）
    thumbs: HashMap<String, egui::TextureHandle>,
    /// 已关闭（点右上角 X）：下一帧发 Close
    close: bool,
    /// 复制成功后复制面板随配置关闭（history_close_after_copy）
    close_after_copy: bool,
    /// 操作反馈 toast：“已复制 / 复制失败 …”
    toast: Option<(String, f64)>,
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
            close_after_copy: crate::config::Config::load().history_close_after_copy,
            toast: None,
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

    /// 复制副本到剪贴板：成功/失败都有可见反馈（toast），返回是否成功供
    /// `history_close_after_copy` 决定是否关面板。
    fn copy_item(&mut self, ctx: &egui::Context, item: &Item) -> bool {
        let ok = match image::open(file_path(item)) {
            Ok(img) => {
                let img = img.into_rgba8();
                let (w, h) = (img.width(), img.height());
                export::copy_to_clipboard(img.as_raw(), w, h).is_ok()
            }
            Err(_) => false,
        };
        if ok {
            self.toast(ctx, "已复制到剪贴板");
        } else {
            self.toast(ctx, "复制失败：副本文件无法读取或剪贴板不可用");
        }
        ok
    }

    /// 打开源文件所在目录并选中（右键「打开目录」用）。
    /// 尽力而为：打开失败也弹提示，不让用户点了毫无反馈。
    fn open_item(&mut self, ctx: &egui::Context, item: &Item) {
        let target = resolve_source(item).unwrap_or_else(|| file_path(item));
        if target.exists() {
            export::open_and_select(&target);
            self.toast(ctx, format!("已打开所在目录: {}", short_dir(&target)));
        } else {
            // 文件已不存在：打开其父目录（若还在）
            let dir = target.parent().map(PathBuf::from);
            if let Some(dir) = dir.filter(|d| d.is_dir()) {
                export::open_in_file_manager(&dir);
                self.toast(ctx, format!("文件不存在，已打开目录: {}", short_dir(&dir)));
            } else {
                self.toast(ctx, "文件不存在，且父目录也已删除");
            }
        }
    }

    /// 录屏单击：用系统默认程序播放实际视频（GIF/MP4），而不是打开缩略图副本。
    /// 视频路径来自 `source`（录制时记的实际产物）；`source` 为空或文件已删
    /// （M11 之前的旧条目）时退化为打开目录，至少给用户一个去处。
    fn play_item(&mut self, ctx: &egui::Context, item: &Item) {
        if let Some(path) = resolve_source(item) {
            export::open_with_default(&path);
            self.toast(ctx, format!("已在默认播放器打开: {}", short_dir(&path)));
        } else {
            // 视频丢了：退化为打开目录（让用户看到原始序列或清理）
            self.open_item(ctx, item);
        }
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

    fn toast(&mut self, ctx: &egui::Context, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ctx.input(|i| i.time) + 2.5));
    }

    /// 面板底部居中反馈条（复制/定位结果），同 pin.rs 的 toast。
    /// 用 `constrain_to(panel_rect)` 钉在面板矩形内（Area 默认锚点是全屏
    /// content_rect，不约束会跑到屏幕底部）。
    fn show_toast(&mut self, ctx: &egui::Context, panel_rect: Rect) {
        let Some((msg, until)) = self.toast.clone() else {
            return;
        };
        if ctx.input(|i| i.time) > until {
            self.toast = None;
            return;
        }
        egui::Area::new(egui::Id::new("history-toast"))
            .order(egui::Order::Foreground)
            .constrain_to(panel_rect)
            // 底部居中：pivot 定 area 的锚点（默认是左上角），fixed_pos 用面板底中点
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(Pos2::new(panel_rect.center().x, panel_rect.max.y - 8.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(egui::Color32::from_black_alpha(210))
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(msg).color(egui::Color32::WHITE))
                                .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    });
            });
        ctx.request_repaint();
    }
}

impl Drop for HistoryApp {
    fn drop(&mut self) {
        // 正常关闭时释放单例锁，让下一次快捷键能立刻重开；只剩 PID 比对，
        // 误读不到别的实例刚写入的锁。
        release_single_instance();
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
                // 鼠标拖拽滚动：egui 默认 drag 只在触摸屏生效，这里显式开启——
                // 列表只有滚轮滚非常难用。默认源（滚动条+滚轮）或上 DragScroll::Always。
                // auto_shrink(false)：默认 ScrollArea 会收缩到内容宽度，内容行比面板
                // 窄时滚动条就落在面板中间；关掉收缩让它撑满面板，滚动条贴到右缘。
                use egui::containers::scroll_area::ScrollSource;
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .scroll_source(ScrollSource::default() | ScrollSource::DRAG)
                    .on_drag_cursor(egui::CursorIcon::Grabbing)
                    .show(ui, |ui| {
                        // 滚动内容强制占满整行宽度：行内 horizontal 仅按子项宽度布局，
                        // 若不设宽度，内容（从而滚动条）会收缩到最宽一行的宽度。
                        ui.set_width(ui.available_width());
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
                            // 每行一张满宽卡片：左缩略图（固定高）+ 右元数据三行
                            // （类型/时间/分辨率，各自一行，右侧不再挤得满满的）。
                            let thumb = self.thumb(&ctx, item);

                            // 缩略图尺寸：固定高 THUMB_H，宽按图高比缩放；横版图放大到
                            // 占满行内剩余全部宽度，竖版图按高比自然更窄。上限 =
                            // 行宽 - 右栏 - 分隔符，避免把右栏挤出面板。
                            let img_size = thumb.size_vec2();
                            let mut size =
                                Vec2::new(img_size.x * (THUMB_H / img_size.y.max(1.0)), THUMB_H);
                            let max_w = (ui.available_width() - META_W - 8.0).max(48.0);
                            if size.x > max_w {
                                size *= max_w / size.x;
                            }
                            // 右栏宽度随图片实际宽度收敛：不超上限，也不宽过图片
                            // （竖版窄图下文字保持配角，不会占掉半边）。
                            let meta_w = META_W.min(size.x.max(48.0));

                            let mut img_rect = Rect::ZERO;
                            ui.horizontal(|ui| {
                                let img_resp = ui.add(egui::Image::from_texture(
                                    egui::load::SizedTexture::new(&thumb, size),
                                ));
                                img_rect = img_resp.rect;
                                // 右栏 → 类型 / 时间 / 分辨率三行居中
                                ui.allocate_ui_with_layout(
                                    egui::vec2(meta_w, img_rect.height()),
                                    egui::Layout::top_down(egui::Align::LEFT),
                                    |ui| {
                                        ui.vertical_centered(|ui| {
                                            ui.label(
                                                egui::RichText::new(if item.kind == Kind::Record {
                                                    "录屏"
                                                } else if item.kind == Kind::Pin {
                                                    "贴图"
                                                } else {
                                                    "截图"
                                                })
                                                .size(13.0)
                                                .strong()
                                                .color(egui::Color32::from_rgb(0xec, 0xec, 0xf0)),
                                            );
                                            ui.add_space(2.0);
                                            ui.label(
                                                egui::RichText::new(fmt_time(item.timestamp))
                                                    .size(11.0)
                                                    .weak(),
                                            );
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{}×{}",
                                                    item.width, item.height
                                                ))
                                                .size(10.0)
                                                .weak(),
                                            );
                                        });
                                    },
                                );
                            });

                            // 整行卡片矩形（缩略图左缘到面板右缘）——用于悬停底色 + 红框
                            let row_rect = Rect::from_min_size(
                                img_rect.min,
                                Vec2::new(
                                    (ui.max_rect().right() - img_rect.min.x).max(0.0),
                                    img_rect.height(),
                                ),
                            );
                            // 行文字区 hover-only：只负责悬停红框，不抢占滚动区拖拽层，
                            // 在文字/空白处按住拖动即滚动列表（缩略图上的点击除外）。
                            let hover = ui
                                .interact(
                                    row_rect,
                                    egui::Id::new(("history-row", &item.filename)),
                                    egui::Sense::hover(),
                                )
                                .hovered();
                            if hover {
                                ui.painter().rect_filled(
                                    row_rect.expand(2.0),
                                    7.0,
                                    egui::Color32::from_rgba_unmultiplied(0xe5, 0x39, 0x35, 18),
                                );
                                ui.painter().rect_stroke(
                                    row_rect.expand(2.0),
                                    7.0,
                                    egui::Stroke::new(
                                        1.5,
                                        egui::Color32::from_rgb(0xe5, 0x39, 0x35),
                                    ),
                                    egui::StrokeKind::Outside,
                                );
                            }

                            // 点击区 = 缩略图（左键复制/播放，右键弹出菜单）。整行用
                            // Sense::click 会抢占滚动区拖拽层，行上就无法拖拽滚动。
                            let click_resp = ui
                                .interact(
                                    img_rect,
                                    egui::Id::new(("history-item", &item.filename)),
                                    egui::Sense::click(),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand);

                            if click_resp.clicked() {
                                // 单击：录屏 = 播放视频；截图/贴图 = 复制
                                match item.kind {
                                    Kind::Record => self.play_item(&ctx, item),
                                    Kind::Shot | Kind::Pin => {
                                        if self.copy_item(&ctx, item) && self.close_after_copy {
                                            self.close = true;
                                        }
                                    }
                                }
                            }
                            // 右键菜单挂在缩略图：贴图 / 打开目录 / 删除
                            click_resp.context_menu(|ui| {
                                if ui.button("贴图").clicked() {
                                    self.pin_item(item);
                                    ui.close();
                                }
                                if ui.button("打开目录").clicked() {
                                    self.open_item(&ctx, item);
                                    ui.close();
                                }
                                if ui.button("删除").clicked() {
                                    remove = Some(item.filename.clone());
                                    ui.close();
                                }
                            });

                            // 行间隔：给红框悬停外扩留出空间，不压到相邻行。
                            ui.add_space(ROW_GAP);
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

        // toast 钉在窗口底部中央；用顶层 ui.max_rect()（含顶栏），约束只落到本窗
        let panel_rect = ui.max_rect();
        self.show_toast(&ctx, panel_rect);

        // Esc 或点「✕」关闭
        if self.close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// 路径的简短显示：只取父目录名 + 文件名（面板窄，完整路径会被截断）。
/// 取不到父目录时退化为整个路径。
fn short_dir(p: &std::path::Path) -> String {
    if let (Some(dir), Some(name)) = (p.parent(), p.file_name()) {
        if let Some(dir_name) = dir.file_name() {
            return format!("{}/{}", dir_name.to_string_lossy(), name.to_string_lossy());
        }
    }
    p.display().to_string()
}

/// 条目已记录的源文件（截图/贴图=原保存路径，录屏=实际 GIF/MP4），
/// 只在存在且未失效时返回。
fn resolve_source(item: &Item) -> Option<PathBuf> {
    if item.source.is_empty() {
        return None;
    }
    let p = PathBuf::from(&item.source);
    p.exists().then_some(p)
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
