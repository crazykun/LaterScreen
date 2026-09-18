# M17 真机验证手册

CI 无桌面环境，Win/mac/Wayland 的 GUI 能力只能真机人工验证。本手册是
[PLAN.md](PLAN.md) M17 台账的操作文档：**脚本项**一条命令出结果，**人工项**
写清步骤与预期。验证通过的项回 PLAN.md M17 勾选并注明日期与机器环境。

## 通用约定

- 脚本项都需要源码树 + Rust 工具链：`git clone && cargo build --release`
- 环境门：`LSCREEN_TEST_E2E=1`（GUI/OCR 套件）、`LSCREEN_TEST_AUDIO=1`
  （音频套件）。不带环境变量直接跑 `--ignored` 测试会静默跳过（防误触发）
- 音频测试追加 `LSCREEN_TEST_AUDIO_KEEP=1` 可保留 MP4 产物供人工播放检查
- 人工项建议用**安装包安装的正式产物**验证（脚本项验证的是源码构建，
  覆盖代码路径；安装器本身的验证见 Windows 人工项）

---

## Windows

### 脚本项

```powershell
# 1. 窗口枚举 Z 序 + 全屏截屏（M9 / M4 采集链路）
#    预期：打印「截屏 WxH（scale=1）+ N 个顶层窗口」与最前窗口标题
$env:LSCREEN_TEST_E2E=1
cargo test -p lscreen-capture --test windows_e2e -- --ignored --nocapture

# 2. 系统 OCR 中英文（M3，Windows.Media.Ocr；测试图现场渲染）
#    预期：识别结果含中文、「hello」与数字
cargo test -p lscreen-ocr --test system_e2e -- --ignored --nocapture

# 3. 录屏音频·麦克风（M14 盲写路径真机点验：WASAPI 采集 + MFT AAC）
#    预期：「N 视频帧 + M 音频帧（≈3.4s），A/V 偏差 <0.5s」
$env:LSCREEN_TEST_AUDIO=1
cargo test -p lscreen-record audio_e2e_mic -- --ignored --nocapture

# 4. 录屏音频·系统声（WASAPI loopback 回录）
#    ⚠ 先让系统出声再跑（真机实测：无渲染流时 loopback 不产包，测试会
#    零数据失败）：另开 PowerShell 循环播放 $p = New-Object Media.SoundPlayer
#    'C:\Windows\Media\Alarm01.wav'; while(1){$p.PlaySync()}，播着不关
#    预期：同上；建议加 LSCREEN_TEST_AUDIO_KEEP=1 播放产物听声音
cargo test -p lscreen-record audio_e2e_system -- --ignored --nocapture
```

### 人工项

- **托盘 + 全局热键（M8）**：裸运行 `lscreen`（无窗口驻留托盘）→ 托盘图标
  出现；右键菜单各项可点（截图/贴图/历史/退出）；按 F1 直接进入截图覆盖层；
  改 `config.toml` 热键后 1 秒内热加载生效（托盘不重启）。
- **覆盖层默认选区（M9）**：`lscreen gui` → 覆盖层出现时默认框住当前最前
  窗口（Z 序正确）；鼠标悬停其他窗口时高亮跳转正确。
- **贴图全套（M12）**：截图后点工具栏贴图 → Shift+滚轮调不透明度（20–100%）；
  穿透按钮后点击穿到下层窗口；R/H/V 旋转翻转；放大到 8x 以上出现像素网格。
- **安装器 + 卸载（v0.9 前置）**：运行 setup exe → per-user 安装到
  %LOCALAPPDATA%\lscreen（不弹 UAC）；开始菜单/桌面入口可启动；「设置→应用」
  卸载后目录清空；**运行中卸载**应弹出「程序正在运行」类失败提示而非静默删半截。
- **历史面板高缩放定位（v0.8.1）**：系统缩放设 125% 与 150% 各验证一次：
  托盘「历史」面板出现在右下任务栏上方、不飞屏不越界；关掉重开位置记忆正确；
  拔掉外接屏后面板不「打开即消失」。
- **MP4 录屏 GUI 出片（M4）**：`lscreen record --mp4` → armed 状态窗 → 点开始
  → 录 5 秒停止 → 产物可播放、帧率/时长正确、点击处有高亮圆环
  （配置开启时）；齿轮热改格式/音频对本次录制生效。
- **滚动截图（v0.8.0，SendInput）**：浏览器开一篇长文 → `lscreen scroll` →
  框选内容区 → 自动滚动拼合出长图，接缝无重影/撕裂；滚动条页面与
  平滑滚动页面各试一次。

---

## macOS

### 脚本项

```bash
# 1. 窗口枚举 + 全屏截屏（含 Retina 逻辑↔物理换算，M9 / v0.8.0）
#    预期：打印「截屏 WxH（scale=2.0 或 1.0）...」；Retina 屏 W/H 应为
#    逻辑分辨率的两倍
LSCREEN_TEST_E2E=1 \
  cargo test -p lscreen-capture --test windows_e2e -- --ignored --nocapture

# 2. Vision OCR 中英文（M3）
LSCREEN_TEST_E2E=1 \
  cargo test -p lscreen-ocr --test system_e2e -- --ignored --nocapture

# 3. 录屏音频·麦克风（M14 盲写路径真机点验：CoreAudio + AudioToolbox）
#    首次运行系统弹麦克风权限（TCC），需允许终端/cargo
#    预期：A/V 偏差 <0.5s；KEEP=1 保留产物可听到录音
LSCREEN_TEST_AUDIO=1 LSCREEN_TEST_AUDIO_KEEP=1 \
  cargo test -p lscreen-record audio_e2e_mic -- --ignored --nocapture

# 4. 录屏音频·系统声（v0.11 盲写路径真机点验：ScreenCaptureKit，macOS 13+）
#    权限走「屏幕录制」（与截图同一 TCC 权限，正常使用已授予）
#    ⚠ 运行期间必须有声音在播放（如音乐）：静默桌面 SCK 可能整程零产包，
#      双轨断言会失败（同 Win loopback 行为）
#    预期：A/V 偏差 <0.5s；KEEP=1 保留产物可听到刚才播放的内容
LSCREEN_TEST_AUDIO=1 LSCREEN_TEST_AUDIO_KEEP=1 \
  cargo test -p lscreen-record audio_e2e_system -- --ignored --nocapture
```

### 人工项

- **托盘 Accessory（v0.6.1）**：裸运行 `lscreen` → 托盘图标出现、**不占
  Dock**（App 激活策略 Accessory）；左键单击出菜单、右键同；菜单项可用。
- **Retina 窗口矩形（M9 / v0.8.0）**：`lscreen gui` → 默认选区精确贴住最前
  窗口边缘（无 1-2px 错位、无半个标题栏混入）；窗口截图产物为物理像素尺寸。
- **贴图全套（M12）**：同 Windows 条目。
- **滚动截图（v0.8.0，CGEvent 滚轮合成）**：同 Windows 条目；另验触控板
  惯性滚动下的拼合质量。
- **MP4 录屏 GUI 出片（M4）**：同 Windows 条目（Retina 下清晰度正常）。
- **系统声内录 GUI 出片（v0.11，ScreenCaptureKit）**：播放音乐 →
  `lscreen record --select --mp4 --audio system` → 录 5-8s → 产物含音乐、
  无爆音/变速；`--audio both` 同时含麦克风与音乐（饱和混合）。macOS 12.x
  机器上 `--audio system` 应报「需 macOS 13.0+」而非崩溃（如有旧系统）。

---

## Wayland（GNOME 或 KDE 任一真会话）

- **portal 交互式截图（M5）**：Wayland 会话运行 `lscreen gui` → 系统截图
  对话框出现 → 选区确认 → 进入标注预览（非直接退出/黑屏）；复制/保存正常。
- **GlobalShortcuts 热键（M5）**：首次绑定走系统授权对话框（KDE 与 GNOME
  的 UX 不同，分别描述记录）；绑定后按热键进入截图；拒绝授权后程序不崩。
- **portal 整屏多屏坐标映射（M5）**：双屏 Wayland 会话 `lscreen shot` →
  产物对应正确的屏幕与区域，无左右屏互换/偏移。

## 混合 DPI 双屏

- **Windows**：主屏 100% + 副屏 125%/150%：覆盖层铺满虚拟桌面、选区坐标
  不错位；贴图/历史面板在副屏打开位置正确。
- **macOS**：Retina 内屏 + 外接 1x 屏：外接屏上窗口矩形换算正确（窗口截图
  不裁偏）；跨屏拖动贴图不糊。
- **X11 假混合 DPI**：`xrandr --output <名> --scale 1.5x1.5` 后覆盖层仍
  铺满、坐标不漂（回退路径，M5）。

## 其他 WM（GNOME/i3，v1.0 不强求，可等社区反馈）

- 跨屏覆盖层 `_NET_WM_FULLSCREEN_MONITORS` 铺满虚拟桌面、可跨屏框选；
- 窗口定位（录制状态窗、历史面板）不被 WM 装饰/摆错工作区。

---

## 验证记录规则

1. 脚本项失败：贴完整 panic 输出开 issue，标注系统版本/设备（如 Win11
   23H2 / MacBookPro M2 macOS 15.x）。
2. 通过一项：回 PLAN.md M17 对应条目把子项改写为 ✅ + 日期 + 机器；
   全平台清零后 M17 勾选，v1.0 解锁。
