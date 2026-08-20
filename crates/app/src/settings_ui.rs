//! 配置面板（M8）：`lscreen config` 打开的独立 eframe 窗口。
//!
//! 保存即写 config.toml；托盘进程监视 mtime 自动热加载（无需重启）。
//! 校验失败（模板/热键/颜色/工具名非法）只提示不落盘。

use crate::config::{self, Config};
use eframe::egui;

pub struct SettingsApp {
    cfg: Config,
    toast: Option<(String, f64)>,
}

impl SettingsApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
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
        match self.cfg.save() {
            Ok(()) => self.toast(ctx, "已保存（托盘进程自动生效）"),
            Err(e) => self.toast(ctx, format!("保存失败: {e}")),
        }
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // 底部按钮条 + 上方可滚动表单。egui 0.35 移除了 TopBottomPanel，
        // 用统一的 Panel::bottom + CentralPanel 分域：Panel 会收缩父 Ui 的
        // 可用区域，CentralPanel 拿剩余空间——不再需要手动 allocate_rect 推进
        // cursor（那会把 ScrollArea 挤到只剩底部一条，导致表单黑屏）。
        egui::Panel::bottom(egui::Id::new("settings-bottom")).show(ui, |ui| {
            ui.add_space(4.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("关闭 (Esc)").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("保存").clicked() {
                    self.save(&ctx);
                }
            });
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(6.0);
                self.form(ui);
            });
        });

        if let Some((msg, until)) = &self.toast {
            if ctx.input(|i| i.time) > *until {
                self.toast = None;
            } else {
                let msg = msg.clone();
                egui::Area::new(egui::Id::new("settings-toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::Vec2::new(0.0, -16.0))
                    .show(&ctx, |ui| {
                        egui::Frame::popup(ui.style())
                            .fill(egui::Color32::from_black_alpha(200))
                            .show(ui, |ui| ui.colored_label(egui::Color32::WHITE, msg));
                    });
                ctx.request_repaint();
            }
        }
    }
}

impl SettingsApp {
    fn form(&mut self, ui: &mut egui::Ui) {
        ui.set_width(ui.max_rect().width() - 16.0);
        ui.heading("LaterScreen 配置");
        ui.add_space(10.0);

        ui.label(egui::RichText::new("保存").strong());
        ui.horizontal(|ui| {
            ui.label("目录");
            ui.text_edit_singleline(&mut self.cfg.save_dir)
                .on_hover_text("留空 = ~/Pictures（不存在则家目录）");
        });
        ui.horizontal(|ui| {
            ui.label("文件名模板");
            ui.text_edit_singleline(&mut self.cfg.filename_template)
                .on_hover_text(
                "占位符：{YYYYMMDD} {HHMMSS} {YYYY} {MM} {DD} {HH} {MI} {SS}；未知 token 原样保留",
            );
        });
        ui.small(format!(
            "示例：{}.png",
            config::render_template(&self.cfg.filename_template, (2026, 8, 19, 10, 15, 20))
        ));
        ui.checkbox(&mut self.cfg.open_dir_after_save, "保存后打开所在目录");
        ui.add_space(10.0);

        ui.label(egui::RichText::new("默认绘制").strong());
        ui.horizontal(|ui| {
            ui.label("工具");
            let current = config::TOOL_NAMES
                .iter()
                .find(|(id, _)| *id == self.cfg.default_tool)
                .map(|(_, label)| *label)
                .unwrap_or("选择");
            egui::ComboBox::from_label("")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (id, label) in config::TOOL_NAMES {
                        ui.selectable_value(&mut self.cfg.default_tool, id.to_string(), *label);
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label("颜色");
            let mut c32 = config::parse_hex_color(&self.cfg.default_color)
                .map(crate::ui::egui_color)
                .unwrap_or(egui::Color32::RED);
            if egui::color_picker::color_picker_color32(
                ui,
                &mut c32,
                egui::color_picker::Alpha::Opaque,
            ) {
                self.cfg.default_color = config::hex_color(egui_to_rgba(c32));
            }
        });
        ui.horizontal(|ui| {
            ui.label("线宽");
            ui.add(egui::Slider::new(&mut self.cfg.default_width, 1.0..=12.0));
        });
        ui.add_space(10.0);

        ui.label(egui::RichText::new("行为").strong());
        ui.checkbox(&mut self.cfg.copy_auto_exit, "复制到剪贴板后自动退出");
        ui.add_space(10.0);

        ui.label(egui::RichText::new("全局热键（托盘模式）").strong());
        ui.horizontal(|ui| {
            ui.label("截图");
            ui.text_edit_singleline(&mut self.cfg.hotkey_screenshot)
                .on_hover_text("如 Ctrl+Alt+A；裸键仅允许 PrintScreen/F1-F12；留空 = 不注册");
        });
        ui.horizontal(|ui| {
            ui.label("取色");
            ui.text_edit_singleline(&mut self.cfg.hotkey_picker);
        });
        ui.horizontal(|ui| {
            ui.label("贴图");
            ui.text_edit_singleline(&mut self.cfg.hotkey_pin);
        });
        ui.small(
            "写法：修饰键 Ctrl/Shift/Alt/Super/Meta(=Cmd) + 主键（字母/数字/F1-F12/PrintScreen 等）；托盘运行中保存即生效",
        );
        ui.add_space(10.0);

        let path = config::config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        ui.label(
            egui::RichText::new(format!("配置文件: {path}"))
                .small()
                .weak(),
        );
        ui.add_space(6.0);
    }
}

fn egui_to_rgba(c: egui::Color32) -> lscreen_core::Rgba {
    lscreen_core::Rgba([c.r(), c.g(), c.b(), 0xff])
}
