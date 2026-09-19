# AGENTS.md

Rust workspace：跨平台截图标注工具，产物为单命令 `lscreen`。设计文档与里程碑（M1–M5）见 `docs/PLAN.md`，为架构事实的最终依据。

## 常用命令

```bash
cargo build --release          # 产物 target/release/lscreen
cargo test --workspace         # 纯单元测试，无需显示器/外部服务
cargo test -p lscreen-core     # 单 crate 测试
cargo test -p lscreen-core qr  # 按名过滤单个测试
cargo check -p lscreen-app     # 快速单 crate 校验
cargo fmt --all                # 格式化（CI 强制 --check）
cargo clippy --workspace --all-targets -- -D warnings
```

仓库无 rustfmt.toml / clippy.toml，遵循 rustfmt 默认风格；CI（`.github/workflows/ci.yml`）包含 fmt/clippy 门槛 + 构建/测试/体积回归；发布打包见 `.github/workflows/release.yml` 与 `scripts/package.sh`（推 v* tag 出全平台包）。

## Crate 边界（硬约束）

- `crates/core`（lscreen-core）：图元模型、撤销栈、命中检测、tiny-skia 导出渲染、取色、二维码。**禁止依赖任何 UI/GUI crate**；CLI 与 egui 覆盖层都只是其上的薄壳。
- `crates/capture`（lscreen-capture）：Linux 走自研 x11rb（X11，纯 Rust）；Win/mac 走 xcap。平台差异必须收敛在此 crate，不得泄漏到 app/core。
- `crates/app`（lscreen）：clap CLI 入口 + egui 覆盖层（`src/ui/`）+ 托盘常驻（`src/tray.rs`）+ 配置（`src/config.rs`）+ 配置面板（`src/settings_ui.rs`）。**裸 `lscreen` = 静默驻留后台托盘**，`lscreen gui` 才直达截图。
- `crates/ocr`（lscreen-ocr）：Windows `Windows.Media.Ocr` / macOS Vision / Linux 子进程调用系统 tesseract（stdin/stdout 管道，TSV 解析），内置 ocrs 纯 Rust 引擎作零依赖兜底。**OCR 是「无动态库依赖」目标的子进程豁免项**：可以 spawn 系统工具（tesseract），但不得引入链接型依赖。
- `crates/record`（lscreen-record）：录屏编码（GIF 走 gifski）。帧源以闭包注入，不依赖截屏实现；任何失败路径都要收尾编码线程并清理半成品文件。**录屏音频（M14）**在 `src/audio/{mod,linux,win,mac}.rs`：共享层（AlignMixer 零点对齐/补静音/双源饱和混合、ToStereo48 f32→s16le/48k 转换、Core 收尾对账）三平台同一 `Pipeline` 接口；Linux 子进程全链路（arecord/parec 采集 → ffmpeg AAC），Win WASAPI+Media Foundation，mac CoreAudio 麦克风 + ScreenCaptureKit 系统声（macOS 13+，v0.11 盲写）+ AudioToolbox AAC。armed 预热 + 录制零点对齐（晚到补静音）；改对齐/混写逻辑时注意 e2e 测试要覆盖起流延迟（parec 监视源 ~1.9s，测试窗口 ≥3.5s）。Win/mac 平台代码原为盲写（Win ✅ 2026-09-17、mac ✅ 2026-09-19 真机点验，各修盲写缺陷见 PLAN M14 节；mac 首跑 TCC 授权与 `LSCREEN_AUDIO_READY_TIMEOUT_MS` 见 docs/VERIFY.md），改动签名前先对照 vendored 绑定源码；本机交叉检查 mac 目标时 openh264 的 C++ 需用假 cc/ar shim 骗过（check 不链接，darwin 编译调用以 `-arch`/`-mmacosx-version-min` 识别后 touch 空产物，宿主调用透传真工具链）。本机也可交叉检查 Linux/Win 目标：rustup（`~/.cargo/bin/cargo`，与 Homebrew 工具链共存互不影响）+ `x86_64-unknown-linux-gnu`/`x86_64-pc-windows-msvc` target；shim **只经 `CC/CXX/AR_<target>` 环境变量生效**（塞进全局 PATH 会把宿主侧 build script 也链接成空文件 → cargo 跑不起来）；win 目标另需 PATH 上有假 `llvm-rc`（winresource 图标嵌入，产空 .res），该目录只放 llvm-rc 一个假件，避免遮蔽宿主 cc。
- `crates/setup`（lscreen-setup）：仅 Windows 的自绘安装器（egui）。主程序经构建期 `LSCREEN_BIN` 环境变量内嵌（build.rs 指纹触发重编译）；未内嵌时为占位可在任意平台编译。per-user 安装（%LOCALAPPDATA% + HKCU），不要引入需要管理员的路径。

## 关键设计决策（改动前必读）

- **双渲染路径**：交互期 `egui::Painter` 绘制，导出用 core 内 tiny-skia 软渲染，两者共享同一份图元数据且必须像素级一致（马赛克=网格色块、橡皮擦=原图回贴）。改图元几何时两条路径都要同步改。
- **撤销/重做是全量快照**（`Vec<Vec<Element>>`），非命令模式；图片本体不进快照。
- **体积硬约束**：发布产物 ≤ 20MB、单文件、无动态库依赖。workspace release profile 已做 `opt-level="z"` + fat LTO + strip + `panic="abort"`（无 unwind，勿依赖 catch_unwind）。新增依赖前先评估体积/链接影响。
- 字体运行时从系统加载（`app/src/font.rs`，fc-match/平台字体目录），不捆绑字体文件。
- egui/eframe 锁 0.35，勿随意升级（上游 API 变动快：0.35 已无 TopBottomPanel，面板用 Panel/手动分域；eframe::App 的入口方法是 `fn ui(&mut self, ui, frame)`）。
- **托盘（M8）**：Linux ksni（纯 Rust SNI/D-Bus 直连，blocking+async-io 特性）；Win/mac tray-icon + winit 事件泵（winit 版本必须与 eframe 依赖一致）。**zbus 钉 =5.18.0**（5.19 上游打包缺陷自身编译失败），勿升。托盘动作全部 spawn 独立子进程（注意子命令名不能省——裸启动会递归驻留托盘）。
- **配置（M8）**：`config.toml` 零配置不生成文件、缺失静默默认、损坏仅告警；托盘每秒轮询 mtime 热加载。默认热键 F1（Ctrl+Alt+A 与 Deepin 系统截图键冲突）。

## 环境注意事项

- 交互模式（`cargo run`）需要真实 X11 桌面（`DISPLAY`）；无头环境只能测 CLI 无界面模式（`lscreen shot` 等）和单元测试。
- 真机验证套件（M17）见 `docs/VERIFY.md`：env 门控的 ignored 测试（`LSCREEN_TEST_E2E=1` / `LSCREEN_TEST_AUDIO=1`），CI 不跑，真机手动执行。
- `.gitignore` 忽略所有 `*.png`（`docs/` 除外）：测试图请用代码生成（core 的 dev-dependency `qrcode` 即此用途），勿提交图片文件。
- 本仓库文档与提交信息使用中文，提交格式为中文 Conventional Commits（如 `feat: M2 取色器+二维码识别+CLI 子命令`）。
