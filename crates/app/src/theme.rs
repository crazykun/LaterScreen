//! 双主题（M12）：按 [`ThemeMode`] 为 egui 同时安装暗/浅两套样式，
//! 再用 `set_theme` 切换（System 模式由 egui 跟随操作系统配色）。
//!
//! 令牌原则：所有窗口（设置/历史）一律从 `ui.visuals()` 取色——
//! `text_color()`（主文字）、`weak_text_color()`（次级文字）、
//! `widgets.inactive.weak_bg_fill`（输入域底色）等，**禁止**在业务代码里
//! 硬编码 RGB，否则切浅色时就是"白字白底"。
//!
//! 强调色 [ACCENT] 是**唯一**允许在业务代码里出现的品牌色——但只作填充
//! （按钮底/竖条/徽章，配白字）；**当文字或描边用时必须走 [`accent_ink`]
//! / [`accent_line`]**，两者的深浅是跟着主题走的（见函数注释）。原因见
//! [`accent_ink`]：品牌红作前景色在浅色下只有 3.6:1，达不到可读线。
//!
//! 选中态（[`egui::Visuals::selection`]）是全框架共享的一个令牌：文本选区
//! 底色 + 选中文字色、`Button::selectable` 的选中底与文字、滑块 trailing
//! fill、TextEdit 聚焦框描边全都读它。因此它必须同时满足「底色与输入域
//! 可区分」和「文字压在底色上可读」——单一品牌红做不到（红底红字 = 选中
//! 文字完全看不见，v0.11.2 及之前的实际表现）。现在按主题各配一组
//! 「浅底 + 深色字」/「深底 + 浅色字」，对比度门槛由下方单测锁住。

use crate::config::ThemeMode;
use eframe::egui;

/// **装饰用**品牌红：按钮填充、卡片竖条、徽章底、Toast 描边——一律配白字。
/// 与默认标注色 `#e53935` 一致（改这里之前先看 [`accent_ink`] 的对比度约束）。
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(0xe5, 0x39, 0x35);

/// 表单控件统一高度：输入框（`input_field` min_size）与下拉框/数字框
/// （`spacing.interact_size.y` 下限）共用同一来源，保证同 Grid 内等高。
pub const FIELD_H: f32 = 32.0;

// ---------------------------------------------------------------- 暗色令牌

const DARK_BG: egui::Color32 = egui::Color32::from_rgb(0x14, 0x14, 0x18);
const DARK_STROKE: egui::Color32 = egui::Color32::from_rgb(0x3d, 0x3d, 0x4c);
const DARK_TEXT: egui::Color32 = egui::Color32::from_rgb(0xec, 0xec, 0xf1);
const DARK_MUTED: egui::Color32 = egui::Color32::from_rgb(0x9a, 0x9a, 0xa5);
const DARK_FIELD: egui::Color32 = egui::Color32::from_rgb(0x1f, 0x1f, 0x27);
/// 页脚条（比面板更深一层）
const DARK_FOOTER: egui::Color32 = egui::Color32::from_rgb(0x11, 0x11, 0x15);

// ---------------------------------------------------------------- 浅色令牌

const LIGHT_BG: egui::Color32 = egui::Color32::from_rgb(0xf4, 0xf4, 0xf7);
const LIGHT_STROKE: egui::Color32 = egui::Color32::from_rgb(0xdc, 0xdc, 0xe3);
const LIGHT_TEXT: egui::Color32 = egui::Color32::from_rgb(0x24, 0x24, 0x2b);
const LIGHT_MUTED: egui::Color32 = egui::Color32::from_rgb(0x6b, 0x6b, 0x76);
const LIGHT_FIELD: egui::Color32 = egui::Color32::from_rgb(0xeb, 0xeb, 0xf0);
const LIGHT_FOOTER: egui::Color32 = egui::Color32::from_rgb(0xea, 0xea, 0xef);

// ---------------------------------------------------------------- 选中态

/// 选中底色：浅色下是品牌红的**浅化**版（红调仍在、但不与输入域糊成一片），
/// 深色下是压暗的红。
const LIGHT_SELECT_BG: egui::Color32 = egui::Color32::from_rgb(0xff, 0xcf, 0xc7);
/// 选中文字色：压在选中底上的**深红**（不是品牌红——品牌红在浅底上只有
/// 约 2.5:1，等于红底红字）
const LIGHT_SELECT_INK: egui::Color32 = egui::Color32::from_rgb(0x7f, 0x15, 0x12);
const DARK_SELECT_BG: egui::Color32 = egui::Color32::from_rgb(0x6b, 0x26, 0x22);
/// 深色下反向：底色压暗、文字提亮（浅粉）
const DARK_SELECT_INK: egui::Color32 = egui::Color32::from_rgb(0xff, 0xd9, 0xd5);

// ---------------------------------------------------------------- 品牌红前景化

/// 品牌红作**文字/图标前景**时的主题化替代色。
///
/// 品牌红 `#e53935` 的绝对亮度落在中间地带：浅色主题下只有 3.6:1
/// （面板）/ 3.5:1（输入域），深色下 4.4:1 / 3.9:1——都不到 WCAG AA 的
/// 4.5:1，小字号的录屏提示/热键文案尤其吃力。因此作前景时按主题换深浅：
/// 浅色压深（`#c62828`，面板 5.1:1）、深色提亮（`#ff7b74`，面板 7.3:1）。
/// 色相不变，仍是同一个品牌红。
pub fn accent_ink(v: &egui::Visuals) -> egui::Color32 {
    if v.dark_mode {
        egui::Color32::from_rgb(0xff, 0x7b, 0x74)
    } else {
        egui::Color32::from_rgb(0xc6, 0x28, 0x28)
    }
}

/// 品牌红作**描边**（输入框聚焦框、热键录入态边框）时的主题化替代色。
/// 与 [`accent_ink`] 同源但可单独调：描边细，需要比文字更高的对比度才
/// 看得清，故两端都比 ink 再推一档。
pub fn accent_line(v: &egui::Visuals) -> egui::Color32 {
    if v.dark_mode {
        egui::Color32::from_rgb(0xff, 0x8a, 0x80)
    } else {
        egui::Color32::from_rgb(0xb7, 0x1c, 0x1c)
    }
}

/// 品牌红填充上的文字色：两主题都是纯白。
pub const ACCENT_ON_TEXT: egui::Color32 = egui::Color32::WHITE;

/// **要放白字**的品牌红填充（按钮/徽章实心底）：比 [`ACCENT`] 深一档。
/// 品牌红本尊配白字只有 4.2:1，不到正文 AA 门槛；深一档后 5.0:1，仍是
/// 同一色系（Material Red 700）。装饰性填充（卡片竖条、图标、Toast 描边）
/// 用 [`ACCENT`] 即可——那里不承载文字，不需要为可读性压深。
pub const ACCENT_FILL: egui::Color32 = egui::Color32::from_rgb(0xd3, 0x2f, 0x2f);

/// Toast /浮层提示的底色：比面板再"抬起"一层（浅色更白、深色更亮），
/// 配 `text_color()` 主文字。此前两主题都是黑底白字 + 品牌红描边，浅色
/// 主题下就是「白面板上糊一块黑」，与全窗格调冲突（用户反馈太丑）。
pub fn toast_fill(v: &egui::Visuals) -> egui::Color32 {
    if v.dark_mode {
        egui::Color32::from_rgb(0x26, 0x26, 0x2e)
    } else {
        egui::Color32::from_rgb(0xff, 0xff, 0xff)
    }
}

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
        let (bg, stroke, text, muted, field, sel_bg, sel_ink) = if dark {
            (
                DARK_BG,
                DARK_STROKE,
                DARK_TEXT,
                DARK_MUTED,
                DARK_FIELD,
                DARK_SELECT_BG,
                DARK_SELECT_INK,
            )
        } else {
            (
                LIGHT_BG,
                LIGHT_STROKE,
                LIGHT_TEXT,
                LIGHT_MUTED,
                LIGHT_FIELD,
                LIGHT_SELECT_BG,
                LIGHT_SELECT_INK,
            )
        };
        let v = &mut style.visuals;
        v.panel_fill = bg;
        v.window_corner_radius = egui::CornerRadius::same(12);
        v.menu_corner_radius = egui::CornerRadius::same(8);
        // 选中态：底色 + 压在底色上的文字色，两者必须成对（详见模块头注释
        // 与 `selection_is_readable` 单测）。**不要**只改其中之一——曾经的
        // 事故就是两者都是品牌红，选中文字彻底看不见
        v.selection.bg_fill = sel_bg;
        // TextEdit 聚焦框也取 selection.stroke：用选中文字色而非底色，
        // 保证细描边在输入域底色上可见（默认淡蓝与品牌红主题不搭）
        v.selection.stroke = egui::Stroke::new(1.5, sel_ink);
        // 超链接/可点文字：走主题化的品牌红前景色（品牌红本尊在浅色下不达标）
        v.hyperlink_color = sel_ink;
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
        // 非交互态（下拉箭头/装饰线）：弱化色 + 输入域底色 + 描边。
        // egui 默认 inactive.bg_stroke 是 Stroke::NONE（width 0），只改 color
        // 不会画线——必须整条 Stroke::new 赋值，否则输入框边框隐形。
        v.widgets.noninteractive.corner_radius = egui::CornerRadius::same(6);
        v.widgets.noninteractive.fg_stroke.color = muted;
        v.widgets.noninteractive.weak_bg_fill = field;
        v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, stroke);
        v.widgets.inactive.weak_bg_fill = field;
        v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, stroke);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = FIELD_H;
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

    #[test]
    fn dark_input_tokens_contrast() {
        // 回归：曾 #16161b vs #141418 通道仅差 2，夜间输入域与面板融为一体
        let lum = |c: egui::Color32| (c.r() as u32 + c.g() as u32 + c.b() as u32) as f32 / 3.0;
        assert!(
            lum(DARK_FIELD) - lum(DARK_BG) >= 8.0,
            "暗色输入域底色需亮于面板底色"
        );
        assert!(
            lum(DARK_STROKE) - lum(DARK_FIELD) >= 16.0,
            "暗色描边需显著亮于输入域底色"
        );
    }

    #[test]
    fn text_edit_frame_paintable() {
        // 回归：只赋 bg_stroke.color 时 width 仍为 0（Stroke::NONE），
        // 输入框边框整条不渲染
        let ctx = egui::Context::default();
        apply(&ctx, ThemeMode::Dark);
        let v = &ctx.style_of(egui::Theme::Dark).visuals;
        let stroke = v.widgets.inactive.bg_stroke;
        assert!(!stroke.is_empty(), "输入框 idle 边框必须有宽度");
        assert_ne!(
            v.text_edit_bg_color(),
            v.panel_fill,
            "输入域底色必须区别于面板底色"
        );
    }

    // -- 对比度标尺（WCAG 相对亮度 / 对比度；用于锁住下面几条门槛） --

    fn channel(c: u8) -> f32 {
        let s = c as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }

    /// 相对亮度（WCAG 2.1 定义）
    fn luminance(c: egui::Color32) -> f32 {
        let (r, g, b) = (channel(c.r()), channel(c.g()), channel(c.b()));
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    /// 对比度（1.0 = 完全相同，21.0 = 黑白）
    fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = (la.max(lb), la.min(lb));
        (hi + 0.05) / (lo + 0.05)
    }

    /// 正文可读线（WCAG AA 小字 4.5:1）
    const AA: f32 = 4.5;

    #[test]
    fn toast_follows_theme() {
        // 回归（用户反馈「浅色下 toast 还是黑底红框，太丑」）：Toast 底色
        // 此前是硬编码的半透明黑，浅色面板上像糊了一块。现在按主题抬起
        // 一层，并要求「主文字在其上可读」+「描边可见」。
        //
        // 注意：**不**要求底色与面板拉开对比度——浅色下只能是「比面板更白」，
        // 纯白对 #f4f4f7 也只有 1.10，靠更强的投影与品牌红描边区分层次
        // （真靠底色对比去凑，就得把面板压暗，反而破坏整体格调）。深色侧
        // 反过来有 1.22，顺带校验一下别退化了。
        let ctx = egui::Context::default();
        apply(&ctx, ThemeMode::Dark);
        for (theme, panel) in [(egui::Theme::Dark, DARK_BG), (egui::Theme::Light, LIGHT_BG)] {
            let v = &ctx.style_of(theme).visuals;
            let fill = toast_fill(v);
            let text = v.text_color();
            if v.dark_mode {
                assert!(
                    contrast(fill, panel) >= 1.15,
                    "深色 Toast 底色与面板过于接近（{:.2}）",
                    contrast(fill, panel)
                );
            } else {
                // 浅色：底色只能是「更白」，靠描边+投影立起来；这里只要求
                // 它确实比面板亮（方向对），不要求对比度数值
                assert!(
                    luminance(fill) > luminance(panel),
                    "浅色 Toast 底色应比面板更亮"
                );
            }
            assert!(
                contrast(text, fill) >= AA,
                "{theme:?} Toast 文字在底色上仅 {:.2}（需 ≥ {AA}）",
                contrast(text, fill)
            );
            let border = accent_line(v);
            assert!(
                contrast(border, fill) >= AA,
                "{theme:?} Toast 描边在底色上仅 {:.2}（需 ≥ {AA}）",
                contrast(border, fill)
            );
        }
    }

    #[test]
    fn selection_is_readable() {
        // 回归（用户主诉「选中底色是红色，但字体看不见」）：v0.11.2 及之前
        // selection.bg_fill 与 selection.stroke.color 都是品牌红 ACCENT，
        // 选中文字 = 红字压红底，对比度 1.0。选中态是全框架共享令牌（文本
        // 选区、Button::selectable、滑块 trailing fill、TextEdit 聚焦框），
        // 所以底色与文字色必须成对校验，二者缺一不可
        let ctx = egui::Context::default();
        apply(&ctx, ThemeMode::Dark);
        for (theme, field, panel) in [
            (egui::Theme::Dark, DARK_FIELD, DARK_BG),
            (egui::Theme::Light, LIGHT_FIELD, LIGHT_BG),
        ] {
            let sel = ctx.style_of(theme).visuals.selection;
            let ratio = contrast(sel.stroke.color, sel.bg_fill);
            assert!(
                ratio >= AA,
                "{theme:?} 选中文字/选中底色 对比度仅 {ratio:.2}（需 ≥ {AA}）"
            );
            // 底色本身也要能被看见：与所在表面（输入域/面板）拉开差距，
            // 否则「选中了但看不出选区范围」
            assert!(
                contrast(sel.bg_fill, field) >= 1.15,
                "{theme:?} 选中底色与输入域底色过于接近（{:.2}）",
                contrast(sel.bg_fill, field)
            );
            assert!(
                contrast(sel.bg_fill, panel) >= 1.15,
                "{theme:?} 选中底色与面板底色过于接近（{:.2}）",
                contrast(sel.bg_fill, panel)
            );
            // 聚焦框描边（细线）在输入域底色上要比正文更显眼
            assert!(
                contrast(sel.stroke.color, field) >= AA,
                "{theme:?} 聚焦框描边在输入域上不可辨（{:.2}）",
                contrast(sel.stroke.color, field)
            );
            assert!(
                !sel.stroke.is_empty(),
                "{theme:?} 聚焦框描边必须有宽度（细线也别设 NONE）"
            );
        }
    }

    #[test]
    fn accent_foreground_is_readable() {
        // 品牌红 ACCENT 的绝对亮度在中间地带：作前景（文字/描边）时两主题
        // 都不达 AA（浅色面板 3.85、深色面板 4.35）。accent_ink/accent_line
        // 是它的主题化替身，这里锁住「替身达标」这一契约——业务代码里凡是
        // 把品牌红当文字/描边用的地方都应改走它们
        let dark = egui::Visuals::dark();
        let light = egui::Visuals::light();
        for (v, panel, field) in [
            (&dark, DARK_BG, DARK_FIELD),
            (&light, LIGHT_BG, LIGHT_FIELD),
        ] {
            let ink = accent_ink(v);
            for (name, bg) in [("面板", panel), ("输入域", field)] {
                let r = contrast(ink, bg);
                assert!(r >= AA, "accent_ink 在{name}上仅 {r:.2}（需 ≥ {AA}）");
            }
            let line = accent_line(v);
            let r = contrast(line, field);
            assert!(r >= AA, "accent_line 在输入域上仅 {r:.2}（需 ≥ {AA}）");
        }
        // 顺带钉住「品牌红本尊不适合当文字」这一事实，防止有人把它改回去
        assert!(
            contrast(ACCENT, LIGHT_BG) < AA,
            "若品牌红在浅色下已达标，accent_ink 可简化为 ACCENT（届时同步改注释）"
        );
    }

    #[test]
    fn accent_fill_carries_white_text() {
        // 承载白字的实心填充（保存按钮等）必须达标：品牌红本尊配白字只有
        // 4.23（差 0.27 到 AA），故按钮统一用深一档的 ACCENT_FILL
        let r = contrast(ACCENT_ON_TEXT, ACCENT_FILL);
        assert!(r >= AA, "白字在 ACCENT_FILL 上仅 {r:.2}（需 ≥ {AA}）");
        // 顺带钉住「品牌红本尊配白字不达标」这一事实，防止有人改回 ACCENT
        assert!(
            contrast(ACCENT_ON_TEXT, ACCENT) < AA,
            "若品牌红配白字已达标，ACCENT_FILL 可简化为 ACCENT（届时同步改注释）"
        );
    }
}
