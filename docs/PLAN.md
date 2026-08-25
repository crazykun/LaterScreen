# LaterScreen 项目计划

跨平台截图工具（Windows / macOS / Linux），Rust 实现。目标对齐 Snipaste 级别的体验：
截图、标注、取色、二维码、OCR、录屏（GIF/MP4）、滚动长截图；单文件、体积小、启动快、小而美常驻。

## 1. 非功能性目标（硬约束）

| 约束 | 目标 | 实现手段 |
|---|---|---|
| 体积 | 最终产物 ≤ 20MB | `opt-level="z"` + fat LTO + strip + `panic="abort"`；避免重型依赖 |
| 单文件 | 无需安装、不依赖动态库 | 静态链接；OCR 模型按需下载、不捆绑 |
| 启动 | 冷启动 < 300ms 出选区 | 无运行时、按需初始化、截屏与窗口创建并行 |
| 内存 | 常态 < 100MB（全屏图 + 双缓冲） | 单份截图内存 + 图元矢量数据，不做多余拷贝 |
| 小而美常驻 | 托盘常驻进程空闲 < 30MB | 常驻体只持有托盘图标 + 配置 + 热键监听，**不预载截图缓冲**；截图/贴图/录屏各自独立窗口，用完释放。CLI 单次调用仍是即起即退 |

## 2. 架构

核心原则：**core 不依赖任何 UI**；CLI 与 GUI 是 core 之上的两个薄壳；
平台差异全部收敛在 platform 抽象层。

```mermaid
graph TD
    CLI[clap 命令行入口] --> APP[app: eframe/egui 覆盖层]
    CLI -->|--ocr/--qr 等无头模式| CORE
    APP --> CORE
    subgraph CORE[core 纯逻辑库]
        MODEL[图元模型 + 撤销栈 + 命中检测]
        RENDER[导出渲染 tiny-skia]
        COLOR[取色/CMYK 换算]
        QR[二维码 rqrr]
    end
    CORE --> CAPTURE[capture: xcap 截屏]
    CORE -.-> OCR[ocr: 系统API trait]
    CORE -.-> REC[record: gifski/系统编码器]
```

### Crate 划分

```
crates/
  core/      lscreen-core     图元模型、撤销栈、命中检测、导出渲染、取色、二维码
  capture/   lscreen-capture  截屏（xcap 封装，屏蔽 X11/Wayland/多显示器差异）
  app/       lscreen          可执行文件：clap CLI + egui 覆盖层
  ocr/       lscreen-ocr      trait + 三平台实现（M3）
  record/    lscreen-record   录屏编码（M4）
  setup/     lscreen-setup    Windows 自绘安装器（egui 单屏向导，替代 NSIS；
                              构建期经 LSCREEN_BIN 内嵌主程序，per-user 安装）
```

### 关键设计决策

1. **双渲染路径**：交互期用 `egui::Painter` 实时绘制（GPU/glow），导出时用
   `tiny-skia` 在 core 内软渲染合成到截图上。两者共享同一份图元数据与几何参数
   （箭头头长、马赛克格子等来自 `Element` 方法）。一致性承诺：**几何一致、
   视觉近似**——文本因 epaint 与 ab_glyph 的光栅化差异（hinting/AA/字距）
   无法做到像素一致，马赛克通过共用 `mosaic_cells` 做到像素一致。
   代价是每种图元两份绘制代码，新增图元时两边都要写。
2. **撤销/重做用快照而非命令模式**：图元是小矢量数据（每个几十字节），
   全量快照 `Vec<Vec<Element>>` 实现简单且绝不出错。图片本体不进快照。
3. **马赛克 = 网格色块**：按笔迹覆盖的网格单元，从原图计算均值色，
   交互层画色块矩形、导出层同样画色块，两边像素级一致。
4. **橡皮擦 = 原图回贴**：按绘制顺序渲染，橡皮擦笔迹用原图对应区域贴回，
   天然擦掉此前的所有标注。
5. **系统 API 优先 + 内置兜底**：OCR（Win `Windows.Media.Ocr` / mac Vision）、
   MP4 编码（Win Media Foundation / mac VideoToolbox）都走系统自带能力，
   零体积零依赖；系统能力未落地前由内置 ocrs 引擎兜底（纯 Rust，模型按需下载）。

## 3. 技术选型

| 功能 | 选型 | 备选/说明 |
|---|---|---|
| UI | egui (eframe 0.35) | 即时模式适合画布高频重绘；备选 winit+tiny-skia 纯软渲染（体积极限方案） |
| 截屏 Linux | **自研：x11rb（X11，纯 Rust）** | Wayland 走 ashpd portal（M5，纯 Rust D-Bus）。弃用 xcap Linux 路径：它强制链接 pipewire C 库，违反"无动态库依赖"目标 |
| 截屏 Win/mac | xcap | 这两个平台走系统 API，无 C 编译依赖 |
| 交互渲染 | egui Painter | — |
| 导出渲染 | tiny-skia | 纯 Rust 2D 软渲染 |
| 文本光栅化 | ab_glyph | 字体运行时从系统加载（fc-match / 平台字体目录），不捆绑 |
| 图像编解码 | image | PNG/JPEG 导出 |
| 剪贴板 | arboard | 支持图像写剪贴板，三平台 |
| CLI | clap (derive) | 子命令直达功能 |
| 二维码 | rqrr | 纯 Rust |
| GIF 编码 | gifski | 纯 Rust，质量最好 |
| MP4 编码 | openh264（Linux，静态链接 C++ 源）+ mp4 crate 封装 | Win MF / mac VideoToolbox 系统编码器待实现；零体积方案 |
| OCR | 系统 API + 内置 ocrs | Win `Windows.Media.Ocr` / mac Vision / Linux tesseract 子进程；内置 ocrs 纯 Rust 兜底 |

## 4. 里程碑

### 版本历程

| 版本 | 日期 | 交付内容 |
|---|---|---|
| v0.1.0 | 2026-08-18 | M1 截图标注、M2 取色/二维码/CLI、M3 OCR（Linux tesseract）、M4a GIF 录屏、M6 工具栏图标化；四种原生包格式 + dmg |
| v0.2.0 | 2026-08-19 | M7 贴图（独立进程 + 缩放/拖拽/工具条）、Win/mac 系统 OCR、内置 ocrs 兜底、Windows 自绘安装器替代 NSIS |
| v0.3.0 | 2026-08-20 | M8 托盘常驻 + 配置面板 + 全局热键（空闲 RSS ≈ 9MB） |
| v0.4.0 | 2026-08-21 | M4 MP4 录屏（openh264 静态链接）、滚动长截图、M5 Wayland portal 整屏、M9 窗口截图（默认截当前窗口）；任务栏图标归属修复 |
| v0.5.0 | 2026-08-25 | M10 录制选区边框 + 识别结果面板可拖拽缩放、M11 截图历史面板；录制 armed 待开始 + 边框闪烁 + 录制格式进配置 |
| v0.5.1 | 2026-08-25 | 修 Windows 缺 VCRUNTIME140.dll 打不开（MSVC 默认动态链 CRT，违反硬约束）；打包期新增导入表校验（未公开发布，内容并入 v0.6.0） |
| **v0.6.0** | **2026-08-25** | **启动失败不再静默：panic hook + 无控制台时弹窗上报、OpenGL 不可用时给出可自救指引；打包修 GNU 产物混入 dist（加 -localtest 后缀）与 7z 分支的目录前缀** |
| **v0.6.1** | **2026-08-25** | **macOS 修到能用：录屏/滚动截图选区后不再消失（同进程跑不了第二个事件循环，框选后 re-exec 转交）、托盘左键弹菜单（不再叠加截图）、托盘不占 Dock（Accessory）、历史面板移到右上角** |

### M1 截图 + 标注 ✅（核心价值）
- [x] workspace 骨架 + 体积优化 profile
- [x] core：图元模型（矩形/椭圆/箭头/直线/曲线/标号/文本/马赛克/橡皮擦）、
      样式（颜色/线宽/字号）、命中检测、撤销/重做
- [x] capture：多显示器截屏
- [x] app：全屏覆盖层、区域框选（可调整边缘）、工具栏、绘制交互、
      Shift 约束（正圆/正方形/水平垂直直线）、悬停选中/拖拽移动/删除
- [x] 快捷键：Ctrl+Z 撤销、Ctrl+Y 重做、Ctrl+S 存文件、Ctrl+C/双击 进剪贴板、Esc 退出
- [x] 导出：tiny-skia 合成 → PNG 文件 / 剪贴板

### M2 取色器 + 二维码 + CLI ✅
- [x] 取景框放大镜（像素级十字线 + 周边像素放大）
- [x] Ctrl+R 复制 RGB / Ctrl+H 复制 HEX / Ctrl+K 复制 CMYK
- [x] 框选区域内二维码识别（rqrr）
- [x] CLI：`lscreen`（交互截图）、`lscreen shot --region x,y,w,h -o f.png`、
      `lscreen pick`（取色）、`lscreen qr`、`lscreen ocr`

### M3 OCR ✅
- [x] `TextRecognizer` trait；识别结果浮层展示 + 一键复制
- [x] Linux：探测系统 tesseract 可执行文件调用（中文方案，未安装时明确引导）
- [x] 内置 ocrs 兜底引擎：纯 Rust 零依赖，系统引擎缺失时的最终兜底；模型约 4MB
      按需下载到 `~/.cache/ocrs`，仅拉丁字母文字（CJK 需 tesseract）
- [x] Windows（Windows.Media.Ocr）/ macOS（objc2 + Vision）原生 OCR ✅（2026-08-18）
- [x] Windows/macOS 原生 OCR 真机验证（CI 无桌面环境，需双系统手动确认）

### M4 录屏 + 滚动截图（技术风险最高，放最后）✅ 2026-08-20
- [x] 选区连续采帧 → gifski 编码 GIF（✅ 已交付 `lscreen record`）
- [x] MP4：Linux 走 **openh264 静态链接**（vendored C++ 源，无动态库依赖；
      无 asm 构建——CI 无 nasm 也能编译，发布可开 asm 提速）+ `mp4` crate
      封装（AnnexB→AVCC、SPS/PPS→avcC）。CLI：`record --mp4`
      （此时 `--quality` 语义 = 目标码率 kbps 200-50000，缺省 4000）。真机验证：
      ffprobe 读回 h264/Constrained Baseline、时长帧数正确、ffmpeg 可解码；
      码率控制 Bitrate 模式 + Constrained Baseline 兼容性最好。
      Win/mac 系统编码器（MF/VideoToolbox）待实现——发布体积预算内
      （12.4MB ≤ 20MB）。失败路径与 GIF 同语义：清半成品、join 编码线程
- [x] 滚动截图：`lscreen scroll`（托盘菜单「滚动截图」同入口）。
      capture 层新增 XTest 滚轮/指针控制（`scroll_wheel`/`warp_pointer`，
      x11rb xtest 特性，FakeInput 后 sync 保证时序）；record 层
      `ScrollStitcher` 帧间拼接：尾部块（区域高 1/6，钳 16–96 行）两阶段
      匹配——签名行（4px 降采样亮度）预筛候选 + 整块 SAD 校验（阈值
      8/255，容抗锯齿噪声），Anchor=块末行定位（起点 = p-(k-1)）。
      交互：框选区域 → 指针移到区域中心驱动滚动 → 状态窗显示拼接高度
      （连续 2 帧无新增 = 到底；内容突变 = 保留已拼部分停止）→ **标注预览
      窗口**（复用 SnipApp 会话，preview 标志）：可滚动画布（首帧适应宽度、
      内容水平居中）+ **完整截图标注工具栏**（矩形/椭圆/箭头/直线/画笔/
      标号/文本/马赛克/橡皮擦、颜色/粗细、撤销重做、保存/贴图/二维码/OCR/
      复制退出）——长图与普通截图同一套标注体验；选区固定整图（禁手柄），
      文本编辑器锚点改用画布帧缓存视图（预览下 content_rect ≠ 画布 rect）。
      预览缩放：Ctrl+滚轮/触摸板捏合（egui zoom_delta）+ Ctrl+=/− 步进 +
      Ctrl+0 适应宽度，锚定指针位置（滚动偏移随缩放补偿）；窗口最小宽
      640 容纳底部工具栏，工具栏过窄时靠左钳住保证可达。
      预览平移：中键拖动任意处 / 「选择」工具左键拖空白；绘制类交互统一
      改用 dragged_by(Primary)，中键与画图不再互抢。
      超 GPU 单纹理上限（8192）的长图显示用整数因子降采样纹理兜底
      （避免上传失败/驱动静默裁剪），保存/复制/OCR 仍走全分辨率原图。
      显式 `-o` 保持直存旧行为（脚本用法）。结束恢复指针原位。
      真机验证：xterm+tail -f 滚动内容 30 步拼出
      604×2860 长图逐行一致。已知限制：悬浮表头/固定侧栏返回 Mismatch
      即停（保已有部分）；区域内动画内容会误判；仅 Linux X11。
      真机 E2E 受宿主终端滚轮行为干扰（xterm 对持续输出会自动跳回底部
      scrollTtyOutput、alt-screen 不转发滚轮），理想宿主是浏览器/编辑器

### M5 打磨
- [x] Wayland portal 路径 ✅ 2026-08-20：`lscreen-capture` 集成 ashpd
      （纯 D-Bus，`screenshot`+`async-io` 特性，zbus 由 workspace 统一钉
      5.18）。纯 Wayland 会话时 `capture_primary/at/all` 走
      xdg-desktop-portal Screenshot（interactive=false 免对话框），返回
      文件解码（PNG/JPEG 视 DE——Deepin 后端给 JPEG，已兼容）后读后即删；
      区域采帧/录屏/指针/窗口枚举在 Wayland 下明确报错或降级。
      真机验证（Deepin 25 portal，X11 会话模拟 Wayland 环境变量）：
      D-Bus 链路通、返回双屏拼接 3840×1080。待办：真实 Wayland 会话
      （GNOME/KDE/wlroots）的覆盖层 GUI 体验——portal 快照是「全部显示器
      拼接、origin(0,0)」，与单屏覆盖层窗口的坐标映射需逐 DE 验证，
      多屏混合 DPI 下可能错位（已知限制，随 M5 后续迭代）
- [ ] 多显示器混合 DPI：X11 下 scale 恒 1 自洽；Win/mac 经 xcap 的
      scale_factor 换算（M9 已处理 mac 窗口矩形）；X11 xrandr --scale
      的假混合 DPI 与 Wayland 多屏拼接映射待真机验证
- [ ] 多屏窗口定位真机验证：`with_position + with_fullscreen` 在部分 WM 上
      fullscreen 可能忽略 position hint 落错屏（CI 测不了，需双屏手动确认）
      ——✅ Deepin 25 (KWin/X11) 双屏实测通过（2026-08-17）：覆盖层正确落在
      鼠标所在屏；其他 WM（GNOME/i3 等）待社区反馈
- [x] CI：三平台构建产物 + 体积回归检查（见 .github/workflows/ci.yml，含
      fmt/clippy 门槛）；openh264 静态链接后 release 12.4MB（预算 20MB 内，
      2026-08-20 实测）。远期注意：ldd 白名单对全静态产物会误报

### M6 界面打磨：图标化工具栏（✅ 2026-08-18）

原状：15 个中文文字按钮横向占用超过 620pt，小屏或窄选区下挤压严重。

- [x] 工具栏按钮改为图标 + hover tooltip（含快捷键提示）
- [x] 图标方案调整：**全部 Painter 手绘 12px 矢量线稿**（比 Unicode 符号更稳——
      不赌 emoji 字体覆盖，任何系统渲染一致；仅标号「1」与 OCR「A」用拉丁字形）。
      不引入 SVG 库、不打包 PNG 图集
- [x] 按钮统一 24×24，间距 2pt，整条工具栏约 470pt（原 620+）
- [x] 颜色选择器改为单个当前色按钮 + 点击展开调色板 popup（egui 0.35 `Popup::menu`）
- [x] 每个图标按钮有 `on_hover_text` 完整文案，禁用态（撤销/重做）灰显不可点

### M7 贴图（Pin to screen）✅ 2026-08-18

Snipaste 的招牌能力：截完把图钉在屏幕上置顶悬浮，方便对照。

- [x] 新增 `lscreen pin` 子命令 + 覆盖层工具栏「贴图」按钮（Ctrl+P）
- [x] 实现：合成当前选区 → 开一个新的 eframe 窗口
      （`with_always_on_top` + `with_decorations(false)` + `with_resizable(false)`），
      窗口初始位置对齐原选区，尺寸等于选区。
      **未用 with_transparent**：贴图内容本身是不透明位图，透明窗口无收益
      且依赖合成器行为，刻意省略
- [x] 交互：拖拽移动窗口（手动定位：指针屏幕坐标取 X11 QueryPointer 绝对值，
      弃用 StartDrag——未激活窗口首次按下被 WM 吞；也弃用 egui 局部坐标增量
      ——窗口被程序移动时静止指针无 MotionNotify，陈旧坐标会自激「乱跑」）、
      滚轮缩放（25%–400%，光标下的图像点锚定不动：InnerSize + OuterPosition
      联动换算）、双击复制、Esc/Delete 关闭
- [x] 图像下方条带工具条：置顶切换/保存/关闭/复制并关闭（复用 ui::toolbar 手绘图标，与
      覆盖层风格一致）；快捷键 Ctrl+C 复制 / Ctrl+S 保存；缩放百分比 toast。
      曾做右键菜单：未激活窗口的右键常被 WM 拿去做焦点转移、时常不弹，弃用
- [x] 生命周期决策：**贴图窗口独立进程**（`lscreen pin` 由覆盖层 spawn 后自身退出）。
      每个贴图是一个只持有一张图的轻量进程，符合「小而美常驻」：常驻体积随贴图数
      线性增长且各自可独立关闭，不共享一个越用越大的主进程。
      图片经 stdin 以 PNG 传入（父进程写完再退出，无临时文件与清理问题）
- [x] 内存：贴图进程只保留一份 RGBA + 纹理，常态目标 < 60MB
- [x] CLI 防呆：无 -i 且 stdin 是 tty 直接报错（不阻塞等 EOF）；
      --pos/--scale 校验前置；负坐标支持（`--pos=-1920,0` 或
      allow_hyphen_values 的空格写法）

### M8 托盘 + 配置面板 ✅ 2026-08-19

托盘是「小而美常驻」的主形态：一个空闲 < 30MB 的常驻体，负责热键监听与配置，
截图/贴图/录屏窗口按需开、关掉即释放。CLI 子命令单次调用仍是即起即退，两种用法并存。

- [x] **默认行为变更（用户决策）**：裸 `lscreen` = 静默驻留后台托盘（分离子进程，
      终端立即返回）；`lscreen gui` 直达交互截图；`lscreen tray --foreground` 前台
      调试/自启动
- [x] 托盘选型落地：**Linux 用 ksni**（纯 Rust 的 StatusNotifierItem/D-Bus 直连，
      零动态库依赖——tray-icon 的 Linux 后端要链接 gtk/libappindicator，违反硬约束，
      弃用）；Win/mac 用 tray-icon（系统原生 API），事件泵复用 eframe 已链接的
      winit 0.30（macOS 要求托盘在主线程已运行的事件循环上创建）
- [x] 托盘菜单：截图、取色、贴图（读剪贴板）、录屏（`record --select` 交互框选）、
      滚动截图、历史、配置、退出；菜单项带热键后缀；Linux 左键单击弹菜单
      （MENU_ON_ACTIVATE），activate 兜底为直接截图
- [x] 配置面板（`lscreen config`，settings_ui.rs）：保存目录、文件名模板、
      默认工具/颜色/线宽、复制后自动退出、保存后打开目录、历史条数/复制后收面板、
      六个全局热键；保存前全部校验（模板/热键/颜色/工具名），非法只提示不落盘
- [x] 全局热键：`global-hotkey` crate（托盘进程内自监听）；**默认 F1 截图**
      （Snipaste 惯例——实测 Ctrl+Alt+A 与 Deepin 系统截图键冲突）；注册失败/
      Wayland 无 X11 时仅告警降级，托盘与菜单不受影响；裸键仅允许
      PrintScreen/F1-F12（裸字母会全局抢占打字）
- [x] 配置持久化：`~/.config/lscreen/config.toml`（Win `%APPDATA%`、
      mac `~/Library/Application Support`）；`toml` + serde，未知字段忽略、
      缺失字段取默认
- [x] 零配置：无文件时静默全默认（不生成文件不告警），配置面板保存才落盘；
      托盘每秒轮询 mtime，面板保存后 1 秒内热加载（热键重注册、菜单文案更新）
- [x] 截图窗口接入配置：默认工具/颜色/线宽初始值、复制后是否自动退出
      （关闭时不退只 toast）、保存目录与文件名模板、保存后打开目录
- [x] 附带交付：`record --select`（框选即录，Esc 取消静默退出）
- [x] 录屏状态窗口（2026-08-19 补）：录制在独立线程跑 `record_gif`，主线程跑
      置顶状态窗口（已录时长/帧数/进度条 + 停止按钮），Esc 或按钮停止、关窗即停；
      托盘 spawn 的录屏子进程脱离终端也能正常结束（此前只能等 --duration 超时）；
      配置面板黑屏修复——egui 0.35 移除 TopBottomPanel 后改用 Panel::bottom +
      CentralPanel 分域（原 allocate_rect 手动分域会把 ScrollArea 挤到底部 40px 条带）
- [x] 常驻内存实测（Deepin 25 / X11）：托盘空闲 RSS ≈ 9MB（目标 ≤ 30MB）；
      热键 F1/F2 唤起截图、菜单动作、托盘退出、配置热加载真机验证通过
- [ ] Win/mac 托盘真机验证（tray-icon + winit 路径，CI 无桌面需手动确认）
- [x] 依赖备注：ksni blocking 需 async 运行时，选 async-io（比 tokio 轻）；
      **zbus 钉 =5.18.0**（5.19 在 default-features=false + blocking-api 组合下
      自身编译失败，上游打包缺陷，修复后放开）
- [x] 任务栏图标归属修复（2026-08-21）：egui 的 `with_app_id` 只在 Wayland 生效，
      X11 下 winit 不设 WM_CLASS、回落到窗口标题（实测 `WM_CLASS="" 标题`），
      任务栏匹配不到 lscreen.desktop 就用错图标（显示成启动来源的 VS Code）。
      修复：app 各窗口构造时经 raw-window-handle 取 X11 window id → capture
      新增 `set_window_class`（x11rb change_property8 显式写 `WM_CLASS="lscreen"`）；
      desktop 补 `StartupWMClass=lscreen`；所有 ViewportBuilder 补
      `with_app_id("lscreen")`（Wayland 侧对齐）。X11 实测 pin 窗口 WM_CLASS
      已变为 `("lscreen","lscreen")`
- [x] 窗口图标兜底（2026-08-21）：任务栏图标走 `.desktop` 的 `Icon=lscreen` 依赖
      hicolor 缓存，目录尺寸不符（997×977 放在 256x256）或缓存未刷新会回退旧图。
      补 capture `set_window_icon`（`_NET_WM_ICON`，CARDINAL 数组 ARGB），窗口构造
      时直接给任务栏/alt-tab 图标，不依赖缓存；源图统一 resize 为 256×256 正方形
      （build.rs ICO 要求正方形）。实测窗口 `_NET_WM_ICON` = 64×64 已生效

### M9 窗口截图（选中最前窗口，默认截当前窗口）✅ 2026-08-20

Snipaste/系统截图的基础体验：进入截图时不必手动框选，**默认选区就是当前
最前面的窗口**；移动鼠标时自动高亮悬停处的窗口，单击即选中该窗口区域。

设计原则：**窗口矩形只用来「吸附选区」，像素仍来自已截好的全屏图**。
不做单窗口独立采集（XGetImage 单窗口 / PrintWindow / CGWindowListCreateImage），
避免被遮挡窗口内容缺失、DWM 圆角阴影裁剪等一堆平台坑；截图语义 =
「屏幕上此刻看到的这个窗口区域」，与现有覆盖层管线零冲突。

- [x] capture 新增窗口枚举 API：
      `WindowInfo { id, title, x/y/w/h, z_order, is_minimized }` +
      `list_windows() -> Vec<WindowInfo>`（按 Z 序自顶向下）+
      `frontmost_window() / window_at(x,y)` +
      `window_rect_in_image()`（平台坐标 → 显示器图像像素，含求交；
      mac 的 CG 逻辑点 → 物理像素换算收敛在此，不泄漏到 app）
      - Linux X11（x11rb，纯 Rust）：`_NET_CLIENT_LIST_STACKING` 取 Z 序，
        `_NET_ACTIVE_WINDOW` 取最前窗口；几何用 GetGeometry +
        TranslateCoordinates 折算到根坐标，`_NET_FRAME_EXTENTS` 补装饰边框；
        过滤 `_NET_WM_STATE_HIDDEN`（最小化）、非当前桌面（`_NET_WM_DESKTOP`，
        sticky 保留）与 DOCK/DESKTOP/MENU 等辅助窗口类型；无 EWMH 的 WM
        返回空列表降级。x11rb 0.13 的 `value32()` 返回 Option 迭代器，
        统一经 `values32()` 展平
      - Windows / macOS：xcap `Window::all()`（两平台天然按 Z 序自顶向下），
        最前窗口 Win = GetForegroundWindow、mac = 活跃 App（xcap `is_focused`）；
        最小化/零尺寸过滤
      - Wayland：portal 无窗口几何能力，返回空列表明确降级为纯手动框选（不报错）
- [x] 关键时序：**窗口列表必须在覆盖层窗口创建前采集**（`overlay_window_list`
      与截屏同时机），否则覆盖层自己就是最前窗口；按 `_NET_WM_PID` +
      `/proc/<pid>/exe` == 自身可执行文件排除自家窗口（贴图/录制状态/配置
      面板——它们是同 exe 的独立进程；覆盖层自身尚未建窗天然不在列表）；
      Win/mac 按 xcap `app_name` 与自身 exe 文件名比对
- [x] 覆盖层交互（对齐 Snipaste）：
      - 进入截图：初始选区 = 最前窗口矩形（与屏幕求交），一步 Enter/
        Ctrl+C/双击即可出图——「默认截图当前窗口」
      - 未按下拖拽时：鼠标移动实时命中悬停窗口（Z 序自顶向下第一个含点者），
        高亮其边框 + 左上角显示窗口标题；单击 = 选中该窗口并进标注
        （可拖边缘微调、可标注，复用现有全部 Editing 交互）
      - 一旦开始拖拽即进入自由框选，行为与现状完全一致
      - 双击 = 第一击选中窗口进标注、第二击触发现有「双击复制」链路，
        点击型工具（标号/文本）例外逻辑不变
      - 空白桌面单击 = 全屏（旧行为保留）；Record 框选模式同样受益
        （窗口单击/Enter 即交付该窗口区域）
- [x] CLI：`lscreen shot --window`（最前窗口直接出图；直接 capture_region
      抓窗口矩形，跨显示器窗口天然正确，优于「截主屏再裁剪」的旧路径）、
      `--window-at x,y`（取该点下窗口，供脚本用）；与 --region clap 互斥
- [x] 配置面板：新增「初始选区」选项（最前窗口/全屏/无），默认最前窗口；
      未知配置值回退默认；旧配置文件缺字段自动取默认
- [x] 验收（Deepin 25 / KWin / X11 真机 2026-08-20）：窗口枚举 Z 序/
      标题/几何正确（含最大化窗 frame extents 与 +1920 副屏坐标）、
      frontmost = 活跃窗口、`shot --window` / `--window-at` 出图尺寸与
      窗口一致；覆盖层交互路径（初始预选/悬停高亮/单击选窗/Enter 出图）
      已实现待人工点验。Win/mac 待真机确认（CI 无桌面）；mac 窗口坐标
      走「CG 点 × 窗口中心所在显示器缩放比」换算，混合 DPI 场景随 M5 一并验证

### M10 录制区域可视化 + 识别结果面板可拖动 ✅ 2026-08-24

两条来自实际使用的体验缺口：录制时看不见录的是哪块、识别结果框钉死在屏幕正中。

- [x] **录制期间常显选区边框**（✅ 2026-08-24）：现在 `run_record` 只开一个
      320×150 的状态窗口（main.rs:715），选区本身没有任何视觉标记——录到第 10 秒
      已经不记得框的是哪里，也无法确认目标窗口有没有移出框外。目标：录制全程在
      选区周围显示常亮/虚线边框，停止即消失。
      - **关键约束：边框不能被录进帧里**。`capture_region` 抓的是屏幕实际像素，
        任何压在选区内的装饰都会出现在成品里。因此边框画在选区**外侧一圈**
        （矩形向外扩 2px），选区内像素一个不碰。同理状态窗口若与选区重叠也会被录进去，
        需要避让（选区外的空白角落，无处可放时才允许重叠并明确提示）
      - 实现选型（Linux X11 优先，与滚动截图同策略）：capture 层新增 4 条细长
        override-redirect 窗口（上/下/左/右边条）而非一个带洞的透明窗口——
        避免依赖合成器的透明与 XShape 挖洞行为（贴图窗口已有「不赌合成器」的先例，
        见 M7）。纯 x11rb 创建、填色、置顶，无 GUI 框架开销；
        用 XShape 的 ShapeInput 设空输入区做点击穿透，边条不抢鼠标
      - 边界情形：选区贴屏幕边缘时外扩超出虚拟桌面 → 钳到桌面范围，接受边框缺一侧
        （不退化成画在选区内，那会污染成品）；多屏跨越选区按虚拟桌面坐标处理
      - 生命周期：RAII guard 持有边条窗口，录制线程正常结束/出错/用户停止/进程
        panic 都要销毁，绝不留残影窗口在屏幕上
      - Win/mac 与 Wayland：先不做，同滚动截图的平台策略（Wayland portal 无法
        创建 override-redirect 覆盖窗）；缺失时录制行为不变，仅无边框
      - **落地记录**：`capture::record_border(x,y,w,h) -> Option<RecordBorder>`，
        guard Drop 即销毁（连接随 guard 存活）。`monitor_bounds()` 一并导出。
        `run_record` 用 `status_window_pos` 把状态窗摆到不与选区重叠的桌面角落
        （右下优先逆时针，纯函数有单测；仅 Linux——Win/mac 多屏 DPI 逻辑坐标
        换算不可靠，维持 WM 默认摆放），四角都避不开（如全屏录制）时显示
        「会被录入成品」橙色提示。实测（Deepin/X11）：边条约 200ms 后可见
        （合成器重绘延迟，录制场景无影响），选区内 0 边条像素、drop 后 0 残影。
- [x] **识别结果面板可拖动 + 可缩放**（✅ 2026-08-21）：QR / OCR 结果窗口
      （ui/mod.rs:683 `show_results`）原先 `.anchor(Align2::CENTER_CENTER)` +
      `.resizable(false)`。**anchor 就是拖不动的根因**——egui 对锚定窗口每帧强制
      写回位置，拖拽位移当帧即被覆盖；而结果框恰好盖在选区中央，挡住的正是刚识别
      的那段原文，无法对照校对
      - anchor 换成 `.default_pos(viewport_rect().center())` + `.pivot(CENTER_CENTER)`：
        pivot 让 default_pos 仍按窗口中心解释，保住首帧居中的观感，之后位置交给
        egui 记忆。注意 egui 0.35 的入口是 `InputState::viewport_rect()`，
        `screen_rect()` 已改名
      - `.resizable(true)` + `.min_width(260)` + `.constrain(true)`（防拖出屏幕外
        再也抓不回来）；`default_width` 460 / `default_height` 340 给初始观感
      - 高度改为随窗口走：`ScrollArea::auto_shrink([false, false])` 撑满，去掉固定
        `max_height(320)`（原先拉大窗口也看不到更多内容）。文本上限 600 → 4000 字，
        仍留上限是因为 egui 会为不可见文本做布局；「复制内容」始终复制完整原文
      - 覆盖层是全屏窗口，拖动在其内部完成，不涉及系统窗口移动；结果面板打开期间
        的按键屏蔽逻辑（ui/mod.rs:496，只留 Esc）保持不变
      - 滚动截图预览复用同一个 SnipApp 会话，自动同步受益

### 录制体验增强（2026-08-24，实际使用反馈）

- [x] **录制不直接开始（armed 状态）**：状态窗先显示「● 待开始」+
      「开始录制 (Enter)」按钮，点按钮或按 Enter 才开录；Esc/关窗/Ctrl+C
      在 armed 阶段 = 取消，静默退出（退出码 0，不产文件）。实现：
      `started: Arc<AtomicBool>`，录制线程进入采帧前先轮询等待（50ms 步进），
      stop 先到即取消；时长从真正开录起算。滚动截图无 armed（一进就滚）
- [x] **录制中边框红/蓝闪烁**：`RecordBorder::set_color`（改
      background_pixel + clear_area 强制重绘），run_record 的闪烁线程在
      started 后每 400ms 红蓝交替；armed 阶段保持静态红边。真机采样
      验证红蓝交替正确、边条仍在选区外侧
- [x] **录制格式进配置**：`config.toml` 新增 `record_format`（gif/mp4，
      默认 gif，非法值按 gif；旧配置无此字段走默认）。CLI `--mp4` 显式
      指定优先于配置。配置面板「保存」卡片加「录制格式」下拉。
      quality 缺省值改为按合并后的格式取（mp4=4000kbps / gif=90）
- [x] **`open_dir_after_save` 未覆盖录制/滚动**：原先只有 GUI 截图保存
      走自动打开目录；record（GIF/MP4）与 scroll `-o` 落盘后同样按配置
      打开所在目录

### M11 截图历史（托盘「历史」→ 缩略图面板，最近 10 张）✅ 2026-08-25

托盘菜单点「历史」打开一个**无边框置顶浮窗**（Snipaste 同款思路：原生菜单
画不了缩略图，用自绘窗口展示缩略图网格）。单击缩略图**复制**（截图/贴图）或
**打开目录并选中**（录屏）；右键贴图/打开/删除。解决「刚截的图找不到去哪了」
的高频痛点。

- [x] **记录时机**：历史 = 每次「产出图片」时追加一条记录，而非事后扫目录。
      原因：保存目录是用户任意指定的、可能混入非 lscreen 图片。写入点收敛在
      `history` 模块，各产出路径统一调用：
      - 截图保存（`ui/mod.rs` `save_and_exit`）、复制（`copy_and_exit`）
      - 贴图保存按钮（`pin.rs` `do_save`）、贴图创建（`pin_and_exit` +
        `pin_from_clipboard`）
      - CLI 直出（`main.rs` `run_shot` / `run_scroll` 显式 -o 分支）
      - **录屏落盘时**（`main.rs` `run_record`，GIF/MP4 都入）
- [x] **存储形态（小而美）**：独立历史目录 `~/.cache/lscreen/history/`
      （三平台 `config::cache_dir()`：Win `%LOCALAPPDATA%\lscreen`、
      mac `~/Library/Caches/lscreen`），每项存一份**全尺寸 PNG 副本**，索引 `index.toml`
      记录（时间戳、来源类型、尺寸，按时间倒序）。上限 `history_max`（默认 10，
      1-50），append 超限裁最旧。**存副本而非只记路径**：再复制/贴图必须能读到
      原图，源文件可能被移动或删除；自包含副本保证历史永远可点开。代价：磁盘约
      N×单张 PNG 大小（10 张 1080p 截图约 20–40MB，可控）。无历史时面板显示
      「暂无历史」占位，不报错不落盘。
      **为何是缓存目录而非配置目录**：历史副本是可再生的派生数据，删掉只丢便利、
      不丢设置；放 config 会让备份/同步工具连着几十 MB 图片一起搬，也违反 XDG
      语义（config 存"用户的选择"，cache 存"可随时丢弃的中间产物"）。**不做旧目录
      迁移**（用户定稿）：历史是易失数据，为它写一套跨文件系统搬迁+回滚不值得，
      老的 `<config>/history/` 用户自行删除即可。运行期小文件（单例锁、唤起信号）
      同样落 cache——它们也是「随时可丢」的状态。
- [x] **可见的体积 + 一键清空**：面板顶栏显示「历史 · N 张 · X MB」
      （`total_bytes()` 累加索引内文件 size），旁边「清空」按钮走二次确认
      （`confirm_clear` 状态，再点一次才执行）→ `clear_all()` 删所有副本 + 重写
      空索引。**只删索引登记过的文件**，不 `rm -rf` 目录，避免误删同目录他物。
      让"该清了"在 UI 上可见，而不是等用户自己翻磁盘发现几百 MB。
- [x] **面板浮窗（`history.rs` `HistoryApp`）**：`egui::Panel::top`(标题+计数+
      ✕) + `CentralPanel` 里 `ScrollArea` 缩略图列表。每行一张**满宽卡片**：左缩略图
      固定高 120px、按图高比缩放并**吃满行内剩余宽度**（横版图不再"框宽 item
      窄"），右栏类型/时间/分辨率三行居中、宽度随图片收敛（竖版窄图下文字保持
      配角）。悬停覆盖**整行**（含文字，红框 + 淡色底），**鼠标可拖拽滚动**
      （`ScrollSource::default() | DRAG`，egui 默认拖拽只在触摸屏生效）。
      单击按 kind 分流：Shot/Pin 复制、Record 用默认播放器播放 `source` 视频
      （`open_with_default`，不再打开缩略图）；点击/播放/打开都有**底部 toast 反馈**，
      按 `history_close_after_copy` 可在复制成功后自动收面板。右键缩略图
      贴图/打开目录/删除。`refresh_if_changed` 轮询 `index.toml` mtime，
      新截图落盘面板自动出现。Esc/✕ 关闭。
- [x] **托盘入口**：`tray.rs` 的 `Action::History` = `spawn_detached(["history"])`，
      `MENU_ACTIONS` 在「滚动截图」与「配置」之间插「历史」（Linux ksni 与
      Win/mac tray-icon 都走普通菜单项，不再用子菜单）。`main.rs` 恢复
      `Cmd::History` + `run_history`（280×420 无边框置顶窗 + `HistoryApp`），
      摆放改为**主屏右下**（`primary_monitor_bounds`）——Deepin 的 dock 在主屏，
      历史面板贴主屏而非虚拟桌面右边界。
- [x] **单实例（PID 锁）**：热键/菜单连按会不断 spawn 新 `lscreen history` 进程，
      叠出一堆面板。`acquire_single_instance()` 在 `run_history` 最开头抢
      `<cache>/history.lock`（内容 = 本进程 PID）：锁内 PID 仍存活则本进程直接
      退出，不建窗口；正常关闭时 `release_single_instance()` 比对 PID 后删锁。
      **存 PID 而非纯文件存在性**：进程崩溃留下的 stale 锁会被下次启动识别为死
      PID 并覆盖，不会把用户永久锁在门外。存活探测 `kill(pid, 0)`（unix，
      EPERM 也算活）/ `OpenProcess`（Windows）。
- [x] **再按热键把面板唤到前台**：单例只是「不开第二个」，面板在后台时用户会
      觉得按了没反应。第二个进程退出前留下 `<cache>/history.raise` 信号文件，
      运行中的面板 `poll_raise` 每 300ms 读一次，命中就消费掉并依次发
      `Minimized(false)` → `Visible(true)` → `Focus`（顺序有讲究：先恢复可见
      才有窗口可聚焦）。**关键是 `request_repaint_after(300ms)` 保持心跳**——
      窗口在后台没有输入事件，eframe 不会主动重绘，纯事件驱动永远轮询不到信号，
      这正是「按了热键像没反应」的根因。用文件而非 D-Bus/socket：面板本就轮询
      `index.toml` mtime 刷新列表，复用同一条轮询，不为一个信号引入 IPC 依赖。
- [x] **录屏缩略图**：GIF/MP4 均**录制时留首帧**——`record_mp4`/`record_gif`
      采帧时把第一帧 RGBA 存入共享槽，录毕另存 `_poster.png` 并记一条，
      `source` 指向实际 GIF/MP4 文件。**录屏点击不复制也不定位**：`source`
      存在且未失效时用系统默认播放器播放视频（`export::open_with_default`），
      源已删/旧条目无 source 时退化为打开目录。
- [x] **打开目录并选中**：`export::open_and_select(path)` 三平台定位文件：
      Windows `explorer /select`、macOS `open -R`、Linux FileManager1 D-Bus
      `ShowItems`（失败降级 `xdg-open` 目录）。zbus 升为 app 直接依赖（已随
      ashpd 在依赖树，不增体积）。右键「打开目录」走此路径。
- [x] **配置**：`config.rs` 增 `history_max: usize`（默认 10）与
      `history_close_after_copy: bool`（默认 false，历史面板复制后自动收）；
      `settings_ui.rs`「保存」卡片增「历史条数」（DragValue 1–50，保存时校验
      非法提示不落盘）；「全局热键」增「历史」一行（`hotkey_history`）。
- [x] **README 同步**：托盘菜单文案补「历史」面板；CLI 表补 `history` 子命令；
      配置节补历史副本存缓存目录的三平台路径与「可直接删」说明。
      `docs/PLAN.md` §6 目录规范补 `app/src/history.rs`。

### 遗留 TODO（review 2026-08-17）

- [x] clipd 静默失败（✅ 2026-08-18）：守护进程在 X 连接 + 协议校验全部通过后
      向 stdout 回写确认字节，父进程读到 ack 才返回 Ok；子进程提前退出则读到
      EOF，报"守护进程启动失败"
- [x] clipd 僵尸进程（✅ 2026-08-18）：父进程用分离线程 wait 子进程；
      不用 `signal(SIGCHLD, SIG_IGN)` 是因为它会全局生效，
      破坏 OCR tesseract 子进程的 wait_with_output
- [ ] Win/mac 指针查询（capture/src/other.rs cursor_position 返回 None）：
      windows-rs GetCursorPos / objc2 NSEvent.mouseLocation，补齐后多屏跟随生效

### 已修缺陷（review 2026-08-18）

- [x] 多屏负坐标区域截屏错位：capture_region 原先钳到根窗口 [0,w]×[0,h]，
      显示器位于主屏左侧/上方（原点为负）时区域被折进主屏；改为钳到
      全部显示器并集。`--region` 参数加 allow_hyphen_values，
      负坐标无需 `--region=-x,…` 等号写法
- [x] 双击与点击型工具冲突：Marker 连点第二击被「双击=复制退出」吞掉并误退出、
      Text 连点在编辑器下遗留空文本图元。现在点击型工具（Marker/Text）的
      双击是连续放置不触发复制；文本编辑模态化，点击画布任意处=提交
- [x] record_gif 失败路径：采帧出错提前 return 会丢下分离的编码线程和
      半成品文件；统一收尾（join 编码线程 + 删除残缺产物）。
      CLI 对 --fps/--quality 提前校验而非静默 clamp
- [x] 空撤销步：点选图元未拖动也压快照，Ctrl+Z 出现一次"无反应"；
      快照推迟到首次真实位移才压（点选不再清空重做栈），松手时
      若快照与现状一致则弹出（History::drop_noop）
- [x] 结果面板（QR/OCR）打开时 Ctrl+C/Enter 仍会复制退出，误触关窗；
      面板期间只保留 Esc 关面板
- [x] Wayland 检测过严/过松：原先要求 WAYLAND_DISPLAY 与 XDG_SESSION_TYPE
      同时命中，缺 SESSION_TYPE 的纯 Wayland 会话漏判报底层错误；改为
      会话类型为 wayland 或（无 DISPLAY 且有 WAYLAND_DISPLAY）即明确报错
- [x] save_png 按扩展名猜格式：`-o foo.jpg` 报 RGBA→JPEG 的费解错误；
      现在无扩展名补 .png，非 png 明确报错"仅支持 PNG 输出"
- [x] 默认保存同秒覆盖：时间戳秒级分辨率，同秒两次保存自动追加序号
- [x] 每帧 clone 整个 elements 列表（画布绘制）与 mosaic_cache 只增不减：
      拆字段借用消 clone，帧末按现存图元回收缓存
- [x] CI 补 fmt/clippy 门槛（原先仅构建/测试/体积）

## 5. 已知风险与对策

| 风险 | 影响 | 对策 |
|---|---|---|
| Wayland 禁止直接抓屏 | Linux 部分桌面不可用 | ✅ M5：portal 整屏截图已通（Deepin 真机验证）；区域采帧/录屏仍 X11 only，覆盖层 GUI 待真实 Wayland 会话验证 |
| 常驻进程内存膨胀 | 违反"小而美" | 常驻体不持有截图缓冲；截图/贴图窗口关闭即释放纹理与 RGBA；✅ M8 实测托盘空闲 RSS ≈ 9MB |
| 全局热键被占用/注册失败 | 热键静默失效 | ✅ 已实现：注册返回值逐条检查，失败告警（含默认 F1 与 Deepin 系统键冲突的实测经验）并保留托盘菜单手动入口；仍支持桌面环境绑定 `lscreen gui` 命令 |
| X11 剪贴板随进程退出丢失 | 复制不可靠 | ✅ 已解决：分离守护子进程（arboard wait()）持有剪贴板，被覆盖后自动退出；遗留确认回执/僵尸收割见「遗留 TODO」 |
| 混合 DPI 多显示器 | 覆盖层/坐标错位 | 单屏已自洽（View 比例映射）；多屏混合 DPI 在 M5 与 capture_all 一并处理 |
| 纯 Rust 无 H.264 编码器 | MP4 依赖问题 | ✅ 已解决：Linux openh264 静态链接（零动态库），release 12.4MB 在预算内；Win/mac 系统编码器待实现 |
| 滚动截图拼接不稳 | 长图错位 | ✅ 尾部块两阶段匹配 + SAD 阈值校验；悬浮头/动画返回 Mismatch 即停保留已拼部分 |
| egui 版本 API 变动快 | 升级成本 | 锁定 minor 版本，UI 层薄、核心不受影响 |
| 产物漏动态库依赖 | 用户机器上打不开 | ✅ v0.5.1：MSVC 默认动态链 CRT，v0.5.0 的 Windows 包在没装 VC++ 运行库的机器上缺 VCRUNTIME140.dll 直接打不开。`.cargo/config.toml` 开 `+crt-static`；`package.sh` 的 `check_win_deps` 扫导入表在出包期拦住（**打包机装过运行库，这类缺陷本地永远测不出来，只能在流水线卡**）。Linux 侧同类门槛见 ci.yml 的 ldd 白名单 |

## 6. 目录规范

```
docs/           设计与计划文档（PLAN.md）+ 项目主页（index.html）
  release-notes/  每个版本一个 <tag>.md，release.yml 直接取作 GitHub Release 正文
scripts/        package.sh 一键打包
packaging/      图标、desktop、Info.plist 等打包素材
crates/         所有库与可执行 crate
  core/src/     model.rs(图元) history.rs(撤销栈) render.rs(导出渲染)
                geom.rs color.rs qr.rs
  capture/src/  lib.rs(平台分发) linux.rs(x11rb + ashpd portal) other.rs(xcap)
  ocr/src/      lib.rs(trait) tesseract.rs win_ocr.rs vision.rs
                ocrs_engine.rs(内置兜底) lang.rs
  record/src/   lib.rs(GIF/MP4 编码 + 采帧) scroll.rs(滚动拼接)
  app/src/      main.rs(CLI 入口) ui/(mod 覆盖层 / toolbar / canvas)
                history.rs(截图历史面板) tray.rs(托盘) settings_ui.rs(配置面板)
                pin.rs(贴图) record_ui.rs(录制状态窗) export.rs config.rs font.rs
  setup/        Windows 自绘安装器
```

## 7. 发布规范

每个版本在 `docs/release-notes/<tag>.md` 留一份变更日志。这不是归档习惯，而是
release.yml 的输入：tag 触发时它 `cp docs/release-notes/$GITHUB_REF_NAME.md`
作为 GitHub Release 正文，文件不存在就落兜底文案「此版本未提供发布说明，详见提交历史」。

**发布说明必须先于 tag 提交**——工作流读的是 tag 指向的那次提交的工作树，
事后补文件不会回填线上正文（v0.6.0 即如此，只能用 `gh release edit --notes-file` 手工修）。

发布顺序（不可逆的步骤放最后）：

1. 写 `docs/release-notes/<tag>.md`
2. 改版本号：根 `Cargo.toml` 的 `[workspace.package] version`、各 crate 间
   path 依赖的 `version` 引脚（不一致 cargo 直接报错）、`cargo update -w` 刷 lock
3. 同步 README 的 tag 示例与 `docs/index.html` 的兜底版本号（正常由 GitHub API
   覆盖，这里只是接口不可用时的回退）、PLAN 的版本历程表
4. 验证：`cargo fmt --all --check`、`cargo clippy --all-targets --all-features`、
   `cargo test --workspace`
5. commit + push main
6. `git tag -a <tag> && git push origin <tag>`，触发 Release 工作流出全平台包

撤回已发布的版本：`gh release delete <tag> --yes` 之后**还要**
`git push origin :refs/tags/<tag>`——`--cleanup-tag` 实测不可靠会留下远端 tag。
被撤回的版本在版本历程表里保留并标注去向，不要删行；已删 release 的
`<tag>.md` 也一并删除，避免留下指向不存在 release 的说明。

写什么：按「用户看得见的影响」而不是提交分类组织。修复类写清**为什么此前没暴露**
（多数是环境差异：打包机装过运行库、开发机有显卡驱动），这比罗列改了哪个文件有用。
