# LaterScreen

[![CI](https://github.com/crazykun/LaterScreen/actions/workflows/ci.yml/badge.svg)](https://github.com/crazykun/LaterScreen/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/crazykun/LaterScreen)](https://github.com/crazykun/LaterScreen/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

跨平台截图标注工具，Rust 编写，命令名 `lscreen`：截图、标注、取色、二维码、OCR、GIF/MP4 录屏、贴图、滚动截图。

- **单文件 ≤ 20MB**，无动态库依赖，拷走即用
- **三平台**：Linux（x64 / arm64 / armv7 / x86）、Windows 10+、macOS
- **GUI + CLI 双形态**：完整交互标注界面，每个功能也都能纯命令行调用

![主界面：框选 → 标注工具栏](docs/img/image.png)

## 快速上手

```bash
lscreen        # 托盘常驻，之后按 F1 随时截图
lscreen gui    # 不常驻，直接进入交互截图（框选 → 标注 → 复制/保存）
```

交互标注含 8 种工具（矩形 / 椭圆 / 箭头 / 画笔 / 自增标号 / 文本 / 马赛克 / 橡皮擦），元素可再编辑（拖拽 / 控制点 / 双击改文本），支持撤销重做、贴图（Ctrl+P）、二维码 / OCR 识别、复制（Ctrl+C）与保存（Ctrl+S）。

托盘常驻时菜单含截图 / 取色 / 贴图 / 录屏 / 滚动截图 / 历史 / 配置，六个动作可各自绑定全局热键（默认只有 F1 截图）。其中**「历史」是一个缩略图浮窗**，列出最近的截图 / 贴图 / 录屏：点击缩略图复制，录屏则用默认播放器播放；右键可贴图 / 打开目录 / 删除；顶栏显示条数与占用体积，可一键清空。同一时刻只有一个面板，面板在后台时再按热键会把它唤到前台。

其余功能均可无界面调用，完整选项以 `lscreen <子命令> --help` 为准：

```bash
lscreen shot -o out.png                  # 无界面截图（--region X,Y,W,H 指定区域）
lscreen record --select --fps 10         # 框选录制 GIF（--mp4 录制 MP4/H.264，Linux）
lscreen scroll                           # 滚动长截图（Linux X11）
lscreen ocr --region 0,0,800,600         # OCR 识别（-i 指定图片，--lang 选语言）
lscreen qr -i photo.png                  # 识别图片中的二维码
lscreen pick                             # 屏幕取色器
lscreen pin -i img.png                   # 把图片钉在屏幕上
lscreen history                          # 历史面板（最近截图 / 贴图 / 录屏）
lscreen config                           # 配置面板
```

> `--region X,Y,W,H` 均为**物理像素**坐标，多显示器时基于虚拟桌面原点。

## 安装

**macOS（Homebrew）**：

```bash
brew install --cask crazykun/ailater/lscreen
```

或从 [Releases](https://github.com/crazykun/LaterScreen/releases) 下载：

- **Linux**：Debian 系 `*.deb`（`sudo apt install ./lscreen_*.deb`）、Fedora 系 `*.rpm`，或通用的 `*.AppImage` / `*.tar.gz`
- **Windows 10+**：`*-setup.exe`（自绘安装器，per-user 免 UAC），或免安装 `*.zip`
- **macOS**：`*.dmg` 拖入 Applications；未签名，首次右键 → 打开，或 `xattr -d com.apple.quarantine /Applications/LaterScreen.app`

或源码安装（需 Rust 工具链 + C/C++ 编译器，无开发库依赖）：

```bash
git clone https://github.com/crazykun/LaterScreen && cd LaterScreen
cargo install --path crates/app
```

## 配置

零配置可用，不生成文件。`lscreen config` 打开面板调整（保存目录、文件名模板、默认工具/颜色、录制格式、历史条数、六个全局热键等），运行中的托盘 1 秒内自动热加载。配置文件：Linux `~/.config/lscreen/config.toml`、Windows `%APPDATA%\lscreen\config.toml`、macOS `~/Library/Application Support/lscreen/config.toml`。

历史副本不放配置目录，而是缓存目录（Linux `~/.cache/lscreen/history/`、Windows `%LOCALAPPDATA%\lscreen\history\`、macOS `~/Library/Caches/lscreen/history/`）：那是可随时删掉、不影响配置的派生数据，嫌占地方直接删整个目录即可。面板顶栏也能看到占用体积并一键清空。

## 运行环境

- **Linux**：交互模式需 X11 桌面；Wayland 下仅整屏截图可用（区域采帧 / 录屏仍需 X11）；全局热键在 Wayland 不可用。OCR 优先系统 tesseract（中文需 `sudo apt install tesseract-ocr tesseract-ocr-chi-sim`），内置纯 Rust ocrs 兜底（仅拉丁字母，首次自动下载模型）
- **Windows 10+ / macOS**：走系统 API，无外部依赖；OCR 用系统引擎（WinRT / Vision），支持中文

## 从源码构建

```bash
cargo build --release                 # 产物 target/release/lscreen
cargo run --release --bin=lscreen     # 构建并运行（裸命令=托盘驻留，gui=直接截图）
cargo test --workspace                # 单元测试（无需显示器）
```

## 打包发布

一键打包脚本 `scripts/package.sh`，产物统一进 `dist/`（含 SHA256SUMS）：

```bash
./scripts/package.sh                            # 打包本机具备工具链的全部默认目标
./scripts/package.sh --list                     # 查看默认目标集与本机可用性
./scripts/package.sh x86_64-unknown-linux-gnu   # 指定目标
```

产物：Linux tar.gz / deb / rpm / AppImage，Windows zip / 自绘安装器 exe，macOS tar.gz / dmg（仅 CI 出包）。交叉编译需装对应 gcc / g++（openh264 为 C++ 源）；rpm 格式需 `apt install rpm`；AppImage 需 [appimagetool](https://github.com/AppImage/appimagetool)。

全平台出包（含 macOS、Windows MSVC）走 GitHub Actions：`git tag v0.7.0 && git push --tags`。

## 架构

```
crates/
  core/      图元模型、撤销栈、命中检测、tiny-skia 导出渲染、取色、二维码（无 UI 依赖）
  capture/   截屏平台层（Linux: x11rb 纯 Rust；Win/mac: xcap 系统 API）
  app/       可执行文件：clap CLI + egui 覆盖层 + 托盘
  ocr/       OCR 引擎（系统引擎 + 内置 ocrs 兜底）
  record/    录屏编码（gifski / openh264）
  setup/     Windows 自绘安装器
```

交互期用 egui 实时绘制，导出用 tiny-skia 软渲染，两条路径共享同一份图元数据。设计细节与里程碑见 [docs/PLAN.md](docs/PLAN.md)。

## License

[MIT](LICENSE)
