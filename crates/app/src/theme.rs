//! 双主题（M12）：按 [`ThemeMode`] 为 egui 同时安装暗/浅两套样式，
//! 再用 `set_theme` 切换（System 模式由 egui 跟随操作系统配色）。
//!
//! 令牌原则：所有窗口（设置/历史）一律从 `ui.visuals()` 取色——
//! `text_color()`（主文字）、`weak_text_color()`（次级文字）、
//! `widgets.inactive.weak_bg_fill`（输入域底色）等，**禁止**在业务代码里
//! 硬编码 RGB，否则切浅色时就是"白字白底"。

use crate::config::ThemeMode;
use eframe::egui;

/// 品牌强调色（与默认标注色一致的红）
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(0xe5, 0x39, 0x35);

// ---------------------------------------------------------------- 暗色令牌

const DARK_BG: egui::Color32 = egui::Color32::from_rgb(0x14, 0x14, 0x18);
const DARK_STROKE: egui::Color32 = egui::Color32::from_rgb(0x2b, 0x2b, 0x35);
const DARK_TEXT: egui::Color32 = egui::Color32::from_rgb(0xec, 0xec, 0xf1);
const DARK_MUTED: egui::Color32 = egui::Color32::from_rgb(0x9a, 0x9a, 0xa5);
const DARK_FIELD: egui::Color32 = egui::Color32::from_rgb(0x16, 0x16, 0x1b);
/// 页脚条（比面板更深一层）
const DARK_FOOTER: egui::Color32 = egui::Color32::from_rgb(0x11, 0x11, 0x15);

// ---------------------------------------------------------------- 浅色令牌

const LIGHT_BG: egui::Color32 = egui::Color32::from_rgb(0xf4, 0xf4, 0xf7);
const LIGHT_STROKE: egui::Color32 = egui::Color32::from_rgb(0xdc, 0xdc, 0xe3);
const LIGHT_TEXT: egui::Color32 = egui::Color32::from_rgb(0x24, 0x24, 0x2b);
const LIGHT_MUTED: egui::Color32 = egui::Color32::from_rgb(0x6b, 0x6b, 0x76);
const LIGHT_FIELD: egui::Color32 = egui::Color32::from_rgb(0xeb, 0xeb, 0xf0);
const LIGHT_FOOTER: egui::Color32 = egui::Color32::from_rgb(0xea, 0xea, 0xef);

/// 页脚条底色（与面板区分的更深/更浅一层），按当前 visuals 主题取。
pub fn footer_fill(v: &egui::Visuals) -> egui::Color32 {
    if v.dark_mode {
        DARK_FOOTER
    } else {
        LIGHT_FOOTER
    }
}

/// 安装/切换主题样式。幂等：两套样式都写入，`ThemePreference` 决定生效者。
pub fn apply(ctx: &egui::Context, mode: ThemeMode) {
    for (theme, dark) in [(egui::Theme::Dark, true), (egui::Theme::Light, false)] {
        let mut style = egui::Style {
            visuals: if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            },
            ..Default::default()
        };
        let (bg, stroke, text, muted, field) = if dark {
            (DARK_BG, DARK_STROKE, DARK_TEXT, DARK_MUTED, DARK_FIELD)
        } else {
            (LIGHT_BG, LIGHT_STROKE, LIGHT_TEXT, LIGHT_MUTED, LIGHT_FIELD)
        };
        let v = &mut style.visuals;
        v.panel_fill = bg;
        v.window_corner_radius = egui::CornerRadius::same(12);
        v.menu_corner_radius = egui::CornerRadius::same(8);
        v.selection.bg_fill = ACCENT;
        v.hyperlink_color = ACCENT;
        // 文字令牌：text_color()/weak_text_color() 即业务代码的取色入口
        v.override_text_color = Some(text);
        v.weak_text_color = Some(muted);
        v.text_edit_bg_color = Some(field);
        for w in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = egui::CornerRadius::same(6);
            w.fg_stroke.color = text;
        }
        // 非交互态（下拉箭头/装饰线）：弱化色 + 输入域底色 + 描边
        v.widgets.noninteractive.corner_radius = egui::CornerRadius::same(6);
        v.widgets.noninteractive.fg_stroke.color = muted;
        v.widgets.noninteractive.weak_bg_fill = field;
        v.widgets.noninteractive.bg_stroke.color = stroke;
        v.widgets.inactive.weak_bg_fill = field;
        v.widgets.inactive.bg_stroke.color = stroke;
        v.widgets.inactive.weak_bg_fill = field;
        v.widgets.inactive.bg_stroke.color = stroke;
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        ctx.set_style_of(theme, std::sync::Arc::new(style));
    }
    ctx.set_theme(match mode {
        ThemeMode::Light => egui::ThemePreference::Light,
        ThemeMode::Dark => egui::ThemePreference::Dark,
        ThemeMode::System => egui::ThemePreference::System,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_follows_dark_mode() {
        assert_eq!(footer_fill(&egui::Visuals::dark()), DARK_FOOTER);
        assert_eq!(footer_fill(&egui::Visuals::light()), LIGHT_FOOTER);
    }
}
