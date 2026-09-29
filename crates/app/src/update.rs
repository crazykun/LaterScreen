//! 更新检测（手动触发）：查 GitHub 最新 release tag，与当前版本比较，
//! 有新版本则提示并给出跳转 release 页的入口。
//!
//! ## 为什么是手动而不是自动
//!
//! 「小而美常驻」的托盘进程不持有任何网络状态，也不在启动/周期 tick 里
//! 联网——静默外连对用户是黑盒行为（隐私与离线可用性都不好）。放在配置
//! 面板里做成显式按钮：用户点了才发一次请求。
//!
//! ## 为什么用 ureq + rustls
//!
//! 硬约束是「单文件、无动态库依赖」。Linux 侧任何走系统 TLS 的方案都会
//! 拖进 OpenSSL（动态库），故用纯 Rust 的 rustls；`ureq` 是同步 API，
//! 比 async runtime 轻（无需 tokio/async-io 全家桶）。代价是几百 KB 体积，
//! 在 20MB 预算内（当前产物约 13MB）。
//!
//! ## 失败语义
//!
//! 无网络 / GitHub 不可达 / 响应不是预期结构，一律降级为「未能确认」，
//! **不**报成「已是最新」——那会骗过用户。所有失败都带可行动的文案。

use eframe::egui;
use serde::Deserialize;
use std::sync::{Arc, Mutex};

/// GitHub 仓库（owner/repo）；release 页由此拼出。
pub const REPO: &str = "crazykun/LaterScreen";

/// Release 页：用户点「去下载」时打开。
pub fn release_page_url() -> String {
    format!("https://github.com/{REPO}/releases/latest")
}

/// 最新 release 查询接口。只读公开信息，无需 token；未认证限额
/// 60 次/小时/IP，手动点几次远达不到。
const LATEST_API: &str = "https://api.github.com/repos/crazykun/LaterScreen/releases/latest";

/// 单次请求上限。手动触发也要有界：挂住的请求会让按钮一直转圈，
/// 而 DNS 黑洞/代理超时场景下这是常见形态。
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// 检查结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 已是最新（或本地版本比远端还新，如自行构建）
    UpToDate,
    /// 有新版本：远端 tag 名（如 "v0.12.0"）
    Newer(String),
}

/// 后台线程把结果放进来的槽；`poll` 取走。放 Checking 状态里，
/// 生命周期与一次检查绑定，不留全局可变状态。
type Slot = Arc<Mutex<Option<Result<Outcome, String>>>>;

/// 更新检查状态机。UI 侧持有一份，按需推进。
#[derive(Default)]
pub struct UpdateCheck {
    state: State,
}

#[derive(Default)]
enum State {
    #[default]
    Idle,
    /// 后台线程在跑（UI 显示「检查中…」，按钮禁用）
    Checking(Slot),
    Done(Result<Outcome, String>),
}

impl UpdateCheck {
    /// 是否正在检查（UI 用它决定按钮禁用/文案）
    pub fn in_flight(&self) -> bool {
        matches!(self.state, State::Checking(_))
    }

    /// 上一次结果（无则 None）
    pub fn result(&self) -> Option<&Result<Outcome, String>> {
        match &self.state {
            State::Done(r) => Some(r),
            _ => None,
        }
    }

    /// 发起一次检查。已经在跑则忽略（连点不会发多次请求）。
    ///
    /// 网络 IO 在独立线程上做，回来只往槽里写 + `request_repaint`——
    /// 绝不在 UI 线程里做阻塞 IO，否则整窗会卡住数秒。
    pub fn start(&mut self, ctx: &egui::Context) {
        if self.in_flight() {
            return;
        }
        let slot: Slot = Arc::new(Mutex::new(None));
        let thread_slot = Arc::clone(&slot);
        // egui::Context 是 Arc 包装的廉价克隆体，克隆后 'static 可安全跨线程
        let ui_ctx = ctx.clone();
        std::thread::spawn(move || {
            let outcome = query_latest();
            if let Ok(mut g) = thread_slot.lock() {
                *g = Some(outcome);
            }
            // 线程不是 UI：只能请求重绘，由下一帧的 poll 收结果
            ui_ctx.request_repaint();
        });
        self.state = State::Checking(slot);
        ctx.request_repaint();
    }

    /// 每帧调用：把后台线程的结果收进状态机。
    pub fn poll(&mut self) {
        let Some(slot) = (match &self.state {
            State::Checking(s) => Some(s),
            _ => None,
        }) else {
            return;
        };
        let taken = slot.lock().ok().and_then(|mut g| g.take());
        if let Some(outcome) = taken {
            self.state = State::Done(outcome);
        }
    }

    /// 清除结果（提示条关闭时）；检查中不打断。
    pub fn clear(&mut self) {
        if !self.in_flight() {
            self.state = State::Idle;
        }
    }
}

// ---------------------------------------------------------------- 网络

/// GitHub release 的 `tag_name`（只取这一个字段，其余忽略——
/// 字段缺失要能容错，GitHub 加字段不该让我们崩）
#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

/// 查最新 release 的 tag；失败返回可展示的中文原因。
fn query_latest() -> Result<Outcome, String> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .build(),
    );
    let mut resp = agent
        .get(LATEST_API)
        .header("Accept", "application/vnd.github+json")
        // GitHub API 要求 UA；不给会 403
        .header(
            "User-Agent",
            format!("LaterScreen/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|e| format!("请求 GitHub 失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub 返回 {}", resp.status()));
    }
    let release: Release = resp
        .body_mut()
        .read_json()
        .map_err(|e| format!("响应解析失败：{e}"))?;
    match compare_to_current(&release.tag_name) {
        Some(Outcome::Newer(_)) => Ok(Outcome::Newer(release.tag_name)),
        Some(Outcome::UpToDate) => Ok(Outcome::UpToDate),
        None => Err(format!("无法理解远端版本号「{}」", release.tag_name)),
    }
}

/// 远端 tag 与当前版本比较。无法解析返回 None（调用方转成错误文案）。
///
/// tag 形如 `v0.11.2`；也容忍不带 `v` 前缀。比较按「点分数字段逐段比」，
/// 不做 semver 完整语义（预发布/构建元数据用不上：本仓库只发正式 tag）。
/// 段数不同时按缺失补 0（`0.11` == `0.11.0`）。
pub fn compare_to_current(tag: &str) -> Option<Outcome> {
    let latest = parse_version(tag)?;
    let current = parse_version(env!("CARGO_PKG_VERSION"))?;
    // 段数不同先补 0 对齐，再逐段比：Vec 的默认字典序会把 [0,11,2,0]
    // 判成大于 [0,11,2]，那不是我们要的「0.11.2.0 == 0.11.2」
    let n = latest.len().max(current.len());
    let pad = |v: &Vec<u64>| -> Vec<u64> {
        let mut v = v.clone();
        while v.len() < n {
            v.push(0);
        }
        v
    };
    Some(if pad(&latest) > pad(&current) {
        Outcome::Newer(tag.trim().to_string())
    } else {
        Outcome::UpToDate
    })
}

/// "v0.11.2" / "0.11" → [0, 11, 2]
fn parse_version(s: &str) -> Option<Vec<u64>> {
    let s = s.trim().strip_prefix('v').unwrap_or(s.trim());
    if s.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for part in s.split('.') {
        // 只认纯数字段：`0.11.2-rc1` 这类预发布标识本仓库不发，
        // 真出现时宁可判「无法理解」也不猜
        let n: u64 = part.parse().ok()?;
        out.push(n);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parse() {
        assert_eq!(parse_version("v0.11.2"), Some(vec![0, 11, 2]));
        assert_eq!(parse_version("0.11.2"), Some(vec![0, 11, 2]));
        assert_eq!(parse_version("1.0"), Some(vec![1, 0]));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("v"), None);
        // 预发布后缀不猜
        assert_eq!(parse_version("0.11.2-rc1"), None);
    }

    #[test]
    fn compare_rules() {
        let cur = env!("CARGO_PKG_VERSION");
        // 远方更新（主/次/补丁任一更大都要报）
        assert_eq!(
            compare_to_current("v99.0.0"),
            Some(Outcome::Newer("v99.0.0".into()))
        );
        assert_eq!(
            compare_to_current("99.0.0"),
            Some(Outcome::Newer("99.0.0".into()))
        );
        // 当前版本自身 = 已是最新
        assert_eq!(compare_to_current(cur), Some(Outcome::UpToDate));
        assert_eq!(
            compare_to_current(&format!("v{cur}")),
            Some(Outcome::UpToDate)
        );
        // 更旧 → 已是最新（本地自构建常见于远端刚删 tag）
        assert_eq!(compare_to_current("v0.0.1"), Some(Outcome::UpToDate));
        // 段数不同按补 0 处理：0.11.2 与 0.11.2.0 相等
        assert_eq!(
            compare_to_current(&format!("{cur}.0")),
            Some(Outcome::UpToDate)
        );
        // 无法理解 → None（调用方转错误文案，绝不能说成「已是最新」）
        assert_eq!(compare_to_current("vnext"), None);
        assert_eq!(compare_to_current(""), None);
    }

    #[test]
    fn state_machine_ignores_double_start() {
        // 连点不重复发请求：in_flight 期间 start 无效
        let ctx = egui::Context::default();
        let mut chk = UpdateCheck::default();
        assert!(!chk.in_flight());
        assert!(chk.result().is_none());
        chk.start(&ctx);
        assert!(chk.in_flight());
        let first = matches!(chk.state, State::Checking(_));
        chk.start(&ctx);
        assert!(first && chk.in_flight(), "第二次 start 应被忽略");
        // 结果还没回来时 poll 不改变状态
        chk.poll();
        assert!(chk.in_flight() || chk.result().is_some());
        // clear 不打断进行中的检查
        chk.clear();
        assert!(chk.in_flight() || chk.result().is_some());
    }

    #[test]
    fn clear_resets_finished_result() {
        let mut chk = UpdateCheck {
            state: State::Done(Ok(Outcome::UpToDate)),
        };
        assert!(chk.result().is_some());
        chk.clear();
        assert!(chk.result().is_none());
        assert!(!chk.in_flight());
    }

    #[test]
    fn urls_point_to_the_repo() {
        assert_eq!(
            release_page_url(),
            "https://github.com/crazykun/LaterScreen/releases/latest"
        );
        assert!(LATEST_API.starts_with("https://api.github.com/repos/crazykun/"));
        assert_eq!(REPO, "crazykun/LaterScreen");
    }
}
