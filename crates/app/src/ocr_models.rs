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

#[derive(Clone, Copy)]
enum JobKind {
    Check,
    Download,
}

#[derive(Default)]
enum State {
    #[default]
    Idle,
    Working {
        kind: JobKind,
        progress: Arc<Mutex<Progress>>,
        rx: mpsc::Receiver<Result<bool, String>>,
    },
    Done(Result<(), String>),
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

    pub fn present(&self) -> bool {
        self.present
    }

    pub fn progress(&self) -> Progress {
        self.progress
    }

    pub fn result(&self) -> Option<&Result<(), String>> {
        match &self.state {
            State::Done(result) => Some(result),
            _ => None,
        }
    }

    /// 一次本地探测，不联网。即使 helper 后续加入哈希校验也不阻塞 UI。
    pub fn check(&mut self, ctx: &egui::Context) {
        self.start_with(ctx, JobKind::Check, |_| {
            Ok(lscreen_ocr::paddle::models_present())
        });
    }

    /// 显式授权下载，不隐式保存/切换引擎；完整性由核心下载 helper 保证。
    pub fn start(&mut self, ctx: &egui::Context) {
        self.start_with(ctx, JobKind::Download, |progress| {
            lscreen_ocr::paddle::download_models(progress)
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
        job: impl FnOnce(&mut dyn FnMut(u64, u64)) -> Result<bool, String> + Send + 'static,
    ) {
        if self.in_flight() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(Mutex::new(Progress::default()));
        let worker_progress = progress.clone();
        self.progress = Progress::default();
        self.state = State::Working { kind, progress, rx };
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
            self.state = State::Done(Err(format!("无法启动模型工作线程：{e}")));
        }
        ctx.request_repaint();
    }

    /// 每帧只读取内存；等待时用低频心跳兜底异常退出（无结果就不会主动重绘）。
    pub fn poll(&mut self, ctx: &egui::Context) {
        let State::Working { progress, rx, .. } = &self.state else {
            return;
        };
        // 先收终态、再读进度：收到结果后 worker 不再写槽，确保最后一帧
        // 不会在「读旧进度 → worker 完成 → 收到结果」的竞态里停在旧数值。
        let outcome = rx.try_recv();
        if let Ok(latest) = progress.lock() {
            self.progress = *latest;
        }
        match outcome {
            Ok(Ok(present)) => {
                self.present = present;
                self.state = State::Done(Ok(()));
            }
            Ok(Err(e)) => {
                self.present = false;
                self.state = State::Done(Err(e));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.state = State::Done(Err("模型工作线程已退出，可重试".into()));
            }
            Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(POLL_INTERVAL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn wait_for_completion(models: &mut OcrModels, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while models.in_flight() {
            models.poll(ctx);
            assert!(std::time::Instant::now() < deadline, "模型任务未结束");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn initial_check_only_records_local_model_presence() {
        let ctx = egui::Context::default();
        let mut models = OcrModels::default();
        assert!(!models.in_flight());
        assert!(!models.present());
        assert!(models.result().is_none());

        models.start_with(&ctx, JobKind::Check, |_| Ok(true));
        assert!(models.checking());
        wait_for_completion(&mut models, &ctx);
        assert!(models.present());
        assert_eq!(models.result(), Some(&Ok(())));

        models.start_with(&ctx, JobKind::Check, |_| Ok(false));
        wait_for_completion(&mut models, &ctx);
        assert!(!models.present());
        assert_eq!(models.result(), Some(&Ok(())));
    }

    #[test]
    fn download_coalesces_aggregate_progress_and_finishes_ready() {
        let ctx = egui::Context::default();
        let mut models = OcrModels::default();
        let (progress_tx, progress_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        models.start_with(&ctx, JobKind::Download, move |progress| {
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
        assert_eq!(models.result(), Some(&Ok(())));
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
        models.start_with(&ctx, JobKind::Download, move |_| {
            finish_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            Ok(true)
        });
        let second_calls = Arc::new(AtomicUsize::new(0));
        let calls = second_calls.clone();
        models.start_with(&ctx, JobKind::Download, move |_| {
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
        models.start_with(&ctx, JobKind::Download, |progress| {
            progress(10, 30);
            Err("模拟下载失败".into())
        });
        wait_for_completion(&mut models, &ctx);
        assert!(!models.present());
        assert_eq!(models.result(), Some(&Err("模拟下载失败".into())));
        models.start_with(&ctx, JobKind::Download, |_| Ok(true));
        assert!(models.result().is_none());
        assert_eq!(models.progress(), Progress::default());
        wait_for_completion(&mut models, &ctx);
        assert!(models.present());
        assert_eq!(models.result(), Some(&Ok(())));
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
            },
            ..Default::default()
        };
        drop(tx);
        models.poll(&ctx);
        assert!(!models.in_flight());
        assert!(models
            .result()
            .unwrap()
            .as_ref()
            .unwrap_err()
            .contains("退出"));
        models.start_with(&ctx, JobKind::Download, |_| Ok(true));
        wait_for_completion(&mut models, &ctx);
        assert!(models.present());
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
