//! 可插拔上传 hook（M15）：外部命令式上传，零体积方案。
//!
//! 不内置任何图床 SDK——ShareX 式上传生态与「离线单文件小而美」冲突。
//! 用户在 `config.toml` 配 `[upload] command = ["sup", "up"]`（argv 数组，
//! **不经 shell**，杜绝注入），本模块把产物**路径**（非内容）经 stdin
//! 交给命令，stdout 的第一个非空行作为 URL 返回。uPic/PicGo/sup 或自写
//! 脚本均可对接；缺省不配置 = 无上传能力，UI 不显示上传入口。
//!
//! 安全与健壮性：
//! - 路径只经 stdin 不进 argv：文件名含空格/特殊字符不构成注入面；
//!   命令本身只来自用户自己的配置文件。
//! - 默认 30s 超时 kill：外置脚本可能挂死，调用方（覆盖层）不能陪等；
//! - stdin 写完立即关闭（EOF）：`cat` 型脚本靠 EOF 才会退出；
//! - stdout/stderr 由独立线程排空：串行 read_to_string 在子进程先填满
//!   另一条管道（64KB 缓冲）时会互相卡死（经典 pipe deadlock）。

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 默认超时。上传大文件到慢速图床可能要十几秒，30s 覆盖正常场景，
/// 挂死脚本及时止损。
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// 跑上传命令（默认 30s 超时）。成功返回 stdout 第一个非空行（URL 约定）；
/// 失败信息直接面向用户展示（含退出码与 stderr 首行）。
pub fn run(cmd: &[String], path: &Path) -> Result<String, String> {
    run_timeout(cmd, path, TIMEOUT)
}

fn run_timeout(cmd: &[String], path: &Path, timeout: Duration) -> Result<String, String> {
    let (prog, args) = cmd
        .split_first()
        .ok_or_else(|| "上传命令为空".to_string())?;
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动上传命令失败（{prog}）: {e}"))?;

    // 写路径后立即 drop 关闭 stdin。路径长度远小于管道缓冲（64KB），
    // write_all 不会阻塞；子进程不读 stdin 就退出时这里可能拿到 EPIPE，
    // 无需当作错误——退出状态自会说明一切。
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(path.to_string_lossy().as_bytes());
        let _ = stdin.write_all(b"\n");
    }

    // 并行排空两条管道，避免子进程写满其一后卡在 write 上
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    loop {
        match child
            .try_wait()
            .map_err(|e| format!("等待上传命令失败: {e}"))?
        {
            Some(status) => {
                let stdout = out.join().unwrap_or_default();
                let stderr = err.join().unwrap_or_default();
                if !status.success() {
                    let detail = first_line(&stderr)
                        .map(|s| format!("：{s}"))
                        .unwrap_or_default();
                    return Err(format!(
                        "上传命令失败（退出码 {}）{detail}",
                        status.code().unwrap_or(-1)
                    ));
                }
                return first_line(&stdout)
                    .ok_or_else(|| "上传命令未返回 URL（stdout 第一个非空行）".to_string());
            }
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait(); // 回收，也让排空线程拿到 EOF 返回
                    return Err(format!("上传超时（{}s）", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// 后台排空一条管道到内存。子进程退出/被 kill 后管道写端关闭，线程自然返回。
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut r) = pipe {
            let _ = r.read_to_string(&mut buf);
        }
        buf
    })
}

/// stdout 约定：第一个非空行 = URL。错误信息取 stderr 首行同理。
/// 输出截断到 200 字符，脚本刷屏时不至于把 toast 撑爆。
fn first_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_family = "unix")]
    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    #[test]
    fn first_line_variants() {
        assert_eq!(
            first_line("\n  \nhttps://a/x.png\n"),
            Some("https://a/x.png".into())
        );
        assert_eq!(first_line("ok\nhttps://a/x.png\n"), Some("ok".into()));
        assert_eq!(first_line(""), None);
        assert_eq!(first_line(" \n \n"), None);
        // 超长输出截断
        let long = "x".repeat(500);
        assert_eq!(first_line(&long).unwrap().len(), 200);
    }

    /// 成功路径：脚本读入路径（cat 到 EOF）后回显 URL
    #[cfg(target_family = "unix")]
    #[test]
    fn success_returns_url() {
        let url = run(
            &sh("p=$(cat); echo \"https://cdn.example.com/$p\""),
            Path::new("/tmp/shot.png"),
        )
        .unwrap();
        assert_eq!(url, "https://cdn.example.com//tmp/shot.png");
    }

    /// 空行/前导空白跳过，取第一个非空行
    #[cfg(target_family = "unix")]
    #[test]
    fn skips_blank_lines() {
        let url = run(
            &sh("cat >/dev/null; echo; echo '  https://a  '"),
            Path::new("x"),
        )
        .unwrap();
        assert_eq!(url, "https://a");
    }

    /// 非零退出：报退出码 + stderr 首行
    #[cfg(target_family = "unix")]
    #[test]
    fn nonzero_exit_reports_stderr() {
        let e = run(&sh("cat >/dev/null; echo boom >&2; exit 3"), Path::new("x")).unwrap_err();
        assert!(e.contains("3"), "{e}");
        assert!(e.contains("boom"), "{e}");
    }

    /// 成功退出但没输出 URL
    #[cfg(target_family = "unix")]
    #[test]
    fn success_without_url_is_error() {
        let e = run(&sh("cat >/dev/null"), Path::new("x")).unwrap_err();
        assert!(e.contains("未返回 URL"), "{e}");
    }

    /// 挂死脚本被超时 kill（用 300ms 短超时，不真等 30s）
    #[cfg(target_family = "unix")]
    #[test]
    fn hung_script_times_out() {
        let e = run_timeout(
            &sh("cat >/dev/null; sleep 5"),
            Path::new("x"),
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(e.contains("超时"), "{e}");
    }

    /// 命令不存在
    #[test]
    fn missing_program_is_error() {
        let e = run(
            &["lscreen-no-such-upload-tool-xyz".to_string()],
            Path::new("x"),
        )
        .unwrap_err();
        assert!(e.contains("启动上传命令失败"), "{e}");
    }

    /// 空命令
    #[test]
    fn empty_command_is_error() {
        assert!(run(&[], Path::new("x")).is_err());
    }
}
