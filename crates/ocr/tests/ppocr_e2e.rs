//! M19 本地 PP-OCR 真机验证；普通 cargo test 不加载模型、不联网。
//!
//! 预先下载好的模型放到独立目录，不读写用户缓存，也不在测试中 set_env：
//! `LSCREEN_TEST_PPOCR=1 LSCREEN_PPOCR_TEST_MODELS=/tmp/tract-spike/models cargo test -p lscreen-ocr --test ppocr_e2e -- --ignored --nocapture`
//! 也接受 `LSCREEN_TEST_E2E=1`。字体来自系统，可用 `LSCREEN_TEST_FONT=/path/to/font.ttf`
//! 固定字体（TTC 默认第 0 个字面）；指定文件不可读或缺字时明确失败，不换字体。
//! 显式开启后缺模型/缺字体必须失败，不能把未执行的真机验证报为通过。
//! 可选 `LSCREEN_PPOCR_FIXTURE_DIR=/tmp/ppocr-fixtures` 保存小字号深浅/彩色 PNG；
//! 仅设置该变量时写入指定目录，默认不落盘，也不写用户模型缓存。

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use ab_glyph::{point, Font, FontVec, ScaleFont};
use image::RgbaImage;
use lscreen_ocr::{paddle::PaddleOcr, BoundingBox, OcrOutput, TextRecognizer};

const ZH_LEFT: &str = "截图文字测试";
const ZH_RIGHT: &str = "独立文本块";
const EN_LINE: &str = "Hello World 123456";
const TRANSPARENT_ZH: &str = "透明背景 截图测试";
const TRANSPARENT_EN: &str = "Alpha text 123456";
const LONG_LINE: &str = "START123 The quick brown fox jumps over the lazy dog 0123456789 END789";
const EDGE_ZH: &str = "边界文字测试";
const EDGE_EN: &str = "Edge text 987654";
const REQUIRED_TEXT: [&str; 8] = [
    ZH_LEFT,
    ZH_RIGHT,
    EN_LINE,
    TRANSPARENT_ZH,
    TRANSPARENT_EN,
    LONG_LINE,
    EDGE_ZH,
    EDGE_EN,
];

fn enabled() -> bool {
    let enabled = ["LSCREEN_TEST_PPOCR", "LSCREEN_TEST_E2E"]
        .iter()
        .any(|name| std::env::var(name).ok().as_deref() == Some("1"));
    if !enabled {
        eprintln!("未运行 PP-OCR 真机验证：需 LSCREEN_TEST_PPOCR=1（或 LSCREEN_TEST_E2E=1）");
    }
    enabled
}

/// 所有 ignored 用例共享一个本地引擎并串行推理，避免并行加载模型争用内存。
fn engine() -> MutexGuard<'static, PaddleOcr> {
    static ENGINE: OnceLock<Mutex<PaddleOcr>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let path = std::env::var_os("LSCREEN_PPOCR_TEST_MODELS")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .expect("请设置 LSCREEN_PPOCR_TEST_MODELS=/path/to/models；测试不会下载模型或使用用户缓存");
            assert!(path.is_dir(), "PP-OCR 测试模型目录不存在: {}", path.display());
            for file in [
                "ch_PP-OCRv4_det_mobile.onnx",
                "ch_PP-OCRv4_rec_mobile.onnx",
                "ppocr_keys_v1.txt",
            ] {
                let file = path.join(file);
                assert!(
                    file.metadata().is_ok_and(|m| m.is_file() && m.len() > 0),
                    "缺少非空 PP-OCR 测试模型: {}；请提前准备完整模型，测试不会自动下载",
                    file.display()
                );
            }
            eprintln!("PP-OCR 只读测试模型目录: {}", path.display());
            let engine = PaddleOcr::with_models_dir(path);
            assert!(engine.available(), "PP-OCR 本地模型不可用: {}", engine.describe());
            Mutex::new(engine)
        })
        // 断言失败不让其余独立 fixture 只报毒锁，仍应报告各自的识别结果。
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn font_candidates() -> Vec<(PathBuf, u32)> {
    if let Some(path) = std::env::var_os("LSCREEN_TEST_FONT") {
        assert!(!path.is_empty(), "LSCREEN_TEST_FONT 不能为空");
        return vec![(path.into(), 0)];
    }
    #[allow(unused_mut)]
    let mut paths = Vec::new();
    #[cfg(target_os = "linux")]
    {
        // fc-match 的成功不代表字体真的有汉字；后面还会检查全部 fixture 字形。
        // fc-list 则只列声明中文覆盖的字体，严格排除 Thin/Light 和非正常字重。
        const FORMAT: &str = "%{file}\t%{index}\t%{style}\t%{weight}\n";
        for (program, pattern) in [
            ("fc-match", "sans-serif:lang=zh-cn:weight=80"),
            ("fc-list", ":lang=zh"),
        ] {
            let Ok(output) = std::process::Command::new(program)
                .args(["-f", FORMAT, pattern])
                .output()
            else {
                continue;
            };
            if !output.status.success() {
                continue;
            }
            let mut candidates = Vec::new();
            for row in String::from_utf8_lossy(&output.stdout).lines() {
                let fields: Vec<&str> = row.split('\t').collect();
                let [file, index, style, weight] = fields.as_slice() else {
                    continue;
                };
                let style = format!("{file} {style}").to_lowercase();
                if ["thin", "light", "hairline"]
                    .iter()
                    .any(|s| style.contains(s))
                {
                    continue;
                }
                if !weight.parse::<u16>().is_ok_and(|w| (80..=100).contains(&w)) {
                    continue;
                }
                let Ok(index) = index.parse::<u32>() else {
                    continue;
                };
                candidates.push((PathBuf::from(file), index));
            }
            // fontconfig 的枚举顺序不稳定，固定为 Sans 优先，再按路径与字面排序。
            candidates.sort_by_key(|(path, index)| {
                let name = path.to_string_lossy().to_lowercase();
                (!name.contains("sans"), name, *index)
            });
            paths.extend(candidates);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        paths.extend(
            ["msyh.ttc", "simhei.ttf", "simsun.ttc"]
                .map(|name| (PathBuf::from(&root).join("Fonts").join(name), 0)),
        );
    }
    #[cfg(target_os = "macos")]
    paths.extend([
        (PathBuf::from("/System/Library/Fonts/PingFang.ttc"), 0),
        (
            PathBuf::from("/System/Library/Fonts/Hiragino Sans GB.ttc"),
            0,
        ),
        (PathBuf::from("/System/Library/Fonts/STHeiti Medium.ttc"), 0),
    ]);
    paths.dedup();
    paths
}

fn load_font() -> FontVec {
    let mut failures = Vec::new();
    for (path, index) in font_candidates() {
        let result = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                FontVec::try_from_vec_and_index(bytes, index).map_err(|e| e.to_string())
            });
        match result {
            Ok(font) => {
                let missing: String = REQUIRED_TEXT
                    .iter()
                    .flat_map(|text| text.chars())
                    .filter(|c| !c.is_whitespace() && font.glyph_id(*c).0 == 0)
                    .collect();
                if missing.is_empty() {
                    eprintln!("PP-OCR fixture 字体: {}（字面 {index}）", path.display());
                    return font;
                }
                failures.push(format!("{}: 缺字 {missing}", path.display()));
            }
            Err(e) => failures.push(format!("{}: {e}", path.display())),
        }
    }
    panic!(
        "未找到完整覆盖测试中英文的常规字重系统 CJK 字体；安装字体或设置 LSCREEN_TEST_FONT=/path/to/font.ttf。{}",
        failures.join("; ")
    );
}

struct Fixture {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    /// 每个独立输入文本块的实际非透明笔画范围。
    ink: Vec<BoundingBox>,
}

fn text_width(font: &FontVec, size: f32, text: &str) -> u32 {
    let scaled = font.as_scaled(size);
    let mut pen = 0.0;
    let mut previous = None;
    for ch in text.chars() {
        let id = scaled.glyph_id(ch);
        if let Some(prev) = previous {
            pen += scaled.kern(prev, id);
        }
        pen += scaled.h_advance(id);
        previous = Some(id);
    }
    pen.ceil() as u32
}

/// x/y 为文本排版左上角；透明用例在透明黑底上画带 alpha 的黑字，
/// 丢弃 alpha 会变成整张黑图，因此能回归真实的白底合成路径。
fn render(
    font: &FontVec,
    size: f32,
    width: u32,
    height: u32,
    lines: &[(&str, u32, u32)],
    transparent: bool,
) -> Fixture {
    let mut fixture = Fixture {
        rgba: vec![if transparent { 0 } else { 255 }; (width * height * 4) as usize],
        width,
        height,
        ink: Vec::new(),
    };
    let scaled = font.as_scaled(size);
    for &(line, x, y) in lines {
        let mut pen = x as f32;
        let baseline = y as f32 + scaled.ascent();
        let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0, 0);
        let mut previous = None;
        for ch in line.chars() {
            let id = scaled.glyph_id(ch);
            if let Some(prev) = previous {
                pen += scaled.kern(prev, id);
            }
            if let Some(outline) =
                font.outline_glyph(id.with_scale_and_position(size, point(pen, baseline)))
            {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, coverage| {
                    if coverage <= 0.0 {
                        return;
                    }
                    let px = bounds.min.x as i32 + gx as i32;
                    let py = bounds.min.y as i32 + gy as i32;
                    assert!(
                        px >= 0 && py >= 0 && px < width as i32 && py < height as i32,
                        "fixture 裁掉了字符 {ch:?}，位置 ({px}, {py}) 不在 {width}×{height} 内"
                    );
                    let (px, py) = (px as u32, py as u32);
                    x0 = x0.min(px);
                    y0 = y0.min(py);
                    x1 = x1.max(px + 1);
                    y1 = y1.max(py + 1);
                    let idx = ((py * width + px) * 4) as usize;
                    if transparent {
                        let old = f32::from(fixture.rgba[idx + 3]);
                        fixture.rgba[idx + 3] = (old + (255.0 - old) * coverage).round() as u8;
                    } else {
                        let value = (f32::from(fixture.rgba[idx]) * (1.0 - coverage)).round() as u8;
                        fixture.rgba[idx..idx + 3].fill(value);
                    }
                });
            }
            pen += scaled.h_advance(id);
            previous = Some(id);
        }
        assert!(x1 > x0 && y1 > y0, "fixture 没画出文字: {line:?}");
        fixture.ink.push(BoundingBox {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        });
    }
    fixture
}

fn recognize(engine: &PaddleOcr, name: &str, image: &Fixture) -> OcrOutput {
    let start = Instant::now();
    let result = engine.recognize(&image.rgba, image.width, image.height);
    eprintln!(
        "PP-OCR {name}: {}×{}，耗时 {:?}（首次调用包含模型验证/加载）",
        image.width,
        image.height,
        start.elapsed()
    );
    let output = result.unwrap_or_else(|e| panic!("PP-OCR {name} 失败: {e}"));
    eprintln!("PP-OCR {name} 结果: {:?}", output.blocks);
    for block in &output.blocks {
        let bbox = block.bbox.expect("PP-OCR 文本块应提供原图 bbox");
        assert!(bbox.width > 0 && bbox.height > 0, "空 bbox: {bbox:?}");
        assert!(
            bbox.x < image.width
                && bbox.y < image.height
                && bbox
                    .x
                    .checked_add(bbox.width)
                    .is_some_and(|right| right <= image.width)
                && bbox
                    .y
                    .checked_add(bbox.height)
                    .is_some_and(|bottom| bottom <= image.height),
            "bbox 越界: {bbox:?}，原图 {}×{}",
            image.width,
            image.height
        );
        if let Some(confidence) = block.confidence {
            assert!(
                confidence.is_finite() && (0.0..=1.0).contains(&confidence),
                "非法置信度: {confidence}"
            );
        }
    }
    output
}

fn compact(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn intersects(a: BoundingBox, b: BoundingBox) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

fn matching_block(output: &OcrOutput, fragments: &[&str]) -> usize {
    output
        .blocks
        .iter()
        .position(|block| {
            let text = compact(&block.text);
            fragments
                .iter()
                .all(|fragment| text.contains(&compact(fragment)))
        })
        .unwrap_or_else(|| panic!("未在单个文本块中识别出 {fragments:?}: {:?}", output.blocks))
}

#[test]
#[ignore = "需要本地模型、系统 CJK 字体及 LSCREEN_TEST_PPOCR=1（或 LSCREEN_TEST_E2E=1）"]
fn ppocr_multiline_keeps_independent_blocks() {
    if !enabled() {
        return;
    }
    let engine = engine();
    let font = load_font();
    // 同一行的两栏与下一行分别是独立文本块，避免简单按 y/间距错误归并。
    let image = render(
        &font,
        72.0,
        1360,
        340,
        &[(ZH_LEFT, 28, 24), (ZH_RIGHT, 850, 24), (EN_LINE, 28, 195)],
        false,
    );
    let output = recognize(&engine, "黑白多行中英文/独立两栏", &image);
    let indices = [
        matching_block(&output, &["截图", "测试"]),
        matching_block(&output, &["独立", "文本"]),
        matching_block(&output, &["hello", "123456"]),
    ];
    assert!(
        indices[0] != indices[1] && indices[0] != indices[2] && indices[1] != indices[2],
        "独立文本被错误归并: {:?}",
        output.blocks
    );
    for (region, index) in image.ink.iter().zip(indices) {
        assert!(
            intersects(*region, output.blocks[index].bbox.unwrap()),
            "识别文字 bbox 与对应输入文本位置不符"
        );
    }
    for block in &output.blocks {
        let bbox = block.bbox.unwrap();
        assert!(
            image
                .ink
                .iter()
                .filter(|region| intersects(bbox, **region))
                .count()
                <= 1,
            "bbox 跨越独立文本块: {bbox:?}"
        );
    }
}

#[test]
#[ignore = "需要本地模型、系统 CJK 字体及 LSCREEN_TEST_PPOCR=1（或 LSCREEN_TEST_E2E=1）"]
fn ppocr_transparent_background_and_blank_images() {
    if !enabled() {
        return;
    }
    let engine = engine();
    let font = load_font();
    let image = render(
        &font,
        72.0,
        1120,
        280,
        &[(TRANSPARENT_ZH, 30, 20), (TRANSPARENT_EN, 30, 150)],
        true,
    );
    let output = recognize(&engine, "透明底中英文", &image);
    let chinese = matching_block(&output, &["透明", "测试"]);
    let english = matching_block(&output, &["alpha", "123456"]);
    assert_ne!(chinese, english, "透明底的两行被错误合并");
    for transparent in [false, true] {
        let blank = render(&font, 72.0, 640, 240, &[], transparent);
        let output = recognize(
            &engine,
            if transparent {
                "全透明空图"
            } else {
                "白底空图"
            },
            &blank,
        );
        assert!(
            output.blocks.is_empty(),
            "空图不应产生文本块: {:?}",
            output.blocks
        );
    }
}

#[test]
#[ignore = "需要本地模型、系统 CJK 字体及 LSCREEN_TEST_PPOCR=1（或 LSCREEN_TEST_E2E=1）"]
fn ppocr_long_line_keeps_start_middle_and_end() {
    if !enabled() {
        return;
    }
    let engine = engine();
    let font = load_font();
    assert!(compact(LONG_LINE).chars().count() > 40);
    let width = text_width(&font, 64.0, LONG_LINE) + 64;
    let image = render(&font, 64.0, width, 170, &[(LONG_LINE, 30, 30)], false);
    let output = recognize(&engine, "长行（超过 40 字符）", &image);
    let text = compact(&output.plain_text());
    for fragment in ["START123", "quickbrownfox", "0123456789", "END789"] {
        assert!(
            text.contains(&compact(fragment)),
            "长行缺少 {fragment:?}，疑似固定宽度压缩/截断: {text:?}"
        );
    }
    assert!(
        text.chars().filter(|c| c.is_ascii_alphanumeric()).count() > 40,
        "长行丢字: {text:?}"
    );
}

#[test]
#[ignore = "需要本地模型、系统 CJK 字体及 LSCREEN_TEST_PPOCR=1（或 LSCREEN_TEST_E2E=1）"]
fn ppocr_edge_text_boxes_stay_inside_original_image() {
    if !enabled() {
        return;
    }
    let engine = engine();
    let font = load_font();
    let size = 72.0;
    let english_width = text_width(&font, size, EDGE_EN);
    let width = text_width(&font, size, EDGE_ZH).max(english_width) + 8;
    let image = render(
        &font,
        size,
        width,
        160,
        &[(EDGE_ZH, 2, 0), (EDGE_EN, width - english_width - 2, 80)],
        false,
    );
    let output = recognize(&engine, "靠近原图边缘的文本框", &image);
    matching_block(&output, &["边界", "测试"]);
    matching_block(&output, &["edge", "987654"]);
    // recognize 已对所有 bbox 检查非零尺寸、溢出与原图边界；这里同时要求确实检出文字。
    assert!(
        output.blocks.len() >= 2,
        "边缘两行未独立检出: {:?}",
        output.blocks
    );
}

#[test]
#[ignore = "需要本地模型、系统 CJK 字体及 LSCREEN_TEST_PPOCR=1（或 LSCREEN_TEST_E2E=1）"]
fn ppocr_screen_size_text_in_light_dark_and_color() {
    if !enabled() {
        return;
    }
    let engine = engine();
    let font = load_font();
    let fixture_dir = std::env::var_os("LSCREEN_PPOCR_FIXTURE_DIR").map(|path| {
        assert!(!path.is_empty(), "LSCREEN_PPOCR_FIXTURE_DIR 不能为空");
        let path = PathBuf::from(path);
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|e| panic!("创建 fixture 目录 {} 失败: {e}", path.display()));
        path
    });
    for variant in ["light", "dark", "color"] {
        // NotoSansCJK 的 32px 排版高对应约 21px 实际字形高，更接近屏幕文本。
        // 960px 宽画布避免 det 把整张小图放大成大字，三种颜色复用同样的布局。
        let mut image = render(
            &font,
            32.0,
            960,
            180,
            &[(ZH_LEFT, 24, 20), ("English text 123456", 24, 70)],
            false,
        );
        eprintln!(
            "PP-OCR size32/{variant} 原图文字高度: {:?}",
            image.ink.iter().map(|bbox| bbox.height).collect::<Vec<_>>()
        );
        for pixel in image.rgba.as_chunks_mut::<4>().0 {
            match variant {
                "dark" => {
                    // 反色只影响 RGB，保留 alpha 和原有抗锯齿覆盖率。
                    for channel in &mut pixel[..3] {
                        *channel = 255 - *channel;
                    }
                }
                "color" if pixel[0] < 255 => {
                    // 深蓝文字，R/B 不对称；按原灰度混入白底，保留抗锯齿。
                    let gray = u16::from(pixel[0]);
                    for (channel, ink) in pixel[..3].iter_mut().zip([28u16, 72, 160]) {
                        *channel = (gray + ((255 - gray) * ink + 127) / 255) as u8;
                    }
                }
                _ => {}
            }
        }
        if let Some(dir) = &fixture_dir {
            let path = dir.join(format!("ppocr-screen-size32-{variant}.png"));
            RgbaImage::from_raw(image.width, image.height, image.rgba.clone())
                .expect("fixture 缓冲与尺寸一致")
                .save(&path)
                .unwrap_or_else(|e| panic!("保存 fixture {} 失败: {e}", path.display()));
            eprintln!("PP-OCR fixture 已保存: {}", path.display());
        }
        let output = recognize(&engine, &format!("屏幕小字号 size32/{variant}"), &image);
        // 验证核心短词而非逐字满分，允许非关键词字符的字体/模型识别差异。
        let chinese = matching_block(&output, &["截图"]);
        let english = matching_block(&output, &["english", "123"]);
        assert_ne!(chinese, english, "size32/{variant} 中英文两行被错误合并");
        for (region, index) in image.ink.iter().zip([chinese, english]) {
            assert!(
                intersects(*region, output.blocks[index].bbox.unwrap()),
                "size32/{variant} 识别框与对应输入行不相交"
            );
        }
        // recognize 统一检查所有 bbox 在原图内及置信度为有限合法值。
    }
}

/// 不依赖模型/字体的离线回归：外部模型目录缺失必须报错，不能自动创建/下载。
/// 只创建自有临时父目录；测试结束删除，不触碰 HOME 或任何现有模型缓存。
#[test]
fn missing_external_models_do_not_create_or_download_files() {
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = std::env::temp_dir().join(format!(
        "lscreen-ppocr-missing-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&parent).expect("创建本次测试专属临时目录失败");
    // 只有成功创建后才接管清理，避免重名创建失败时误删既有目录。
    let parent = TempDir(parent);
    let missing = parent.0.join("not-downloaded");
    let engine = PaddleOcr::with_models_dir(missing.clone());
    assert!(!engine.available(), "外部目录无模型，不应声明模型已就绪");
    let error = engine
        .recognize(&[255; 4 * 4 * 4], 4, 4)
        .expect_err("缺模型应明确报错，不能回退或下载");
    assert!(
        error.to_string().contains("模型"),
        "缺模型错误缺少说明: {error}"
    );
    assert!(!missing.exists(), "缺模型的外部目录不应被创建");
    assert_eq!(
        std::fs::read_dir(&parent.0).unwrap().count(),
        0,
        "测试目录出现下载产物"
    );
}
