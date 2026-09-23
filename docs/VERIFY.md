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
  穿透按钮后图片区点击穿到下层窗口、**工具条仍可操作（再点穿透按钮恢复，
  条带按钮缩放/关闭在穿透中也可用）**；穿透中点条带缩放（窗口变尺寸后条带
  输入区跟随，无「点空」死区）；R/H/V 旋转翻转；放大到 8x 以上出现像素网格。
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

### 前置：TCC 权限（cargo 测试进程归属 Terminal）

测试二进制无签名无 bundle，TCC 按责任进程归属 **Terminal**（或你跑
cargo 的终端 App）：

- **屏幕录制**：系统设置 → 隐私与安全性 → 屏幕录制 → 开启终端，然后
  **重启终端**（不重启对新进程不生效）。未授权时窗口枚举只看得到
  Menubar、SCK 系统声报「用户拒绝了…TCC」——这两项不是代码缺陷
- **麦克风**：无需预授权。首次跑 `audio_e2e_mic` 时系统弹授权框，
  CoreAudio 调用会**同步阻塞**到用户应答（实测 AudioDeviceStart 挂起
  ~60s），默认 5s 就绪等待必然超时——首跑请加
  `LSCREEN_AUDIO_READY_TIMEOUT_MS=120000`（1s–300s 钳位），点「允许」
  后授权落在 Terminal 上，之后不用再加
- **辅助功能（裸 F 键拦截用）**：CGEventTap 兜底层要的 TCC 权限，归属
  与上面同理——`cargo run` 开发态落在 **Terminal**，安装包态落在
  **lscreen 本体**。按下面人工项的步骤授权即可，未授权只是裸 F 键降级，
  其余功能不受影响
- 系统声测试期间必须有声音播放：`while true; do afplay
  /System/Library/Sounds/Glass.aiff; sleep 0.3; done` 后台挂着即可
  （mac 的 SCK 静默期也持续产包，与 Win loopback 不同，但保险起见
  仍保持有声）

### 脚本项

```bash
# 1. 窗口枚举 + 全屏截屏（含 Retina 逻辑↔物理换算，M9 / v0.8.0）
#    预期：打印「截屏 WxH（scale=2.0 或 1.0）...」；Retina 屏 W/H 应为
#    逻辑分辨率的两倍；窗口数应为真实桌面窗口数（仅 1 个 = 屏幕录制
#    权限未生效，见上节前置）
LSCREEN_TEST_E2E=1 \
  cargo test -p lscreen-capture --test windows_e2e -- --ignored --nocapture

# 2. Vision OCR 中英文（M3）
LSCREEN_TEST_E2E=1 \
  cargo test -p lscreen-ocr --test system_e2e -- --ignored --nocapture

# 3. 录屏音频·麦克风（M14 盲写路径真机点验：CoreAudio + AudioToolbox）
#    首次运行系统弹麦克风权限（TCC），需允许终端/cargo（见上节前置）
#    预期：A/V 偏差 <0.5s；KEEP=1 保留产物可听到录音
LSCREEN_TEST_AUDIO=1 LSCREEN_TEST_AUDIO_KEEP=1 \
  LSCREEN_AUDIO_READY_TIMEOUT_MS=120000 \
  cargo test -p lscreen-record audio_e2e_mic -- --ignored --nocapture

# 4. 录屏音频·系统声（v0.11 盲写路径真机点验：ScreenCaptureKit，macOS 13+）
#    权限走「屏幕录制」（与截图同一 TCC 权限，前置见上节）
#    ⚠ 运行期间必须有声音在播放（如音乐）：保险起见保持有声（SCK 在
#      mac 实测静默期也产包，但保持有声可同时验证非零数据）
#    预期：A/V 偏差 <0.5s；KEEP=1 保留产物可听到刚才播放的内容
LSCREEN_TEST_AUDIO=1 LSCREEN_TEST_AUDIO_KEEP=1 \
  cargo test -p lscreen-record audio_e2e_system -- --ignored --nocapture
```

### 人工项

- **托盘 Accessory（v0.6.1）**：裸运行 `lscreen` → 托盘图标出现、**不占
  Dock**（App 激活策略 Accessory）；左键单击出菜单、右键同；菜单项可用。
- **全局热键·裸 F 键拦截（CGEventTap 兜底层）**：macOS 默认（未开启
  「将 F1、F2 等键用作标准功能键」）把裸 F1–F12 翻译成媒体键，Carbon
  `RegisterEventHotKey` 收不到——兜底层在 kCGHIDEventTap 装**主动** tap
  消费按键并触发动作（Snipaste 同机制），需辅助功能权限。点验顺序：
  1. **未授权降级**：媒体键模式 + 无辅助功能 → 启动托盘（stderr 可见）
     出 remediation 告警、系统弹一次授权引导（隐私与安全性▸辅助功能）；
     此时 `fn+F1` 应立即可用
  2. **授权生效**：辅助功能列表添加/勾选承载进程（cargo 态=Terminal，
     安装包=lscreen）→ **不重启**，数秒内（tick 重同步 + 探测 3s 缓存）
     裸 F1 触发截图，且**屏幕亮度不变**（tap 把 keyDown 消费在媒体翻译
     之前）——亮度也跟着变 = 没拦住，见第 6 条
  3. **只拦已注册键**：未绑定的其他 F 键亮度/媒体功能照常；带修饰键
     （Ctrl/Alt/Shift/Cmd+F1）不受影响仍走 Carbon；按住 F1 自动重复只
     触发一次动作
  4. **标准功能键模式防双触发**：开启「将 F1、F2 等键用作标准功能键」
     （`defaults read -g com.apple.keyboard.fnState` = 1）→ 裸 F1 仍触发
     且**只触发一次**（tap 探测到该模式让位 Carbon）
  5. **运行中切换**：托盘常驻时切换上述系统开关 → ≤4s 内行为跟随
     （3s 探测缓存 + 1s tick）
  6. **HID 层假设（若 2 失败）**：裸 F1 无反应或亮度也变，说明该
     macOS 版本把媒体翻译放在 HID tap 之前——备用方案改
     `kCGSessionEventTap` + NX_SYSDEFINED 解码（mac_fnkey_tap.rs 头注
     释留了切换点），需按真机行为重写事件判别
  （2026-09-22 已自动化部分：`LSCREEN_TEST_E2E=1 cargo test -p lscreen
  -- --ignored --nocapture fnkey` 直调 tap 回调验证四分支语义 + 报告
  本机 fnState/AX 状态，真机 ✅；真托盘日志确认 `TapStatus::Installed`。
  **合成按键无法端到端**：CGEventPost 注入的事件不流经 HID 主动 tap、
  也不触发 Carbon 全局热键匹配——最后一英里（真键盘 → 拦截 → 动作）
  只能人工按第 2/3 条点验）
- **托盘热键 manager（mac 回归护栏）**：v0.11.1 前的 mac 托盘热键
  **自 M8 起全程失效**——构造期与 setup() 双重 `Hotkeys::new()`，第二次
  InstallEventHandler 因旧 handler 未卸返回 handlerAlreadyInstalled，
  manager 被替换为 None（上游 global-hotkey 把 OSStatus 吞成残留 errno
  22，报错文案误导向 Wayland 分支，长期未察觉）。修复：构造期改
  `Hotkeys::empty()`，setup() 为唯一创建点。点验：启动托盘 stderr
  **不再出现**「全局热键不可用（Wayland 会话无 X11？…）」；组合键热键
  （如 Ctrl+Alt+P）可触发动作（✅ 2026-09-22：报错消失、manager 创建
  成功、tap Installed；动作触发待真键盘点验）
- **覆盖层原地出现（弃用原生 fullscreen）**：mac 的 `with_fullscreen`
  会独占一个新 Space，覆盖层入场/退出必播横移切换动画（曾表现为「截图
  时屏幕往右切了一屏才弹出窗口」）。现改为无边界贴屏窗 + CanJoinAllSpaces
  + 菜单栏之上层级。点验（截图/录屏/取色三入口都要）：按热键 → 覆盖层
  **原地直接出现**（无任何 Space 动画），压住菜单栏与 Dock（顶部条带
  变暗）；Esc 原地退出回到原样。Retina 下冻结帧清晰度应与之前一致
  （逻辑坐标贴屏首次真正生效，若有模糊/半屏错位即换算问题）；多显示器
  时光标所在屏出现、另一屏不受影响；首帧应直接是冻结帧（若出现白闪，
  需补建窗 alpha 0→首帧后置 1 的处理）
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
