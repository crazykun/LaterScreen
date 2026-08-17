# LaterScreen

跨平台截图标注工具（Windows / macOS / Linux）。单文件、体积小、启动快、用完即走。
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
- ✅ CLI 无界面模式
- ⬜ 录屏 / 长截图（M4）、Wayland / 多屏 / CI（M5）

## 使用

```bash
lscreen                                # 交互式截图（框选 → 标注 → 复制/保存）
lscreen pick                           # 屏幕取色器（单击复制 HEX 并退出）
lscreen qr                             # 识别主屏上的二维码，输出到 stdout
lscreen qr -i photo.png                # 识别图片文件中的二维码
lscreen ocr --region 0,0,800,600      # 识别屏幕区域文字（Linux 需安装 tesseract）
lscreen ocr -i doc.png --lang chi_sim --lang eng   # 识别图片文字
lscreen shot -o out.png                # 无界面截全屏
lscreen shot --region 100,100,800,600 --clipboard   # 截区域进剪贴板
```

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

全局唤起快捷键请在系统/桌面环境的快捷键设置中绑定 `lscreen` 命令（进程不驻留）。

## 构建

```bash
cargo build --release        # 产物 target/release/lscreen
```

Linux 构建仅需 Rust 工具链（X11 协议为纯 Rust 实现，无 C 库依赖）。

## 架构

见 [doc/PLAN.md](doc/PLAN.md)。`crates/core`（图元模型/撤销/导出渲染，无 UI 依赖）、
`crates/capture`（截屏平台层）、`crates/app`（CLI + egui 覆盖层）。
