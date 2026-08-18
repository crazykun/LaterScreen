# LaterScreen

跨平台截图标注工具（Windows / macOS / Linux）。单文件、体积小、启动快、小而美常驻。
命令名 `lscreen`，官网 [ailater.com](https://ailater.com)。

## 当前状态（M1–M3 已完成）

- ✅ 截屏：Linux X11（x11rb 纯 Rust）、Win/mac（xcap 系统 API）
- ✅ 交互式标注：矩形、椭圆（Shift 正圆/正方形）、箭头、画笔曲线（Shift 直线）、
  自增标号、文本、马赛克、橡皮擦（恢复原图）
- ✅ 编辑已绘制元素：悬停高亮、拖拽移动、控制点调整、Delete 删除、双击文本改内容
- ✅ 撤销（Ctrl+Z）/ 重做（Ctrl+Y）
- ✅ 保存 PNG（Ctrl+S）、复制到剪贴板（Ctrl+C / Enter / 双击选区）
- ✅ 取景框拾色器：像素放大镜，Ctrl+R / Ctrl+H / Ctrl+K 复制 RGB / HEX / CMYK
- ✅ 二维码识别：工具栏按钮识别选区，或 `lscreen qr` 命令行识别屏幕/图片
- ✅ OCR 文字识别：Linux（tesseract，唯一外部依赖），工具栏 OCR 按钮 + `lscreen ocr` 命令；
  Windows.Media.Ocr / macOS Vision 待 CI 真机环境接入
- ✅ GIF 录屏：`lscreen record`（gifski 纯 Rust 编码，Ctrl+C 停止）
- ✅ CLI 无界面模式
- ⬜ MP4 录屏 / 滚动长截图 / 录制 GUI（M4b）、Wayland / 多屏 / CI（M5）

## 使用

```bash
lscreen                                # 交互式截图（框选 → 标注 → 复制/保存）
lscreen pick                           # 屏幕取色器（单击复制 HEX 并退出）
lscreen qr                             # 识别主屏上的二维码，输出到 stdout
lscreen qr -i photo.png                # 识别图片文件中的二维码
lscreen ocr --region 0,0,800,600      # 识别屏幕区域文字（Linux 需安装 tesseract）
lscreen ocr -i doc.png --lang chi_sim --lang eng   # 识别图片文字
lscreen record --region 0,0,800,600 --fps 10 -o demo.gif  # 录屏 GIF，Ctrl+C 停止
lscreen shot -o out.png                # 无界面截全屏
lscreen shot --region 100,100,800,600 --clipboard   # 截区域进剪贴板
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
| `gui` | — | 无选项，与不带子命令等价 |

交互模式快捷键：

| 键 | 功能 |
|---|---|
| 拖拽 / 单击 | 框选区域 / 全屏 |
| Ctrl+R / Ctrl+H / Ctrl+K | 复制指针处颜色 RGB / HEX / CMYK |
| Ctrl+Z / Ctrl+Y | 撤销 / 重做 |
| Ctrl+S | 保存 PNG 并退出 |
| Ctrl+C / Enter / 双击 | 复制到剪贴板并退出 |
| Delete | 删除选中元素 |
| Esc | 关闭弹窗 / 取消选中 / 退出 |

全局唤起快捷键请在系统/桌面环境的快捷键设置中绑定 `lscreen` 命令。
托盘常驻模式（含内置热键与配置面板）规划中，见 `doc/PLAN.md` M8。

注意：所有 `--region X,Y,W,H` 参数使用**物理像素**坐标（截图/录屏的实际像素），
HiDPI 缩放下与桌面环境显示的"逻辑分辨率"不同。多显示器时坐标基于虚拟桌面原点。

## 构建与运行

```bash
cargo build --release        # 产物 target/release/lscreen
cargo run --release          # 构建并直接进入交互截图（等价 lscreen）
cargo test --workspace       # 单元测试（无需显示器）

./target/release/lscreen --help              # 查看全部子命令
./target/release/lscreen shot --help         # 查看某子命令的选项
```

Linux 构建仅需 Rust 工具链（X11 协议为纯 Rust 实现，无 C 库依赖）。

安装到系统（可选，全局命令 + 桌面快捷键绑定用）：

```bash
cargo install --path crates/app              # 装入 ~/.cargo/bin/lscreen
# 或直接复制单文件：
cp target/release/lscreen ~/.local/bin/
```

运行环境要求：

- 交互模式（`lscreen` / `pick`）需要 X11 桌面（`DISPLAY`）；Wayland 会话暂不支持（M5）
- `ocr` 在 Linux 依赖系统 tesseract（`sudo apt install tesseract-ocr tesseract-ocr-chi-sim`），
  未安装时会给出明确引导；其余功能零外部依赖
- Linux 上复制后由分离的守护子进程持有剪贴板，被覆盖后自动退出，无需常驻

## 架构

见 [doc/PLAN.md](doc/PLAN.md)。`crates/core`（图元模型/撤销/导出渲染，无 UI 依赖）、
`crates/capture`（截屏平台层）、`crates/app`（CLI + egui 覆盖层）。
