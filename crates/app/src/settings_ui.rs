//! 配置面板（M8）：`lscreen config` 打开的独立 eframe 窗口。
//!
//! 保存即写 config.toml；托盘进程监视 mtime 自动热加载（无需重启）。
//! 校验失败（模板/热键/颜色/工具名非法）只提示不落盘。
//!
//! 视觉：双主题（暗/浅，见 [`crate::theme`]，品牌色强调 + 卡片分域），
//! 颜色一律从 `ui.visuals()` 取令牌，仅作用于本窗口的 egui Context。

use crate::config::{self, Config};
use crate::export;
use crate::theme;
use eframe::egui;

// ---------------------------------------------------------------- 取色令牌

/// 主文字（跟随主题）
fn text(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().text_color()
}
/// 次级文字（标签/提示）
fn muted(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().weak_text_color()
}
/// 输入域底色（热键按钮/徽章）
fn field(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().widgets.inactive.weak_bg_fill
}
/// 卡片描边
fn line(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().widgets.noninteractive.bg_stroke.color
}
/// 品牌色作**文字**用（录屏提示/热键文案等）：走主题化前景色，别直接用
/// `theme::ACCENT`——品牌色当文字在浅色下不达 AA（红 3.6:1），见 theme::accent_ink
fn accent_text(ui: &egui::Ui) -> egui::Color32 {
    theme::accent_ink(ui.visuals())
}
/// 品牌色作**描边**用（聚焦/录入态边框）：比 ink 再推一档，见
/// `theme::accent_line`
fn accent_border(ui: &egui::Ui) -> egui::Color32 {
    theme::accent_line(ui.visuals())
}

// ---------------------------------------------------------------- 页签导航

/// 左侧导航分区：右侧内容连续滚动页里的分组锚点（见 CentralPanel 的
/// scroll-spy），不再是独立页签。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SettingsTab {
    #[default]
    General,
    Screenshot,
    Record,
    Ocr,
    Hotkeys,
    About,
}

impl SettingsTab {
    /// 导航顺序即展示顺序；label/icon 一一对应，新增变体时两处同步补
    /// （label 的 match 不带通配分支，漏加会编译失败）。
    const ALL: [Self; 6] = [
        Self::General,
        Self::Screenshot,
        Self::Record,
        Self::Ocr,
        Self::Hotkeys,
        Self::About,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::General => "通用",
            Self::Screenshot => "截图",
            Self::Record => "录屏",
            Self::Ocr => "文字识别",
            Self::Hotkeys => "热键",
            Self::About => "关于",
        }
    }

    /// 线稿图标与工具栏同一套约定（比例坐标，任意尺寸可画）。
    fn icon(self) -> fn(&egui::Painter, egui::Rect, egui::Color32) {
        match self {
            Self::General => crate::ui::toolbar::draw_sliders,
            Self::Screenshot => crate::ui::toolbar::draw_viewfinder,
            Self::Record => crate::ui::toolbar::draw_video,
            Self::Ocr => crate::ui::toolbar::draw_ocr,
            Self::Hotkeys => crate::ui::toolbar::draw_keyboard,
            Self::About => crate::ui::toolbar::draw_info,
        }
    }
}

/// 滚动侦测（scroll-spy）：视口顶部下探 `lead` 像素的判定线落在哪个分区
/// 区间内，就高亮哪个分区；滚到最底时强制选中最后一节——矮分区永远碰
/// 不到判定线，不兜底的话「关于」永远高亮不了。`tops` 为各分区起点在
/// 内容坐标系里的 y（同一空间内单调递增即可，与绝对原点无关）。
fn scroll_spy(tops: &[(SettingsTab, f32)], line: f32, at_bottom: bool) -> SettingsTab {
    let mut current = tops.first().map_or(SettingsTab::default(), |&(tab, _)| tab);
    for &(tab, top) in tops {
        if top <= line {
            current = tab;
        }
    }
    if at_bottom {
        if let Some(&(tab, _)) = tops.last() {
            current = tab;
        }
    }
    current
}

/// 导航项：图标 + 文字整行可点。扁平风选中态 = 淡 tint 底 + `accent_ink`
/// 前景 + 左侧品牌色指示条，未选中悬停 = field 灰。tint 按主题分：浅色是
/// 半透明品牌色（白卡片上淡红 tint 干净）；暗色是**中性提亮**（半透明白，
/// 压暗的红在暗底上发闷，用户反馈「选中的红色不好看」——红交给文字与
/// 指示条表达）。文字/图标走主题化前景色保证对比度（同 accent_ink 契约）。
fn nav_item(
    ui: &mut egui::Ui,
    selected: bool,
    label: &str,
    icon: fn(&egui::Painter, egui::Rect, egui::Color32),
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::click());
    let v = ui.visuals();
    let selected_fill = if v.dark_mode {
        // 中性高光：半透明白叠面板（勿写字面量色值，alpha 即全部自由度）
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 22)
    } else {
        // 半透明品牌色叠面板，色相自动跟随 theme::ACCENT
        egui::Color32::from_rgba_unmultiplied(
            theme::ACCENT.r(),
            theme::ACCENT.g(),
            theme::ACCENT.b(),
            26,
        )
    };
    if selected {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(8), selected_fill);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(8), field(ui));
    }
    let fg = if selected {
        theme::accent_ink(v)
    } else {
        text(ui)
    };
    if selected {
        // 左侧品牌色指示条：让「当前在哪一节」不依赖底色也能读出来
        ui.painter().rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(rect.left() + 2.0, rect.center().y),
                egui::vec2(3.0, rect.height() * 0.55),
            ),
            egui::CornerRadius::same(2),
            theme::accent_line(v),
        );
    }
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 24.0, rect.center().y),
        egui::Vec2::splat(16.0),
    );
    icon(ui.painter(), icon_rect, fg);
    ui.painter().text(
        egui::pos2(rect.left() + 40.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.5),
        fg,
    );
    resp
}

// ---------------------------------------------------------------- 卡片容器

/// 分区卡片：标题左侧一条强调色小竖条 + 圆角深色面板。扁平风：无描边，
/// 轮廓全靠卡片底色与窗口底的色调差（见 theme.rs 卡片令牌注释）。
fn card<R>(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(theme::card_fill(ui.visuals()))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(14))
        .outer_margin(egui::Margin {
            bottom: 10,
            ..Default::default()
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // 标题行：强调竖条 + 文字
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(3.0, 14.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, egui::CornerRadius::same(2), theme::ACCENT);
                ui.strong(egui::RichText::new(title).color(text(ui)).size(14.0));
            });
            ui.add_space(10.0);
            body(ui)
        })
        .inner
}

// ---------------------------------------------------------------- App

/// 用户尝试关闭配置面板时，未保存保护层给 UI 的处理结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseDecision {
    /// 当前配置没有改动，可以立即关闭窗口。
    CloseNow,
    /// 当前配置有未保存改动，需要先让用户确认是否放弃。
    ConfirmDiscard,
}

/// 配置编辑会话的未保存保护状态。
///
/// `saved` 来自窗口打开时加载的配置，或最近一次成功落盘的配置；UI 每帧把
/// 当前编辑值传入，从而统一判断保存按钮、Esc、关闭按钮和系统关闭事件。
struct UnsavedChanges {
    saved: Config,
    confirming_discard: bool,
}

impl UnsavedChanges {
    /// 以已落盘配置创建保护状态；输入来自配置窗口初始化，后续用于判断是否有改动。
    fn new(saved: &Config) -> Self {
        Self {
            saved: saved.clone(),
            confirming_discard: false,
        }
    }

    /// 比较当前编辑值和最近一次成功保存值；返回结果供保存按钮和关闭流程共用。
    fn is_dirty(&self, current: &Config) -> bool {
        current != &self.saved
    }

    /// 处理一次关闭请求；有改动时转入确认态，无改动时允许窗口直接关闭。
    fn request_close(&mut self, current: &Config) -> CloseDecision {
        if self.is_dirty(current) {
            self.confirming_discard = true;
            CloseDecision::ConfirmDiscard
        } else {
            CloseDecision::CloseNow
        }
    }

    /// 用户选择继续编辑后退出确认态；当前配置保持不变，仍然标记为未保存。
    fn continue_editing(&mut self) {
        self.confirming_discard = false;
    }

    /// 返回是否正在等待用户确认放弃修改，供确认弹窗决定是否展示。
    fn confirming_discard(&self) -> bool {
        self.confirming_discard
    }

    /// 在配置成功落盘后更新基线；输入是刚写入磁盘的配置，返回后的编辑态为干净。
    fn mark_saved(&mut self, current: &Config) {
        self.saved = current.clone();
        self.confirming_discard = false;
    }

    /// 放弃编辑并把当前配置恢复为最近保存值；调用方随后重新应用主题并关闭窗口。
    fn discard_into(&mut self, current: &mut Config) {
        *current = self.saved.clone();
        self.confirming_discard = false;
    }
}

pub struct SettingsApp {
    cfg: Config,
    /// 当前高亮的分区：点击导航立即更新，滚动时由 scroll-spy 跟随；
    /// 不持久化，每次打开默认「通用」。
    active_tab: SettingsTab,
    /// 待跳转的分区（点击导航设置，下一帧渲染到该分区起点时消费并瞬时
    /// 滚动过去）；跳转帧跳过 scroll-spy，防止旧滚动位置把高亮拉回去。
    jump_target: Option<SettingsTab>,
    /// 最近一次成功落盘的配置与关闭确认状态，用于统一保护所有未保存编辑。
    unsaved: UnsavedChanges,
    toast: Option<(String, f64)>,
    /// 正在捕获哪个热键字段（0=截图 1=取色 2=贴图）
    recording: Option<usize>,
    /// 捕获提示（裸键被拒等瞬时反馈）
    record_hint: Option<(String, f64)>,
    /// 热键下标越界时的写入兜底槽（正常路径用不到，见 `hotkey_field_mut`）
    hotkey_spill: String,
    /// 更新检测状态机（手动触发，见 `update` 模块）
    update: crate::update::UpdateCheck,
    /// OCR 模型探测/下载独立于可保存配置，不参与 dirty 比较。
    ocr_models: crate::ocr_models::OcrModels,
    ocr_models_dir: Option<std::path::PathBuf>,
    /// 只读启动环境，不在设置窗口修改全局环境变量。
    ocr_engine_env: Option<String>,
    /// 进行中的系统目录选择对话框（后台线程 → mpsc 回传）。
    /// None = 没有打开；Some(rx) = 等待用户选择/取消。
    dir_pick: Option<std::sync::mpsc::Receiver<Option<std::path::PathBuf>>>,
}

impl SettingsApp {
    /// 创建配置窗口；配置从磁盘加载，副本作为本次编辑会话的未保存判断基线。
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let cfg = Config::load();
        let unsaved = UnsavedChanges::new(&cfg);
        crate::apply_window_class(cc);
        theme::apply(&cc.egui_ctx, cfg.theme);
        // 独立窗口必须自己挂中文字体；先用 core Renderer 验证可解析
        // （epaint 对坏字体是 panic 而非 Err）
        let font = crate::font::load_system_font();
        if let Some(bytes) = font {
            if lscreen_core::render::Renderer::new(Some(bytes.clone())).has_font() {
                crate::font::setup_egui_fonts(&cc.egui_ctx, bytes);
            }
        }
        let mut ocr_models = crate::ocr_models::OcrModels::default();
        // 进门探测走静音通道：结果落卡片，不弹「检查完成」toast
        ocr_models.check_on_open(&cc.egui_ctx);
        Self {
            cfg,
            active_tab: SettingsTab::default(),
            unsaved,
            toast: None,
            recording: None,
            record_hint: None,
            hotkey_spill: String::new(),
            update: crate::update::UpdateCheck::default(),
            ocr_models,
            ocr_models_dir: lscreen_ocr::paddle::models_dir(),
            ocr_engine_env: std::env::var_os("LSCREEN_OCR_ENGINE")
                .map(|value| value.to_string_lossy().into_owned()),
            dir_pick: None,
            jump_target: None,
        }
    }

    fn toast(&mut self, ctx: &egui::Context, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ctx.input(|i| i.time) + 2.5));
    }

    /// 打开系统文件夹选择器（后台线程）。Win/mac 走原生对话框（模态自持
    /// 消息泵），Linux 走 XDG portal（D-Bus，rfd 0.17 的 pollster 阻塞执行
    /// 器）——都不能堵 egui UI 线程，结果经 mpsc 回帧循环；对话框开着时
    /// 重复点击被忽略。
    fn browse_dir(&mut self, ctx: &egui::Context) {
        if self.dir_pick.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.dir_pick = Some(rx);
        let ui_ctx = ctx.clone();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("选择保存目录")
                .pick_folder();
            let _ = tx.send(picked);
            ui_ctx.request_repaint();
        });
    }

    /// 保存：先全部校验再落盘，非法字段就地提示。
    fn save(&mut self, ctx: &egui::Context) {
        if let Err(e) = config::validate_template(&self.cfg.filename_template) {
            self.toast(ctx, format!("文件名模板无效: {e}"));
            return;
        }
        // 先取副本再校验：避免循环持有 &self.cfg 的同时调 self.toast 可变借用
        for (field, label) in crate::tray::HOTKEY_FIELDS.iter().map(|(f, l, _)| (*f, *l)) {
            let raw = crate::tray::hotkey_field(&self.cfg, field);
            if raw.trim().is_empty() {
                continue;
            }
            if let Err(e) = crate::tray::parse_hotkey(raw) {
                self.toast(ctx, format!("{label}热键「{raw}」无效: {e}"));
                return;
            }
        }
        // 重复组合：同组合绑两个动作时后者静默失效（HotKey 的 id 由
        // 修饰键+主键直接算出，事件只会命中先注册的那个），必须在落盘前拦下
        if let Some((first, second)) = crate::tray::duplicate_hotkey(&self.cfg) {
            self.toast(
                ctx,
                format!(
                    "「{first}」与「{second}」绑了同一个热键：同时只有一个会生效，请改掉其中一个"
                ),
            );
            return;
        }
        if config::parse_hex_color(&self.cfg.default_color).is_none() {
            self.toast(
                ctx,
                format!("默认颜色无效（应为 #RRGGBB）: {}", self.cfg.default_color),
            );
            return;
        }
        if config::tool_from_name(&self.cfg.default_tool).is_none() {
            self.toast(ctx, format!("未知工具名: {}", self.cfg.default_tool));
            return;
        }
        if !(1..=50).contains(&self.cfg.history_max) {
            self.toast(ctx, "历史条数无效（应为 1-50）".to_string());
            return;
        }
        match self.cfg.save() {
            Ok(()) => {
                // 只有真实落盘成功才推进基线；保存失败时必须继续保持未保存状态，
                // 否则用户关闭窗口会丢掉尚未写入磁盘的配置。
                self.unsaved.mark_saved(&self.cfg);
                self.toast(ctx, "已保存（托盘进程自动生效）");
            }
            Err(e) => self.toast(ctx, format!("保存失败: {e}")),
        }
    }

    /// 响应关闭按钮、Esc 或系统标题栏关闭；无改动直接关窗，有改动则打开确认框。
    fn request_close(&mut self, ctx: &egui::Context) {
        match self.unsaved.request_close(&self.cfg) {
            CloseDecision::CloseNow => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            CloseDecision::ConfirmDiscard => {
                // 关闭确认优先于热键录入，避免录入态继续吞掉确认框的 Esc。
                self.recording = None;
            }
        }
    }

    /// 绘制未保存确认框；放弃时恢复最近保存值和主题，继续编辑则只关闭确认框。
    fn discard_confirmation(&mut self, ctx: &egui::Context) {
        if !self.unsaved.confirming_discard() {
            return;
        }

        // 视觉与页面同语言：卡片标题条 + 弱化说明 + 底部栏同款按钮（等高、
        // 圆角 8、强调实心底/幽灵双形态）。安全动作「继续编辑」放最右：确认弹窗
        // 的默认位置给无损选项，误触回车/Esc 都不会丢修改。
        // 弹窗与页面同语言：主题化底色 + 柔和投影 + 深色半透明遮罩，
        // 扁平风下不再描边（悬浮层级靠投影与遮罩表达）
        let style = ctx.style_of(ctx.theme());
        let frame = egui::Frame::NONE
            .fill(theme::card_fill(&style.visuals))
            .corner_radius(12)
            .inner_margin(egui::Margin::same(20))
            .shadow(egui::epaint::Shadow {
                offset: [0, 10],
                blur: 28,
                spread: 0,
                color: egui::Color32::from_black_alpha(110),
            });
        let response = egui::Modal::new(egui::Id::new("settings-discard-confirmation"))
            .frame(frame)
            .backdrop_color(egui::Color32::from_black_alpha(120))
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                ui.horizontal(|ui| {
                    let (bar, _) =
                        ui.allocate_exact_size(egui::vec2(3.0, 16.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(bar, egui::CornerRadius::same(2), theme::ACCENT);
                    ui.strong(egui::RichText::new("放弃未保存的修改？").size(15.0));
                });
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("当前修改尚未保存，关闭后将无法恢复。").color(muted(ui)),
                );
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let continue_editing = ui
                        .add(
                            egui::Button::new(egui::RichText::new("继续编辑").color(text(ui)))
                                .corner_radius(8)
                                .min_size(egui::vec2(96.0, 34.0)),
                        )
                        .clicked();
                    ui.add_space(8.0);
                    let discard = ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("放弃修改")
                                    .color(theme::ACCENT_ON_TEXT)
                                    .strong(),
                            )
                            .fill(theme::ACCENT_FILL)
                            .corner_radius(8)
                            .min_size(egui::vec2(96.0, 34.0)),
                        )
                        .on_hover_text("恢复到上次保存的配置并关闭")
                        .clicked();
                    (discard, continue_editing)
                })
                .inner
            });
        let (discard, continue_editing) = response.inner;

        if discard {
            // 主题选择会即时预览，因此放弃编辑时必须在关窗前恢复已保存主题，
            // 避免窗口关闭动画或其它共享视图短暂保留未保存外观。
            self.unsaved.discard_into(&mut self.cfg);
            theme::apply(ctx, self.cfg.theme);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if continue_editing || response.should_close() {
            self.unsaved.continue_editing();
        }
    }

    /// 热键捕获：recording 期间吃掉全部按键事件。
    /// - 修饰键+主键 → 生成 "Ctrl+Alt+A" 并校验，非法（如裸字母）就地提示
    /// - Esc → 取消录制（保留原值）；Backspace/Delete → 清空绑定
    fn capture_hotkey(&mut self, ctx: &egui::Context) {
        if self.recording.is_none() {
            return;
        }
        let mut captured: Option<(String, egui::Modifiers, egui::Key)> = None;
        let mut cancel = false;
        let mut clear = false;
        ctx.input_mut(|i| {
            let mut keep = Vec::new();
            for ev in std::mem::take(&mut i.events) {
                // 录制期间所有按键事件（含释放）都吃掉，避免落到输入框/关窗
                if let egui::Event::Key {
                    key,
                    modifiers,
                    pressed: true,
                    ..
                } = ev
                {
                    match key {
                        egui::Key::Escape => cancel = true,
                        egui::Key::Backspace | egui::Key::Delete if modifiers.is_none() => {
                            clear = true
                        }
                        k if is_modifier_key(k) => {}
                        _ => captured = Some((hotkey_string(&modifiers, &key), modifiers, key)),
                    }
                } else if !matches!(ev, egui::Event::Key { .. }) {
                    keep.push(ev);
                }
            }
            i.events = keep;
        });
        let idx = self.recording.unwrap_or(0);
        if let Some((text, mods, key)) = captured {
            match crate::tray::parse_hotkey(&text) {
                Ok(hk) => {
                    // 重复组合就地拦下：同组合绑两个动作时后者静默失效（全局
                    // 热键事件按 HotKey id 命中先注册者）。这里**不**先把值写进
                    // cfg 再判——冲突时留在录制态并提示，用户看到的是「按了没
                    // 进去」，比事后才发现某个热键不工作好用
                    if let Some(other) = crate::tray::hotkey_conflict_with(&self.cfg, &hk, idx) {
                        self.record_hint = Some((
                            format!(
                                "「{text}」已被「{other}」占用：同一个组合绑两个动作时只有一个\
                                 会生效，请按其它组合或 Esc 取消"
                            ),
                            ctx.input(|i| i.time) + 4.0,
                        ));
                        return;
                    }
                    // mac 裸 F 键：Carbon 注册收不到媒体键模式事件，但托盘的
                    // CGEventTap 兜底层持有辅助功能权限时可直接生效，录入时就地提醒
                    if cfg!(target_os = "macos") && crate::tray::is_bare_fn_key(&hk) {
                        self.record_hint = Some((
                            format!(
                                "已录入「{text}」。macOS 默认将裸 F 键用作媒体键；授权辅助功能\
                                 （隐私与安全性▸辅助功能 添加 lscreen）后直接生效，未授权时\
                                 可用 fn+此键 或开启「将 F1、F2 等键用作标准功能键」"
                            ),
                            ctx.input(|i| i.time) + 8.0,
                        ));
                    }
                    *self.hotkey_field_mut(idx) = text;
                    self.recording = None;
                }
                Err(e) => {
                    let _ = (mods, key);
                    let msg = format!("「{text}」不可用：{e}");
                    self.record_hint = Some((msg, ctx.input(|i| i.time) + 1.5));
                }
            }
        } else if clear {
            *self.hotkey_field_mut(idx) = String::new();
            self.recording = None;
        } else if cancel {
            self.recording = None;
        }
    }

    /// 按下标取热键字段的可变引用。下标语义 = [`crate::tray::HOTKEY_FIELDS`]
    /// 的行序（UI 行、录制态索引、托盘注册顺序共用），改那边要同步这里。
    fn hotkey_field_mut(&mut self, idx: usize) -> &mut String {
        let Some((field, _, _)) = crate::tray::HOTKEY_FIELDS.get(idx) else {
            // 越界不应发生（下标由遍历产生）；真发生了就把写入丢到一个临时
            // 槽里而不是 panic——配置面板崩掉会连未保存的编辑一起丢
            return &mut self.hotkey_spill;
        };
        let field = *field;
        match field {
            "hotkey_screenshot" => &mut self.cfg.hotkey_screenshot,
            "hotkey_picker" => &mut self.cfg.hotkey_picker,
            "hotkey_pin" => &mut self.cfg.hotkey_pin,
            "hotkey_record" => &mut self.cfg.hotkey_record,
            "hotkey_scroll" => &mut self.cfg.hotkey_scroll,
            "hotkey_delay" => &mut self.cfg.hotkey_delay,
            "hotkey_history" => &mut self.cfg.hotkey_history,
            _ => &mut self.hotkey_spill,
        }
    }

    /// 热键捕获输入框：点击进入录制态，随后直接按键录入。
    fn hotkey_capture(&mut self, ui: &mut egui::Ui, idx: usize, value: &str) {
        let active = self.recording == Some(idx);
        // 扁平化：空闲/已设值都是纯填充无边框；唯一保留的描边是录入态的
        // 强调环——「正在监听键盘」需要一圈明确的焦点语义
        let (label, recording_ring) = if active {
            (
                egui::RichText::new("按下组合键…  (Esc 取消 / Backspace 清除)")
                    .color(accent_text(ui))
                    .monospace(),
                true,
            )
        } else if value.is_empty() {
            (
                egui::RichText::new("点击设置热键")
                    .color(muted(ui))
                    .monospace(),
                false,
            )
        } else {
            (
                egui::RichText::new(format!("{value}（点击修改）"))
                    .color(text(ui))
                    .monospace(),
                false,
            )
        };
        let mut btn = egui::Button::new(label)
            .min_size(egui::vec2(0.0, crate::theme::FIELD_H))
            .corner_radius(8)
            .fill(field(ui));
        if recording_ring {
            btn = btn.stroke(egui::Stroke::new(1.5, accent_border(ui)));
        }
        let resp = ui
            .add_sized([300.0, crate::theme::FIELD_H], btn)
            .on_hover_text("点击后按下想要的组合键（如 Ctrl+Alt+A）");
        if resp.clicked() {
            self.recording = if active { None } else { Some(idx) };
        }
        if active {
            ui.ctx().request_repaint();
        }
    }
}

/// 物理修饰键按下（Shift/Ctrl/Alt/Super 的左右变体）——不作为主键。
fn is_modifier_key(k: egui::Key) -> bool {
    matches!(
        k,
        egui::Key::ShiftLeft
            | egui::Key::ShiftRight
            | egui::Key::ControlLeft
            | egui::Key::ControlRight
            | egui::Key::AltLeft
            | egui::Key::AltRight
            | egui::Key::SuperLeft
            | egui::Key::SuperRight
    )
}

/// 按下事件 → 热键文案（与 tray::parse_hotkey 的书写约定一致）。
fn hotkey_string(mods: &egui::Modifiers, key: &egui::Key) -> String {
    let mut s = String::new();
    if mods.ctrl {
        s.push_str("Ctrl+");
    }
    if mods.shift {
        s.push_str("Shift+");
    }
    if mods.alt {
        s.push_str("Alt+");
    }
    if mods.mac_cmd {
        s.push_str("Meta+");
    }
    // egui Key::name 与 parse_key 的别名差异
    s.push_str(match key.name() {
        "Equals" => "Equal",
        "Backtick" => "`",
        n => n,
    });
    s
}

impl eframe::App for SettingsApp {
    /// eframe 默认清屏色是恒定的半透明黑（无视主题），透明面板后面就是它
    /// ——浅色模式下会露出黑底。改为跟随主题的面板底色。
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    /// 绘制配置面板并统一处理保存快捷键和三种关闭入口的未保存保护。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // 模型任务完成时给一次明确 toast：此前「校验」跑完没有任何可见
        // 反馈（只有两行灰字回到点击前的样子），用户看不出结果。
        // 打开窗口时的自动探测（silent）不弹——每次进门都「检查完成」
        // 是噪音，状态在卡片里本来就看得到
        if let Some(done) = self.ocr_models.poll(&ctx) {
            if done.silent {
                // 静默任务的终态只落卡片
            } else {
                let msg = match (done.kind, done.ok) {
                    ("check", true) if done.present => "检查完成：PP-OCR 模型已就绪",
                    ("check", true) => "检查完成：本机未安装 PP-OCR 模型",
                    ("download", true) => "下载完成：PP-OCR 模型已就绪",
                    ("verify", true) => "校验通过：模型文件完整",
                    ("check", false) => "模型检查未通过",
                    ("download", false) => "模型下载失败",
                    _ => "模型校验未通过",
                };
                self.toast(&ctx, msg);
            }
        }
        // 两个后台状态机都在帧级收尾：切走「关于」页后，进行中的更新检查
        // 也要被 poll（结果留在槽里等下次进页会显示，但状态要先落定）
        self.update.poll();
        // 目录选择对话框回填：路径直接落进未保存的编辑副本（走常规保存流程）
        if let Some(rx) = self.dir_pick.as_ref() {
            if let Ok(picked) = rx.try_recv() {
                self.dir_pick = None;
                if let Some(path) = picked {
                    self.cfg.save_dir = path.to_string_lossy().into_owned();
                    self.toast(&ctx, format!("已选择目录：{}", path.display()));
                }
            }
        }
        // 热键捕获必须最先处理：吃掉所有按键事件（含 Esc，防止录制中关窗）
        self.capture_hotkey(&ctx);
        // Wayland 自绘标题栏：✕ 走与底部「关闭」相同的未保存确认流程
        if lscreen_capture::is_wayland()
            && crate::ui::toolbar::window_chrome(ui, "settings", "LaterScreen 设置")
        {
            self.request_close(&ctx);
        }
        // 系统标题栏关闭会在本帧标记 close_requested；有修改时必须立即取消，
        // 等用户在确认框作出选择；无修改时不发 CancelClose，让 eframe 正常退出。
        if ctx.input(|i| i.viewport().close_requested()) && self.unsaved.is_dirty(&self.cfg) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_close(&ctx);
        }

        let save_shortcut =
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S));
        if save_shortcut && self.unsaved.is_dirty(&self.cfg) {
            // 快捷键和保存按钮共用完整校验与落盘流程；失败只提示，不推进保存基线。
            self.save(&ctx);
        }

        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            if self.unsaved.confirming_discard() {
                self.unsaved.continue_editing();
            } else {
                self.request_close(&ctx);
            }
        }

        let dirty = self.unsaved.is_dirty(&self.cfg);

        // 底部按钮条（固定高度，勿动：见上一版 Panel 高度自增 bug 的复盘注释）
        egui::Panel::bottom(egui::Id::new("settings-bottom"))
            .exact_size(64.0)
            .frame(
                egui::Frame::NONE
                    .fill(theme::footer_fill(ui.visuals()))
                    .stroke(egui::Stroke::new(1.0, line(ui)))
                    .inner_margin(egui::Margin::symmetric(16, 0)),
            )
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 主按钮双形态：有待保存修改时品牌色实心底白字（品牌色只作
                    // 填充，配白字见 theme::ACCENT_FILL）；无修改时中性幽灵
                    // ——暗淡的强调色底曾被误读为「危险/异常」，灰幽灵让
                    // 「可保存 ↔ 不可保存」的状态变化一眼可见
                    let save_btn = if dirty {
                        egui::Button::new(
                            egui::RichText::new("保存")
                                .color(theme::ACCENT_ON_TEXT)
                                .strong(),
                        )
                        .fill(theme::ACCENT_FILL)
                        .corner_radius(8)
                        .min_size(egui::vec2(96.0, 34.0))
                    } else {
                        egui::Button::new(egui::RichText::new("保存").color(muted(ui)))
                            .corner_radius(8)
                            .min_size(egui::vec2(96.0, 34.0))
                    };
                    let save_response = ui.add_enabled(dirty, save_btn).on_hover_text(if dirty {
                        "保存修改（Ctrl/Cmd+S）"
                    } else {
                        "当前没有需要保存的修改"
                    });
                    if save_response.clicked() {
                        self.save(&ctx);
                    }
                    ui.add_space(8.0);
                    // 次按钮：幽灵描边。标签随状态切换——有修改时叫「取消」
                    // （点了会走放弃确认，语义是放弃修改退出），无修改时叫
                    // 「关闭」（直接关，无需暗示「取消」什么）
                    let close_btn = egui::Button::new(
                        egui::RichText::new(if dirty { "取消" } else { "关闭" }).color(muted(ui)),
                    )
                    .corner_radius(8)
                    .min_size(egui::vec2(80.0, 34.0));
                    if ui
                        .add(close_btn)
                        .on_hover_text(if dirty {
                            "放弃修改并关闭"
                        } else {
                            "关闭设置"
                        })
                        .clicked()
                    {
                        self.request_close(&ctx);
                    }
                    if dirty {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("有未保存修改")
                                .color(muted(ui))
                                .size(12.0),
                        );
                    }
                });
            });
        // 左侧导航：右侧是连续滚动页，这里只做「当前分区」指示 + 点击跳转
        // （scroll-spy 联动见 CentralPanel）。声明在 bottom 之后 → 只占保存
        // 栏以上空间；Panel::left 默认可拖宽，这里锁死。扁平风：导航与窗口
        // 底同色（融入「页框」），唯一的白色块是右侧内容卡片。
        egui::Panel::left(egui::Id::new("settings-nav"))
            .exact_size(148.0)
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::same(8)),
            )
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.add_space(4.0);
                for tab in SettingsTab::ALL {
                    let selected = self.active_tab == tab;
                    if nav_item(ui, selected, tab.label(), tab.icon()).clicked() {
                        self.active_tab = tab;
                        self.jump_target = Some(tab);
                    }
                }
            });
        // 右侧内容：全部分区连续排布在同一滚动页里（VS Code 设置式），
        // 左侧导航与滚动位置双向联动：滚动 → scroll-spy 高亮；点击 → 瞬时
        // 跳转到分区起点。
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ui, |ui| {
                // 本帧点击导航 → 立即跳转并跳过本帧滚动侦测：pass 内的
                // cursor 位置还是旧 offset，侦测会把高亮拉回旧分区。
                let jump = self.jump_target.take();
                let mut tops: Vec<(SettingsTab, f32)> = Vec::with_capacity(SettingsTab::ALL.len());
                let scroll_out = egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // pass 内 cursor 随 offset 平移（content_max_rect 起点 =
                    // inner_rect.min - offset），减去首分区起点归一化成与
                    // `state.offset` 同一空间的内容坐标，scroll-spy 才可比
                    let base = ui.cursor().top();
                    for (index, tab) in SettingsTab::ALL.iter().enumerate() {
                        if index > 0 {
                            // 分区间空隙比组内卡片间距略大，扫读时能看出分组
                            ui.add_space(6.0);
                        }
                        if jump == Some(*tab) {
                            // 瞬时跳转（不用默认平滑动画）：动画期间侦测会
                            // 沿途经高亮中间分区，滚轮输入也会与动画目标拉扯
                            ui.scroll_to_cursor_animation(
                                Some(egui::Align::Min),
                                egui::style::ScrollAnimation::none(),
                            );
                        }
                        tops.push((*tab, ui.cursor().top() - base));
                        match tab {
                            SettingsTab::General => self.tab_general(ui),
                            SettingsTab::Screenshot => self.tab_screenshot(ui),
                            SettingsTab::Record => self.tab_record(ui),
                            SettingsTab::Ocr => self.tab_ocr(ui),
                            SettingsTab::Hotkeys => self.tab_hotkeys(ui),
                            SettingsTab::About => self.tab_about(ui),
                        }
                    }
                });
                if jump.is_none()
                    && scroll_out.content_size.y > scroll_out.inner_rect.height() + 2.0
                {
                    // 判定线：视口顶部下探 80px。滞后一帧无感；滚到底兜底选
                    // 最后一节（矮分区碰不到判定线，「关于」否则永远高亮不了）。
                    // 内容不超高时跳过侦测：at_bottom 恒真会把高亮错拉到末节，
                    // 保持点击/默认状态即可
                    let line = scroll_out.state.offset.y + 80.0;
                    let at_bottom = scroll_out.state.offset.y
                        >= scroll_out.content_size.y - scroll_out.inner_rect.height() - 2.0;
                    self.active_tab = scroll_spy(&tops, line, at_bottom);
                }
            });

        if let Some((msg, until)) = &self.toast {
            if ctx.input(|i| i.time) > *until {
                self.toast = None;
            } else {
                let msg = msg.clone();
                egui::Area::new(egui::Id::new("settings-toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::Vec2::new(0.0, -72.0))
                    .show(&ctx, |ui| {
                        // Toast 跟随主题：底色比面板抬起一层、文字用主文字令牌、
                        // 描边用主题化的品牌色（此前两主题都是黑底强调色描边，浅色下
                        // 是在白面板上糊一块黑，用户反馈太丑）
                        egui::Frame::NONE
                            .fill(theme::toast_fill(ui.visuals()))
                            .stroke(egui::Stroke::new(1.0, accent_border(ui)))
                            .corner_radius(8)
                            .inner_margin(egui::Margin::symmetric(14, 8))
                            .shadow(egui::epaint::Shadow {
                                offset: [0, 3],
                                blur: 12,
                                spread: 0,
                                color: egui::Color32::from_black_alpha(38),
                            })
                            .show(ui, |ui| ui.colored_label(text(ui), msg));
                    });
                ctx.request_repaint();
            }
        }

        self.discard_confirmation(&ctx);
        self.poll_raise(&ctx);
    }
}

// ---------------------------------------------------------------- 页面

impl SettingsApp {
    /// 单例唤起心跳（v0.11.4）：第二个 `lscreen config` 进程会留下 raise
    /// 信号。窗口在后台时无输入事件，eframe 不会主动重绘——必须自己
    /// `request_repaint_after` 保持心跳（与 history::poll_raise 同一坑）。
    fn poll_raise(&mut self, ctx: &egui::Context) {
        const POLL: std::time::Duration = std::time::Duration::from_millis(300);
        if crate::instance::take_raise(crate::instance::Kind::Settings) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        ctx.request_repaint_after(POLL);
    }

    fn tab_general(&mut self, ui: &mut egui::Ui) {
        card(ui, "通用", |ui| {
            // 卡片级宽度：Grid 测量趟里单元格的 available_width 不可靠
            // （实测出现过 17px），用它在单元格里减预留会算出负宽度直接
            // panic——宽度一律从这里的稳定值推导，且钳死下限双保险
            let card_w = ui.available_width();
            egui::Grid::new("save-grid")
                .num_columns(2)
                .spacing([12.0, 12.0])
                .show(ui, |ui| {
                    row_label(ui, "主题");
                    egui::ComboBox::from_id_salt("theme")
                        .selected_text(self.cfg.theme.label())
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for mode in [config::ThemeMode::System, config::ThemeMode::Light, config::ThemeMode::Dark] {
                                if ui.selectable_value(&mut self.cfg.theme, mode, mode.label()).changed() {
                                    theme::apply(ui.ctx(), self.cfg.theme);
                                }
                            }
                        });
                    ui.end_row();

                    row_label(ui, "目录");
                    ui.horizontal(|ui| {
                        let input_w = (card_w - 152.0).max(160.0); // 预留 标签列+浏览按钮，钳底防负
                        input_field(
                            ui,
                            egui::TextEdit::singleline(&mut self.cfg.save_dir)
                                .hint_text("留空 = ~/Pictures"),
                            input_w,
                        )
                        .on_hover_text("保存截图/录屏的目录；留空用 ~/Pictures（不存在则家目录）");
                        // 浏览：调系统文件夹选择器（后台线程，见 browse_dir），
                        // 选完直接落进未保存的编辑副本
                        if ui
                            .add_enabled(
                                self.dir_pick.is_none(),
                                egui::Button::new(egui::RichText::new("浏览…").color(muted(ui)))
                                    .corner_radius(8)
                                    .min_size(egui::vec2(64.0, crate::theme::FIELD_H)),
                            )
                            .on_hover_text("打开系统文件夹选择器；对话框开着时不可重复点击")
                            .clicked()
                        {
                            self.browse_dir(ui.ctx());
                        }
                    });
                    ui.end_row();

                    row_label(ui, "文件名模板");
                    ui.horizontal(|ui| {
                        let input_w = (card_w - 100.0).max(240.0); // 预留 标签列+「?」，钳底防负
                        input_field(
                            ui,
                            egui::TextEdit::singleline(&mut self.cfg.filename_template),
                            input_w,
                        )
                        .on_hover_text(
                            "占位符：{YYYYMMDD} {HHMMSS} {YYYY} {MM} {DD} {HH} {MI} {SS}；未知 token 原样保留",
                        );
                        // 「?」悬停入口：占位符列表原本只在输入框悬停提示里，
                        // 不看说明书发现不了（可用性反馈）
                        ui.label(egui::RichText::new("?").color(muted(ui)).size(12.0))
                            .on_hover_text(
                                "支持的占位符：{YYYYMMDD} 年月日、{HHMMSS} 时分秒、\
                                 {YYYY}/{MM}/{DD} 年/月/日、{HH}/{MI}/{SS} 时/分/秒；\
                                 未知 token 原样保留",
                            );
                    });
                    ui.end_row();

                    row_label(ui, "历史条数");
                    ui.horizontal(|ui| {
                        // ± 步进：DragValue 单独摆着像普通文本框，加减按钮给
                        // 「这是个数量」的直观操作
                        if ui
                            .add(
                                egui::Button::new("−")
                                    .corner_radius(8)
                                    .min_size(egui::vec2(24.0, 24.0)),
                            )
                            .clicked()
                        {
                            self.cfg.history_max = self.cfg.history_max.saturating_sub(1).max(1);
                        }
                        // egui 0.35 的 DragValue 无 on_hover_text：经 ui.add 的 Response 挂
                        ui.add(
                            egui::DragValue::new(&mut self.cfg.history_max)
                                .range(1..=50)
                                .speed(0.1),
                        );
                        if ui
                            .add(
                                egui::Button::new("+")
                                    .corner_radius(8)
                                    .min_size(egui::vec2(24.0, 24.0)),
                            )
                            .clicked()
                        {
                            self.cfg.history_max = (self.cfg.history_max + 1).min(50);
                        }
                    })
                    .response
                    .on_hover_text("托盘「历史」窗口保留的最近截图/贴图/录屏条数（1-50）");
                    ui.end_row();
                });
            // 模板实时预览：每帧按当前编辑值渲染，改动即时可见。不用 small()
            // ——小字号在灰度 AA 下笔画稀释严重，正文字号 + 弱色足够区分
            // 层级（用户反馈「发白」的重灾区）
            ui.label(
                egui::RichText::new(format!(
                    "ⓘ 示例  {}.png",
                    config::render_template(&self.cfg.filename_template, (2026, 8, 19, 10, 15, 20))
                ))
                .color(muted(ui)),
            )
            .on_hover_text(
                "随文件名模板实时更新；支持 {YYYYMMDD} {HHMMSS} {YYYY} {MM} {DD} {HH} {MI} {SS}",
            );
            ui.add_space(6.0);
            ui.checkbox(&mut self.cfg.open_dir_after_save, "保存后自动打开所在目录");
            ui.add_space(4.0);
            // 「打开目录」独立成行：原先和复选框挤在一行，容易误触旁边的勾选
            // 必须用 link：Label（ui.small）默认 Sense::hover，clicked() 永远不触发
            if ui
                .link(egui::RichText::new("打开目录").color(accent_text(ui)))
                .on_hover_text("调用系统文件管理器打开当前保存目录")
                .clicked()
            {
                // 面板内存中的目录（未保存的编辑也生效）；未设置或不存在
                // 则退回默认目录（~/Pictures 或家目录）
                let dir = self
                    .cfg
                    .save_dir_override()
                    .filter(|p| p.is_dir())
                    .unwrap_or_else(export::default_dir);
                export::open_in_file_manager(&dir);
            }
            ui.add_space(6.0);
            ui.checkbox(&mut self.cfg.copy_auto_exit, "复制到剪贴板后自动退出");
            ui.add_space(4.0);
            ui.checkbox(
                &mut self.cfg.history_close_after_copy,
                "历史面板点击复制后自动关闭",
            );
        });
    }

    fn tab_record(&mut self, ui: &mut egui::Ui) {
        card(ui, "录屏", |ui| {
            egui::Grid::new("record-grid")
                .num_columns(2)
                .spacing([12.0, 12.0])
                .show(ui, |ui| {
                    row_label(ui, "录制格式");
                    let current = config::RECORD_FORMAT_NAMES
                        .iter()
                        .find(|(id, _)| *id == self.cfg.record_format)
                        .map(|(_, label)| *label)
                        .unwrap_or("GIF 动图");
                    egui::ComboBox::from_id_salt("record-format")
                        .selected_text(current)
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for (id, label) in config::RECORD_FORMAT_NAMES {
                                ui.selectable_value(
                                    &mut self.cfg.record_format,
                                    id.to_string(),
                                    *label,
                                );
                            }
                        })
                        .response
                        .on_hover_text("录屏（GIF/MP4）的默认格式；命令行 --mp4 显式指定时优先于此");
                    ui.end_row();

                    // 音频录制（M14/v0.11）：三平台全选项（mac 系统声 = ScreenCaptureKit）
                    {
                        row_label(ui, "录制音频");
                        let current = config::RECORD_AUDIO_NAMES
                            .iter()
                            .find(|(id, _)| *id == self.cfg.record_audio)
                            .map(|(_, label)| *label)
                            .unwrap_or("关");
                        #[cfg(target_os = "linux")]
                        let hint = "仅 MP4 格式生效（GIF 不含音轨）；命令行 --audio 显式指定时优先于此。\
                                    需要系统安装 ffmpeg 与 arecord/parec，缺失时录制会直接提示";
                        #[cfg(windows)]
                        let hint = "仅 MP4 格式生效（GIF 不含音轨）；命令行 --audio 显式指定时优先于此。\
                                    走系统 WASAPI/Media Foundation 直采，无需安装任何工具";
                        #[cfg(target_os = "macos")]
                        let hint = "仅 MP4 格式生效（GIF 不含音轨）；命令行 --audio 显式指定时优先于此。\
                                    麦克风走 CoreAudio；系统声走 ScreenCaptureKit（macOS 13+，\
                                    需「屏幕录制」权限——与截图同一权限）";
                        egui::ComboBox::from_id_salt("record-audio")
                            .selected_text(current)
                            .width(ui.available_width())
                            .show_ui(ui, |ui| {
                                for (id, label) in config::RECORD_AUDIO_NAMES {
                                    ui.selectable_value(
                                        &mut self.cfg.record_audio,
                                        id.to_string(),
                                        *label,
                                    );
                                }
                            })
                            .response
                            .on_hover_text(hint);
                        ui.end_row();
                    }

                    row_label(ui, "点击高亮");
                    ui.checkbox(&mut self.cfg.record_click_highlight, "录制时高亮鼠标点击")
                        .on_hover_text(
                            "鼠标按下处叠加扩散圆环（300ms 淡出），GIF/MP4 均生效；教程录制友好。录制前也可用状态窗的齿轮临时修改",
                        );
                    ui.end_row();
                });
        });
    }

    fn tab_screenshot(&mut self, ui: &mut egui::Ui) {
        card(ui, "默认绘制", |ui| {
            egui::Grid::new("draw-grid")
                .num_columns(2)
                .spacing([12.0, 12.0])
                .show(ui, |ui| {
                    row_label(ui, "工具");
                    let current = config::TOOL_NAMES
                        .iter()
                        .find(|(id, _)| *id == self.cfg.default_tool)
                        .map(|(_, label)| *label)
                        .unwrap_or("选择");
                    egui::ComboBox::from_id_salt("tool")
                        .selected_text(current)
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for (id, label) in config::TOOL_NAMES {
                                ui.selectable_value(
                                    &mut self.cfg.default_tool,
                                    id.to_string(),
                                    *label,
                                );
                            }
                        });
                    ui.end_row();

                    row_label(ui, "颜色");
                    ui.horizontal(|ui| {
                        let mut c32 = config::parse_hex_color(&self.cfg.default_color)
                            .map(crate::ui::egui_color)
                            .unwrap_or(egui::Color32::RED);
                        // 紧凑色块按钮 + 弹层取色器（内联展开式会占大块空间）
                        if egui::color_picker::color_edit_button_srgba(
                            ui,
                            &mut c32,
                            egui::color_picker::Alpha::Opaque,
                        )
                        .changed()
                        {
                            self.cfg.default_color = config::hex_color(egui_to_rgba(c32));
                        }
                        ui.label(
                            egui::RichText::new(self.cfg.default_color.clone())
                                .color(muted(ui))
                                .monospace(),
                        );
                    });
                    ui.end_row();

                    row_label(ui, "线宽");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Slider::new(&mut self.cfg.default_width, 1.0..=12.0)
                                .show_value(false),
                        );
                        ui.label(
                            egui::RichText::new(format!("{:.0} px", self.cfg.default_width))
                                .color(muted(ui))
                                .monospace(),
                        );
                    });
                    ui.end_row();
                });
        });

        card(ui, "初始选区", |ui| {
            egui::Grid::new("selection-grid")
                .num_columns(2)
                .spacing([12.0, 12.0])
                .show(ui, |ui| {
                    row_label(ui, "初始选区");
                    let current = config::SELECTION_NAMES
                        .iter()
                        .find(|(id, _)| *id == self.cfg.default_selection)
                        .map(|(_, label)| *label)
                        .unwrap_or("最前窗口");
                    egui::ComboBox::from_id_salt("initial-selection")
                        .selected_text(current)
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for (id, label) in config::SELECTION_NAMES {
                                ui.selectable_value(
                                    &mut self.cfg.default_selection,
                                    id.to_string(),
                                    *label,
                                );
                            }
                        })
                        .response
                        .on_hover_text(
                            "进入截图时预选的区域：最前窗口/上次选区可直接 Enter/双击出图；\
                             上次选区在显示器布局变化后自动作废回退最前窗口",
                        );
                    ui.end_row();
                });
        });
    }

    fn tab_ocr(&mut self, ui: &mut egui::Ui) {
        card(ui, "文字识别", |ui| self.ocr_settings(ui));
    }

    fn tab_hotkeys(&mut self, ui: &mut egui::Ui) {
        card(ui, "全局热键", |ui| {
            egui::Grid::new("hotkeys-grid")
                .num_columns(2)
                .spacing([12.0, 12.0])
                .show(ui, |ui| {
                    // 行序与值都取自 tray::HOTKEY_FIELDS（下标即字段索引，
                    // 见 hotkey_field_mut），新增可绑动作时这里不用改
                    for (i, (field, label)) in crate::tray::HOTKEY_FIELDS
                        .iter()
                        .map(|(f, l, _)| (*f, *l))
                        .enumerate()
                    {
                        row_label(ui, label);
                        let value = crate::tray::hotkey_field(&self.cfg, field).to_string();
                        self.hotkey_capture(ui, i, &value);
                        ui.end_row();
                    }
                });
            ui.add_space(2.0);
            let mut hint = "点击右侧输入框后直接按下组合键即可录入；Backspace 清除，Esc 取消；留空 = 不注册。托盘运行中保存即生效。".to_string();
            // mac：裸 F 键在系统默认下是媒体/系统键（亮度/Spotlight/听写等），
            // 按键到不了应用——托盘注册处会探测 fnState 记日志，这里按当前
            // 配置给一条常驻提醒（措辞对 fn 开关两种状态都成立）
            if cfg!(target_os = "macos") {
                let raws = [
                    self.cfg.hotkey_screenshot.as_str(),
                    self.cfg.hotkey_picker.as_str(),
                    self.cfg.hotkey_pin.as_str(),
                    self.cfg.hotkey_record.as_str(),
                    self.cfg.hotkey_scroll.as_str(),
                    self.cfg.hotkey_delay.as_str(),
                    self.cfg.hotkey_history.as_str(),
                ];
                let has_bare_fn = raws.iter().any(|raw| {
                    crate::tray::parse_hotkey(raw)
                        .map(|hk| crate::tray::is_bare_fn_key(&hk))
                        .unwrap_or(false)
                });
                if has_bare_fn {
                    hint.push_str(
                        "　⚠ 有裸 F 键热键：macOS 默认将 F1–F12 用作媒体键。授权辅助功能\
                         （隐私与安全性▸辅助功能 添加 lscreen）后直接生效；未授权时可用\
                         fn+该键 或开启「将 F1、F2 等键用作标准功能键」（系统设置▸键盘）",
                    );
                }
            }
            if let Some((msg, until)) = &self.record_hint {
                if ui.input(|i| i.time) < *until {
                    hint = msg.clone();
                    ui.ctx().request_repaint();
                } else {
                    self.record_hint = None;
                }
            }
            ui.small(egui::RichText::new(hint).color(muted(ui)));
        });
    }

    fn tab_about(&mut self, ui: &mut egui::Ui) {
        card(ui, "关于", |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("LaterScreen")
                        .color(text(ui))
                        .size(15.0)
                        .strong(),
                );
                ui.add_space(8.0);
                egui::Frame::NONE
                    .fill(field(ui))
                    .corner_radius(egui::CornerRadius::same(255))
                    .inner_margin(egui::Margin::symmetric(8, 2))
                    .show(ui, |ui| {
                        let ver = format!("v{}", env!("CARGO_PKG_VERSION"));
                        ui.label(egui::RichText::new(ver).color(muted(ui)).size(11.0));
                    });
            });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("截图 · 标注 · 取色 · 录屏 · 贴图")
                    .color(muted(ui))
                    .size(12.0),
            );
            ui.add_space(6.0);
            if let Some(path) = config::config_path() {
                ui.label(
                    egui::RichText::new(format!("配置文件  {}", path.display()))
                        .color(muted(ui))
                        .size(11.0),
                );
            }
        });
        card(ui, "检查更新", |ui| self.update_row(ui));
    }

    /// 引擎配置与模型下载分离：前者需要保存，后者成功后立即可供 auto 使用。
    fn ocr_settings(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("ocr-grid")
            .num_columns(2)
            .spacing([12.0, 12.0])
            .show(ui, |ui| {
                row_label(ui, "引擎");
                let current = config::OCR_ENGINE_NAMES
                    .iter()
                    .find(|(id, _)| *id == self.cfg.ocr.engine)
                    .map(|(_, label)| *label)
                    .unwrap_or("自动");
                egui::ComboBox::from_id_salt("ocr-engine")
                    .selected_text(current)
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for (id, label) in config::OCR_ENGINE_NAMES {
                            ui.selectable_value(&mut self.cfg.ocr.engine, id.to_string(), *label);
                        }
                    });
                ui.end_row();
            });
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "自动模式不会下载 PP-OCR 模型；已下载的 PP-OCR 优先，否则使用系统引擎或 ocrs 兜底。",
            )
            .color(muted(ui)),
        );
        ui.label(
            egui::RichText::new(
                "显式选择 PP-OCR 并保存后，首次识别会下载约 15.6 MB 模型（也可点击下方按钮预先下载）。",
            )
            .color(muted(ui)),
        );
        ui.label(
            egui::RichText::new(
                "模型就绪后在本机离线识别，不上传图像；ocrs 仅支持拉丁文字，首次使用也可能下载自身模型。",
            )
            .color(muted(ui)),
        );
        ui.label(
            egui::RichText::new(
                "下载无需保存，完成后即可供自动模式使用；切换引擎需点击「保存」，下次识别即生效。",
            )
            .color(muted(ui)),
        );
        if let Some(value) = &self.ocr_engine_env {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "检测到 LSCREEN_OCR_ENGINE={value:?}：有效的显式环境变量优先于配置，空值/auto 不覆盖配置。\
                     本窗口不修改环境变量；若要解除覆盖，请修改启动环境后重新启动相应进程。"
                ))
                .color(accent_text(ui)),
            );
        }
        ui.add_space(8.0);
        let busy = self.ocr_models.in_flight();
        let checking = self.ocr_models.checking();
        let failed = matches!(self.ocr_models.result(), Some((_, Err(_))));
        let present = self.ocr_models.present();

        // 逐文件轻量状态（按固定字节数判定，不做哈希）：哈希结论看下方
        // 最近一次「校验完整性」的结果行；下载进行中不读盘（文件在变动）
        let states = if busy {
            None
        } else {
            lscreen_ocr::paddle::file_states()
        };
        if let Some(states) = states {
            for (file, size, ready) in states {
                let label = match file {
                    "ch_PP-OCRv4_det_mobile.onnx" => "检测模型",
                    "ch_PP-OCRv4_rec_mobile.onnx" => "识别模型",
                    _ => "字典",
                };
                let (mark, color) = if ready {
                    ("✓", text(ui))
                } else {
                    ("✗", accent_text(ui))
                };
                ui.label(
                    egui::RichText::new(format!(
                        "{mark} {label}（{:.1} MB）",
                        size as f64 / 1_000_000.0
                    ))
                    .color(color)
                    .size(12.5),
                );
            }
            ui.add_space(4.0);
        }

        ui.horizontal_wrapped(|ui| {
            // 主按钮按状态分流：齐备 → 只读校验（SHA-256）；缺失/失败 → 下载
            // （下载器会跳过完好文件、重下损坏文件，即「修复」路径）
            let button_label = if failed {
                "重新下载 / 修复 PP-OCR 模型"
            } else if present {
                "校验完整性（SHA-256）"
            } else {
                "下载 PP-OCR 模型（约 15.6 MB）"
            };
            if ui
                .add_enabled(!busy, egui::Button::new(button_label))
                .clicked()
            {
                if present && !failed {
                    self.ocr_models.verify(ui.ctx());
                } else {
                    self.ocr_models.start(ui.ctx());
                }
            }
            if ui
                .add_enabled(!busy, egui::Button::new("刷新本地状态"))
                .on_hover_text("只检查模型文件，不联网；可用于手工放入模型或其它进程下载后的刷新")
                .clicked()
            {
                self.ocr_models.check(ui.ctx());
            }
        });
        if busy {
            if checking {
                ui.label(egui::RichText::new("正在检查本地模型文件…").color(muted(ui)));
            } else if matches!(self.ocr_models.active_kind(), Some("verify")) {
                ui.label(
                    egui::RichText::new("正在校验完整性（逐文件 SHA-256，约 15.6 MB）…")
                        .color(muted(ui)),
                );
            } else {
                let progress = self.ocr_models.progress();
                let label = if progress.total == 0 {
                    "准备下载…".into()
                } else {
                    format!(
                        "{:.1} / {:.1} MB（三个文件合计）",
                        progress.done as f64 / 1_000_000.0,
                        progress.total as f64 / 1_000_000.0,
                    )
                };
                ui.add(
                    egui::ProgressBar::new(progress.fraction())
                        .text(label)
                        .animate(false)
                        .desired_width(ui.available_width()),
                );
                ui.label(egui::RichText::new("下载期间请保持设置窗口打开。").color(muted(ui)));
            }
        } else if let Some((kind, outcome)) = self.ocr_models.result() {
            // 最近一次操作的结果行：明确「检查/下载/校验」各自的结论
            match outcome {
                Ok(_) => {
                    let msg = match kind {
                        "check" if present => "检查完成：模型文件齐备，自动模式将优先使用。",
                        "check" => "检查完成：本机未安装 PP-OCR 模型。",
                        "download" => "下载完成：模型已就绪，自动模式将优先使用。",
                        "verify" => "校验通过：三个模型文件 SHA-256 与大小全部一致。",
                        _ => "",
                    };
                    ui.label(egui::RichText::new(msg).color(text(ui)));
                }
                Err(e) => {
                    ui.label(
                        egui::RichText::new(format!(
                            "上次操作失败：{e}。可检查网络和目录权限后重试或修复。"
                        ))
                        .color(accent_text(ui)),
                    );
                }
            }
        } else if present {
            ui.label(
                egui::RichText::new("模型文件齐备；自动模式优先，识别加载时会再次校验完整性。")
                    .color(muted(ui)),
            );
        } else {
            ui.label(
                egui::RichText::new("尚无完整的 PP-OCR 模型；点击上方按钮下载。").color(muted(ui)),
            );
        }
        if let Some(path) = &self.ocr_models_dir {
            ui.label(
                egui::RichText::new(format!("模型目录  {}", path.display()))
                    .color(muted(ui))
                    .size(11.0),
            );
        }
    }

    /// 「检查更新」区：结果横幅 + 当前版本 + 按钮 + 结果文案。
    ///
    /// 手动触发而非自动联网（理由见 `update` 模块头）：按钮点一次发一次
    /// 请求，网络 IO 在后台线程，检查中按钮禁用并持续重绘。
    /// poll 已提升到帧级（`ui()` 开头）：切走本页时后台检查仍被收尾。
    fn update_row(&mut self, ui: &mut egui::Ui) {
        // 新版本提示条（只在确实查到更新时出现）：品牌色描边卡片 +
        // 「去下载」跳 release 页。紧贴「检查更新」按钮上方——检查动作
        // 发生在哪，结果就出现在哪；页面长于视口时放页顶会落在视野外
        // （v0.11.4 实测：查到更新后用户停在页底，看不见提示条）
        if let Some(Ok(crate::update::Outcome::Newer(tag))) = self.update.result() {
            let tag = tag.clone();
            egui::Frame::NONE
                .fill(field(ui))
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .outer_margin(egui::Margin {
                    bottom: 10,
                    ..Default::default()
                })
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(format!("有新版本 {tag}"))
                                .color(accent_text(ui))
                                .strong(),
                        );
                        ui.add_space(8.0);
                        if ui
                            .link(
                                egui::RichText::new("去下载")
                                    .color(accent_text(ui))
                                    .strong(),
                            )
                            .on_hover_text("在浏览器打开最新版本的下载页")
                            .clicked()
                        {
                            crate::update::open_in_browser(&crate::update::release_page_url());
                        }
                        ui.add_space(6.0);
                        if ui.small_button("知道了").clicked() {
                            self.update.clear();
                        }
                    });
                });
        }
        ui.horizontal(|ui| {
            let checking = self.update.in_flight();
            let btn = egui::Button::new(if checking {
                egui::RichText::new("检查中…").color(muted(ui))
            } else {
                egui::RichText::new("检查更新").color(text(ui))
            })
            .corner_radius(8)
            .min_size(egui::vec2(88.0, crate::theme::FIELD_H))
            .fill(field(ui));
            if ui
                .add_enabled(!checking, btn)
                .on_hover_text("查询 GitHub 上是否有新版本（手动触发，不会后台自动联网）")
                .clicked()
            {
                self.update.start(ui.ctx());
            }
            ui.add_space(10.0);
            // 结果文案：失败要可行动，**不**把失败说成「已是最新」；
            // 未检查过时只报当前版本，不暗示任何结论
            match self.update.result() {
                None if !checking => {
                    ui.label(
                        egui::RichText::new(format!("当前 v{}", env!("CARGO_PKG_VERSION")))
                            .color(muted(ui))
                            .size(12.0),
                    );
                }
                Some(Ok(crate::update::Outcome::UpToDate)) => {
                    ui.label(
                        egui::RichText::new(format!(
                            "当前 v{}，已是最新",
                            env!("CARGO_PKG_VERSION")
                        ))
                        .color(muted(ui))
                        .size(12.0),
                    );
                }
                // 有新版本时不在这里重复文案——提示条就在按钮正上方
                Some(Err(e)) => {
                    ui.label(
                        egui::RichText::new(format!("未能确认：{e}"))
                            .color(muted(ui))
                            .size(12.0),
                    );
                }
                _ => {}
            }
        });
        // 检查中保持重绘：结果回来时线程会 request_repaint，这里补上
        // 「请求已发出、等待中」期间的帧（否则窗口可能休眠不刷）
        if self.update.in_flight() {
            ui.ctx().request_repaint();
        }
    }
}

/// Grid 左列的标签。表单标签是主 UI 文本而非次要信息：小号中文在 Wayland
/// 灰度抗锯齿下笔画被稀释（nominal 弱色实测核心只剩 #909098），再用弱色
/// 只会更显「发白」（用户反馈），必须用主文字色。
fn row_label(ui: &mut egui::Ui, label: &str) {
    ui.label(egui::RichText::new(label).color(text(ui)));
}

/// 通用输入框：高度精确钉在 `theme::FIELD_H`，与下拉框/数字框（interact_size
/// 下限）同 Grid 内等高。
///
/// egui 0.35 的 TextEdit **忽略 `min_size.y`**（高度 = 字体行高，单行仅
/// ~20px），原先的 min_size 写法一直静默失效——这正是输入框比下拉框矮一截
/// 的根源。这里改为：定高容器 + 关闭内建边框 + 自绘。扁平风：空闲态只有
/// field 底色（轮廓 = 与卡片底色的色调差），聚焦画 selection 描边环。
fn input_field(ui: &mut egui::Ui, builder: egui::TextEdit<'_>, width: f32) -> egui::Response {
    let field_h = crate::theme::FIELD_H;
    let pad_x = ui.spacing().button_padding.x;
    let (rect, outer) = ui.allocate_exact_size(egui::vec2(width, field_h), egui::Sense::hover());
    // 绘制顺序 = 底色 → 文字 → 聚焦环。底色若画在文字之后（第一版的写法）
    // 会把内容整个盖住——表现为「模板空白」「目录选完不回显」，路径其实
    // 已经写入，只是被不透明背景压在下面
    ui.painter().rect_filled(rect, 8.0, field(ui));
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            // 水平内边距由内层可用矩形让出（pad_x = button_padding.x，与
            // ComboBox/Button 同一来源）：走 TextEdit 的 margin 会在
            // egui 里把光标命中区也算进去，实际字形反而贴到边框上
            .max_rect(rect.shrink2(egui::vec2(pad_x, 0.0)))
            // centered_and_justified：内层 TextEdit 自身只有一行高（~20px），
            // 必须在 32px 容器里垂直居中——top_down 会把文字顶到上沿
            .layout(egui::Layout::centered_and_justified(
                egui::Direction::TopDown,
            )),
    );
    let inner = content.add(
        builder
            .desired_width(rect.width() - pad_x * 2.0)
            .frame(egui::Frame::NONE)
            .vertical_align(egui::Align::Center),
    );
    // 空闲描边走 widgets.inactive.bg_stroke（随主题：浅色 NONE = 纯色调
    // 分层，暗色有可见边框，见 theme.rs 控件描边策略）；聚焦换 selection
    // 焦点环。两者取同一令牌来源，暗色下与内建 TextEdit/下拉框外观一致
    let stroke = if inner.has_focus() {
        ui.visuals().selection.stroke
    } else {
        ui.visuals().widgets.inactive.bg_stroke
    };
    if !stroke.is_empty() {
        ui.painter()
            .rect_stroke(rect, 8.0, stroke, egui::StrokeKind::Inside);
    }
    // 悬停提示挂在外层定高矩形上（调用方只做 on_hover_text）；内部点击/
    // 编辑交互仍由 TextEdit 自己处理
    outer
}

fn egui_to_rgba(c: egui::Color32) -> lscreen_core::Rgba {
    lscreen_core::Rgba([c.r(), c.g(), c.b(), 0xff])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_control_heights_are_uniform() {
        // 「输入框/选择框统一高度」契约：theme::apply 把 interact_size.y
        // 钉在 FIELD_H，所有表单控件都必须落在这个高度上（checkbox 例外，
        // 它是伴随标签的勾选件，不是表单行控件）
        for mode in [config::ThemeMode::Dark, config::ThemeMode::Light] {
            let ctx = egui::Context::default();
            theme::apply(&ctx, mode);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 400.0),
                )),
                ..Default::default()
            };
            let mut heights = None;
            let _ = ctx.run_ui(input, |ui| {
                let mut s = String::from("x");
                let text = input_field(ui, egui::TextEdit::singleline(&mut s), 200.0)
                    .rect
                    .height();
                let mut n = 10usize;
                let drag = ui.add(egui::DragValue::new(&mut n)).rect.height();
                let combo = egui::ComboBox::from_id_salt("probe-combo")
                    .selected_text("x")
                    .width(120.0)
                    .show_ui(ui, |_ui| {})
                    .response
                    .rect
                    .height();
                let mut v = 3.0f32;
                let slider = ui.add(egui::Slider::new(&mut v, 1.0..=12.0)).rect.height();
                heights = Some((text, drag, combo, slider));
            });
            let (text, drag, combo, slider) = heights.expect("run_ui 未执行表单探测");
            let field_h = crate::theme::FIELD_H;
            assert_eq!(text, field_h, "TextEdit 高度偏离 FIELD_H（{mode:?}）");
            assert_eq!(drag, field_h, "DragValue 高度偏离 FIELD_H（{mode:?}）");
            assert_eq!(combo, field_h, "ComboBox 高度偏离 FIELD_H（{mode:?}）");
            assert_eq!(slider, field_h, "Slider 高度偏离 FIELD_H（{mode:?}）");
        }
    }

    /// 离屏渲染 input_field 的「有文本 / 空+hint」两种状态，供人工核对
    /// 文本与占位符是否真的画了出来（回显缺陷的实证手段）。CI 不跑：
    /// `cargo test -p lscreen settings_input_field_preview -- --ignored --nocapture`
    /// 离屏渲染「输入框 + 下方同宽下拉框」的横向对齐快照（回显/左内边距
    /// 目视核对）。CI 不跑：
    /// `cargo test -p lscreen settings_input_align_preview -- --ignored --nocapture`
    #[test]
    #[ignore = "生成 /tmp/lscreen_settings_input_align_{dark,light}.png 供人工核对左对齐"]
    fn settings_input_align_preview() {
        for (mode, tag) in [
            (config::ThemeMode::Dark, "dark"),
            (config::ThemeMode::Light, "light"),
        ] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(4.0);
            theme::apply(&ctx, mode);
            let size = egui::Vec2::new(400.0, 120.0);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..Default::default()
            };
            let mut text = String::from("lscreen_{YYYYMMDD}_101520");
            let out = ctx.run_ui(input, |ui| {
                egui::Frame::NONE
                    .fill(ui.visuals().panel_fill)
                    .show(ui, |ui| {
                        ui.add_space(16.0);
                        input_field(ui, egui::TextEdit::singleline(&mut text), 360.0);
                        ui.add_space(8.0);
                        egui::ComboBox::from_id_salt("preview-align-combo")
                            .selected_text("跟随系统")
                            .width(360.0)
                            .show_ui(ui, |_ui| {});
                    });
            });
            let path = format!("/tmp/lscreen_settings_input_align_{tag}.png");
            let (w, h) = crate::ui::preview::rasterize_png(&ctx, out, size, &path);
            println!("已生成 {path}（{w}x{h}）");
        }
    }

    #[test]
    #[ignore = "生成 /tmp/lscreen_settings_input_{text,hint}.png 供人工核对输入框回显"]
    fn settings_input_field_preview() {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(4.0);
        theme::apply(&ctx, config::ThemeMode::Dark);
        let size = egui::Vec2::new(400.0, 120.0);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        let mut text = String::from("lscreen_{YYYYMMDD}_101520");
        let out = ctx.run_ui(input, |ui| {
            egui::Frame::NONE
                .fill(ui.visuals().panel_fill)
                .show(ui, |ui| {
                    ui.add_space(20.0);
                    input_field(ui, egui::TextEdit::singleline(&mut text), 360.0);
                    ui.add_space(20.0);
                    let mut empty = String::new();
                    input_field(
                        ui,
                        egui::TextEdit::singleline(&mut empty).hint_text("留空 = ~/Pictures"),
                        360.0,
                    );
                });
        });
        let path = "/tmp/lscreen_settings_input_text.png";
        let (w, h) = crate::ui::preview::rasterize_png(&ctx, out, size, path);
        println!("已生成 {path}（{w}x{h}）");
        // hint 版单独一张
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(4.0);
        theme::apply(&ctx, config::ThemeMode::Dark);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        let mut empty = String::new();
        let out = ctx.run_ui(input, |ui| {
            egui::Frame::NONE
                .fill(ui.visuals().panel_fill)
                .show(ui, |ui| {
                    ui.add_space(20.0);
                    input_field(
                        ui,
                        egui::TextEdit::singleline(&mut empty).hint_text("留空 = ~/Pictures"),
                        360.0,
                    );
                });
        });
        let path = "/tmp/lscreen_settings_input_hint.png";
        let (w, h) = crate::ui::preview::rasterize_png(&ctx, out, size, path);
        println!("已生成 {path}（{w}x{h}）");
    }

    #[test]
    fn input_field_paints_text_above_fill() {
        // 回显回归：定高包装曾把不透明底色画在文字**之后**——文字被底色
        // 整个盖住，表现恰好是「文件名模板空白」「目录选完不回显」（路径
        // 写进去了但看不见）。这里直接断言 tessellate 后的顶点顺序：
        // 底色矩形的顶点必须全部先于文本字形顶点
        let ctx = egui::Context::default();
        theme::apply(&ctx, config::ThemeMode::Dark);
        let fill_color = ctx
            .style_of(ctx.theme())
            .visuals
            .widgets
            .inactive
            .weak_bg_fill;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 100.0),
            )),
            ..Default::default()
        };
        let input_rect = egui::Rect::from_min_size(egui::pos2(20.0, 30.0), egui::vec2(360.0, 32.0));
        let mut text = String::from("回显文本 abc123");
        let out = ctx.run_ui(input, |ui| {
            ui.allocate_space(egui::vec2(20.0, 30.0)); // 让输入框落在 input_rect 处
            input_field(ui, egui::TextEdit::singleline(&mut text), 360.0);
        });
        let shapes = ctx.tessellate(out.shapes, out.pixels_per_point);
        let mut last_fill = None;
        let mut first_glyph = None;
        let mut seq = 0usize;
        for clipped in &shapes {
            if let egui::epaint::Primitive::Mesh(mesh) = &clipped.primitive {
                for v in &mesh.vertices {
                    if v.uv == egui::epaint::WHITE_UV
                        && v.color == fill_color
                        && input_rect.contains(v.pos)
                    {
                        last_fill = Some(seq);
                    } else if v.uv != egui::epaint::WHITE_UV && first_glyph.is_none() {
                        first_glyph = Some(seq);
                    }
                    seq += 1;
                }
            }
        }
        let (last_fill, first_glyph) = (
            last_fill.expect("未找到输入框底色顶点"),
            first_glyph.expect("未找到文本字形顶点（回显会失效）"),
        );
        assert!(
            last_fill < first_glyph,
            "底色画到了文字之后（文字被盖住 = 模板空白/目录不回显的根因）: fill@{last_fill} >= glyph@{first_glyph}"
        );

        // 垂直居中回归：内层 TextEdit 只有一行高，曾在 32px 容器里被
        // top_down 布局顶到上沿（用户反馈「文字偏高」）。字形包围盒的
        // 垂直中心应与输入框中心基本重合（±4px 容纳字形上下伸出）
        let mut glyph_y = (0.0, 0usize);
        for clipped in &shapes {
            if let egui::epaint::Primitive::Mesh(mesh) = &clipped.primitive {
                for v in &mesh.vertices {
                    if v.uv != egui::epaint::WHITE_UV && input_rect.contains(v.pos) {
                        glyph_y.0 += v.pos.y;
                        glyph_y.1 += 1;
                    }
                }
            }
        }
        assert!(glyph_y.1 > 0, "未找到文本字形顶点");
        let glyph_center = glyph_y.0 / glyph_y.1 as f32;
        let expected = input_rect.center().y;
        assert!(
            (glyph_center - expected).abs() < 4.0,
            "文字未垂直居中：字形中心 {glyph_center}，输入框中心 {expected}"
        );
    }

    /// 「文字太靠左 / 下拉与输入框选中不是一个颜色」回归：输入框文本与下拉
    /// 选中文本必须共用 `spacing.button_padding.x` 作左内边距，且两者的
    /// 「选中」语义都从同一组 selection 令牌取色。扁平化（v0.14.1）后契约
    /// 收窄为**填充同源**：下拉 active/open 用 selection 底色（不再画边框，
    /// egui 默认灰填充也一并排除）；输入框聚焦环仍走 selection.stroke。
    #[test]
    fn input_text_left_offset_matches_combo_and_selection_tokens_are_shared() {
        for mode in [config::ThemeMode::Dark, config::ThemeMode::Light] {
            let ctx = egui::Context::default();
            theme::apply(&ctx, mode);
            let v = &ctx.style_of(ctx.theme()).visuals;
            // 下拉展开/按下（active、open）必须和输入框聚焦同源于 selection；
            // 描边随主题策略：暗色有 selection 环，浅色无边框
            for w in [&v.widgets.active, &v.widgets.open] {
                if v.dark_mode {
                    assert_eq!(
                        w.bg_stroke.color, v.selection.stroke.color,
                        "暗色下拉展开描边与输入框聚焦环不同源"
                    );
                } else {
                    assert!(
                        w.bg_stroke.is_empty(),
                        "浅色下拉应无边框（扁平风：状态反馈只走填充）"
                    );
                }
                assert_eq!(
                    w.weak_bg_fill, v.selection.bg_fill,
                    "下拉 {mode:?} 底色未走 selection 令牌"
                );
            }
            // 输入框底色仍是 field（聚焦只强调描边，不整体变成选中底）
            assert_eq!(v.text_edit_bg_color(), v.widgets.inactive.weak_bg_fill);

            let mut input_rect = egui::Rect::NAN;
            let mut combo_rect = egui::Rect::NAN;
            let mut text = String::from("abc");
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 400.0),
                )),
                ..Default::default()
            };
            // 只画两个控件，各占一段互不相交的纵向区间，便于按矩形归属字形
            let out = ctx.run_ui(raw, |ui| {
                input_rect = input_field(ui, egui::TextEdit::singleline(&mut text), 300.0).rect;
                ui.add_space(40.0);
                combo_rect = egui::ComboBox::from_id_salt("probe-align-combo")
                    .selected_text("abc")
                    .width(300.0)
                    .show_ui(ui, |_ui| {})
                    .response
                    .rect;
            });
            let shapes = ctx.tessellate(out.shapes, out.pixels_per_point);
            let glyph_left = |rect: egui::Rect| -> f32 {
                let mut min = f32::MAX;
                let mut found = 0usize;
                for clipped in &shapes {
                    if let egui::epaint::Primitive::Mesh(mesh) = &clipped.primitive {
                        for vtx in &mesh.vertices {
                            // 字形顶点 uv 非 WHITE_UV；用矩形横向/纵向范围归属，
                            // 避免把另一个控件的字形算进来
                            if vtx.uv != egui::epaint::WHITE_UV && rect.contains(vtx.pos) {
                                min = min.min(vtx.pos.x);
                                found += 1;
                            }
                        }
                    }
                }
                assert!(found > 0, "未在 {rect:?} 内找到字形顶点");
                min
            };
            let input_text_x = glyph_left(input_rect) - input_rect.left();
            // Button 的内边距同样是 button_padding.x：它代表「表单控件统一的
            // 左内边距」，输入框必须与之相等，否则同列里就会显得更靠左
            let combo_text_x = glyph_left(combo_rect) - combo_rect.left();
            assert!(
                (input_text_x - combo_text_x).abs() < 1.5,
                "输入框左内边距 {input_text_x} 与同类控件 {combo_text_x} 不一致（{mode:?}）"
            );
            assert!(
                input_text_x >= 10.0,
                "输入框左内边距过小会显得文字贴边：{input_text_x}（{mode:?}）"
            );
        }
    }

    #[test]
    fn scroll_spy_follows_judge_line_and_pins_bottom() {
        let tops = [
            (SettingsTab::General, 0.0),
            (SettingsTab::Screenshot, 300.0),
            (SettingsTab::Record, 500.0),
            (SettingsTab::Ocr, 700.0),
            (SettingsTab::Hotkeys, 900.0),
            (SettingsTab::About, 1100.0),
        ];
        // 顶部：判定线在首分区内
        assert_eq!(scroll_spy(&tops, 80.0, false), SettingsTab::General);
        // 判定线越过第二分区起点 → 高亮跟随
        assert_eq!(scroll_spy(&tops, 310.0, false), SettingsTab::Screenshot);
        // 判定线正好落在分区起点上 → 该分区接管（边界含等号）
        assert_eq!(scroll_spy(&tops, 500.0, false), SettingsTab::Record);
        // 矮分区（如「关于」）起点永远够不到判定线，滚到底必须兜底选中
        assert_eq!(scroll_spy(&tops, 950.0, true), SettingsTab::About);
        // 空列表兜底默认分区
        assert_eq!(scroll_spy(&[], 80.0, false), SettingsTab::General);
    }

    #[test]
    fn settings_tabs_have_unique_labels_and_distinct_icons() {
        let mut labels = std::collections::BTreeSet::new();
        let mut icons = std::collections::BTreeSet::new();
        for tab in SettingsTab::ALL {
            assert!(!tab.label().is_empty());
            assert!(
                labels.insert(tab.label()),
                "重复的 tab 标签: {}",
                tab.label()
            );
            assert!(
                icons.insert(tab.icon() as usize),
                "重复的 tab 图标: {}",
                tab.label()
            );
        }
        assert_eq!(SettingsTab::ALL.len(), 6);
        assert_eq!(SettingsTab::default(), SettingsTab::General);
    }

    #[test]
    fn ocr_engine_config_change_is_dirty_until_saved_or_discarded() {
        let original = Config::default();
        let mut edited = original.clone();
        let mut guard = UnsavedChanges::new(&original);
        edited.ocr.engine = "ppocr".into();

        assert!(guard.is_dirty(&edited));
        assert_eq!(guard.request_close(&edited), CloseDecision::ConfirmDiscard);
        guard.discard_into(&mut edited);
        assert_eq!(edited.ocr.engine, "auto");
        assert!(!guard.is_dirty(&edited));

        edited.ocr.engine = "ppocr".into();
        guard.mark_saved(&edited);
        assert!(!guard.is_dirty(&edited));
        assert_eq!(guard.request_close(&edited), CloseDecision::CloseNow);
    }

    #[test]
    fn changed_config_requires_confirmation_and_can_continue_editing() {
        let original = Config::default();
        let mut edited = original.clone();
        edited.save_dir = "D:/截图".to_string();
        let mut guard = UnsavedChanges::new(&original);

        assert_eq!(guard.request_close(&edited), CloseDecision::ConfirmDiscard);
        assert!(guard.confirming_discard());

        guard.continue_editing();
        assert!(!guard.confirming_discard());
        assert!(guard.is_dirty(&edited));
    }

    #[test]
    fn successful_save_clears_dirty_state_and_allows_close() {
        let original = Config::default();
        let mut edited = original.clone();
        edited.default_width = 8.0;
        let mut guard = UnsavedChanges::new(&original);
        assert!(guard.is_dirty(&edited));

        guard.mark_saved(&edited);

        assert!(!guard.is_dirty(&edited));
        assert_eq!(guard.request_close(&edited), CloseDecision::CloseNow);
    }

    #[test]
    fn discard_restores_saved_config_including_previewed_theme() {
        let original = Config {
            theme: config::ThemeMode::Light,
            ..Config::default()
        };
        let mut edited = original.clone();
        edited.theme = config::ThemeMode::Dark;
        let mut guard = UnsavedChanges::new(&original);
        assert_eq!(guard.request_close(&edited), CloseDecision::ConfirmDiscard);

        guard.discard_into(&mut edited);

        assert_eq!(edited.theme, config::ThemeMode::Light);
        assert!(!guard.is_dirty(&edited));
        assert!(!guard.confirming_discard());
    }

    /// 离屏渲染左侧导航栏（首个 tab 选中态）双主题各一张 PNG，供人工核对
    /// 图标造型与选中/悬停底色（无窗口依赖，光栅化见 ui::preview）。CI 不跑：
    /// `cargo test -p lscreen settings_nav_preview -- --ignored --nocapture`
    #[test]
    #[ignore = "生成 /tmp/lscreen_settings_nav_{dark,light}.png 供人工核对导航栏"]
    fn settings_nav_preview() {
        for (mode, tag) in [
            (config::ThemeMode::Dark, "dark"),
            (config::ThemeMode::Light, "light"),
        ] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(4.0); // 4x 渲染，看得清线稿
            theme::apply(&ctx, mode);
            let size = egui::Vec2::new(148.0, 260.0);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..Default::default()
            };
            let out = ctx.run_ui(input, |ui| {
                egui::Frame::NONE
                    .fill(theme::card_fill(ui.visuals()))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        // 裸 run_ui 的根 ui 可用宽度无界，真实 Panel::left 会
                        // 把内容约束在面板宽内——这里显式对齐同一约束，
                        // 否则 nav_item 的 available_width 会吞掉右侧内边距
                        ui.set_max_width(size.x - 16.0);
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.add_space(4.0);
                        for (i, tab) in SettingsTab::ALL.into_iter().enumerate() {
                            nav_item(ui, i == 0, tab.label(), tab.icon());
                        }
                    });
            });
            let path = format!("/tmp/lscreen_settings_nav_{tag}.png");
            let (w, h) = crate::ui::preview::rasterize_png(&ctx, out, size, &path);
            println!("已生成 {path}（{w}x{h}）");
        }
    }
}
