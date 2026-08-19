//! Windows 目标：把 packaging/icon.png 编成多尺寸 ICO 嵌入 exe 资源。
//! 快捷方式/任务栏/资源管理器随 exe 图标自动生效，无需 SetIconLocation。
//! 单一来源 icon.png，构建期生成（PNG→BMP 帧），不提交 .ico 二进制。

use std::env;
use std::error::Error;
use std::fs::File;
use std::io::Write;
use std::path::Path;

fn main() -> Result<(), Box<dyn Error>> {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    let png = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/icon.png");
    println!("cargo:rerun-if-changed={}", png.display());
    let out = Path::new(&env::var("OUT_DIR")?).join("icon.ico");
    write_ico(&png, &out)?;

    let mut res = winresource::WindowsResource::new();
    res.set_icon(&out.display().to_string());
    res.compile()?;
    Ok(())
}

/// 多尺寸 ICO：≤128px 为 32bpp BMP 帧（BGRA 自底向上 + 零 AND 掩码），
/// 256px 为 PNG 帧（Vista+ 官方支持且体积最小）。
fn write_ico(png: &Path, ico: &Path) -> Result<(), Box<dyn Error>> {
    let src = image::open(png)?.into_rgba8();
    if src.width() != src.height() {
        return Err("icon.png 必须是正方形".into());
    }
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];

    let mut dir: Vec<u8> = Vec::new();
    let mut body: Vec<u8> = Vec::new();
    let mut n = 0u16;
    for &s in &sizes {
        let frame = if s == src.width() {
            src.clone()
        } else {
            image::imageops::resize(&src, s, s, image::imageops::FilterType::Lanczos3)
        };
        let (w, h) = (frame.width(), frame.height());
        let start = body.len() as u32;

        if s == 256 {
            let mut png_buf = Vec::new();
            frame.write_to(
                &mut std::io::Cursor::new(&mut png_buf),
                image::ImageFormat::Png,
            )?;
            body.extend_from_slice(&png_buf);
        } else {
            // BITMAPINFOHEADER：biHeight = 2h（XOR + AND 平面）
            let header: [u8; 40] = build_bmp_header(w, h);
            body.extend_from_slice(&header);
            for y in (0..h).rev() {
                for x in 0..w {
                    let px = frame.get_pixel(x, y);
                    body.extend_from_slice(&[px[2], px[1], px[0], px[3]]); // BGRA
                }
            }
            // AND 掩码全零（alpha 已承担透明度），行按 32bit 对齐
            let row = (w as usize).div_ceil(32) * 4;
            body.extend(std::iter::repeat_n(0, row * h as usize));
        }

        // ICONDIRENTRY：宽高 256 编码为 0
        dir.extend_from_slice(&[
            if w == 256 { 0 } else { w as u8 },
            if h == 256 { 0 } else { h as u8 },
            0,
            0,
            1,
            0,
            32,
            0,
        ]);
        dir.extend_from_slice(&(body.len() as u32 - start).to_le_bytes());
        dir.extend_from_slice(&(start + 6 + 16 * sizes.len() as u32).to_le_bytes());
        n += 1;
    }

    let mut f = File::create(ico)?;
    f.write_all(&[0, 0, 1, 0])?;
    f.write_all(&n.to_le_bytes())?;
    f.write_all(&dir)?;
    f.write_all(&body)?;
    Ok(())
}

fn build_bmp_header(w: u32, h: u32) -> [u8; 40] {
    let mut b = [0u8; 40];
    b[0..4].copy_from_slice(&40u32.to_le_bytes()); // biSize
    b[4..8].copy_from_slice(&w.to_le_bytes());
    b[8..12].copy_from_slice(&(h * 2).to_le_bytes()); // XOR+AND
    b[12..14].copy_from_slice(&1u16.to_le_bytes()); // planes
    b[14..16].copy_from_slice(&32u16.to_le_bytes()); // bpp
    b
}
