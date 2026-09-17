//! M17 真机验证（Windows/macOS）：窗口枚举 Z 序与全屏截屏冒烟。
//! CI 无真实桌面会话，仅真机手动跑：
//! `LSCREEN_TEST_E2E=1 cargo test -p lscreen-capture --test windows_e2e -- --ignored`
//! 步骤与预期见 docs/VERIFY.md。

#![cfg(any(windows, target_os = "macos"))]

use lscreen_capture::{capture_primary, frontmost_window, list_windows, window_at};

#[ignore = "需要真实桌面会话（LSCREEN_TEST_E2E=1 显式开启）"]
#[test]
fn window_enum_and_primary_capture() {
    if std::env::var("LSCREEN_TEST_E2E").ok().as_deref() != Some("1") {
        return;
    }

    // 窗口枚举（M9）：真机桌面至少有 shell/终端等顶层窗口
    let wins = list_windows();
    assert!(!wins.is_empty(), "顶层窗口列表为空（Z 序枚举失败？）");
    // 列表按 Z 序自顶向下：z_order 单调不增
    let z: Vec<u32> = wins.iter().map(|w| w.z_order).collect();
    assert!(
        z.windows(2).all(|p| p[0] >= p[1]),
        "Z 序应自顶向下非增: {z:?}"
    );
    // 尺寸合理（非零、非巨型）
    for w in &wins {
        assert!(w.width > 0 && w.height > 0, "{} 尺寸异常", w.title);
        assert!(
            w.width <= 16384 && w.height <= 16384,
            "{} 尺寸越界",
            w.title
        );
    }

    // 最前窗口 + 点命中（M9 默认选区的数据基础）
    let f = frontmost_window().expect("应存在最前窗口");
    println!("最前窗口: {}", f.title);
    let (cx, cy) = (f.x + f.width as i32 / 2, f.y + f.height as i32 / 2);
    let hit = window_at(cx, cy).expect("窗口中心点应命中某窗口");
    assert!(hit.contains(cx, cy), "命中窗口不含查询点");

    // 全屏截屏冒烟（M4/M5 采集链路）：尺寸与缓冲一致、像素可读
    let shot = capture_primary().expect("全屏截屏失败");
    assert!(shot.width > 0 && shot.height > 0, "截屏尺寸异常");
    assert_eq!(
        shot.rgba.len(),
        (shot.width * shot.height * 4) as usize,
        "RGBA 缓冲与尺寸不符"
    );
    assert!(shot.pixel(shot.width / 2, shot.height / 2).is_some());
    println!(
        "截屏 {}x{}（scale={}）+ {} 个顶层窗口",
        shot.width,
        shot.height,
        shot.scale,
        wins.len()
    );
}
