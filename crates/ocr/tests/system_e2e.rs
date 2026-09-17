//! M17 真机验证（Windows/macOS）：系统 OCR 引擎（WinRT / Vision）中英文
//! 识别。测试图用 ab_glyph + 系统 CJK 字体现场渲染（仓库约定不提交图片
//! 文件），黑字白底 64px，是系统引擎最擅长的输入形态。
//! `LSCREEN_TEST_E2E=1 cargo test -p lscreen-ocr --test system_e2e -- --ignored`
//! 步骤与预期见 docs/VERIFY.md。

#![cfg(any(windows, target_os = "macos"))]

use ab_glyph::{point, Font, FontVec, Glyph, ScaleFont};

/// 平台内置中文字体候选（首个存在者胜出）
fn font_candidates() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &[
            r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑（Win10+ 必有）
            r"C:\Windows\Fonts\simhei.ttf", // 黑体
            r"C:\Windows\Fonts\simsun.ttc", // 宋体
        ]
    }
    #[cfg(target_os = "macos")]
    {
        &[
            "/System/Library/Fonts/PingFang.ttc", // 苹方
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
        ]
    }
}

fn load_font() -> FontVec {
    let mut err = String::new();
    for path in font_candidates() {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        // ttc 取第 0 个字面；ttf 的 try_from_vec 失败时也按索引 0 再试一次
        if let Ok(f) = FontVec::try_from_vec(bytes.clone()) {
            return f;
        }
        match FontVec::try_from_vec_and_index(bytes, 0) {
            Ok(f) => return f,
            Err(e) => err.push_str(&format!("{path}: {e}; ")),
        }
    }
    panic!("未找到可用系统 CJK 字体（{err}）");
}

/// 渲染两行测试文本（中文 + 英文数字），白底黑字，返回 (RGBA, w, h)
fn render_test_image() -> (Vec<u8>, u32, u32) {
    let font = load_font();
    let (w, h) = (1024u32, 280u32);
    let mut buf = vec![255u8; (w * h * 4) as usize];
    let size = 64.0f32;
    let scaled = font.as_scaled(size);
    let line_h = scaled.height() + scaled.line_gap();

    let lines = ["你好世界 屏幕截图测试", "Hello World 123456"];
    for (i, line) in lines.iter().enumerate() {
        let baseline = 40.0 + line_h * i as f32 + scaled.ascent();
        let mut pen_x = 40.0f32;
        for ch in line.chars() {
            let gid = scaled.glyph_id(ch);
            let glyph: Glyph = gid.with_scale_and_position(size, point(pen_x, baseline));
            if let Some(outlined) = font.outline_glyph(glyph) {
                let bb = outlined.px_bounds();
                outlined.draw(|gx, gy, cov| {
                    let px = bb.min.x as u32 + gx;
                    let py = bb.min.y as u32 + gy;
                    if px >= w || py >= h {
                        return;
                    }
                    // 白底上黑字：覆盖率直接压暗
                    let idx = ((py * w + px) * 4) as usize;
                    let v = 255 - (cov * 255.0) as u8;
                    buf[idx] = v;
                    buf[idx + 1] = v;
                    buf[idx + 2] = v;
                    buf[idx + 3] = 255;
                });
            }
            pen_x += scaled.h_advance(gid);
        }
    }
    (buf, w, h)
}

#[ignore = "需要真实系统环境（LSCREEN_TEST_E2E=1 显式开启）"]
#[test]
fn system_engine_zh_en() {
    if std::env::var("LSCREEN_TEST_E2E").ok().as_deref() != Some("1") {
        return;
    }
    // 显式选系统引擎（WinRT / Vision），避免落到兜底引擎掩盖问题
    std::env::set_var("LSCREEN_OCR_ENGINE", "system");
    let engine = lscreen_ocr::default_engine(&["chi_sim".to_string(), "eng".to_string()]);
    assert!(engine.available(), "系统引擎不可用: {}", engine.describe());

    let (rgba, w, h) = render_test_image();
    let out = engine.recognize(&rgba, w, h).expect("识别失败");
    let text = out.plain_text();
    println!("识别结果: {text:?}");

    let lower = text.to_lowercase();
    assert!(lower.contains("hello"), "英文未识别出: {text:?}");
    assert!(
        text.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)),
        "中文未识别出: {text:?}"
    );
    assert!(
        text.chars().filter(|c| c.is_ascii_digit()).count() >= 4,
        "数字未识别出: {text:?}"
    );
}
