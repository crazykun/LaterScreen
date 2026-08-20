//! 配置（M8）：`~/.config/lscreen/config.toml`（Win `%APPDATA%`、
//! mac `~/Library/Application Support`）。
//!
//! 原则：
//! - 零配置可用：无文件时一切走默认值，**不生成文件**；保存动作（配置面板）
//!   才落盘。
//! - 向前兼容：未知字段忽略、缺失字段取默认（serde 默认值，不拒绝未知键）。
//! - 解析失败（文件损坏）不致命：stderr 提示 + 全默认值。

use lscreen_core::{Rgba, Style, Tool};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 默认文件名模板：`lscreen_20260819_101520.png`
pub const DEFAULT_TEMPLATE: &str = "lscreen_{YYYYMMDD}_{HHMMSS}";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Config {
    /// 图片保存目录；空 = 默认（~/Pictures，不存在则家目录）
    pub save_dir: String,
    /// 文件名模板，支持 {YYYYMMDD} {HHMMSS} {YYYY} {MM} {DD} {HH} {MI} {SS}，
    /// 未知 token 原样保留
    pub filename_template: String,
    /// 默认工具（select/rect/ellipse/arrow/line/curve/marker/text/mosaic/eraser）
    pub default_tool: String,
    /// 默认颜色，#RRGGBB
    pub default_color: String,
    /// 默认线宽 1-12
    pub default_width: f32,
    /// 复制到剪贴板后自动退出
    pub copy_auto_exit: bool,
    /// 保存文件后打开所在目录
    pub open_dir_after_save: bool,
    /// 全局热键（托盘模式生效），如 "Ctrl+Alt+A"；留空 = 不注册。
    /// 截图默认 F1（Snipaste 惯例、系统冲突率低——Ctrl+Alt+A 在 Deepin
    /// 等桌面是系统截图键）；裸键仅允许 PrintScreen/F1-F12
    pub hotkey_screenshot: String,
    pub hotkey_picker: String,
    pub hotkey_pin: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            save_dir: String::new(),
            filename_template: DEFAULT_TEMPLATE.to_string(),
            default_tool: "select".to_string(),
            default_color: "#e53935".to_string(),
            default_width: 3.0,
            copy_auto_exit: true,
            open_dir_after_save: false,
            hotkey_screenshot: "F1".to_string(),
            hotkey_picker: String::new(),
            hotkey_pin: String::new(),
        }
    }
}

impl Config {
    /// 读取配置：无文件 → 静默默认值（零配置原则，不生成文件不告警）；
    /// 损坏 → 默认值并提示（不中断业务）。
    pub fn load() -> Config {
        match Self::load_inner() {
            Ok(cfg) => cfg,
            Err((path, e)) => {
                eprintln!(
                    "lscreen: 配置文件解析失败，使用默认值（{e}）: {}",
                    path.display()
                );
                Config::default()
            }
        }
    }

    fn load_inner() -> Result<Config, (PathBuf, String)> {
        let path = config_path().ok_or_else(|| (PathBuf::from("."), "无法定位配置目录".into()))?;
        // 无配置文件是正常形态，静默走默认值
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| (path.clone(), e.to_string()))?;
        toml::from_str(&text).map_err(|e| (path, e.to_string()))
    }

    /// 保存到配置文件（创建目录）。仅在配置面板点击保存时调用。
    pub fn save(&self) -> Result<(), String> {
        let path = config_path().ok_or_else(|| "无法定位配置目录".to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("写入配置失败: {e}"))
    }

    /// 配置覆盖的保存目录；空串 = None（用默认目录）
    pub fn save_dir_override(&self) -> Option<PathBuf> {
        let d = self.save_dir.trim();
        if d.is_empty() {
            None
        } else {
            Some(PathBuf::from(d))
        }
    }

    pub fn tool(&self) -> Tool {
        tool_from_name(&self.default_tool).unwrap_or(Tool::Select)
    }

    pub fn style(&self) -> Style {
        Style {
            color: parse_hex_color(&self.default_color).unwrap_or(Rgba::RED),
            width: default_width_clamp(self.default_width),
            font_size: 12.0 + default_width_clamp(self.default_width) * 4.0,
        }
    }
}

/// 配置文件路径。
pub fn config_path() -> Option<PathBuf> {
    Some(config_dir()?.join("config.toml"))
}

/// 平台配置目录：Linux `$XDG_CONFIG_HOME`/`~/.config`、Win `%APPDATA%`、
/// mac `~/Library/Application Support` 下的 `lscreen/`。
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|d| d.join("lscreen"))
            // APPDATA 罕见缺失时退回 %USERPROFILE%\AppData\Roaming
            .or_else(|| home_dir().map(|h| h.join("AppData/Roaming/lscreen")))
    }
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|h| h.join("Library/Application Support/lscreen"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
            let p = PathBuf::from(dir);
            if p.is_absolute() {
                return Some(p.join("lscreen"));
            }
        }
        home_dir().map(|h| h.join(".config/lscreen"))
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

// ---------------------------------------------------------------- 工具名

pub const TOOL_NAMES: &[(&str, &str)] = &[
    ("select", "选择"),
    ("rect", "矩形"),
    ("ellipse", "椭圆"),
    ("arrow", "箭头"),
    ("line", "直线"),
    ("curve", "画笔"),
    ("marker", "标号"),
    ("text", "文本"),
    ("mosaic", "马赛克"),
    ("eraser", "橡皮"),
];

pub fn tool_from_name(name: &str) -> Option<Tool> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "select" => Tool::Select,
        "rect" | "rectangle" => Tool::Rect,
        "ellipse" | "circle" => Tool::Ellipse,
        "arrow" => Tool::Arrow,
        "line" => Tool::Line,
        "curve" | "pen" | "brush" => Tool::Curve,
        "marker" | "number" => Tool::Marker,
        "text" => Tool::Text,
        "mosaic" | "blur" => Tool::Mosaic,
        "eraser" => Tool::Eraser,
        _ => return None,
    })
}

// ---------------------------------------------------------------- 颜色

pub fn parse_hex_color(s: &str) -> Option<Rgba> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    let (r, g, b) = ((v >> 16) as u8, (v >> 8) as u8, v as u8);
    Some(Rgba([r, g, b, 0xff]))
}

pub fn hex_color(c: Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

pub fn default_width_clamp(w: f32) -> f32 {
    if w.is_finite() {
        w.clamp(1.0, 12.0)
    } else {
        3.0
    }
}

// ---------------------------------------------------------------- 文件名模板

/// 渲染文件名模板。时间参数为 (年,月,日,时,分,秒)。
/// 未知 token 原样保留，方便用户写 `{n}` 之类自定义占位符再手动改。
pub fn render_template(template: &str, t: (i64, u32, u32, u32, u32, u32)) -> String {
    let (y, mo, d, h, mi, s) = t;
    let mut out = template.to_string();
    let pairs: [(&str, String); 8] = [
        ("{YYYYMMDD}", format!("{y:04}{mo:02}{d:02}")),
        ("{HHMMSS}", format!("{h:02}{mi:02}{s:02}")),
        ("{YYYY}", format!("{y:04}")),
        ("{MM}", format!("{mo:02}")),
        ("{DD}", format!("{d:02}")),
        ("{HH}", format!("{h:02}")),
        ("{MI}", format!("{mi:02}")),
        ("{SS}", format!("{s:02}")),
    ];
    for (token, val) in pairs {
        if out.contains(token) {
            out = out.replace(token, &val);
        }
    }
    out
}

/// 校验模板：渲染后不得为空、不得含路径分隔符（防止模板注入路径穿越）。
pub fn validate_template(template: &str) -> Result<(), String> {
    let rendered = render_template(template, (2026, 1, 2, 3, 4, 5));
    if rendered.trim().is_empty() {
        return Err("模板渲染结果为空".into());
    }
    if rendered.contains('/') || rendered.contains('\\') || rendered.contains("..") {
        return Err("文件名不能包含路径分隔符".into());
    }
    Ok(())
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn defaults_on_empty() {
        let c = cfg("");
        assert_eq!(c, Config::default());
    }

    #[test]
    fn unknown_fields_ignored() {
        let c = cfg("future_field = 42\nfilename_template = \"shot_{YYYY}\"");
        assert_eq!(c.filename_template, "shot_{YYYY}");
    }

    #[test]
    fn roundtrip() {
        let c = Config {
            save_dir: "/tmp/pics".into(),
            default_tool: "arrow".into(),
            ..Default::default()
        };
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn template_tokens() {
        let t = (2026, 8, 19, 10, 15, 20);
        assert_eq!(
            render_template(DEFAULT_TEMPLATE, t),
            "lscreen_20260819_101520"
        );
        assert_eq!(
            render_template("{YYYY}-{MM}-{DD} {HH}:{MI}:{SS}", t),
            "2026-08-19 10:15:20"
        );
        // 未知 token 原样保留
        assert_eq!(render_template("a{XX}b", t), "a{XX}b");
    }

    #[test]
    fn template_validation() {
        assert!(validate_template(DEFAULT_TEMPLATE).is_ok());
        assert!(validate_template("{YYYY}").is_ok());
        assert!(validate_template("").is_err());
        assert!(validate_template("../etc/{YYYY}").is_err());
        assert!(validate_template("a/b").is_err());
    }

    #[test]
    fn tools_and_colors() {
        assert_eq!(tool_from_name("Rect"), Some(Tool::Rect));
        assert_eq!(tool_from_name(" select "), Some(Tool::Select));
        assert_eq!(tool_from_name("nope"), None);
        let c = parse_hex_color("#E53935").unwrap();
        assert_eq!(hex_color(c), "#e53935");
        assert!(parse_hex_color("#12345").is_none());
        assert!(parse_hex_color("red").is_none());
    }

    #[test]
    fn style_from_config() {
        let c = Config {
            default_width: 99.0, // 越界钳回 12
            default_color: "#00ff00".into(),
            ..Default::default()
        };
        let s = c.style();
        assert_eq!(s.width, 12.0);
        assert_eq!(s.color, Rgba([0x00, 0xff, 0x00, 0xff]));
        assert_eq!(s.font_size, 12.0 + 12.0 * 4.0);
    }
}
