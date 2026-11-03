//! 测试辅助（仅 `#[cfg(test)]` 编译）：egui 离屏 tessellate + 逐三角形软件
//! 光栅化 → PNG。供 icon_preview（pin.rs）与 settings_nav_preview
//! （settings_ui.rs）共用。只画无纹理几何（uv==WHITE_UV）；文字字形
//! 的 uv 落在字体图集内，会被跳过——预览里别依赖文字内容。

use eframe::egui;
use egui::epaint::{Primitive, WHITE_UV};

/// 把一帧的绘制结果光栅化保存为 PNG，返回像素尺寸。
pub(crate) fn rasterize_png(
    ctx: &egui::Context,
    out: egui::FullOutput,
    logical_size: egui::Vec2,
    path: &str,
) -> (usize, usize) {
    let prims = ctx.tessellate(out.shapes, out.pixels_per_point);

    // 软件光栅化：逐三角形重心覆盖，顶点 alpha 插值 + clamp 处理
    // 线段衔接处的少量重叠。线条/填充共用同一 mesh（texture 均为
    // Managed(0)），靠 uv 区分：uv==WHITE_UV 才是无纹理几何
    let ppp = out.pixels_per_point;
    let w = (logical_size.x * ppp) as usize;
    let h = (logical_size.y * ppp) as usize;
    // 累积 alpha 与预乘 rgb，末端归一化后对深灰底做 src-over，
    // 保留 Frame 底色与线条白色的层次
    let mut acc_a = vec![0.0f32; w * h];
    let mut acc_c = vec![[0.0f32; 3]; w * h];
    for cp in &prims {
        let Primitive::Mesh(mesh) = &cp.primitive else {
            continue;
        };
        // as_chunks 借出 [[u32; 3]]，索引访问免逐元素越界检查
        for tri in mesh.indices.as_chunks::<3>().0 {
            let v = |i: u32| &mesh.vertices[i as usize];
            let (a, b, c) = (v(tri[0]), v(tri[1]), v(tri[2]));
            if [a, b, c].iter().any(|v| v.uv != WHITE_UV) {
                continue;
            }
            let (ax, ay) = (a.pos.x * ppp, a.pos.y * ppp);
            let (bx, by) = (b.pos.x * ppp, b.pos.y * ppp);
            let (cx, cy) = (c.pos.x * ppp, c.pos.y * ppp);
            let area2 = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
            if area2.abs() < 1e-9 {
                continue;
            }
            let min_x = (ax.min(bx).min(cx).floor().max(0.0) as usize).min(w - 1);
            let min_y = (ay.min(by).min(cy).floor().max(0.0) as usize).min(h - 1);
            let max_x = (ax.max(bx).max(cx).ceil() as usize).min(w - 1);
            let max_y = (ay.max(by).max(cy).ceil() as usize).min(h - 1);
            for py in min_y..=max_y {
                for px in min_x..=max_x {
                    let (sx, sy) = (px as f32 + 0.5, py as f32 + 0.5);
                    let wa = ((bx - sx) * (cy - sy) - (by - sy) * (cx - sx)) / area2;
                    let wb = ((cx - sx) * (ay - sy) - (cy - sy) * (ax - sx)) / area2;
                    let wc = 1.0 - wa - wb;
                    if wa >= -1e-6 && wb >= -1e-6 && wc >= -1e-6 {
                        let (wa, wb, wc) = (wa.max(0.0), wb.max(0.0), wc.max(0.0));
                        // Color32 为预乘 alpha：rgb 通道直接累积
                        let chans =
                            |v: &egui::epaint::Vertex| [v.color.r(), v.color.g(), v.color.b()];
                        let (ca, cb, cc) = (chans(a), chans(b), chans(c));
                        let i = py * w + px;
                        acc_a[i] += wa * f32::from(a.color.a())
                            + wb * f32::from(b.color.a())
                            + wc * f32::from(c.color.a());
                        for (k, slot) in acc_c[i].iter_mut().enumerate() {
                            *slot += wa * f32::from(ca[k])
                                + wb * f32::from(cb[k])
                                + wc * f32::from(cc[k]);
                        }
                    }
                }
            }
        }
    }
    let mut img = image::RgbaImage::new(w as u32, h as u32);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let i = y as usize * w + x as usize;
        let a = (acc_a[i] / 255.0).min(1.0);
        // 累积色去预乘还原原色（预乘 rgb = 原色×alpha/255），再 src-over
        let mut px = [40u8; 3];
        if acc_a[i] > 1e-3 {
            for (k, ch) in px.iter_mut().enumerate() {
                let c = acc_c[i][k] * 255.0 / acc_a[i];
                *ch = (c * a + 40.0 * (1.0 - a)).round() as u8;
            }
        }
        *p = image::Rgba([px[0], px[1], px[2], 255]);
    }
    img.save(path).unwrap();
    (w, h)
}
