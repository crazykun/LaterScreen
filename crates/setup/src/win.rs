//! Windows 安装后端：写文件、快捷方式（IShellLink COM）、HKCU 卸载注册、
//! 自删除。全部 per-user（%LOCALAPPDATA% + HKCU），不弹 UAC。

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IPersistFile,
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{FOLDERID_Desktop, IShellLinkW, SHGetKnownFolderPath, ShellLink};

/// CREATE_NO_WINDOW：spawn cmd 时不闪控制台黑框
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 共享冲突（目标文件被运行中的进程锁定）
const ERROR_SHARING_VIOLATION: i32 = 32;

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\LaterScreen";

pub struct InstallPlan {
    pub dir: PathBuf,
    pub desktop_shortcut: bool,
    pub start_menu: bool,
}

pub fn default_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(|p| PathBuf::from(p).join("Programs").join("LaterScreen"))
        .unwrap_or_else(|| PathBuf::from(r"C:\LaterScreen"))
}

/// 桌面目录：SHGetKnownFolderPath 处理 OneDrive 重定向；失败退回经典路径。
fn desktop_dir() -> PathBuf {
    unsafe {
        if let Ok(pwstr) = SHGetKnownFolderPath(&FOLDERID_Desktop, Default::default(), None) {
            let s = PCWSTR(pwstr.0).to_string().unwrap_or_default();
            CoTaskMemFree(Some(pwstr.0 as _));
            if !s.is_empty() {
                return PathBuf::from(s);
            }
        }
    }
    let home = std::env::var_os("USERPROFILE").unwrap_or_default();
    PathBuf::from(home).join("Desktop")
}

fn start_menu_dir() -> Result<PathBuf, String> {
    std::env::var_os("APPDATA")
        .map(|p| PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"))
        .ok_or_else(|| "缺少 APPDATA 环境变量".into())
}

/// 创建 .lnk（调用方需已 CoInitializeEx）。
fn create_shortcut(link: &Path, target: &Path, desc: &str) -> Result<(), String> {
    unsafe {
        let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("创建 ShellLink 失败: {e}"))?;
        sl.SetPath(&HSTRING::from(target.as_os_str()))
            .map_err(|e| format!("SetPath 失败: {e}"))?;
        sl.SetDescription(&HSTRING::from(desc))
            .map_err(|e| format!("SetDescription 失败: {e}"))?;
        if let Some(dir) = target.parent() {
            sl.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))
                .map_err(|e| format!("SetWorkingDirectory 失败: {e}"))?;
        }
        let pf: IPersistFile = sl
            .cast()
            .map_err(|e| format!("cast IPersistFile 失败: {e}"))?;
        pf.Save(&HSTRING::from(link.as_os_str()), true)
            .map_err(|e| format!("保存快捷方式失败: {e}"))?;
        Ok(())
    }
}

pub fn install(
    bin: &[u8],
    plan: &InstallPlan,
    version: &str,
    mut prog: impl FnMut(f32, &str),
) -> Result<(), String> {
    prog(0.05, "创建安装目录");
    std::fs::create_dir_all(&plan.dir).map_err(|e| format!("创建目录失败: {e}"))?;

    let exe = plan.dir.join("lscreen.exe");
    prog(0.25, "写入 lscreen.exe");
    std::fs::write(&exe, bin).map_err(|e| {
        if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) {
            "写入失败：LaterScreen 正在运行，请先退出后重试".to_string()
        } else {
            format!("写入 lscreen.exe 失败: {e}")
        }
    })?;

    prog(0.55, "写入卸载程序");
    let self_exe = std::env::current_exe().map_err(|e| format!("定位安装器自身失败: {e}"))?;
    let uninst = plan.dir.join("uninstall.exe");
    std::fs::copy(&self_exe, &uninst).map_err(|e| format!("写入 uninstall.exe 失败: {e}"))?;

    prog(0.72, "创建快捷方式");
    let co = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let r = (|| -> Result<(), String> {
        if plan.start_menu {
            let lnk = start_menu_dir()?.join("LaterScreen.lnk");
            create_shortcut(&lnk, &exe, "LaterScreen 截图标注")?;
        }
        if plan.desktop_shortcut {
            let lnk = desktop_dir().join("LaterScreen.lnk");
            create_shortcut(&lnk, &exe, "LaterScreen 截图标注")?;
        }
        Ok(())
    })();
    if co.is_ok() {
        unsafe { CoUninitialize() };
    }
    r?;

    prog(0.88, "注册卸载信息");
    let key = windows_registry::CURRENT_USER
        .create(UNINSTALL_KEY)
        .map_err(|e| format!("写注册表失败: {e}"))?;
    let set = |k: &str, v: &str| {
        key.set_string(k, v)
            .map_err(|e| format!("写注册表 {k} 失败: {e}"))
    };
    set("DisplayName", "LaterScreen")?;
    set("DisplayVersion", version)?;
    set("InstallLocation", &plan.dir.display().to_string())?;
    set("DisplayIcon", &format!("{},0", exe.display()))?;
    set("UninstallString", &format!("\"{}\"", uninst.display()))?;
    key.set_u32("EstimatedSize", (bin.len() as u32).div_ceil(1024))
        .map_err(|e| format!("写注册表 EstimatedSize 失败: {e}"))?;
    key.set_u32("NoModify", 1)
        .map_err(|e| format!("写注册表 NoModify 失败: {e}"))?;
    key.set_u32("NoRepair", 1)
        .map_err(|e| format!("写注册表 NoRepair 失败: {e}"))?;

    prog(1.0, "安装完成");
    Ok(())
}

/// 卸载：本函数运行于 uninstall.exe 自身，其所在目录即安装目录。
pub fn uninstall(mut prog: impl FnMut(f32, &str)) -> Result<(), String> {
    let self_exe = std::env::current_exe().map_err(|e| format!("定位自身失败: {e}"))?;
    let Some(dir) = self_exe.parent().map(Path::to_path_buf) else {
        return Err("无法确定安装目录".into());
    };

    prog(0.1, "删除快捷方式");
    let _ = std::fs::remove_file(desktop_dir().join("LaterScreen.lnk"));
    let _ = std::fs::remove_file(start_menu_dir()?.join("LaterScreen.lnk"));

    prog(0.3, "清理注册表");
    windows_registry::CURRENT_USER
        .remove_tree(UNINSTALL_KEY)
        .map_err(|e| format!("删除注册表键失败: {e}"))?;

    prog(0.55, "删除程序文件");
    let _ = std::fs::remove_file(dir.join("lscreen.exe"));

    // uninstall.exe 自身被锁不能直删：cmd 延迟 2 秒删除并顺带收掉目录
    // （若目录里留有用户文件，rmdir 失败无害，剩下的目录用户手动清理）
    prog(0.85, "移除安装目录");
    let script = format!(
        "/c timeout /t 2 /nobreak >nul & del /f /q \"{}\" & rmdir \"{}\"",
        self_exe.display(),
        dir.display()
    );
    Command::new("cmd.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .raw_arg(script)
        .spawn()
        .map_err(|e| format!("调度自删除失败: {e}"))?;

    prog(1.0, "卸载完成");
    Ok(())
}

/// 启动已安装的 LaterScreen（完成页「运行」按钮）。
pub fn launch(dir: &Path) {
    let _ = Command::new(dir.join("lscreen.exe")).spawn();
}
