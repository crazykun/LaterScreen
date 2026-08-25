//! lscreen-setup：LaterScreen 的 Windows 自绘安装器（egui，替代 NSIS 向导）。
//!
//! 形态：构建期由 scripts/package.sh 以
//! `LSCREEN_BIN=<lscreen.exe 路径> cargo build -p lscreen-setup` 把主程序
//! 内嵌进本二进制（见 build.rs）；未内嵌时占位文件，UI 会明确提示。
//!
//! 安装模型：per-user——`%LOCALAPPDATA%\Programs\LaterScreen` + HKCU 卸载注册，
//! 不弹 UAC、无需管理员。卸载程序 = 本二进制副本（uninstall.exe），
//! 以 `--uninstall` 运行进入卸载流程。

// GUI 子系统：双击安装器不再带出控制台黑框
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
mod app;
#[cfg(target_os = "windows")]
mod win;

/// 构建期内嵌的主程序（LSCREEN_BIN，见 build.rs）。放 main.rs（非 cfg 门控）
/// 是为了占位/内嵌判定在任意平台可单测。
#[cfg(any(target_os = "windows", test))]
static BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/lscreen.bin"));
/// 引用 build.rs 导出的指纹：内嵌内容变化 → env 变 → crate 重编译
#[cfg(any(target_os = "windows", test))]
const _: () = {
    let _ = env!("LSCREEN_EMBED_STAMP");
};
/// 占位判定：真实主程序不可能这么小
#[cfg(any(target_os = "windows", test))]
fn embed_ok() -> bool {
    BIN.len() > 1024
}

fn main() {
    #[cfg(not(target_os = "windows"))]
    {
        eprintln!("lscreen-setup 仅支持 Windows（Linux/macOS 请用各平台包或 tar.gz/zip）");
        std::process::exit(1);
    }
    #[cfg(target_os = "windows")]
    {
        // panic = "abort" 下 panic 仍会先跑 hook。安装器是 GUI 子系统，
        // stderr 没有接收方——没有这个 hook，启动期 panic 的表现就是
        // 「双击后转一下圈，什么都没发生」，用户与开发者都无从下手。
        std::panic::set_hook(Box::new(|info| {
            win::error_dialog(&format!("内部错误：{info}"));
        }));
        let uninstall = std::env::args_os().any(|a| a == std::ffi::OsStr::new("--uninstall"));
        if let Err(e) = app::run(uninstall) {
            eprintln!("启动失败: {e}");
            win::error_dialog(&format!("启动失败：{e}"));
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LSCREEN_BIN 未设置（普通 cargo test）时验证占位路径；
    /// 打包脚本设置后由 package 流程验证真实内嵌（见 scripts/package.sh）。
    #[test]
    fn embed_placeholder_or_real() {
        if std::env::var_os("LSCREEN_BIN").is_some() {
            assert!(
                embed_ok(),
                "设置了 LSCREEN_BIN 但内嵌仍是占位文件（stamp 失效？）"
            );
        } else {
            assert!(!embed_ok());
        }
    }
}
