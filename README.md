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

交互标注含 8 种工具（矩形 / 椭圆 / 箭头 / 画笔 / 自增标号 / 文本 / 马赛克 / 橡皮擦），元素可再编辑（拖拽 / 控制点 / 双击改文本），支持撤销重做、贴图（Ctrl+P）、二维码 / OCR 识别、复制（Ctrl+C）与保存（Ctrl+S）。文本工具可开**背景色**（当前色圆角底 + 自动黑/白对比字色，浅色截图上白字不可读的解法）；工具栏可**生成二维码**（输入文本插入标注层，可拖动、四角等比缩放，与截图一起导出）；取景放大镜旁有**取色历史**色块（最近 8 色，点击选用后 Ctrl+R/H/K 复制其它格式）。

托盘常驻时菜单含截图 / 取色 / 贴图 ▸（显示贴图 / 关闭穿透 / 关闭所有贴图）/ 录屏 / 滚动截图 / 延时截图 / 历史 / 配置，七个动作可各自绑定全局热键（默认只有 F1 截图）。其中**「历史」是一个缩略图浮窗**，列出最近的截图 / 贴图 / 录屏：点击缩略图复制，录屏则用默认播放器播放；右键可贴图 / 打开目录 / 删除；顶栏显示条数与占用体积，可一键清空。同一时刻只有一个面板，面板在后台时再按热键会把它唤到前台。

**贴图**（Snipaste 式置顶悬浮）：滚轮缩放，工具条有 −/百分比/＋ 控件（点百分比重置 100%，键盘 +/−/0）；**Shift+滚轮调不透明度（20–100%）**；R 旋转 90°、H/V 水平/垂直翻转（复制与保存所见即所得）；高倍放大（≥8x）显示像素网格；**点击穿透**——点击穿到下层窗口，Esc 或托盘菜单「贴图 → 关闭穿透」恢复。

其余功能均可无界面调用，完整选项以 `lscreen <子命令> --help` 为准：

```bash
lscreen shot -o out.png                  # 无界面截图（--region X,Y,W,H 指定区域）
lscreen gui --delay 3                    # 延时 3 秒截图（shot --delay 同支持）
lscreen record --select --fps 10         # 框选录制 GIF（--mp4 录制 MP4/H.264）
                                          # 录制时鼠标点击处显示扩散圆环（配置面板可关）
lscreen record --mp4 --audio system      # 录 MP4 并加音轨（mic/system/both/off；mac 暂无
                                          # system/both，需麦克风用 mic；缺省读配置「录制音频」）
lscreen scroll                           # 滚动长截图（Linux X11）
lscreen ocr --region 0,0,800,600         # OCR 识别（-i 指定图片，--lang 选语言）
lscreen qr -i photo.png                  # 识别图片中的二维码
lscreen qr-gen "https://example.com" -o qr.png   # 生成二维码 PNG（--ecc L/M/Q/H）
lscreen pick                             # 屏幕取色器
lscreen pin -i img.png                   # 把图片钉在屏幕上
lscreen upload img.png                   # 上传：交给自配命令，stdout 返回 URL 并复制
lscreen history                          # 历史面板（最近截图 / 贴图 / 录屏）
lscreen config                           # 配置面板
```

> `--region X,Y,W,H` 均为**物理像素**坐标，多显示器时基于虚拟桌面原点。
>
> `upload` 需先在配置文件启用上传命令（见下节），未配置时工具栏/贴图也不显示上传按钮。

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

零配置可用，不生成文件。`lscreen config` 打开面板调整（保存目录、文件名模板、配置窗口主题（自动/浅色/夜间）、默认工具/颜色、初始选区（最前窗口 / 上次选区 / 全屏 / 无——「上次选区」在显示器布局变化后自动作废回退）、录制格式、录制音频（仅 MP4 生效；mac 面板只给 关/麦克风）、录制点击高亮、历史条数、七个全局热键等），运行中的托盘 1 秒内自动热加载。配置文件：Linux `~/.config/lscreen/config.toml`、Windows `%APPDATA%\lscreen\config.toml`、macOS `~/Library/Application Support/lscreen/config.toml`。自动主题由 egui 跟随当前操作系统配色。

历史副本不放配置目录，而是缓存目录（Linux `~/.cache/lscreen/history/`、Windows `%LOCALAPPDATA%\lscreen\history\`、macOS `~/Library/Caches/lscreen/history/`）：那是可随时删掉、不影响配置的派生数据，嫌占地方直接删整个目录即可。面板顶栏也能看到占用体积并一键清空。

**上传 hook（可选）**：不内置任何图床 SDK，配置 `[upload]` 后把产物交给任意外部命令（uPic/PicGo/sup 或自写脚本均可）：

```toml
[upload]
command = ["/usr/local/bin/uploader", "--token", "xxx"]
```

约定：产物**路径**经 stdin 传给命令（不经 shell、路径不进 argv，无注入面）；stdout 第一个非空行视为 URL，自动复制到剪贴板并记入历史条目（面板右键可「复制链接」）；非零退出把 stderr 提示给用户；命令挂死 30 秒自动终止。配置后覆盖层工具栏与贴图工具条会出现「上传」按钮，`lscreen upload <file>` 是同一入口的脚本化用法。

## 运行环境

- **Linux**：交互模式需 X11 桌面；Wayland 下仅整屏截图可用（区域采帧 / 录屏仍需 X11）；全局热键在 Wayland 不可用。OCR 优先系统 tesseract（中文需 `sudo apt install tesseract-ocr tesseract-ocr-chi-sim`），内置纯 Rust ocrs 兜底（仅拉丁字母，首次自动下载模型）。录屏音频（`--audio`，仅 MP4）运行时调系统工具：需 `ffmpeg` 与 `arecord` 或 `parec`（任一，PipeWire/PulseAudio 桌面通常自带 parec），缺失时录制开始前会明确报错而不是录完才发现没声
- **Windows 10+**：走系统 API，无外部依赖；OCR 用系统引擎（WinRT），支持中文。录屏音频走 WASAPI 采集 + Media Foundation AAC（麦克风/系统声/混合均可），无需安装任何工具
- **macOS**：走系统 API，无外部依赖；OCR 用 Vision，支持中文。录屏音频支持麦克风（CoreAudio + AudioToolbox AAC）；系统声内录暂不支持（待 ScreenCaptureKit，配置为 system/both 时自动降级麦克风并提示）

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

全平台出包（含 macOS、Windows MSVC）走 GitHub Actions：`git tag v0.10.1 && git push --tags`。

## 架构

```
crates/
  core/      图元模型、撤销栈、命中检测、tiny-skia 导出渲染、取色、二维码（无 UI 依赖）
  capture/   截屏平台层（Linux: x11rb 纯 Rust；Win/mac: xcap 系统 API）
  app/       可执行文件：clap CLI + egui 覆盖层 + 托盘
  ocr/       OCR 引擎（系统引擎 + 内置 ocrs 兜底）
  record/    录屏编码（gifski / openh264；音频：Linux=arecord/parec+ffmpeg 子进程链，
             Win=WASAPI+Media Foundation，mac=CoreAudio+AudioToolbox，系统 API 直采）
  setup/     Windows 自绘安装器
```

交互期用 egui 实时绘制，导出用 tiny-skia 软渲染，两条路径共享同一份图元数据。设计细节与里程碑见 [docs/PLAN.md](docs/PLAN.md)。

## License

[MIT](LICENSE)
