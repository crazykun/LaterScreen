//! 配置面板（M8）：`lscreen config` 打开的独立 eframe 窗口。
//!
//! 保存即写 config.toml；托盘进程监视 mtime 自动热加载（无需重启）。
//! 校验失败（模板/热键/颜色/工具名非法）只提示不落盘。
//!
//! 视觉：自定义暗色主题（深灰底 + 卡片分域 + 品牌红强调），与 egui
//! 默认"工具感"样式区分；仅作用于本窗口的 egui Context。

use crate::config::{self, Config};
use crate::export;
use eframe::egui;

// ---------------------------------------------------------------- 设计令牌

/// 品牌强调色（与默认标注色一致的红）
const ACCENT: egui::Color32 = egui::Color32::from_rgb(0xe5, 0x39, 0x35);
/// 窗口底色
const BG: egui::Color32 = egui::Color32::from_rgb(0x14, 0x14, 0x18);
/// 卡片底色
const CARD: egui::Color32 = egui::Color32::from_rgb(0x1d, 0x1d, 0x24);
/// 卡片描边
const CARD_STROKE: egui::Color32 = egui::Color32::from_rgb(0x2b, 0x2b, 0x35);
/// 次级文字（标签/提示）
const MUTED: egui::Color32 = egui::Color32::from_rgb(0x9a, 0x9a, 0xa5);
/// 主文字
const TEXT: egui::Color32 = egui::Color32::from_rgb(0xec, 0xec, 0xf1);
/// 页脚条底色（与卡片区分的更深一层）
const FOOTER: egui::Color32 = egui::Color32::from_rgb(0x11, 0x11, 0x15);

/// 应用自定义暗色视觉：控件圆角、悬浮态、选中色。
fn apply_theme(ctx: &egui::Context) {
    let mut style = egui::Style {
        visuals: egui::Visuals::dark(),
        ..Default::default()
    };
    let v = &mut style.visuals;
    v.panel_fill = BG;
    v.window_corner_radius = egui::CornerRadius::same(12);
    v.menu_corner_radius = egui::CornerRadius::same(8);
    v.selection.bg_fill = ACCENT;
    v.hyperlink_color = ACCENT;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = egui::CornerRadius::same(6);
        w.fg_stroke.color = TEXT;
    }
    // 输入框（noninteractive 承载背景）与卡片同层，弱化"嵌入感"
    v.widgets.noninteractive.weak_bg_fill = egui::Color32::from_rgb(0x16, 0x16, 0x1b);
    v.widgets.noninteractive.bg_stroke.color = CARD_STROKE;
    v.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(0x16, 0x16, 0x1b);
    v.widgets.inactive.bg_stroke.color = CARD_STROKE;
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 30.0;
    ctx.set_style_of(egui::Theme::Dark, std::sync::Arc::new(style));
}

// ---------------------------------------------------------------- 卡片容器

/// 分区卡片：标题左侧一条强调色小竖条 + 圆角深色面板。
fn card<R>(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(CARD)
        .stroke(egui::Stroke::new(1.0, CARD_STROKE))
        .corner_radius(10)
        .inner_margin(egui::Margin::same(14))
        .outer_margin(egui::Margin {
            bottom: 10,
            ..Default::default()
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // 标题行：强调竖条 + 文字
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(3.0, 14.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, egui::CornerRadius::same(2), ACCENT);
                ui.strong(egui::RichText::new(title).color(TEXT).size(14.0));
            });
            ui.add_space(10.0);
            body(ui)
        })
        .inner
}

// ---------------------------------------------------------------- App

pub struct SettingsApp {
    cfg: Config,
    toast: Option<(String, f64)>,
    /// 正在捕获哪个热键字段（0=截图 1=取色 2=贴图）
    recording: Option<usize>,
    /// 捕获提示（裸键被拒等瞬时反馈）
    record_hint: Option<(String, f64)>,
}

impl SettingsApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::apply_window_class(cc);
        apply_theme(&cc.egui_ctx);
        // 独立窗口必须自己挂中文字体；先用 core Renderer 验证可解析
        // （epaint 对坏字体是 panic 而非 Err）
        let font = crate::font::load_system_font();
        if let Some(bytes) = font {
            if lscreen_core::render::Renderer::new(Some(bytes.clone())).has_font() {
                crate::font::setup_egui_fonts(&cc.egui_ctx, bytes);
            }
        }
        Self {
            cfg: Config::load(),
            toast: None,
            recording: None,
            record_hint: None,
        }
    }

    fn toast(&mut self, ctx: &egui::Context, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ctx.input(|i| i.time) + 2.5));
    }

    /// 保存：先全部校验再落盘，非法字段就地提示。
    fn save(&mut self, ctx: &egui::Context) {
        if let Err(e) = config::validate_template(&self.cfg.filename_template) {
            self.toast(ctx, format!("文件名模板无效: {e}"));
            return;
        }
        // 先取副本再校验：避免循环持有 &self.cfg 的同时调 self.toast 可变借用
        let hotkeys = [
            ("截图热键", self.cfg.hotkey_screenshot.clone()),
            ("取色热键", self.cfg.hotkey_picker.clone()),
            ("贴图热键", self.cfg.hotkey_pin.clone()),
            ("录屏热键", self.cfg.hotkey_record.clone()),
            ("滚动截图热键", self.cfg.hotkey_scroll.clone()),
            ("历史热键", self.cfg.hotkey_history.clone()),
        ];
        for (name, raw) in hotkeys {
            if raw.trim().is_empty() {
                continue;
            }
            if let Err(e) = crate::tray::parse_hotkey(&raw) {
                self.toast(ctx, format!("{name}「{raw}」无效: {e}"));
                return;
            }
        }
        if config::parse_hex_color(&self.cfg.default_color).is_none() {
            self.toast(
                ctx,
                format!("默认颜色无效（应为 #RRGGBB）: {}", self.cfg.default_color),
            );
            return;
        }
        if config::tool_from_name(&self.cfg.default_tool).is_none() {
            self.toast(ctx, format!("未知工具名: {}", self.cfg.default_tool));
            return;
        }
        if !(1..=50).contains(&self.cfg.history_max) {
            self.toast(ctx, "历史条数无效（应为 1-50）".to_string());
            return;
        }
        match self.cfg.save() {
            Ok(()) => self.toast(ctx, "已保存（托盘进程自动生效）"),
            Err(e) => self.toast(ctx, format!("保存失败: {e}")),
        }
    }

    /// 热键捕获：recording 期间吃掉全部按键事件。
    /// - 修饰键+主键 → 生成 "Ctrl+Alt+A" 并校验，非法（如裸字母）就地提示
    /// - Esc → 取消录制（保留原值）；Backspace/Delete → 清空绑定
    fn capture_hotkey(&mut self, ctx: &egui::Context) {
        if self.recording.is_none() {
            return;
        }
        let mut captured: Option<(String, egui::Modifiers, egui::Key)> = None;
        let mut cancel = false;
        let mut clear = false;
        ctx.input_mut(|i| {
            let mut keep = Vec::new();
            for ev in std::mem::take(&mut i.events) {
                // 录制期间所有按键事件（含释放）都吃掉，避免落到输入框/关窗
                if let egui::Event::Key {
                    key,
                    modifiers,
                    pressed: true,
                    ..
                } = ev
                {
                    match key {
                        egui::Key::Escape => cancel = true,
                        egui::Key::Backspace | egui::Key::Delete if modifiers.is_none() => {
                            clear = true
                        }
                        k if is_modifier_key(k) => {}
                        _ => captured = Some((hotkey_string(&modifiers, &key), modifiers, key)),
                    }
                } else if !matches!(ev, egui::Event::Key { .. }) {
                    keep.push(ev);
                }
            }
            i.events = keep;
        });
        let idx = self.recording.unwrap_or(0);
        if let Some((text, mods, key)) = captured {
            match crate::tray::parse_hotkey(&text) {
                Ok(_) => {
                    *self.hotkey_field_mut(idx) = text;
                    self.recording = None;
                }
                Err(e) => {
                    let _ = (mods, key);
                    let msg = format!("「{text}」不可用：{e}");
                    self.record_hint = Some((msg, ctx.input(|i| i.time) + 1.5));
                }
            }
        } else if clear {
            *self.hotkey_field_mut(idx) = String::new();
            self.recording = None;
        } else if cancel {
            self.recording = None;
        }
    }

    fn hotkey_field_mut(&mut self, idx: usize) -> &mut String {
        match idx {
            0 => &mut self.cfg.hotkey_screenshot,
            1 => &mut self.cfg.hotkey_picker,
            2 => &mut self.cfg.hotkey_pin,
            3 => &mut self.cfg.hotkey_record,
            4 => &mut self.cfg.hotkey_scroll,
            _ => &mut self.cfg.hotkey_history,
        }
    }

    /// 热键捕获输入框：点击进入录制态，随后直接按键录入。
    fn hotkey_capture(&mut self, ui: &mut egui::Ui, idx: usize, value: &str) {
        let active = self.recording == Some(idx);
        let (label, stroke) = if active {
            (
                egui::RichText::new("按下组合键…  (Esc 取消 / Backspace 清除)")
                    .color(ACCENT)
                    .monospace(),
                egui::Stroke::new(1.5, ACCENT),
            )
        } else if value.is_empty() {
            (
                egui::RichText::new("点击设置热键").color(MUTED).monospace(),
                egui::Stroke::new(1.0, CARD_STROKE),
            )
        } else {
            (
                egui::RichText::new(format!("{value}（点击修改）"))
                    .color(TEXT)
                    .monospace(),
                egui::Stroke::new(1.0, CARD_STROKE),
            )
        };
        let btn = egui::Button::new(label)
            .min_size(egui::vec2(0.0, 32.0))
            .stroke(stroke)
            .corner_radius(6)
            .fill(egui::Color32::from_rgb(0x16, 0x16, 0x1b));
        let resp = ui
            .add_sized([300.0, 32.0], btn)
            .on_hover_text("点击后按下想要的组合键（如 Ctrl+Alt+A）");
        if resp.clicked() {
            self.recording = if active { None } else { Some(idx) };
        }
        if active {
            ui.ctx().request_repaint();
        }
    }
}

/// 物理修饰键按下（Shift/Ctrl/Alt/Super 的左右变体）——不作为主键。
fn is_modifier_key(k: egui::Key) -> bool {
    matches!(
        k,
        egui::Key::ShiftLeft
            | egui::Key::ShiftRight
            | egui::Key::ControlLeft
            | egui::Key::ControlRight
            | egui::Key::AltLeft
            | egui::Key::AltRight
            | egui::Key::SuperLeft
            | egui::Key::SuperRight
    )
}

/// 按下事件 → 热键文案（与 tray::parse_hotkey 的书写约定一致）。
fn hotkey_string(mods: &egui::Modifiers, key: &egui::Key) -> String {
    let mut s = String::new();
    if mods.ctrl {
        s.push_str("Ctrl+");
    }
    if mods.shift {
        s.push_str("Shift+");
    }
    if mods.alt {
        s.push_str("Alt+");
    }
    if mods.mac_cmd {
        s.push_str("Meta+");
    }
    // egui Key::name 与 parse_key 的别名差异
    s.push_str(match key.name() {
        "Equals" => "Equal",
        "Backtick" => "`",
        n => n,
    });
    s
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // 热键捕获必须最先处理：吃掉所有按键事件（含 Esc，防止录制中关窗）
        self.capture_hotkey(&ctx);
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // 底部按钮条（固定高度，勿动：见上一版 Panel 高度自增 bug 的复盘注释）
        egui::Panel::bottom(egui::Id::new("settings-bottom"))
            .exact_size(64.0)
            .frame(
                egui::Frame::NONE
                    .fill(FOOTER)
                    .stroke(egui::Stroke::new(1.0, CARD_STROKE))
                    .inner_margin(egui::Margin::symmetric(16, 0)),
            )
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 主按钮：品牌红底白字
                    let save_btn = egui::Button::new(
                        egui::RichText::new("保存")
                            .color(egui::Color32::WHITE)
                            .strong(),
                    )
                    .fill(ACCENT)
                    .corner_radius(8)
                    .min_size(egui::vec2(96.0, 34.0));
                    if ui.add(save_btn).clicked() {
                        self.save(&ctx);
                    }
                    ui.add_space(8.0);
                    // 次按钮：幽灵描边
                    let close_btn = egui::Button::new(egui::RichText::new("关闭").color(MUTED))
                        .corner_radius(8)
                        .min_size(egui::vec2(80.0, 34.0))
                        .stroke(egui::Stroke::new(1.0, CARD_STROKE));
                    if ui.add(close_btn).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::same(16)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.page(ui);
                });
            });

        if let Some((msg, until)) = &self.toast {
            if ctx.input(|i| i.time) > *until {
                self.toast = None;
            } else {
                let msg = msg.clone();
                egui::Area::new(egui::Id::new("settings-toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::Vec2::new(0.0, -72.0))
                    .show(&ctx, |ui| {
                        egui::Frame::NONE
                            .fill(egui::Color32::from_black_alpha(230))
                            .stroke(egui::Stroke::new(1.0, ACCENT))
                            .corner_radius(8)
                            .inner_margin(egui::Margin::symmetric(14, 8))
                            .show(ui, |ui| ui.colored_label(egui::Color32::WHITE, msg));
                    });
                ctx.request_repaint();
            }
        }
    }
}

// ---------------------------------------------------------------- 页面

impl SettingsApp {
    fn page(&mut self, ui: &mut egui::Ui) {
        // 页头
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("LaterScreen")
                    .color(TEXT)
                    .size(19.0)
                    .strong(),
            );
            ui.add_space(8.0);
            egui::Frame::NONE
                .fill(egui::Color32::from_rgb(0x26, 0x26, 0x30))
                .corner_radius(egui::CornerRadius::same(255))
                .inner_margin(egui::Margin::symmetric(8, 2))
                .show(ui, |ui| {
                    let ver = format!("v{}", env!("CARGO_PKG_VERSION"));
                    ui.label(egui::RichText::new(ver).color(MUTED).size(11.0));
                });
        });
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("截图 · 标注 · 取色 · 录屏 · 贴图")
                .color(MUTED)
                .size(12.0),
        );
        ui.add_space(14.0);

        card(ui, "保存", |ui| {
            egui::Grid::new("save-grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    row_label(ui, "目录");
                    input_field(
                        ui,
                        egui::TextEdit::singleline(&mut self.cfg.save_dir)
                            .hint_text("留空 = ~/Pictures")
                            .desired_width(f32::INFINITY),
                    )
                    .on_hover_text("保存截图/录屏的目录；留空用 ~/Pictures（不存在则家目录）");
                    ui.end_row();

                    row_label(ui, "文件名模板");
                    input_field(
                        ui,
                        egui::TextEdit::singleline(&mut self.cfg.filename_template)
                            .desired_width(f32::INFINITY),
                    )
                    .on_hover_text(
                        "占位符：{YYYYMMDD} {HHMMSS} {YYYY} {MM} {DD} {HH} {MI} {SS}；未知 token 原样保留",
                    );
                    ui.end_row();

                    row_label(ui, "录制格式");
                    let current = config::RECORD_FORMAT_NAMES
                        .iter()
                        .find(|(id, _)| *id == self.cfg.record_format)
                        .map(|(_, label)| *label)
                        .unwrap_or("GIF 动图");
                    egui::ComboBox::from_id_salt("record-format")
                        .selected_text(egui::RichText::new(current).color(TEXT))
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for (id, label) in config::RECORD_FORMAT_NAMES {
                                ui.selectable_value(
                                    &mut self.cfg.record_format,
                                    id.to_string(),
                                    *label,
                                );
                            }
                        })
                        .response
                        .on_hover_text("录屏（GIF/MP4）的默认格式；命令行 --mp4 显式指定时优先于此");
                    ui.end_row();

                    row_label(ui, "历史条数");
                    // egui 0.35 的 DragValue 无 on_hover_text：经 ui.add 的 Response 挂
                    ui.add(
                        egui::DragValue::new(&mut self.cfg.history_max)
                            .range(1..=50)
                            .speed(0.1),
                    )
                    .on_hover_text("托盘「历史」窗口保留的最近截图/贴图/录屏条数（1-50）");
                    ui.end_row();
                });
            ui.small(
                egui::RichText::new(format!(
                    "示例  {}.png",
                    config::render_template(&self.cfg.filename_template, (2026, 8, 19, 10, 15, 20))
                ))
                .color(MUTED),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.checkbox(
                    &mut self.cfg.open_dir_after_save,
                    egui::RichText::new("保存后自动打开所在目录").color(TEXT),
                );
                ui.add_space(8.0);
                // 必须用 link：Label（ui.small）默认 Sense::hover，clicked() 永远不触发
                if ui
                    .link(egui::RichText::new("打开目录").color(ACCENT))
                    .on_hover_text("调用系统文件管理器打开当前保存目录")
                    .clicked()
                {
                    // 面板内存中的目录（未保存的编辑也生效）；未设置或不存在
                    // 则退回默认目录（~/Pictures 或家目录）
                    let dir = self
                        .cfg
                        .save_dir_override()
                        .filter(|p| p.is_dir())
                        .unwrap_or_else(export::default_dir);
                    export::open_in_file_manager(&dir);
                }
            });
        });

        card(ui, "默认绘制", |ui| {
            egui::Grid::new("draw-grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    row_label(ui, "工具");
                    let current = config::TOOL_NAMES
                        .iter()
                        .find(|(id, _)| *id == self.cfg.default_tool)
                        .map(|(_, label)| *label)
                        .unwrap_or("选择");
                    egui::ComboBox::from_id_salt("tool")
                        .selected_text(egui::RichText::new(current).color(TEXT))
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for (id, label) in config::TOOL_NAMES {
                                ui.selectable_value(
                                    &mut self.cfg.default_tool,
                                    id.to_string(),
                                    *label,
                                );
                            }
                        });
                    ui.end_row();

                    row_label(ui, "颜色");
                    ui.horizontal(|ui| {
                        let mut c32 = config::parse_hex_color(&self.cfg.default_color)
                            .map(crate::ui::egui_color)
                            .unwrap_or(egui::Color32::RED);
                        // 紧凑色块按钮 + 弹层取色器（内联展开式会占大块空间）
                        if egui::color_picker::color_edit_button_srgba(
                            ui,
                            &mut c32,
                            egui::color_picker::Alpha::Opaque,
                        )
                        .changed()
                        {
                            self.cfg.default_color = config::hex_color(egui_to_rgba(c32));
                        }
                        ui.label(
                            egui::RichText::new(self.cfg.default_color.clone())
                                .color(MUTED)
                                .monospace(),
                        );
                    });
                    ui.end_row();

                    row_label(ui, "线宽");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Slider::new(&mut self.cfg.default_width, 1.0..=12.0)
                                .show_value(false),
                        );
                        ui.label(
                            egui::RichText::new(format!("{:.0} px", self.cfg.default_width))
                                .color(MUTED)
                                .monospace(),
                        );
                    });
                    ui.end_row();
                });
        });

        card(ui, "行为", |ui| {
            egui::Grid::new("behavior-grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    row_label(ui, "初始选区");
                    let current = config::SELECTION_NAMES
                        .iter()
                        .find(|(id, _)| *id == self.cfg.default_selection)
                        .map(|(_, label)| *label)
                        .unwrap_or("最前窗口");
                    egui::ComboBox::from_id_salt("initial-selection")
                        .selected_text(egui::RichText::new(current).color(TEXT))
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for (id, label) in config::SELECTION_NAMES {
                                ui.selectable_value(
                                    &mut self.cfg.default_selection,
                                    id.to_string(),
                                    *label,
                                );
                            }
                        })
                        .response
                        .on_hover_text("进入截图时预选的区域：最前窗口可直接 Enter/双击出图");
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.checkbox(
                &mut self.cfg.copy_auto_exit,
                egui::RichText::new("复制到剪贴板后自动退出").color(TEXT),
            );
            ui.add_space(4.0);
            ui.checkbox(
                &mut self.cfg.history_close_after_copy,
                egui::RichText::new("历史面板点击复制后自动关闭").color(TEXT),
            );
        });

        card(ui, "全局热键", |ui| {
            egui::Grid::new("hotkeys-grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    row_label(ui, "截图");
                    self.hotkey_capture(ui, 0, &self.cfg.hotkey_screenshot.clone());
                    ui.end_row();
                    row_label(ui, "取色");
                    self.hotkey_capture(ui, 1, &self.cfg.hotkey_picker.clone());
                    ui.end_row();
                    row_label(ui, "贴图");
                    self.hotkey_capture(ui, 2, &self.cfg.hotkey_pin.clone());
                    ui.end_row();
                    row_label(ui, "录屏");
                    self.hotkey_capture(ui, 3, &self.cfg.hotkey_record.clone());
                    ui.end_row();
                    row_label(ui, "滚动截图");
                    self.hotkey_capture(ui, 4, &self.cfg.hotkey_scroll.clone());
                    ui.end_row();
                    row_label(ui, "历史");
                    self.hotkey_capture(ui, 5, &self.cfg.hotkey_history.clone());
                    ui.end_row();
                });
            ui.add_space(2.0);
            let mut hint = "点击右侧输入框后直接按下组合键即可录入；Backspace 清除，Esc 取消；留空 = 不注册。托盘运行中保存即生效。".to_string();
            if let Some((msg, until)) = &self.record_hint {
                if ui.input(|i| i.time) < *until {
                    hint = msg.clone();
                    ui.ctx().request_repaint();
                } else {
                    self.record_hint = None;
                }
            }
            ui.small(egui::RichText::new(hint).color(MUTED));
        });

        if let Some(path) = config::config_path() {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(format!("配置文件  {}", path.display()))
                    .color(MUTED)
                    .size(11.0),
            );
        }
        ui.add_space(6.0);
    }
}

/// Grid 左列的弱色标签。
fn row_label(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).color(MUTED));
}

/// 通用加高输入框（min_size 保证 32px 高）。
fn input_field(ui: &mut egui::Ui, builder: egui::TextEdit<'_>) -> egui::Response {
    ui.add(builder.min_size(egui::vec2(0.0, 32.0)))
}

fn egui_to_rgba(c: egui::Color32) -> lscreen_core::Rgba {
    lscreen_core::Rgba([c.r(), c.g(), c.b(), 0xff])
}
