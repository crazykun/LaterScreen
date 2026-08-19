# LaterScreen

[![CI](https://github.com/crazykun/LaterScreen/actions/workflows/ci.yml/badge.svg)](https://github.com/crazykun/LaterScreen/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/crazykun/LaterScreen)](https://github.com/crazykun/LaterScreen/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

跨平台截图标注工具（Linux / Windows / macOS），Rust 编写。
**单文件、≤20MB、无动态库依赖、冷启动即用**，命令名 `lscreen`。
目标对齐 Snipaste 级体验：截图、标注、取色、二维码、OCR、GIF 录屏、贴图。

## 特性

**交互截图**（`lscreen`）

- 拖拽框选 / 单击全屏；框选时带像素放大镜（网格取景 + 实时色值）
- 选区可再调整：拖角点/边缘缩放（框外一圈也可命中）、框内拖拽移动、
  工具栏宽 × 高输入框精确设定尺寸
- 9 个工具：选择（移动/编辑元素）+ 8 种标注——矩形、椭圆（Shift 正圆/正方形）、
  箭头、画笔（Shift 直线）、自增标号、文本（支持中文输入法）、马赛克、橡皮擦（恢复原图）
- 已绘制元素可编辑：悬停高亮、拖拽移动、控制点调整、Delete 删除、双击文本改内容
- 调色板：8 预设色 + 完整取色器；线宽/字号联动滑杆；全量撤销/重做
- 导出：复制到剪贴板（Ctrl+C / Enter / 双击）、保存 PNG（Ctrl+S）
- 贴图：把选区钉在屏幕上置顶悬浮（Ctrl+P / 工具栏图钉按钮），独立进程可多个并存，
  拖拽移动、滚轮缩放（25%–400%，光标锚定）、双击复制、置顶切换、
  图像下方条带工具条（不遮挡内容）
- 选区内容直接识别：二维码 / OCR 文字，结果一键复制

**命令行直达**（无界面，适合脚本与快捷键绑定）

- `shot` 截屏、`record` GIF 录屏（gifski 编码）、`qr` 二维码识别、
  `ocr` 文字识别、`pick` 屏幕取色器、`pin` 贴图（`lscreen pin -i img.png`）

## 安装

从 [Releases](https://github.com/crazykun/LaterScreen/releases) 下载对应平台的包
（Linux 覆盖 x64 / arm64 / armv7 / x86 四种架构）：

| 平台 | 包 | 安装方式 |
|---|---|---|
| Debian / Ubuntu / Deepin | `lscreen_*.deb` | `sudo apt install ./lscreen_*.deb` |
| Fedora / RHEL / openSUSE | `lscreen-*.rpm` | `sudo rpm -i lscreen-*.rpm` |
| 任意 Linux 发行版 | `*.AppImage` | `chmod +x` 后直接运行 |
| 任意 Linux 发行版 | `*.tar.gz` | 解压后将 `lscreen` 放入 PATH |
| Windows | `*-setup.exe` | 运行自绘安装器（单用户安装免 UAC，含快捷方式与卸载器） |
| Windows（免安装） | `*.zip` | 解压即用 |
| macOS | `*.dmg` | 拖入 Applications；未签名，首次需右键 → 打开 |

或从源码安装（需 Rust 工具链）：

```bash
git clone https://github.com/crazykun/LaterScreen && cd LaterScreen
cargo install --path crates/app        # 装入 ~/.cargo/bin/lscreen
```

## 快速上手

```bash
lscreen                                # 交互式截图（框选 → 标注 → 复制/保存）
lscreen pick                           # 屏幕取色器（单击复制 HEX 并退出）
lscreen qr                             # 识别主屏上的二维码，输出到 stdout
lscreen qr -i photo.png                # 识别图片文件中的二维码
lscreen ocr --region 0,0,800,600       # 识别屏幕区域文字（自动选择引擎）
lscreen ocr -i doc.png --lang chi_sim --lang eng          # 识别图片文字
lscreen record --region 0,0,800,600 --fps 10 -o demo.gif  # 录屏 GIF，Ctrl+C 停止
lscreen shot -o out.png                # 无界面截全屏
lscreen shot --region 100,100,800,600 --clipboard         # 截区域进剪贴板
```

各子命令选项（完整以 `lscreen <子命令> --help` 为准）：

| 子命令 | 选项 | 说明 |
|---|---|---|
| `shot` | `--region X,Y,W,H` | 截取区域，缺省整主屏 |
| | `-o, --output <路径>` | 输出 PNG 路径，缺省 `~/Pictures/lscreen_时间戳.png` |
| | `-c, --clipboard` | 同时复制到剪贴板 |
| `record` | `--region X,Y,W,H` | 录制区域，缺省整主屏 |
| | `--duration <秒>` | 最长录制时长，缺省 30 |
| | `--fps <1-30>` | 帧率，缺省 10 |
| | `--quality <1-100>` | GIF 编码质量，缺省 90 |
| | `-o, --output <路径>` | 输出 `.gif` 路径，缺省 `~/Pictures` |
| `ocr` | `--region X,Y,W,H` | 识别区域，缺省整主屏 |
| | `-i, --input <图片>` | 从图片识别（PNG/JPEG），指定时忽略 `--region` |
| | `--lang <语言>` | 识别语言，可多次，如 `--lang chi_sim --lang eng` |
| `qr` | `--region X,Y,W,H` | 识别区域，缺省整主屏 |
| | `-i, --input <图片>` | 从图片识别，指定时忽略 `--region` |
| `pick` | — | 无选项；Ctrl+R/H/K 复制 RGB/HEX/CMYK |
| `pin` | `-i, --input <图片>` | 贴图的图片文件；缺省从 stdin 读 PNG（覆盖层内部通道） |
| | `--pos X,Y` | 窗口初始位置（逻辑点），支持负坐标 |
| | `--scale <比例>` | 屏幕缩放比（物理像素/逻辑点），缺省 1.0 |
| `gui` | — | 无选项，与不带子命令等价 |

交互模式快捷键：

| 键 | 功能 |
|---|---|
| 拖拽 / 单击 | 框选区域 / 全屏 |
| Ctrl+R / Ctrl+H / Ctrl+K | 复制指针处颜色 RGB / HEX / CMYK |
| Ctrl+Z / Ctrl+Y（或 Ctrl+Shift+Z） | 撤销 / 重做 |
| Ctrl+S | 保存 PNG 并退出 |
| Ctrl+C / Enter / 双击 | 复制到剪贴板并退出 |
| Delete / Backspace | 删除选中元素 |
| Esc | 关闭弹窗 / 取消选中 / 退出 |

全局唤起快捷键请在系统/桌面环境的快捷键设置中绑定 `lscreen` 命令。
托盘常驻模式（含内置热键与配置面板）规划中，见路线图。

注意：所有 `--region X,Y,W,H` 参数使用**物理像素**坐标（截图/录屏的实际像素），
HiDPI 缩放下与桌面环境显示的"逻辑分辨率"不同。多显示器时坐标基于虚拟桌面原点。

## 运行环境

- **Linux**：交互模式需 X11 桌面（`DISPLAY`）；Wayland 会话暂不支持（规划中）。
  截屏为纯 Rust X11 协议实现，运行时无需任何额外库
- **Windows / macOS**：走系统 API（xcap），无外部依赖
- `ocr` 自动选择引擎：Windows 用系统 `Windows.Media.Ocr`、macOS 用系统 Vision
  （支持中文、零依赖），Linux 优先系统 tesseract（支持中文，`sudo apt install
  tesseract-ocr tesseract-ocr-chi-sim`）；系统引擎不可用时回退内置纯 Rust ocrs
  引擎（零依赖，仅拉丁字母，首次使用自动下载约 4MB 模型到 `~/.cache/ocrs`）
- Linux 上复制后由分离的守护子进程持有剪贴板，被覆盖后自动退出，无需常驻

## 从源码构建

```bash
cargo build --release        # 产物 target/release/lscreen
cargo run --release          # 构建并直接进入交互截图
cargo test --workspace       # 单元测试（无需显示器）

./target/release/lscreen --help        # 查看全部子命令
```

Linux 构建仅需 Rust 工具链，无 C 库依赖。开发约定见 [AGENTS.md](AGENTS.md)。

## 打包发布

一键打包脚本 `scripts/package.sh`，产物统一进 `dist/`（含 SHA256SUMS）：

- **Linux**（x64 / arm64 / armv7 / x86）：tar.gz + deb + rpm + AppImage
- **Windows**（x64）：zip + 自绘安装器 exe（egui 单屏向导，per-user 安装，
  内嵌主程序，`crates/setup`；无需 NSIS/makensis）
- **macOS**（arm64 / x64）：tar.gz + dmg（仅 CI 出包）

```bash
scripts/package.sh              # 打包本机具备工具链的全部默认目标
scripts/package.sh --list       # 查看默认目标集与本机可用性
scripts/package.sh aarch64-unknown-linux-gnu   # 指定目标

# Linux 交叉目标只需装对应交叉 gcc（项目链接期仅依赖 libc）：
sudo apt install gcc-aarch64-linux-gnu gcc-arm-linux-gnueabihf \
                 gcc-i686-linux-gnu gcc-mingw-w64-x86-64
# 原生包格式的工具（缺哪个就跳过哪种格式，不影响 tar.gz/zip）：
sudo apt install rpm             # rpm 包（Windows 安装器由 cargo 自行构建）
# AppImage: github.com/AppImage/appimagetool 下载放入 PATH
```

全平台出包（含 macOS、Windows MSVC）走 GitHub Actions：

```bash
git tag v0.2.0 && git push --tags   # 自动构建全平台包并发布 GitHub Release
```

## 路线图

已完成：截图标注、取色器、二维码、OCR（Windows 系统 OCR / macOS Vision /
Linux tesseract + 内置 ocrs 兜底）、GIF 录屏、图标化工具栏、CLI 无界面模式、
贴图（Pin to screen）、Windows 自绘安装器、全平台打包发布。

规划中（详见 [docs/PLAN.md](docs/PLAN.md)）：

- MP4 录屏（系统编码器）、滚动长截图
- Wayland 支持（xdg-desktop-portal）、混合 DPI 多显示器
- 托盘常驻 + 内置全局热键 + 配置面板

## 架构

```
crates/
  core/      图元模型、撤销栈、命中检测、tiny-skia 导出渲染、取色、二维码（无 UI 依赖）
  capture/   截屏平台层（Linux: x11rb 纯 Rust；Win/mac: xcap 系统 API）
  app/       可执行文件：clap CLI + egui 覆盖层
  ocr/       OCR trait + 引擎实现（tesseract 子进程 / 内置 ocrs）
  record/    GIF 录屏编码（gifski）
```

交互期用 egui Painter 实时绘制，导出用 tiny-skia 软渲染合成，两条路径共享同一份
图元数据。设计细节与里程碑见 [docs/PLAN.md](docs/PLAN.md)。

## License

[MIT](LICENSE)
