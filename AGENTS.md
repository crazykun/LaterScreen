# AGENTS.md

Rust workspace：跨平台截图标注工具，产物为单命令 `lscreen`。设计文档与里程碑（M1–M5）见 `doc/PLAN.md`，为架构事实的最终依据。

## 常用命令

```bash
cargo build --release          # 产物 target/release/lscreen
cargo test --workspace         # 纯单元测试，无需显示器/外部服务
cargo test -p lscreen-core     # 单 crate 测试
cargo test -p lscreen-core qr  # 按名过滤单个测试
cargo check -p lscreen-app     # 快速单 crate 校验
```

仓库无 rustfmt.toml / clippy.toml，遵循 rustfmt 默认风格。CI 见 `.github/workflows/ci.yml`（构建/测试/体积回归）；发布打包见 `.github/workflows/release.yml` 与 `scripts/package.sh`（推 v* tag 出全平台包）。

## Crate 边界（硬约束）

- `crates/core`（lscreen-core）：图元模型、撤销栈、命中检测、tiny-skia 导出渲染、取色、二维码。**禁止依赖任何 UI/GUI crate**；CLI 与 egui 覆盖层都只是其上的薄壳。
- `crates/capture`（lscreen-capture）：Linux 走自研 x11rb（X11，纯 Rust）；Win/mac 走 xcap。平台差异必须收敛在此 crate，不得泄漏到 app/core。
- `crates/app`（lscreen）：clap CLI 入口 + egui 覆盖层（`src/ui/`）。
- `crates/ocr`（lscreen-ocr）：Linux 通过子进程调用系统 tesseract（stdin/stdout 管道，TSV 解析）；未装时明确引导。**OCR 是「无动态库依赖」目标的唯一豁免项**，不得引入链接型依赖。

## 关键设计决策（改动前必读）

- **双渲染路径**：交互期 `egui::Painter` 绘制，导出用 core 内 tiny-skia 软渲染，两者共享同一份图元数据且必须像素级一致（马赛克=网格色块、橡皮擦=原图回贴）。改图元几何时两条路径都要同步改。
- **撤销/重做是全量快照**（`Vec<Vec<Element>>`），非命令模式；图片本体不进快照。
- **体积硬约束**：发布产物 ≤ 10MB、单文件、无动态库依赖。workspace release profile 已做 `opt-level="z"` + fat LTO + strip + `panic="abort"`（无 unwind，勿依赖 catch_unwind）。新增依赖前先评估体积/链接影响。
- 字体运行时从系统加载（`app/src/font.rs`，fc-match/平台字体目录），不捆绑字体文件。
- egui/eframe 锁 0.35，勿随意升级（上游 API 变动快）。

## 环境注意事项

- 交互模式（`cargo run`）需要真实 X11 桌面（`DISPLAY`）；无头环境只能测 CLI 无界面模式（`lscreen shot` 等）和单元测试。
- `.gitignore` 忽略所有 `*.png`（`doc/` 除外）：测试图请用代码生成（core 的 dev-dependency `qrcode` 即此用途），勿提交图片文件。
- 本仓库文档与提交信息使用中文，提交格式为中文 Conventional Commits（如 `feat: M2 取色器+二维码识别+CLI 子命令`）。
