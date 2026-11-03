//! PP-OCR 本地模型状态（M19）：只在打开设置/手动刷新时检查，显式点击才下载。
//! IO 与完整性校验由 OCR crate 在后台线程完成；UI 每帧仅收消息/读进度槽，
//! 不哈希模型、不调用 curl，也不修改引擎配置。下载完成即可供 auto 使用。

use eframe::egui;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// 最多每 100ms 刷新一次下载进度；没有每 chunk 唤醒 UI 或无界消息积压。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// OCR crate 已汇总三个文件的字节数，这里只保留最新快照，不再逐文件累加。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

impl Progress {
    pub fn fraction(self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64).clamp(0.0, 1.0) as f32
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    /// 轻量检查：三个文件按固定字节数判定存在性，不联网不哈希
    Check,
    /// 下载缺失文件（已完好的文件跳过），完成后立即可供自动模式使用
    Download,
    /// 全量完整性校验：逐文件 SHA-256 + 字节数，只报通过/失败
    Verify,
}

impl JobKind {
    fn label(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Download => "download",
            Self::Verify => "verify",
        }
    }
}

/// 一次后台任务的终态；`poll` 在状态转入 Done 的那一帧返回一次。
/// `silent` = 打开窗口时的自动探测，结果只反映在卡片状态里，调用方
/// 不应为它弹 toast（每次进门都「检查完成」是噪音）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobFinished {
    pub kind: &'static str,
    pub ok: bool,
    pub present: bool,
    pub silent: bool,
}

#[derive(Default)]
enum State {
    #[default]
    Idle,
    Working {
        kind: JobKind,
        progress: Arc<Mutex<Progress>>,
        rx: mpsc::Receiver<Result<bool, String>>,
        silent: bool,
    },
    Done {
        kind: JobKind,
        outcome: Result<bool, String>,
    },
}

#[derive(Default)]
pub struct OcrModels {
    state: State,
    present: bool,
    progress: Progress,
}

impl OcrModels {
    pub fn in_flight(&self) -> bool {
        matches!(self.state, State::Working { .. })
    }

    pub fn checking(&self) -> bool {
        matches!(
            self.state,
            State::Working {
                kind: JobKind::Check,
                ..
            }
        )
    }

    /// 进行中任务的来源标签（"check"/"download"/"verify"）；空闲为 None。
    pub fn active_kind(&self) -> Option<&'static str> {
        match &self.state {
            State::Working { kind, .. } => Some(kind.label()),
            _ => None,
        }
    }

    pub fn present(&self) -> bool {
        self.present
    }

    pub fn progress(&self) -> Progress {
        self.progress
    }

    /// 最近一次任务的来源与结果；`None` = 打开窗口后还没有跑过任何任务。
    pub fn result(&self) -> Option<(&'static str, &Result<bool, String>)> {
        match &self.state {
            State::Done { kind, outcome } => Some((kind.label(), outcome)),
            _ => None,
        }
    }

    /// 一次本地探测，不联网。即使 helper 后续加入哈希校验也不阻塞 UI。
    pub fn check(&mut self, ctx: &egui::Context) {
        self.start_with(ctx, JobKind::Check, false, |_| {
            Ok(lscreen_ocr::paddle::models_present())
        });
    }

    /// 打开设置窗口时的自动探测：结果只落在卡片状态里，完成不弹 toast。
    pub fn check_on_open(&mut self, ctx: &egui::Context) {
        self.start_with(ctx, JobKind::Check, true, |_| {
            Ok(lscreen_ocr::paddle::models_present())
        });
    }

    /// 显式授权下载，不隐式保存/切换引擎；完整性由核心下载 helper 保证。
    /// 已完好的文件会在下载器内跳过，因此这也是「修复」路径（重下校验
    /// 不通过的文件）。
    pub fn start(&mut self, ctx: &egui::Context) {
        self.start_with(ctx, JobKind::Download, false, |progress| {
            lscreen_ocr::paddle::download_models(progress)
                .map(|()| true)
                .map_err(|e| e.to_string())
        });
    }

    /// 全量 SHA-256 完整性校验，不联网；给出明确的通过/失败结论。
    pub fn verify(&mut self, ctx: &egui::Context) {
        self.start_with(ctx, JobKind::Verify, false, |_| {
            lscreen_ocr::paddle::verify_installed_models()
                .map(|()| true)
                .map_err(|e| e.to_string())
        });
    }

    /// 注入后台工作便于离线单测。终态用 channel 传递：线程退出但没结果时，
    /// Disconnected 也会结束 busy；不依赖 panic=abort 下无效的 catch_unwind。
    fn start_with(
        &mut self,
        ctx: &egui::Context,
        kind: JobKind,
        silent: bool,
        job: impl FnOnce(&mut dyn FnMut(u64, u64)) -> Result<bool, String> + Send + 'static,
    ) {
        if self.in_flight() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(Mutex::new(Progress::default()));
        let worker_progress = progress.clone();
        self.progress = Progress::default();
        self.state = State::Working {
            kind,
            progress,
            rx,
            silent,
        };
        let ui_ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("ocr-models".into())
            .spawn(move || {
                let result = job(&mut |done, total| {
                    if let Ok(mut latest) = worker_progress.lock() {
                        *latest = Progress { done, total };
                    }
                });
                let _ = tx.send(result);
                ui_ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.state = State::Done {
                kind,
                outcome: Err(format!("无法启动模型工作线程：{e}")),
            };
        }
        ctx.request_repaint();
    }

    /// 每帧只读取内存；等待时用低频心跳兜底异常退出（无结果就不会主动重绘）。
    /// 状态转入 Done 的那一帧返回一次终态，调用方用它给 toast 反馈。
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<JobFinished> {
        let State::Working {
            kind,
            progress,
            rx,
            silent,
        } = &self.state
        else {
            return None;
        };
        let kind = *kind;
        let silent = *silent;
        // 先收终态、再读进度：收到结果后 worker 不再写槽，确保最后一帧
        // 不会在「读旧进度 → worker 完成 → 收到结果」的竞态里停在旧数值。
        let outcome = rx.try_recv();
        if let Ok(latest) = progress.lock() {
            self.progress = *latest;
        }
        match outcome {
            Ok(Ok(present)) => {
                self.present = present;
                self.state = State::Done {
                    kind,
                    outcome: Ok(present),
                };
                Some(JobFinished {
                    kind: kind.label(),
                    ok: true,
                    present,
                    silent,
                })
            }
            Ok(Err(e)) => {
                self.present = false;
                self.state = State::Done {
                    kind,
                    outcome: Err(e),
                };
                Some(JobFinished {
                    kind: kind.label(),
                    ok: false,
                    present: false,
                    silent,
                })
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.present = false;
                self.state = State::Done {
                    kind,
                    outcome: Err("模型工作线程已退出，可重试".into()),
                };
                Some(JobFinished {
                    kind: kind.label(),
                    ok: false,
                    present: false,
                    silent,
                })
            }
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(POLL_INTERVAL);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn wait_for_completion(models: &mut OcrModels, ctx: &egui::Context) -> Option<JobFinished> {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut finished = None;
        while models.in_flight() {
            if let Some(done) = models.poll(ctx) {
                finished = Some(done);
            }
            assert!(std::time::Instant::now() < deadline, "模型任务未结束");
            std::thread::sleep(Duration::from_millis(1));
        }
        finished
    }

    #[test]
    fn initial_check_only_records_local_model_presence() {
        let ctx = egui::Context::default();
        let mut models = OcrModels::default();
        assert!(!models.in_flight());
        assert!(!models.present());
        assert!(models.result().is_none());

        models.start_with(&ctx, JobKind::Check, false, |_| Ok(true));
        assert!(models.checking());
        wait_for_completion(&mut models, &ctx);
        assert!(models.present());
        assert_eq!(
            models.result(),
            Some(("check", &Ok(true))),
            "检查来源与结果要一起可见"
        );

        models.start_with(&ctx, JobKind::Check, false, |_| Ok(false));
        wait_for_completion(&mut models, &ctx);
        assert!(!models.present());
        assert_eq!(models.result(), Some(("check", &Ok(false))));
    }

    #[test]
    fn download_coalesces_aggregate_progress_and_finishes_ready() {
        let ctx = egui::Context::default();
        let mut models = OcrModels::default();
        let (progress_tx, progress_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        models.start_with(&ctx, JobKind::Download, false, move |progress| {
            for done in 1..=1_000 {
                progress(done, 3_000);
            }
            progress_tx.send(()).unwrap();
            finish_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            progress(3_000, 3_000);
            Ok(true)
        });
        progress_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        models.poll(&ctx);
        assert!(models.in_flight());
        assert!(!models.checking());
        assert_eq!(
            models.progress(),
            Progress {
                done: 1_000,
                total: 3_000
            }
        );
        assert!(models.result().is_none());
        assert!(!models.present());

        finish_tx.send(()).unwrap();
        wait_for_completion(&mut models, &ctx);
        assert!(models.present());
        assert_eq!(models.result(), Some(("download", &Ok(true))));
        assert_eq!(
            models.progress(),
            Progress {
                done: 3_000,
                total: 3_000
            }
        );
    }

    #[test]
    fn duplicate_start_does_not_launch_another_worker() {
        let ctx = egui::Context::default();
        let mut models = OcrModels::default();
        let (finish_tx, finish_rx) = mpsc::channel();
        models.start_with(&ctx, JobKind::Download, false, move |_| {
            finish_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            Ok(true)
        });
        let second_calls = Arc::new(AtomicUsize::new(0));
        let calls = second_calls.clone();
        models.start_with(&ctx, JobKind::Download, false, move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        });
        finish_tx.send(()).unwrap();
        wait_for_completion(&mut models, &ctx);
        assert!(models.present());
        assert_eq!(second_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failed_download_can_be_retried_without_stale_error_or_progress() {
        let ctx = egui::Context::default();
        let mut models = OcrModels::default();
        models.start_with(&ctx, JobKind::Download, false, |progress| {
            progress(10, 30);
            Err("模拟下载失败".into())
        });
        wait_for_completion(&mut models, &ctx);
        assert!(!models.present());
        assert_eq!(
            models.result(),
            Some(("download", &Err("模拟下载失败".into())))
        );
        models.start_with(&ctx, JobKind::Download, false, |_| Ok(true));
        assert!(models.result().is_none());
        assert_eq!(models.progress(), Progress::default());
        wait_for_completion(&mut models, &ctx);
        assert!(models.present());
        assert_eq!(models.result(), Some(("download", &Ok(true))));
    }

    #[test]
    fn verify_reports_explicit_result_once() {
        let ctx = egui::Context::default();
        let mut models = OcrModels::default();
        models.start_with(&ctx, JobKind::Verify, false, |_| Ok(true));
        assert!(!models.checking());
        // 终态只在转入 Done 的那一帧返回一次，之后稳定可查
        let finished = wait_for_completion(&mut models, &ctx);
        assert_eq!(
            finished,
            Some(JobFinished {
                kind: "verify",
                ok: true,
                present: true,
                silent: false
            })
        );
        assert_eq!(models.poll(&ctx), None, "完成事件不能重复发");
        assert!(models.present());
        assert_eq!(models.result(), Some(("verify", &Ok(true))));

        // 校验失败：present 置否，来源仍是 verify，便于 UI 给「修复」引导
        models.start_with(&ctx, JobKind::Verify, false, |_| {
            Err("SHA-256 校验失败".into())
        });
        let finished = wait_for_completion(&mut models, &ctx);
        assert_eq!(
            finished,
            Some(JobFinished {
                kind: "verify",
                ok: false,
                present: false,
                silent: false
            })
        );
        assert!(!models.present());
        assert_eq!(
            models.result(),
            Some(("verify", &Err("SHA-256 校验失败".into())))
        );
    }

    #[test]
    fn disconnected_worker_becomes_retryable_error() {
        let ctx = egui::Context::default();
        let (tx, rx) = mpsc::channel();
        let mut models = OcrModels {
            state: State::Working {
                kind: JobKind::Download,
                progress: Arc::new(Mutex::new(Progress::default())),
                rx,
                silent: false,
            },
            ..Default::default()
        };
        drop(tx);
        models.poll(&ctx);
        assert!(!models.in_flight());
        assert!(models
            .result()
            .unwrap()
            .1
            .as_ref()
            .unwrap_err()
            .contains("退出"));
        models.start_with(&ctx, JobKind::Download, false, |_| Ok(true));
        wait_for_completion(&mut models, &ctx);
        assert!(models.present());
    }

    #[test]
    fn open_time_probe_is_silent_button_check_is_not() {
        let ctx = egui::Context::default();
        let mut models = OcrModels::default();
        models.check_on_open(&ctx);
        let done = wait_for_completion(&mut models, &ctx).expect("应有终态");
        // 只断言与机器无关的字段：present 取决于本机是否装过模型（CI 上
        // 必然没有），断死 true 会在干净机器上翻车（v0.14.1 CI 实测）
        assert_eq!(done.kind, "check");
        assert!(done.ok);
        assert!(
            done.silent,
            "打开窗口的自动探测必须带 silent 标记（进门不弹 toast）"
        );
        models.check(&ctx);
        let done = wait_for_completion(&mut models, &ctx).expect("应有终态");
        assert_eq!(done.kind, "check");
        assert!(done.ok);
        assert!(!done.silent, "手动「检查」按钮要有完成反馈");
    }

    #[test]
    fn progress_fraction_handles_unknown_total_and_clamps() {
        assert_eq!(Progress::default().fraction(), 0.0);
        assert_eq!(Progress { done: 5, total: 10 }.fraction(), 0.5);
        assert_eq!(
            Progress {
                done: 12,
                total: 10
            }
            .fraction(),
            1.0
        );
    }
}
