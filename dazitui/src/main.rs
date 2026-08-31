use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use dazitui_core::ThemePreset;
use dazitui_core::normalize_online_content;
use dazitui_core::{
    ApiClient, ApiError, AuthSession, BUILTIN_SETS, BuiltinProgress, BuiltinSet, CharStatus,
    CodeHint, CompetitionRank, CompetitionRankRow, CompetitionType, DbTask, DbWorker,
    ErrorRecordItem, ErrorType, HeatmapLayout, RankColumnConfig, RankColumnId,
    HintCell, HintHand, KeyboardMode, KeypressRecordItem, LoadError, LoadOptions, Rgb, SchemeDict,
    SchemeInfo, Session, SessionRecord, Settings, SettingsStore, Stats, StatsDb, Text, TextSource,
    Theme, TokenStore, default_rime_data_dir, discover_schemes, env_credentials,
    format_stats_share_text, format_time, hint_cell_widths, is_auth_failure, key_accuracy_pct,
    layout_code_hint_line, load_builtin_text, load_builtin_text_shuffled, load_text_from_clipboard,
    load_text_from_file, load_text_from_string, lttb_downsample, normalize_scheme_to_id,
    osc52_clipboard, pack_words_by_width, prewarm_segmenter, resolve_scheme_path_via_discovery,
    save_text_to_file, today_ymd, word_ratio_pct,
};

/// 方案源文件热监控封装（issue #91 / #93），基于 `notify`。
mod scheme_watcher;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::prelude::Stylize;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span, Text as TextLines};
use ratatui::widgets::{
    Axis, Block, BorderType, Chart, Clear, Dataset, GraphType, Paragraph, Wrap,
};
use ratatui_image::StatefulImage;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_themes::{ThemeName, ThemePalette};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 统计视图子页面 / Tab。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum StatsTab {
    #[default]
    WpmTrend,
    Heatmap,
    ErrorRanking,
}

/// 速度趋势图的时间跨度范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum WpmChartRange {
    #[default]
    Recent30,
    Recent100,
    All,
}

impl WpmChartRange {
    fn next(self) -> Self {
        match self {
            Self::Recent30 => Self::Recent100,
            Self::Recent100 => Self::All,
            Self::All => Self::Recent30,
        }
    }

    fn limit(self) -> Option<usize> {
        match self {
            Self::Recent30 => Some(30),
            Self::Recent100 => Some(100),
            Self::All => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Recent30 => "近 30 场",
            Self::Recent100 => "近 100 场",
            Self::All => "全部历史",
        }
    }
}

/// 速度演进趋势图展示指标（WPM 词速 / KPS 击速）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TrendMetric {
    #[default]
    Wpm, // 每分钟字数
    Kps, // 每秒击键数
}

impl TrendMetric {
    fn next(self) -> Self {
        match self {
            Self::Wpm => Self::Kps,
            Self::Kps => Self::Wpm,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Wpm => "WPM 词速",
            Self::Kps => "KPS 击速",
        }
    }
}

/// 键位热力图数据视角。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum HeatmapSource {
    #[default]
    SchemeProjected, // 方案反查击键
    RawKeypress, // 物理捕获击键
}

impl HeatmapSource {
    fn next(self) -> Self {
        match self {
            Self::SchemeProjected => Self::RawKeypress,
            Self::RawKeypress => Self::SchemeProjected,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::SchemeProjected => "方案反查视角",
            Self::RawKeypress => "物理击键视角",
        }
    }
}

/// 错字/错词排行榜焦点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ErrorRankingFocus {
    #[default]
    Chars, // 高频错字榜
    Words, // 高频错词榜
}

impl ErrorRankingFocus {
    fn toggle(self) -> Self {
        match self {
            Self::Chars => Self::Words,
            Self::Words => Self::Chars,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Chars => "高频错字榜",
            Self::Words => "高频错词榜",
        }
    }
}

/// 统计视图状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StatsViewState {
    tab: StatsTab,
    trend_metric: TrendMetric,
    wpm_range: WpmChartRange,
    heatmap_layout: HeatmapLayout,
    heatmap_source: HeatmapSource,
    error_ranking_focus: ErrorRankingFocus,
    char_scroll: usize,
    word_scroll: usize,
    char_selected: usize,
    word_selected: usize,
    status_msg: Option<String>,
}

impl StatsViewState {
    fn new(heatmap_layout: HeatmapLayout) -> Self {
        Self {
            tab: StatsTab::default(),
            trend_metric: TrendMetric::default(),
            wpm_range: WpmChartRange::default(),
            heatmap_layout,
            heatmap_source: HeatmapSource::default(),
            error_ranking_focus: ErrorRankingFocus::default(),
            char_scroll: 0,
            word_scroll: 0,
            char_selected: 0,
            word_selected: 0,
            status_msg: None,
        }
    }
}

/// 将 core 的 ThemePreset 映射为 ratatui_themes 的 ThemePalette。
pub fn theme_palette(preset: ThemePreset) -> ThemePalette {
    let name = match preset {
        ThemePreset::CatppuccinMocha => ThemeName::CatppuccinMocha,
        ThemePreset::Cyberpunk => ThemeName::Cyberpunk,
        ThemePreset::Nord => ThemeName::Nord,
        ThemePreset::Dracula => ThemeName::Dracula,
        ThemePreset::Gruvbox => ThemeName::GruvboxDark,
        ThemePreset::RosePine => ThemeName::RosePine,
        ThemePreset::Kanagawa => ThemeName::Kanagawa,
        ThemePreset::OneDark => ThemeName::OneDarkPro,
    };
    name.palette()
}

/// 赞赏与支持二维码图片字节数据（编译期嵌入）。
static WECHAT_IMG_BYTES: &[u8] = include_bytes!("../../assets/sponsor/wechat.png");
static ALIPAY_IMG_BYTES: &[u8] = include_bytes!("../../assets/sponsor/alipay.jpg");

/// 赞赏与支持视图渲染协议缓存。
struct SponsorViewState {
    wechat: StatefulProtocol,
    alipay: StatefulProtocol,
}

/// 单个比赛榜单视图状态：缓存数据、加载中与错误。
#[derive(Debug, Default)]
struct RankBoard {
    /// 已拉取到的榜单（`None` = 尚未拉取 / 加载中）。
    data: Option<CompetitionRank>,
    /// 是否正在后台拉取。
    loading: bool,
    /// 该 Tab 的上次错误（`None` = 无错误）。
    error: Option<String>,
    /// 榜单列表滚动偏移。
    scroll: u16,
}

/// 在线排行榜视图状态：三个比赛 Tab 各自缓存一份榜单。
#[derive(Debug)]
struct OnlineRankState {
    /// 当前选中的比赛 Tab。
    active_tab: CompetitionType,
    /// 当前拉取的期次日期（今天 `YYYY-MM-DD`），跨天自动刷新。
    date: String,
    /// 三个比赛各自榜单缓存（按比赛类型索引）。
    boards: HashMap<CompetitionType, RankBoard>,
    /// 全局错误提示（网络失败等），优先于各 board 局部错误展示。
    error: Option<String>,
}

/// 在线排行榜「自定义列」弹窗：列出四列并支持勾选显隐。
#[derive(Debug, Default)]
struct RankColumnModal {
    /// 当前高亮选中的列下标（对应 `RankColumnId::ALL` 顺序）。
    selected: usize,
}

/// 列定制弹窗的按键动作。
enum RankColumnModalAction {
    /// 关闭弹窗（并落盘）。
    Close,
    /// 维持打开。
    None,
}

/// 处理列定制弹窗按键：↑↓/kj 移动选择，Space 切换显隐，Esc/q/Q 关闭。
fn rank_column_modal_input(
    modal: &mut RankColumnModal,
    config: &mut RankColumnConfig,
    key: KeyEvent,
) -> RankColumnModalAction {
    let n = RankColumnId::ALL.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            modal.selected = modal.selected.wrapping_sub(1) % n;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            modal.selected = (modal.selected + 1) % n;
        }
        KeyCode::Char(' ') => {
            let id = RankColumnId::ALL[modal.selected];
            let visible = config.is_visible(id);
            config.set_visible(id, !visible);
        }
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => return RankColumnModalAction::Close,
        _ => {}
    }
    RankColumnModalAction::None
}

/// 跟打应用状态。
#[allow(clippy::large_enum_variant)]
enum AppState {
    /// 跟打中。
    Typing,
    /// 已出成绩（成绩视图），携带成绩、上传状态与用时。
    Finished {
        stats: Stats,
        upload: UploadState,
        elapsed: Duration,
    },
    /// 载文浏览：功能栏显示文件列表，可预览与载入。
    Browsing,
    /// 内置赛文浏览：功能栏显示套题列表，可载入。
    BrowsingBuiltin,
    /// 设置视图：切换主题等外观设置。
    Settings,
    /// 统计视图：速度趋势图、键位热力图与错字排行榜。
    Stats(StatsViewState),
    /// 赞赏与支持视图：展示微信与支付宝二维码。
    Sponsor,
    /// 在线排行榜视图：三个比赛 Tab（极速杯/锦标赛/键神杯）的独立榜单，后台非阻塞拉取。
    OnlineRank(OnlineRankState),
    /// 开始跟打前的准备倒计时：显示 3-2-1 弹窗，期间拦截所有输入；
    /// 倒计时结束自动进入 `Typing` 并开始计时。
    Countdown {
        deadline: Instant,
        source: CountdownSource,
    },
}

/// 准备倒计时从哪个浏览界面进入，取消时回到对应界面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CountdownSource {
    Browsing,
    BrowsingBuiltin,
    /// 从暂停态继续跟打：保留已累计用时，倒计时结束后从暂停处续接计时。
    Resume,
    /// 从 52dazi 在线赛文下载完成后进入：倒计时结束后直接开打（在线赛文不可重打、完成自动上传）。
    Online,
}

/// 设置视图焦点项下标。
const FOCUS_THEME: usize = 0;
const FOCUS_RATIO: usize = 1;
const FOCUS_BOLD: usize = 2;
const FOCUS_KEYBOARD: usize = 3;
const FOCUS_SCHEME: usize = 4;
const FOCUS_INPUT_METHOD: usize = 5;
const FOCUS_GROUP_SIZE: usize = 6;
const FOCUS_CODE_HINT: usize = 7;
const FOCUS_MONITOR_SCHEME: usize = 8;
/// 设置视图焦点项总数。
const SETTINGS_FOCUS_COUNT: usize = 9;

/// 成绩视图「错字时间线」一屏最多可见的错字条数（超出部分滚动查看）。
const ERROR_TIMELINE_VISIBLE: usize = 8;

/// 极小终端降级展示时「错字时间线」并进成绩摘要块的可见条数。
const ERROR_TIMELINE_COMPACT_ROWS: usize = 3;

/// 开始跟打前的准备倒计时时长。
const COUNTDOWN_SECS: Duration = Duration::from_secs(3);
/// 成功热重载后「方案已重载」状态栏闪现的持续时间（约 2 秒后淡出）。
const SCHEME_RELOAD_FLASH_DURATION: Duration = Duration::from_millis(2000);
/// 热重载防抖时长：连续写盘（原子保存多事件）在此时窗内合并为一次重载（issue #94 验收 300ms）。
const SCHEME_RELOAD_DEBOUNCE: Duration = Duration::from_millis(300);

/// 预设中「自定义」项的标签（哨兵值，选中即打开文本弹窗）。
const SCHEME_CUSTOM: &str = "自定义";

/// 反查方案下拉选项：无（关闭反查）/ 自动发现的真方案 / 自定义。
#[derive(Debug, Clone, PartialEq)]
enum SchemeOption {
    /// 无反查（空串）。
    None,
    /// 自动发现到的方案（schema_id）。
    Discovered(String),
    /// 自定义：打开文本弹窗输入任意方案名 / 文件路径。
    Custom,
}

/// 依据当前发现的方案构建下拉选项列表（无 + 发现到的方案 + 自定义）。
fn build_scheme_options(discovered: &[SchemeInfo]) -> Vec<SchemeOption> {
    let mut opts: Vec<SchemeOption> = vec![SchemeOption::None];
    for s in discovered {
        opts.push(SchemeOption::Discovered(s.id.clone()));
    }
    opts.push(SchemeOption::Custom);
    opts
}

/// 当前 `scheme`（schema_id）在选项列表中的下标；自定义/未知值落到「自定义」项。
fn scheme_option_index(opts: &[SchemeOption], current: &str) -> usize {
    if current.is_empty() {
        return opts
            .iter()
            .position(|o| matches!(o, SchemeOption::None))
            .unwrap_or(0);
    }
    if let Some(idx) = opts
        .iter()
        .position(|o| matches!(o, SchemeOption::Discovered(id) if id == current))
    {
        return idx;
    }
    // 自定义或未知值：定位到最后的「自定义」项。
    opts.len() - 1
}

/// 将选项转为存储的 scheme 值；自定义项用哨兵 `自定义` 标记（选中即打开弹窗）。
fn scheme_option_id(o: &SchemeOption) -> String {
    match o {
        SchemeOption::None => String::new(),
        SchemeOption::Discovered(id) => id.clone(),
        SchemeOption::Custom => SCHEME_CUSTOM.to_string(),
    }
}

/// 向后轮转方案（→ / 右）。
fn scheme_next_option(opts: &[SchemeOption], current: &str) -> String {
    let idx = scheme_option_index(opts, current);
    let next = (idx + 1) % opts.len();
    scheme_option_id(&opts[next])
}

/// 向前轮转方案（← / 左）。
fn scheme_prev_option(opts: &[SchemeOption], current: &str) -> String {
    let idx = scheme_option_index(opts, current);
    let prev = if idx == 0 { opts.len() - 1 } else { idx - 1 };
    scheme_option_id(&opts[prev])
}

/// 当前选定方案在下拉中的展示标签。
fn scheme_current_label(app: &App) -> String {
    let s = &app.settings.scheme;
    if s.is_empty() {
        return "无（关闭反查）".to_string();
    }
    if let Some(info) = app.discovered.iter().find(|d| &d.id == s) {
        return info.display_label();
    }
    if s == SCHEME_CUSTOM {
        return SCHEME_CUSTOM.to_string();
    }
    format!("{s}（自定义）")
}

/// 输入法预设列表（顺序即轮转顺序）。
/// 最后一项「自定义」表示用户自行输入任意名称。
const INPUT_METHOD_PRESETS: &[&str] = &[
    "", // 无（空串）
    "虎码",
    "五笔86",
    "五笔98",
    "小鹤音形",
    "仓颉",
    "郑码",
    "宇浩",
    "双拼",
    "全拼",
    "空明码并击",
    "拼读并击",
    "麓鸣·空明·并击",
    "虎码并击",
    "自定义", // 末项：打开自定义弹窗
];

/// 预设中「自定义」项的标签。
const INPUT_METHOD_CUSTOM: &str = "自定义";

/// 当前输入法在预设列表中是否精确匹配某个预设（排除自定义）。
fn input_method_preset_index(im: &str) -> usize {
    INPUT_METHOD_PRESETS
        .iter()
        .position(|&p| p == im && p != INPUT_METHOD_CUSTOM)
        .unwrap_or(INPUT_METHOD_PRESETS.len() - 1) // 未命中 → 「自定义」下标
}

/// 输入法设置项的显示标签。
fn input_method_display(im: &str) -> &str {
    if im.is_empty() { "无" } else { im }
}

/// 向前轮转输入法预设（← 键），返回下一个预设值（不含「自定义」末项逻辑，由调用方处理弹窗）。
fn cycle_input_method_prev(current: &str) -> String {
    let idx = input_method_preset_index(current);
    let prev = if idx == 0 {
        INPUT_METHOD_PRESETS.len() - 1
    } else {
        idx - 1
    };
    INPUT_METHOD_PRESETS[prev].to_string()
}

/// 向后轮转输入法预设（→ 键），返回下一个预设值。
fn cycle_input_method_next(current: &str) -> String {
    let idx = input_method_preset_index(current);
    let next = (idx + 1) % INPUT_METHOD_PRESETS.len();
    INPUT_METHOD_PRESETS[next].to_string()
}

/// 功能栏可导航菜单项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarMenuItem {
    LoadFile,
    BuiltinText,
    FreeInput,
    Clipboard,
    OnlineJisu,
    OnlineJinbiao,
    OnlineJianshen,
    OnlineRank,
    Stats,
    Settings,
    Sponsor,
    Login,
}

const SIDEBAR_MENU_ITEMS: &[SidebarMenuItem] = &[
    SidebarMenuItem::LoadFile,
    SidebarMenuItem::BuiltinText,
    SidebarMenuItem::FreeInput,
    SidebarMenuItem::Clipboard,
    SidebarMenuItem::OnlineJisu,
    SidebarMenuItem::OnlineJinbiao,
    SidebarMenuItem::OnlineJianshen,
    SidebarMenuItem::OnlineRank,
    SidebarMenuItem::Stats,
    SidebarMenuItem::Settings,
    SidebarMenuItem::Sponsor,
    SidebarMenuItem::Login,
];

/// 成绩视图里的成绩上传状态（在线赛文完成跟打后自动上传）。
#[derive(Debug, Clone, PartialEq)]
enum UploadState {
    /// 非在线赛文：无需上传；`copied_stats` 非空表示完成时已把统计分享文本复制到剪贴板（自由发文/离线赛文）。
    NotApplicable { copied_stats: Option<String> },
    /// 上传中（同步网络请求期间）。
    Uploading,
    /// 上传成功：结构化排名（`None` = 服务器未返回）。
    /// 分享文本已在上传成功时写入剪贴板，成绩视图不再重复展示（顶部摘要已含全部指标）。
    Success { ranking: Option<String> },
    /// 上传失败：友好文案 + 是否需要重新登录 + 原始服务器错误（次要信息）。
    /// `copied_stats` 非空表示失败时已把统计分享文本复制到剪贴板（成绩不因网络问题丢失）。
    Failed {
        message: String,
        need_relogin: bool,
        detail: Option<String>,
        copied_stats: Option<String>,
    },
}

/// 自由发文模态框焦点字段。
const FREE_INPUT_FOCUS_TITLE: usize = 0;
const FREE_INPUT_FOCUS_CONTENT: usize = 1;
const FREE_INPUT_FOCUS_SAVE_CHECKBOX: usize = 2;
const FREE_INPUT_FOCUS_SAVE_PATH: usize = 3;
const FREE_INPUT_FOCUS_SUBMIT_BTN: usize = 4;
const FREE_INPUT_FOCUS_CANCEL_BTN: usize = 5;

/// 自由发文模态框状态。
#[derive(Debug, Clone)]
struct FreeInputModal {
    title: String,
    content: String,
    save_to_file: bool,
    save_path: String,
    focus: usize,
    error: Option<String>,
}

impl FreeInputModal {
    fn new() -> Self {
        Self {
            title: "自由发文".to_string(),
            content: String::new(),
            save_to_file: false,
            save_path: "./自由发文.txt".to_string(),
            focus: FREE_INPUT_FOCUS_CONTENT,
            error: None,
        }
    }

    fn update_default_save_path(&mut self) {
        if !self.save_to_file
            && (self.save_path.starts_with("./") && self.save_path.ends_with(".txt"))
        {
            let sanitized = if self.title.trim().is_empty() {
                "自由发文"
            } else {
                self.title.trim()
            };
            self.save_path = format!("./{sanitized}.txt");
        }
    }

    fn next_focus(&mut self) {
        self.focus = match self.focus {
            FREE_INPUT_FOCUS_TITLE => FREE_INPUT_FOCUS_CONTENT,
            FREE_INPUT_FOCUS_CONTENT => FREE_INPUT_FOCUS_SAVE_CHECKBOX,
            FREE_INPUT_FOCUS_SAVE_CHECKBOX => {
                if self.save_to_file {
                    FREE_INPUT_FOCUS_SAVE_PATH
                } else {
                    FREE_INPUT_FOCUS_SUBMIT_BTN
                }
            }
            FREE_INPUT_FOCUS_SAVE_PATH => FREE_INPUT_FOCUS_SUBMIT_BTN,
            FREE_INPUT_FOCUS_SUBMIT_BTN => FREE_INPUT_FOCUS_CANCEL_BTN,
            _ => FREE_INPUT_FOCUS_TITLE,
        };
        self.error = None;
    }

    fn prev_focus(&mut self) {
        self.focus = match self.focus {
            FREE_INPUT_FOCUS_TITLE => FREE_INPUT_FOCUS_CANCEL_BTN,
            FREE_INPUT_FOCUS_CONTENT => FREE_INPUT_FOCUS_TITLE,
            FREE_INPUT_FOCUS_SAVE_CHECKBOX => FREE_INPUT_FOCUS_CONTENT,
            FREE_INPUT_FOCUS_SAVE_PATH => FREE_INPUT_FOCUS_SAVE_CHECKBOX,
            FREE_INPUT_FOCUS_SUBMIT_BTN => {
                if self.save_to_file {
                    FREE_INPUT_FOCUS_SAVE_PATH
                } else {
                    FREE_INPUT_FOCUS_SAVE_CHECKBOX
                }
            }
            FREE_INPUT_FOCUS_CANCEL_BTN => FREE_INPUT_FOCUS_SUBMIT_BTN,
            _ => FREE_INPUT_FOCUS_TITLE,
        };
        self.error = None;
    }
}

/// 自由发文模态框按键动作。
#[derive(Debug, PartialEq, Eq)]
enum FreeInputAction {
    None,
    Submit {
        title: String,
        content: String,
        save: Option<PathBuf>,
    },
    Cancel,
}

/// 实时虚拟键盘按键状态机。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveKeyboard {
    /// 键名（规范化后的小写字符或特殊键名）-> 最近触发激活的时间戳。
    pub active_keys: std::collections::HashMap<String, Instant>,
}

impl LiveKeyboard {
    /// 创建新实例。
    pub fn new() -> Self {
        Self {
            active_keys: std::collections::HashMap::new(),
        }
    }

    /// 重置所有激活按键。
    pub fn clear(&mut self) {
        self.active_keys.clear();
    }

    /// 规范化按键标识。
    pub fn normalize_key(key: &str) -> String {
        match key {
            " " | "Space" | "space" | "Space (空格)" => "Space".to_string(),
            "Backspace" | "backspace" | "Bksp" | "bksp" => "Backspace".to_string(),
            "Tab" | "tab" => "Tab".to_string(),
            "Enter" | "enter" => "Enter".to_string(),
            "Caps" | "caps" | "CapsLock" => "Caps".to_string(),
            "Shift" | "shift" => "Shift".to_string(),
            "Ctrl" | "ctrl" | "Control" => "Ctrl".to_string(),
            "Alt" | "alt" => "Alt".to_string(),
            "Esc" | "esc" => "Esc".to_string(),
            "Lower" | "lower" => "Lower".to_string(),
            "Raise" | "raise" => "Raise".to_string(),
            "Left" | "←" => "Left".to_string(),
            "Down" | "↓" => "Down".to_string(),
            "Up" | "↑" => "Up".to_string(),
            "Right" | "→" => "Right".to_string(),
            other => {
                if other.chars().count() == 1 {
                    let c = other.chars().next().unwrap();
                    c.to_ascii_lowercase().to_string()
                } else {
                    other.to_ascii_lowercase()
                }
            }
        }
    }

    /// 触发单个按键激活。
    pub fn press_key(&mut self, key: &str, now: Instant) {
        let norm = Self::normalize_key(key);
        self.active_keys.insert(norm, now);
    }

    /// 触发单字符按键激活。
    pub fn press_char(&mut self, c: char, now: Instant) {
        if c == ' ' {
            self.press_key("Space", now);
        } else if c.is_ascii() {
            self.press_key(&c.to_string(), now);
        }
    }

    /// 批量触发按键激活（用于汉字方案反查）。
    pub fn press_keys<'a, I>(&mut self, keys: I, now: Instant)
    where
        I: IntoIterator<Item = &'a str>,
    {
        for k in keys {
            self.press_key(k, now);
        }
    }

    /// 计算给定键位在时间点 `now` 的样式（高亮/衰减/常态）。
    pub fn get_key_style(&self, key: &str, palette: &ThemePalette, now: Instant) -> Style {
        let norm = Self::normalize_key(key);
        if let Some(&pressed_at) = self.active_keys.get(&norm) {
            let elapsed_ms = now.saturating_duration_since(pressed_at).as_millis();
            if elapsed_ms <= 100 {
                // 强高亮 (0-100ms): 强调色背景反白 + 加粗
                Style::default()
                    .fg(palette.bg)
                    .bg(palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else if elapsed_ms <= 250 {
                // 余温衰减 (100-250ms): 强调色前景色 + 加粗
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                // 常态
                Style::default().fg(palette.muted)
            }
        } else {
            // 常态
            Style::default().fg(palette.muted)
        }
    }
}

/// 应用全部状态（TUI 层）。
struct App {
    /// 当前赛文（载文后替换）。
    text: Text,
    session: Session,
    start: Instant,
    /// 累计活跃打字用时（暂停前积累的时间）。
    accumulated_elapsed: Duration,
    /// 当前活跃打字段的起始时刻（暂停时置为 None）。
    active_start: Option<Instant>,
    /// 是否处于暂停状态（打字中途按 Tab 触发）。
    paused: bool,
    state: AppState,
    /// 功能栏是否展开。
    sidebar_visible: bool,
    /// 功能栏当前选中的菜单项下标。
    sidebar_selected: usize,
    /// 功能栏通用临时通知/提示。
    sidebar_notice: Option<String>,
    /// 载文浏览的文件列表。
    browse_files: Vec<PathBuf>,
    /// 文件列表当前选中下标。
    browse_selection: usize,
    /// 内置赛文浏览当前选中下标。
    builtin_selection: usize,
    /// 载文失败时的错误提示。
    browse_error: Option<String>,
    /// token 持久化存储。
    token_store: TokenStore,
    /// 请求携带的 52dazi token（登录后获得；持久化仅用于请求携带，不代表已登录）。
    token: Option<String>,
    /// 本次进程是否已建立登录会话（持有 session cookie）。
    /// 会话不持久化：即使启动时加载了持久化 token 也不视为已登录，需重新登录才能上传成绩。
    logged_in: bool,
    /// 52dazi 客户端。
    api: ApiClient,
    /// 登录模态框（`None` = 未打开）。
    login_form: Option<LoginForm>,
    /// 登录相关的临时提示（展示在功能栏）。
    login_notice: Option<String>,
    /// 在线赛文加载中（`Some(类型)` = 正在下载该比赛赛文）。
    online_loading: Option<CompetitionType>,
    /// 在线载文错误提示（展示在功能栏）。
    online_error: Option<String>,
    /// 外观设置。
    settings: Settings,
    /// 设置持久化存储。
    settings_store: SettingsStore,
    /// 设置视图当前焦点项（FOCUS_THEME/FOCUS_RATIO/FOCUS_BOLD/FOCUS_KEYBOARD/FOCUS_SCHEME/FOCUS_INPUT_METHOD）。
    settings_focus: usize,
    /// 成绩视图「错字时间线」当前选中项下标（每次进入成绩视图重置为 0）。
    error_point_selected: usize,
    /// 成绩视图「错字时间线」滚动偏移，始终跟随选中项保持在可见窗口内。
    error_point_scroll: usize,
    /// 内置赛文浏览中的乱序开关（`true` = 载入时打乱顺序）。
    builtin_shuffle: bool,
    /// 内置赛文浏览器预览缓存 `(title, body)`。
    /// 乱序开时存乱序版预览（避免每帧重新随机导致闪烁），关时存顺序版预览。
    /// 在 `open_builtin_browser` 与 Up/Down/s 按键时重新生成。
    builtin_preview: Option<(String, String)>,
    /// 内置赛文续打弹窗：`Some((赛文, 已存已完成组数, 总组数))` 时显示「继续/重开/重置」选择。
    resume_prompt: Option<(BuiltinSet, usize, usize)>,
    /// 上次已落盘存档的已完成组数（跟打中用于增量保存，避免每键写盘）。
    last_saved_completed: usize,
    /// 自定义设置文本弹窗（`None` = 未打开）。
    text_setting_modal: Option<TextSettingModal>,
    /// 自由发文编辑弹窗（`None` = 未打开）。
    free_input_modal: Option<FreeInputModal>,
    /// 实时虚拟键盘状态。
    live_keyboard: LiveKeyboard,
    /// 当前输入法方案码表（用于汉字方案反查击键与键盘涟漪点亮）。
    scheme_dict: Option<SchemeDict>,
    /// 自动发现的输入方案列表（启动扫描一次 fcitx5 部署目录）。
    discovered: Vec<SchemeInfo>,
    /// 按 schema_id 缓存已加载的方案码表，切回已加载方案瞬时生效（无需重新解析大词典）。
    scheme_cache: HashMap<String, SchemeDict>,
    /// 正在后台加载的方案 id（非阻塞「方案加载中…」角标用）。
    scheme_loading: Option<String>,
    /// 异步方案加载器：后台线程解析 `.dict.yaml`，经通道回传，主循环每帧 poll。
    scheme_loader: SchemeLoader,
    /// 异步排行榜加载器：后台线程拉取比赛榜单，经通道回传，主循环每帧 poll（避免 TUI 冻结）。
    rank_loader: RankLoader,
    /// 在线排行榜「自定义列」弹窗（`None` = 未打开）。
    rank_column_modal: Option<RankColumnModal>,
    /// 方案源文件热监控器（issue #91/#93/#94）。`None` 表示 `notify` 初始化失败，
    /// 此时安全降级为「不监控」（不影响既有加载/切换逻辑）。
    /// 开/关开关（设置项 `monitor_scheme`，默认开启）由 #96 接入；关闭时
    /// `rebuild_scheme_watcher` 会卸载监控闭包，不影响既有加载/切换逻辑。
    scheme_watcher: Option<scheme_watcher::SchemeWatcher>,
    /// 当前已交给监控器的源文件路径闭包，用于重载成功后重建监控（应对原子保存换 inode）。
    scheme_watch_paths: Option<Vec<PathBuf>>,
    /// 检测到改动后、真正发起重载前的防抖时刻；期间忽略后续事件，避免重复重载风暴。
    scheme_reload_pending_at: Option<Instant>,
    /// 热重载派发标记：置 true 表示当前正在等待一次由热监控触发的后台重载结果，
    /// 该结果回传时才应闪现「方案已重载」（区分初始加载/手动切方案，避免误闪）。
    scheme_hot_reload_expected: bool,
    /// 成功热重载后状态栏闪现「方案已重载」的截止时刻；`now < 该时刻` 时显示，过期即淡出。
    scheme_reload_flash_at: Option<Instant>,
    /// 热重载失败提示（如 YAML 写坏）。`Some` 时状态栏报错；成功重载后清空（与成功闪现互斥）。
    scheme_reload_error: Option<String>,
    /// 后台数据库异步写入 Worker。
    db_worker: Option<DbWorker>,
    /// 赞赏与支持视图图片协议缓存。
    sponsor_state: RefCell<Option<SponsorViewState>>,
}

/// 后台异步方案加载的一次性结果回传。
struct SchemeLoadResult {
    /// 加载的方案 schema_id。
    id: String,
    /// 加载成功的码表；路径缺失/解析失败时为 `None`。
    dict: Option<SchemeDict>,
    /// 加载失败原因（`dict == None` 时有值），用于状态栏报错（issue #98）。
    error: Option<String>,
}

/// 异步方案加载器：派生后台线程解析大型 `.dict.yaml`（如 51MB 空明拳），
/// 经通道回传结果，主循环每帧 `poll_scheme_loader` 消费，期间 TUI 不冻结。
struct SchemeLoader {
    sender: mpsc::Sender<SchemeLoadResult>,
    receiver: mpsc::Receiver<SchemeLoadResult>,
}

impl SchemeLoader {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self { sender, receiver }
    }

    /// 派发一次后台加载：在独立线程解析 `path` 并回传结果（含失败原因）。调用方需自行保证不重复派发同一方案。
    fn request(&self, id: String, path: PathBuf) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let loaded = SchemeDict::load_from_file(&path);
            let dict = loaded.as_ref().ok().cloned();
            let error = loaded.err().map(|e| e.to_string());
            let _ = sender.send(SchemeLoadResult { id, dict, error });
        });
    }
}

/// 异步排行榜加载器：派生后台线程调用 `ApiClient::get_competition_rank`，
/// 经通道回传结果，主循环每帧 `poll_rank_loader` 消费，期间 TUI 不冻结（与 `SchemeLoader` 同构）。
struct RankLoader {
    sender: mpsc::Sender<RankLoadResult>,
    receiver: mpsc::Receiver<RankLoadResult>,
}

/// 后台排行榜拉取的一次性结果回传。
struct RankLoadResult {
    /// 拉取的比赛类型。
    competition_type: CompetitionType,
    /// 拉取的期次日期（与请求时一致，用于校验是否仍匹配当前视图）。
    date: String,
    /// 拉取结果：成功为榜单，失败为错误。
    result: Result<CompetitionRank, ApiError>,
}

impl RankLoader {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self { sender, receiver }
    }

    /// 派发一次后台排行榜拉取：在独立线程调用 `client.get_competition_rank` 并回传结果。
    /// `client` 需在调用前 clone（线程所有权移交），其内部会话与 `app.api` 共享。
    fn request(&self, client: ApiClient, competition_type: CompetitionType, date: String) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = client.get_competition_rank(competition_type, &date);
            let _ = sender.send(RankLoadResult {
                competition_type,
                date,
                result,
            });
        });
    }
}

/// 登录模态框输入状态。
#[derive(Debug, Default)]
struct LoginForm {
    username: String,
    password: String,
    /// 焦点字段：0 = 用户名，1 = 密码。
    focus: usize,
    /// 提交中（网络请求进行中）。
    busy: bool,
    /// 错误提示。
    error: Option<String>,
}

/// 登录模态框按键动作。
#[derive(Debug, PartialEq, Eq)]
enum LoginAction {
    None,
    Submit,
    Cancel,
}

/// 文本设置弹窗目标字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextSettingTarget {
    Scheme,
    InputMethod,
}

/// 文本设置模态框按键动作。
#[derive(Debug, PartialEq, Eq)]
enum TextSettingModalAction {
    None,
    Save(TextSettingTarget, String),
    Cancel,
}

/// 自定义设置文本弹窗状态（用于反查方案路径与输入法名称）。
#[derive(Debug)]
struct TextSettingModal {
    target: TextSettingTarget,
    /// 当前正在编辑的文本。
    input: String,
}

impl TextSettingModal {
    /// 新建弹窗，预填当前自定义值（若为「无」或预设，则置空）。
    fn new(target: TextSettingTarget, current: &str) -> Self {
        let is_preset = match target {
            TextSettingTarget::Scheme => current == SCHEME_CUSTOM,
            TextSettingTarget::InputMethod => INPUT_METHOD_PRESETS.contains(&current),
        };
        let prefill = if is_preset {
            String::new()
        } else {
            current.to_string()
        };
        Self {
            target,
            input: prefill,
        }
    }

    /// 最大允许输入字符数。
    fn max_chars(&self) -> usize {
        match self.target {
            TextSettingTarget::Scheme => 128,
            TextSettingTarget::InputMethod => Settings::INPUT_METHOD_MAX_CHARS,
        }
    }

    /// 追加字符。
    fn push_char(&mut self, c: char) {
        if self.input.chars().count() < self.max_chars() {
            self.input.push(c);
        }
    }

    /// 删除末字符（Backspace）。
    fn pop_char(&mut self) {
        let mut chars = self.input.chars();
        chars.next_back();
        self.input = chars.as_str().to_string();
    }

    fn commit(&self) -> String {
        match self.target {
            TextSettingTarget::Scheme => self.input.trim().to_string(),
            TextSettingTarget::InputMethod => Settings::clamp_input_method(&self.input),
        }
    }
}

impl App {
    fn new(text: Text) -> Self {
        std::thread::Builder::new()
            .name("dazitui-prewarm".into())
            .spawn(prewarm_segmenter)
            .ok();
        Self::new_with(
            text,
            TokenStore::with_default_path(),
            ApiClient::new(),
            SettingsStore::with_default_path(),
            DbWorker::start(StatsDb::default_path()).ok(),
        )
    }

    /// 指定 token 存储、API 客户端与设置存储（测试注入；生产用 `new`）。
    fn new_with(
        text: Text,
        token_store: TokenStore,
        api: ApiClient,
        settings_store: SettingsStore,
        db_worker: Option<DbWorker>,
    ) -> Self {
        let mut settings = settings_store.load();
        // 规范化为 schema_id（向后兼容旧版写入路径的配置文件）。
        settings.scheme = normalize_scheme_to_id(&settings.scheme);
        // 自动发现 fcitx5 部署目录中的真方案。
        let discovered = discover_schemes(&default_rime_data_dir());
        // 首启零配置：scheme 为空时自动选第一个发现的真方案并持久化。
        if settings.scheme.is_empty() {
            if let Some(first) = discovered.first() {
                settings.scheme = first.id.clone();
                let _ = settings_store.save(&settings);
            }
        }
        let session = {
            let wb = text.session_word_boundaries();
            Session::new_gated_with_words_and_size(
                &text.content,
                text.source.is_builtin(),
                &wb,
                settings.group_size as usize,
            )
        };
        // 自动登录与会话恢复：若未登录且有环境变量则尝试自动登录。
        let login_notice = if !api.is_logged_in()
            && let Some((user, pass)) = env_credentials(|k| std::env::var(k).ok())
        {
            match api.login(&user, &pass) {
                Ok(_) => Some("已通过环境变量登录".to_string()),
                Err(e) => Some(format!("自动登录失败: {}", api_error_text(&e))),
            }
        } else {
            None
        };
        let logged_in = api.is_logged_in();
        let token = api.current_token();
        let mut app = Self {
            text,
            session,
            start: Instant::now(),
            accumulated_elapsed: Duration::ZERO,
            active_start: None,
            paused: false,
            state: AppState::Typing,
            sidebar_visible: true,
            sidebar_selected: 0,
            sidebar_notice: None,
            browse_files: Vec::new(),
            browse_selection: 0,
            builtin_selection: 0,
            browse_error: None,
            error_point_selected: 0,
            error_point_scroll: 0,
            token_store,
            token,
            logged_in,
            api,
            login_form: None,
            login_notice,
            online_loading: None,
            online_error: None,
            settings,
            settings_store,
            settings_focus: FOCUS_THEME,
            builtin_shuffle: false,
            builtin_preview: None,
            resume_prompt: None,
            last_saved_completed: 0,
            text_setting_modal: None,
            free_input_modal: None,
            live_keyboard: LiveKeyboard::new(),
            scheme_dict: None,
            discovered,
            scheme_cache: HashMap::new(),
            scheme_loading: None,
            scheme_loader: SchemeLoader::new(),
            rank_loader: RankLoader::new(),
            rank_column_modal: None,
            scheme_watcher: scheme_watcher::SchemeWatcher::new().ok(),
            scheme_watch_paths: None,
            scheme_reload_pending_at: None,
            scheme_hot_reload_expected: false,
            scheme_reload_flash_at: None,
            scheme_reload_error: None,
            db_worker,
            sponsor_state: RefCell::new(None),
        };
        app.reload_scheme_dict();
        app
    }

    /// 成绩视图「错字时间线」的错字条目数；非成绩视图返回 `None`。
    fn error_point_count(&self) -> Option<usize> {
        match &self.state {
            AppState::Finished { stats, .. } => Some(stats.error_points.len()),
            _ => None,
        }
    }

    /// 成绩视图「错字时间线」：选中项相对当前位置偏移 `delta` 条（正负均可），并同步滚动窗口。
    ///
    /// 越界自动夹取到首/末条；非成绩视图或无错字时无操作。
    fn move_error_point(&mut self, delta: isize) {
        let Some(total) = self.error_point_count() else {
            return;
        };
        if total == 0 {
            return;
        }
        let current = self.error_point_selected.min(total - 1) as isize;
        let next = (current + delta).clamp(0, total as isize - 1) as usize;
        self.error_point_selected = next;
        self.error_point_scroll =
            clamp_error_scroll(next, self.error_point_scroll, total, ERROR_TIMELINE_VISIBLE);
    }

    /// 成绩视图「错字时间线」：直接选中第 `idx` 条（越界夹取到首/末条）。
    fn select_error_point(&mut self, idx: usize) {
        let Some(total) = self.error_point_count() else {
            return;
        };
        if total == 0 {
            return;
        }
        let target = idx.min(total - 1) as isize;
        let current = self.error_point_selected.min(total - 1) as isize;
        self.move_error_point(target - current);
    }

    /// 进入设置视图：刷新「已发现方案」列表（按当前 fcitx5 部署目录），再切到 Settings 状态。
    ///
    /// 满足 spec #82 Out-of-Scope「打开设置时按当时目录刷新」——新部署的方案立即可见。
    fn enter_settings(&mut self) {
        self.discovered = discover_schemes(&default_rime_data_dir());
        self.state = AppState::Settings;
    }

    /// 根据当前选中的 `schema_id` 重新加载方案码表（异步、非阻塞）。
    ///
    /// 优先级：缓存命中（瞬时切换）→ 发现结果精确匹配 → 自定义映射 → 旧式多目录解析。
    /// 定位到路径后交后台线程解析，加载期间 `scheme_loading` 标记当前方案，主循环 poll 回传结果。
    pub fn reload_scheme_dict(&mut self) {
        let id = self.settings.scheme.clone();
        if id.is_empty() {
            self.scheme_dict = None;
            return;
        }
        // 1. 缓存命中：瞬时切换，无需后台加载。
        if let Some(cached) = self.scheme_cache.get(&id) {
            self.scheme_dict = Some(cached.clone());
            // 重建热监控闭包到当前方案（issue #95）：切回已缓存方案时若不复建监控，
            // 会仍然监控旧方案的源文件，导致热重载错乱。
            self.rebuild_scheme_watcher();
            return;
        }
        // 2. 已在加载同一方案：避免重复派发。
        if self.scheme_loading.as_deref() == Some(id.as_str()) {
            return;
        }
        // 3. 定位 .schema.yaml 路径（发现结果优先，自定义回退，最后旧式多目录解析）。
        let path = resolve_scheme_path_via_discovery(
            &id,
            &self.discovered,
            &self.settings.scheme_dict_paths,
        );
        match path {
            Some(path) => {
                self.scheme_loading = Some(id.clone());
                self.scheme_loader.request(id, path);
            }
            None => {
                // 无法定位：清空当前反查（保持与「无」一致的行为）。
                if self.scheme_loading.is_none() {
                    self.scheme_dict = None;
                }
            }
        }
    }

    /// 每帧从异步加载通道取回已完成的结果，填充缓存并激活当前方案。
    /// 主循环在绘制前调用，确保本帧即反映加载结果。
    fn poll_scheme_loader(&mut self) {
        while let Ok(result) = self.scheme_loader.receiver.try_recv() {
            if let Some(dict) = result.dict {
                self.scheme_cache.insert(result.id.clone(), dict.clone());
                // 仅当仍是当前选中方案时才激活，并据此建立/重建热监控（#94/#95）。
                if self.settings.scheme == result.id {
                    self.scheme_dict = Some(dict);
                    self.rebuild_scheme_watcher();
                    // 本次成功回传来自热监控触发的重载：闪现「方案已重载」，并清除既有失败提示（互斥）。
                    if self.scheme_hot_reload_expected {
                        self.scheme_hot_reload_expected = false;
                        self.scheme_reload_flash_at =
                            Some(Instant::now() + SCHEME_RELOAD_FLASH_DURATION);
                        self.scheme_reload_error.take();
                    }
                }
            } else if self.settings.scheme == result.id {
                // 加载失败（路径缺失/解析错误）。
                if self.scheme_hot_reload_expected {
                    // 热监控触发的重载失败：保留上一版方案（不清空、不空白），
                    // 并在状态栏置位失败提示（issue #98）。
                    self.scheme_hot_reload_expected = false;
                    let reason = result
                        .error
                        .clone()
                        .unwrap_or_else(|| "未知错误".to_string());
                    self.scheme_reload_error = Some(format!("方案重载失败：{reason}"));
                    // 失败优先：清除可能残留的成功闪现，使报错立即显示（与成功闪现互斥）。
                    self.scheme_reload_flash_at = None;
                } else {
                    // 非热重载（如初始/手动切换到坏方案）：维持既有清空行为。
                    self.scheme_dict = None;
                }
            }
            if self.scheme_loading.as_deref() == Some(result.id.as_str()) {
                self.scheme_loading = None;
            }
        }
    }

    /// 每帧从异步排行榜通道取回已完成的结果，填充对应比赛 Tab 的榜单缓存。
    /// 仅当当前仍处于在线排行榜视图且比赛/期次匹配时才落盘，避免悬空结果覆盖。
    fn poll_rank_loader(&mut self) {
        while let Ok(result) = self.rank_loader.receiver.try_recv() {
            if let AppState::OnlineRank(state) = &mut self.state {
                // 仅接受与当前期次日期一致的回包：跨天后的旧请求作废（#107）。
                if state.date != result.date {
                    continue;
                }
                // 无论当前激活 Tab 是否为本回包所属比赛，都写入对应缓存，
                // 保证按 Tab 切换不重复拉取（#104）。仅当该榜为当前激活 Tab 时自动滚到我的行。
                let board = state.boards.entry(result.competition_type).or_default();
                board.loading = false;
                match result.result {
                    Ok(rank) => {
                        let is_active = state.active_tab == result.competition_type;
                        if is_active {
                            // 登录态下自动滚动到当前用户所在行，使高亮行立即可见。
                            if let Some(mine) = rank.my_rank_result.first() {
                                if let Some(idx) =
                                    rank.rank_result.iter().position(|r| r.rank == mine.rank)
                                {
                                    board.scroll = idx as u16;
                                }
                            }
                        }
                        board.data = Some(rank);
                        board.error = None;
                    }
                    Err(e) => {
                        board.error = Some(api_error_text(&e));
                    }
                }
            }
        }
    }

    /// 进入在线排行榜视图：初始化三 Tab 状态并立即拉取默认（极速杯）榜单。
    fn open_online_rank(&mut self) -> io::Result<()> {
        let date = today_ymd();
        self.state = AppState::OnlineRank(OnlineRankState {
            active_tab: CompetitionType::Jisu,
            date: date.clone(),
            boards: HashMap::new(),
            error: None,
        });
        self.fetch_rank(CompetitionType::Jisu, &date);
        Ok(())
    }

    /// 打开「自定义列」弹窗（从在线排行榜视图内）。
    fn open_rank_column_modal(&mut self) {
        self.rank_column_modal = Some(RankColumnModal::default());
    }

    /// 关闭「自定义列」弹窗，并将当前列显隐配置落盘持久化。
    fn close_rank_column_modal(&mut self) {
        self.rank_column_modal = None;
        let _ = self.settings_store.save(&self.settings);
    }

    /// 触发指定比赛榜单的后台拉取：先标记该 Tab 加载中，再派发后台线程。
    /// 已登录时 `app.api` 携带 token，服务端会在 `my_rank_result` 回填当前用户整行。
    fn fetch_rank(&mut self, competition_type: CompetitionType, date: &str) {
        if let AppState::OnlineRank(state) = &mut self.state {
            state.boards.entry(competition_type).or_default().loading = true;
            state.error = None;
        }
        let client = self.api.clone();
        let date = date.to_string();
        self.rank_loader.request(client, competition_type, date);
    }

    /// 切换排行榜当前 Tab 并拉取对应榜单（封装对 `app.state` 的可变借用，避免与 `fetch_rank` 冲突）。
    /// 切换时同步刷新期次日期，确保跨自然日后 `snum` 仍为当天（#107）。
    fn switch_rank_tab(&mut self, tab: CompetitionType) {
        let date = if let AppState::OnlineRank(state) = &mut self.state {
            let today = today_ymd();
            if today != state.date {
                state.date = today.clone();
            }
            state.active_tab = tab;
            state.date.clone()
        } else {
            return;
        };
        self.fetch_rank(tab, &date);
    }

    /// 手动刷新当前 Tab：若期次日期已跨天则更新，并重新拉取。
    fn refresh_rank(&mut self) {
        let (tab, today) = if let AppState::OnlineRank(state) = &mut self.state {
            let today = today_ymd();
            if today != state.date {
                state.date = today.clone();
            }
            (state.active_tab, state.date.clone())
        } else {
            return;
        };
        self.fetch_rank(tab, &today);
    }

    /// 调整当前 Tab 榜单滚动偏移（`delta < 0` 上滚，`> 0` 下滚）。
    fn rank_scroll(&mut self, delta: i32) {
        if let AppState::OnlineRank(state) = &mut self.state {
            if let Some(board) = state.boards.get_mut(&state.active_tab) {
                if delta < 0 {
                    board.scroll = board.scroll.saturating_sub((-delta) as u16);
                } else {
                    board.scroll = board.scroll.saturating_add(delta as u16);
                }
            }
        }
    }

    /// 用当前 `scheme_dict` 的源文件闭包配置热监控。仅在成功从磁盘加载到码表后调用，
    /// 因此天然覆盖「切换方案后重建监控闭包」（#95）。
    ///
    /// 受总开关 `settings.monitor_scheme` 控制（issue #96）：关闭时卸载监控、不排定重载。
    fn rebuild_scheme_watcher(&mut self) {
        // 总开关关闭：卸载任何已建监控，保持「不监控」状态。
        if !self.settings.monitor_scheme {
            if let Some(w) = self.scheme_watcher.as_mut() {
                w.set_paths(&[]);
            }
            self.scheme_watch_paths = None;
            self.scheme_reload_pending_at = None;
            return;
        }
        if let (Some(watcher), Some(dict)) = (self.scheme_watcher.as_mut(), self.scheme_dict.as_ref())
        {
            let paths: Vec<PathBuf> = dict.source_paths().to_vec();
            watcher.set_paths(&paths);
            self.scheme_watch_paths = Some(paths);
            // 重置防抖，避免上次排定的重载在新方案上误触发。
            self.scheme_reload_pending_at = None;
        }
    }

    /// 每帧调用：检测方案源文件改动，经防抖后驱逐缓存并发起重载（复用既有 `SchemeLoader` 通道）。
    ///
    /// 受总开关 `settings.monitor_scheme` 控制（issue #96/#94）：关闭时直接返回，不检测、不重载。
    ///
    /// 关键陷阱（issue #94）：必须先 `scheme_cache.remove(current_id)` 再调用 `reload_scheme_dict()`，
    /// 否则后者会因缓存命中而短路返回，重载不会发生（这是热重载「改了不生效」的根因）。
    fn poll_scheme_hot_reload(&mut self) {
        // 闪现提示过期即清除（即使总开关关闭也清理，避免残留旧提示）。
        if let Some(at) = self.scheme_reload_flash_at {
            if Instant::now() >= at {
                self.scheme_reload_flash_at = None;
            }
        }
        // 总开关关闭：清空任何排定中的重载并停止检测。
        if !self.settings.monitor_scheme {
            self.scheme_reload_pending_at = None;
            return;
        }
        // 每帧排空监控通道：吸收本次及突发写盘（原子保存 temp+rename 多事件）产生的所有事件，
        // 避免残留事件在防抖到期后二次触发重载（issue #94 防抖合并）。
        let changed = self
            .scheme_watcher
            .as_mut()
            .map(|w| w.drain_changed())
            .unwrap_or(false);
        if changed {
            // 任意改动都重置防抖计时：连续写盘在 `SCHEME_RELOAD_DEBOUNCE` 窗口内只触发一次重载。
            self.scheme_reload_pending_at = Some(Instant::now() + SCHEME_RELOAD_DEBOUNCE);
            return;
        }
        // 无新改动且防抖到点：执行一次真正重载（期间忽略事件避免重复风暴）。
        if let Some(at) = self.scheme_reload_pending_at {
            if Instant::now() >= at {
                self.scheme_reload_pending_at = None;
                // 标记本次重载由热监控触发，结果回传时才闪现成功提示（区分初始/手动加载）。
                self.scheme_hot_reload_expected = true;
                // 先驱逐缓存，破除 `reload_scheme_dict` 的缓存命中短路。
                let id = self.settings.scheme.clone();
                self.scheme_cache.remove(&id);
                self.reload_scheme_dict();
            }
        }
    }

    /// 当前是否应显示「方案已重载」闪现提示（成功热重载后约 2 秒）。仅测试使用。
    #[cfg(test)]
    fn scheme_reload_flash_active(&self) -> bool {
        matches!(self.scheme_reload_flash_at, Some(at) if Instant::now() < at)
    }

    /// 状态栏闪现/报错提示的内容与样式；无提示时为 `None`。
    /// 优先级：成功闪现 > 失败报错（成功闪现期间已清除报错，issue #97/#98）。
    /// 成功闪现淡出：剩余不足 600ms 时改用 `muted` 色，避免硬截断观感突兀（issue #97）。
    fn scheme_reload_status(&self) -> Option<(String, Style)> {
        // 成功闪现优先
        if let Some(at) = self.scheme_reload_flash_at {
            if Instant::now() < at {
                let remaining = at.saturating_duration_since(Instant::now());
                let color = if remaining < Duration::from_millis(600) {
                    self.palette().muted
                } else {
                    self.palette().accent
                };
                return Some(("✓ 方案已重载".to_string(), Style::default().fg(color)));
            }
        }
        // 失败报错（持续显示，直到下次成功重载清除）
        if let Some(err) = &self.scheme_reload_error {
            return Some((
                format!("⚠ {err}"),
                Style::default().fg(self.palette().error),
            ));
        }
        None
    }

    /// 计算当前总活跃用时（已累计用时 + 当前活跃段）。
    fn current_elapsed(&self) -> Duration {
        if let Some(active) = self.active_start {
            self.accumulated_elapsed + active.elapsed()
        } else {
            self.accumulated_elapsed
        }
    }

    /// 暂停跟打计时。
    fn pause(&mut self) {
        if let Some(active) = self.active_start.take() {
            self.accumulated_elapsed += active.elapsed();
        }
        self.paused = true;
    }

    /// 确保跟打计时处于启动活跃态。
    fn touch_typing(&mut self) {
        if self.paused || self.active_start.is_none() {
            self.active_start = Some(Instant::now());
            self.paused = false;
        }
    }

    /// 当前主题的语义色板。
    fn theme(&self) -> Theme {
        Theme::preset(self.settings.theme)
    }

    /// 当前主题调色板（ratatui-themes）。
    fn palette(&self) -> ThemePalette {
        theme_palette(self.settings.theme)
    }

    /// 切换到下一主题并即时持久化。
    fn next_theme(&mut self) {
        self.settings.theme = self.settings.theme.next();
        let _ = self.settings_store.save(&self.settings);
    }

    /// 切换到上一主题并即时持久化。
    fn prev_theme(&mut self) {
        self.settings.theme = self.settings.theme.prev();
        let _ = self.settings_store.save(&self.settings);
    }

    /// 调整对照区占比（±5%，越界截断）并即时持久化。
    fn adjust_ratio(&mut self, delta: i8) {
        self.settings.reference_ratio = adjust_ratio_value(self.settings.reference_ratio, delta);
        let _ = self.settings_store.save(&self.settings);
    }

    /// 切换粗体开关并即时持久化。
    fn toggle_bold(&mut self) {
        self.settings.bold = !self.settings.bold;
        let _ = self.settings_store.save(&self.settings);
    }

    /// 切换遍码提示开关并即时持久化。
    fn toggle_code_hint(&mut self) {
        self.settings.code_hint = !self.settings.code_hint;
        let _ = self.settings_store.save(&self.settings);
    }

    /// 切换方案热监控总开关并即时持久化；开关即时生效（开→重建监控，关→卸载监控）。
    fn toggle_monitor_scheme(&mut self) {
        self.settings.monitor_scheme = !self.settings.monitor_scheme;
        let _ = self.settings_store.save(&self.settings);
        // 复用 arm 逻辑：开则重建监控、关则卸载，保证开关即时生效。
        self.rebuild_scheme_watcher();
    }

    /// 切换到下一实时键盘模式并即时持久化。
    fn next_keyboard_mode(&mut self) {
        self.settings.keyboard_mode = self.settings.keyboard_mode.next();
        let _ = self.settings_store.save(&self.settings);
    }

    /// 切换到上一实时键盘模式并即时持久化。
    fn prev_keyboard_mode(&mut self) {
        self.settings.keyboard_mode = self.settings.keyboard_mode.prev();
        let _ = self.settings_store.save(&self.settings);
    }

    /// 打开登录模态框。
    fn open_login(&mut self) {
        self.login_form = Some(LoginForm::default());
        self.login_notice = None;
    }

    /// 关闭登录模态框（不改变登录状态）。
    fn close_login(&mut self) {
        self.login_form = None;
    }

    /// 打开自由发文模态框。
    fn open_free_input(&mut self) {
        self.free_input_modal = Some(FreeInputModal::new());
        self.sidebar_notice = None;
    }

    /// 关闭自由发文模态框。
    fn close_free_input(&mut self) {
        self.free_input_modal = None;
    }

    /// 提交自由发文：处理内容、可选保存文件、载入赛文并开启新跟打。
    fn submit_free_input(&mut self, title: String, content: String, save: Option<PathBuf>) {
        let final_title = if title.trim().is_empty() {
            "自由发文".to_string()
        } else {
            title.trim().to_string()
        };
        if let Some(path) = save
            && let Err(e) = save_text_to_file(&path, &content)
            && let Some(modal) = self.free_input_modal.as_mut()
        {
            modal.error = Some(format!("保存文件失败: {e}"));
            return;
        }
        match load_text_from_string(
            &final_title,
            content,
            TextSource::Custom,
            &LoadOptions::default(),
        ) {
            Ok(text) => {
                self.text = text;
                self.free_input_modal = None;
                self.restart();
                self.sidebar_notice = Some(format!("已载入: {final_title}"));
            }
            Err(err) => {
                if let Some(modal) = self.free_input_modal.as_mut() {
                    modal.error = Some(match err {
                        LoadError::Empty => "赛文正文为空或处理后为空".to_string(),
                        _ => "载入赛文失败".to_string(),
                    });
                }
            }
        }
    }

    /// 从剪贴板载入赛文并开始跟打。
    fn load_from_clipboard(&mut self) {
        match load_text_from_clipboard(&LoadOptions::default()) {
            Ok(text) => {
                self.text = text;
                self.restart();
                self.sidebar_notice = Some("已载入剪贴板赛文".to_string());
            }
            Err(err) => {
                self.sidebar_notice = Some(match err {
                    LoadError::Empty => "剪贴板为空或处理后为空".to_string(),
                    LoadError::ReadFailed => "无法读取系统剪贴板".to_string(),
                    LoadError::NotFound => "剪贴板未找到文本".to_string(),
                });
            }
        }
    }

    /// 打开赞赏与支持视图。
    fn open_sponsor(&mut self) {
        self.state = AppState::Sponsor;
    }

    /// 提交登录：调用网关，成功后持久化 token。
    fn submit_login(&mut self) {
        let Some(form) = self.login_form.as_mut() else {
            return;
        };
        if form.username.is_empty() || form.password.is_empty() {
            form.error = Some("用户名和密码不能为空".to_string());
            return;
        }
        form.busy = true;
        form.error = None;
        match self.api.login(&form.username, &form.password) {
            Ok(r) => {
                let _ = self.token_store.save(&r.token);
                self.token = Some(r.token);
                self.logged_in = true;
                self.login_form = None;
                self.login_notice = Some("登录成功".to_string());
                if let AppState::Finished { stats, elapsed, .. } = &self.state {
                    let stats = stats.clone();
                    let elapsed = *elapsed;
                    self.do_upload(&stats, elapsed);
                }
            }
            Err(e) => {
                form.busy = false;
                form.error = Some(api_error_text(&e));
            }
        }
    }

    /// 重打当前赛文：重置会话与计时。
    /// 若当前赛文为乱序版，重新打乱以获得新的随机排列。
    fn restart(&mut self) {
        if self.text.shuffled
            && let TextSource::Builtin { set } = self.text.source
        {
            self.text = load_builtin_text_shuffled(set);
        }
        let wb = self.text.session_word_boundaries();
        self.session = Session::new_gated_with_words_and_size(
            &self.text.content,
            self.text.source.is_builtin(),
            &wb,
            self.settings.group_size as usize,
        );
        self.start = Instant::now();
        self.accumulated_elapsed = Duration::ZERO;
        self.last_saved_completed = 0;
        self.active_start = None;
        self.paused = false;
        self.live_keyboard.clear();
        self.state = AppState::Typing;
        self.browse_error = None;
    }

    /// 仅重建当前赛文会话与清状态，不重置计时、不切状态（供进入倒计时准备阶段）。
    fn prepare_session(&mut self) {
        if self.text.shuffled
            && let TextSource::Builtin { set } = self.text.source
        {
            self.text = load_builtin_text_shuffled(set);
        }
        let wb = self.text.session_word_boundaries();
        self.session = Session::new_gated_with_words_and_size(
            &self.text.content,
            self.text.source.is_builtin(),
            &wb,
            self.settings.group_size as usize,
        );
        self.last_saved_completed = 0;
        self.live_keyboard.clear();
        self.browse_error = None;
    }

    /// 进入开始倒计时：准备会话并弹出 3-2-1 弹窗，倒计时结束后才真正开始计时。
    fn enter_countdown(&mut self, source: CountdownSource) {
        self.prepare_session();
        self.state = AppState::Countdown {
            deadline: Instant::now() + COUNTDOWN_SECS,
            source,
        };
    }

    /// 进入继续跟打倒计时：保留当前会话与已累计用时，仅冻结计时并弹出 2-1 弹窗。
    /// 暂停态（paused=true, active_start=None）下计时本就冻结，倒计时期间保持不变。
    fn enter_resume_countdown(&mut self) {
        self.state = AppState::Countdown {
            deadline: Instant::now() + COUNTDOWN_SECS,
            source: CountdownSource::Resume,
        };
    }

    /// 倒计时结束：真正开始跟打（重置计时并进入 Typing）。
    fn launch_countdown(&mut self) {
        self.start = Instant::now();
        self.accumulated_elapsed = Duration::ZERO;
        self.active_start = None;
        self.paused = false;
        self.live_keyboard.clear();
        self.state = AppState::Typing;
    }

    /// 继续跟打倒计时结束：从暂停处续接计时（不清零），回到 Typing 态。
    fn complete_resume_countdown(&mut self) {
        self.active_start = Some(Instant::now());
        self.paused = false;
        self.state = AppState::Typing;
    }

    /// 取消倒计时，回到进入前的界面（续打则回到暂停态）。
    fn cancel_countdown(&mut self, source: CountdownSource) {
        self.state = match source {
            CountdownSource::Browsing => AppState::Browsing,
            CountdownSource::BrowsingBuiltin => AppState::BrowsingBuiltin,
            CountdownSource::Resume => AppState::Typing,
            // 在线赛文无独立浏览态，取消后回到就绪态（功能栏可见，会话已清空）。
            CountdownSource::Online => AppState::Typing,
        };
    }

    /// 若正处于倒计时且已到期，按来源自动进入跟打或续接（主循环每帧调用）。
    fn advance_countdown_if_due(&mut self) {
        if let AppState::Countdown { deadline, source } = self.state {
            if Instant::now() >= deadline {
                match source {
                    CountdownSource::Browsing | CountdownSource::BrowsingBuiltin => {
                        self.launch_countdown();
                    }
                    CountdownSource::Resume => self.complete_resume_countdown(),
                    // 在线赛文倒计时结束：与选文一致，重置计时后直接开打。
                    CountdownSource::Online => self.launch_countdown(),
                }
            }
        }
    }

    /// 完成跟打：计算成绩并进入成绩视图。
    ///
    /// 在线赛文置为「上传中」并返回 `Some((成绩, 用时))` 供调用方继续上传；
    /// 离线赛文直接进入成绩视图，返回 `None`。
    fn finish_typing(&mut self) -> Option<(Stats, Duration)> {
        if let Some(active) = self.active_start.take() {
            self.accumulated_elapsed += active.elapsed();
        }
        let elapsed = if self.accumulated_elapsed.is_zero() {
            self.start.elapsed()
        } else {
            self.accumulated_elapsed
        };
        let stats = self.session.finish(elapsed);
        // 新一轮成绩：错字时间线从头开始选中。
        self.error_point_selected = 0;
        self.error_point_scroll = 0;

        // 异步持久化有效练习流水到 SQLite 数据库（过滤字数为0或用时 < 0.5s 的瞬时/无效跟打，防止脏数据污染）
        if let Some(worker) = &self.db_worker
            && stats.typed_chars > 0
            && elapsed >= Duration::from_millis(500)
            && stats.wpm <= 2000.0
        {
            let session_record = SessionRecord::from_stats(
                &stats,
                elapsed,
                &self.text.title,
                &self.settings.input_method,
            );
            let session_id = session_record.id.clone();
            let word_index = self.text.build_word_index();
            let errors: Vec<ErrorRecordItem> = stats
                .error_points
                .iter()
                .enumerate()
                .filter_map(|(idx, ep)| {
                    let (target_char, actual_char, error_type_str) = match &ep.error_type {
                        ErrorType::Mismatch { typed, expected } => {
                            (*expected, Some(*typed), "Mismatch")
                        }
                        ErrorType::Backspace { deleted } => (None, Some(*deleted), "Backspace"),
                    };
                    // 过滤标点符号与特殊字符（仅统计汉字、字母与数字）
                    if target_char.is_some_and(|c| !c.is_alphanumeric())
                        || (target_char.is_none()
                            && actual_char.is_some_and(|c| !c.is_alphanumeric()))
                    {
                        return None;
                    }
                    let target_word = target_char
                        .and_then(|ch| word_index.find_word_containing_char(ch))
                        .or_else(|| word_index.get_word_at(idx))
                        .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
                        .map(|w| w.to_string());
                    Some(ErrorRecordItem::new(
                        &session_id,
                        ep.time_secs,
                        idx as u32,
                        target_char,
                        actual_char,
                        target_word,
                        error_type_str,
                    ))
                })
                .collect();

            let keys: Vec<KeypressRecordItem> = stats
                .key_frequency
                .iter()
                .map(|(k, count)| KeypressRecordItem {
                    session_id: session_id.clone(),
                    key_code: k.clone(),
                    press_count: *count,
                    is_raw: true,
                })
                .collect();

            let _ = worker.send(DbTask::SaveSession {
                session: session_record,
                errors,
                keys,
            });
        }

        let is_online = self.text.is_online();
        // 内置赛文整本打完：把进度记为「全部完成」并落盘，供下次打开时提示重置。
        if self.text.source.is_builtin() {
            self.save_builtin_progress(self.session.total_groups());
        }
        if is_online {
            self.state = AppState::Finished {
                stats: stats.clone(),
                upload: UploadState::Uploading,
                elapsed,
            };
            Some((stats, elapsed))
        } else {
            let copied_stats = if copies_stats_to_clipboard(self.text.source) {
                let share = format_stats_share_text(
                    &self.text,
                    &stats,
                    elapsed,
                    &self.settings.input_method,
                    None,
                );
                write_clipboard(&share);
                Some(share)
            } else {
                None
            };
            self.state = AppState::Finished {
                stats,
                upload: UploadState::NotApplicable { copied_stats },
                elapsed,
            };
            None
        }
    }

    /// 进入载文浏览：扫描当前目录文本文件。
    fn open_browser(&mut self) {
        self.browse_files = list_text_files(&std::env::current_dir().unwrap_or_default());
        self.browse_selection = 0;
        self.browse_error = None;
        self.state = AppState::Browsing;
    }

    /// 载入当前选中的文件，成功后开始新跟打。
    fn load_selected(&mut self) {
        let Some(path) = self.browse_files.get(self.browse_selection).cloned() else {
            return;
        };
        match load_text_from_file(&path) {
            Ok(text) => {
                self.text = text;
                self.enter_countdown(CountdownSource::Browsing);
            }
            Err(err) => {
                self.browse_error = Some(match err {
                    LoadError::NotFound => "文件不存在".to_string(),
                    LoadError::Empty => "文件为空或处理后为空".to_string(),
                    LoadError::ReadFailed => "无法读取文件".to_string(),
                });
            }
        }
    }

    /// 进入内置赛文浏览：展示套题列表，可载入。
    fn open_builtin_browser(&mut self) {
        self.builtin_selection = 0;
        self.refresh_builtin_preview();
        self.state = AppState::BrowsingBuiltin;
    }

    /// 重新生成内置赛文浏览器预览缓存。
    /// 乱序开时加载打乱版（每次调用随机不同），关时顺序版。
    /// 在 `open_builtin_browser`、Up/Down 选区变化、s 切换乱序时调用。
    fn refresh_builtin_preview(&mut self) {
        let group_size = self.settings.group_size as usize;
        self.builtin_preview = Some(match BUILTIN_SETS.get(self.builtin_selection) {
            Some(&set) if self.builtin_shuffle => {
                let text = load_builtin_text_shuffled(set);
                let body = if set.is_words() {
                    let boundaries = text.word_boundaries.as_ref().unwrap();
                    let chars: Vec<char> = text.content.chars().collect();
                    builtin_word_preview(boundaries, &chars, group_size)
                } else {
                    builtin_char_preview(&text.content)
                };
                (text.title, body)
            }
            Some(&set) if set.is_words() => {
                let no_commas = set.content_no_commas();
                let boundaries = set.word_boundaries();
                let chars: Vec<char> = no_commas.chars().collect();
                (
                    set.name().to_string(),
                    builtin_word_preview(&boundaries, &chars, group_size),
                )
            }
            Some(&set) => (set.name().to_string(), builtin_char_preview(set.content())),
            None => ("预览".to_string(), "（无内置赛文）".to_string()),
        });
    }

    /// 循环切换内置赛文分组大小档位（5 -> 10 -> 15 -> 20 -> 25 -> 30 -> 50）并即时持久化。
    fn cycle_group_size(&mut self) {
        self.settings.group_size = Settings::next_group_size_preset(self.settings.group_size);
        let _ = self.settings_store.save(&self.settings);
        self.refresh_builtin_preview();
        if self.text.source.is_builtin() && self.session.is_empty() {
            let wb = self.text.session_word_boundaries();
            self.session = Session::new_gated_with_words_and_size(
                &self.text.content,
                true,
                &wb,
                self.settings.group_size as usize,
            );
        }
    }

    /// 载入当前选中的内置赛文：若已有存档进度则弹出「继续/重开/重置」选择，
    /// 否则直接进入第 0 组跟打。
    fn load_selected_builtin(&mut self) {
        let Some(set) = BUILTIN_SETS.get(self.builtin_selection).copied() else {
            return;
        };
        if let Some(p) = self.builtin_progress_for(set) {
            if p.completed_groups > 0 {
                let total = self.builtin_total_groups(set, p.group_size as usize);
                self.resume_prompt = Some((set, p.completed_groups as usize, total));
                return;
            }
        }
        self.text = if self.builtin_shuffle {
            load_builtin_text_shuffled(set)
        } else {
            load_builtin_text(set)
        };
        self.enter_countdown(CountdownSource::BrowsingBuiltin);
    }

    /// 从指定已完成组数开始某个内置赛文跟打。
    ///
    /// 若该赛文有存档，还原其「每赛文单独记」的分组大小；清空计时与暂停态，
    /// 并用 `set_completed_groups` 把会话跳到对应组。
    fn start_builtin_set(&mut self, set: BuiltinSet, completed_groups: usize) {
        if let Some(p) = self.builtin_progress_for(set) {
            self.settings.group_size = p.group_size;
        }
        self.text = if self.builtin_shuffle {
            load_builtin_text_shuffled(set)
        } else {
            load_builtin_text(set)
        };
        let wb = self.text.session_word_boundaries();
        self.session = Session::new_gated_with_words_and_size(
            &self.text.content,
            self.text.source.is_builtin(),
            &wb,
            self.settings.group_size as usize,
        );
        self.session.set_completed_groups(completed_groups);
        self.start = Instant::now();
        self.accumulated_elapsed = Duration::ZERO;
        self.active_start = None;
        self.paused = false;
        self.live_keyboard.clear();
        self.last_saved_completed = completed_groups;
        self.state = AppState::Typing;
    }

    /// 读取某内置赛文的已存进度。
    fn builtin_progress_for(&self, set: BuiltinSet) -> Option<BuiltinProgress> {
        self.settings.builtin_progress.get(set.name()).copied()
    }

    /// 计算某内置赛文在指定分组大小下的总组数（用于续打弹窗与进度展示）。
    fn builtin_total_groups(&self, set: BuiltinSet, group_size: usize) -> usize {
        let text = if self.builtin_shuffle {
            load_builtin_text_shuffled(set)
        } else {
            load_builtin_text(set)
        };
        let wb = text.session_word_boundaries();
        Session::new_gated_with_words_and_size(&text.content, true, &wb, group_size.max(1))
            .total_groups()
    }

    /// 保存当前内置赛文的进度（已完成组数 + 分组大小）。
    fn save_builtin_progress(&mut self, completed_groups: usize) {
        if let TextSource::Builtin { set } = self.text.source {
            let entry = BuiltinProgress {
                completed_groups: completed_groups as u32,
                group_size: self.settings.group_size,
            };
            self.settings
                .builtin_progress
                .insert(set.name().to_string(), entry);
            let _ = self.settings_store.save(&self.settings);
        }
    }

    /// 清除某内置赛文的存档进度。
    fn clear_builtin_progress(&mut self, set: BuiltinSet) {
        self.settings.builtin_progress.remove(set.name());
        let _ = self.settings_store.save(&self.settings);
    }

    /// 跟打中增量落盘：仅当已完成组数比上次存档更多时才写设置文件。
    fn persist_builtin_progress_if_changed(&mut self) {
        if !self.text.source.is_builtin() {
            return;
        }
        let cg = self.session.completed_groups();
        if cg > self.last_saved_completed {
            self.save_builtin_progress(cg);
            self.last_saved_completed = cg;
        }
    }

    /// 按比赛类型下载在线赛文并进入跟打。
    ///
    /// 调用前 `online_loading` 已由事件循环置为 `Some` 并渲染（保证「加载中...」可见）；
    /// 这里执行同步下载，成功后替换赛文，失败则回填错误提示。
    ///
    /// 不做 token 有效性预探测：getBaseInfo 的 isLogin 恒为 0、无法反映登录态，
    /// token 有效性唯一由上传接口（uploadResult）真实校验，失败走「登录已失效」提示。
    fn download_online(&mut self, competition_type: CompetitionType) {
        if !self.logged_in && !self.api.is_logged_in() {
            self.online_loading = None;
            self.online_error = Some("请先登录 52dazi（Ctrl-O）".to_string());
            return;
        }
        if !self.api.is_logged_in()
            && let Some(token) = &self.token
        {
            self.api.set_session(Some(AuthSession::from_token(token)));
        }
        match self.api.get_content(competition_type) {
            Ok(comp) => {
                let content = normalize_online_content(&comp.content);
                if content.is_empty() {
                    // 去空白后为空（如服务端返回空白/纯空格的赛文）：不进入跟打，
                    // 避免开打一个「完成即结束」的退化赛文。保持旧赛文不变，仅报错。
                    self.online_loading = None;
                    self.online_error = Some("赛文内容为空或仅含空白".to_string());
                    return;
                }
                self.text = Text {
                    title: comp.title,
                    content,
                    source: TextSource::Online { competition_type },
                    word_boundaries: None,
                    shuffled: false,
                };
                self.online_loading = None;
                self.online_error = None;
                // 进入三秒准备倒计时（与选文一致），倒计时结束由主循环 launch_countdown 真正开打。
                self.enter_countdown(CountdownSource::Online);
            }
            Err(e) => {
                self.online_loading = None;
                self.online_error = Some(api_error_text(&e));
            }
        }
    }

    /// 上传成绩并更新成绩视图状态（在线赛文完成跟打后调用）。
    fn do_upload(&mut self, stats: &Stats, elapsed: Duration) {
        let upload = self.perform_upload(stats, elapsed);
        self.state = AppState::Finished {
            stats: stats.clone(),
            upload,
            elapsed,
        };
    }

    /// 执行上传：调用 API 客户端一站式上传成绩（包含指标计算、payload 构建、网关通信、自动重登与分享文本生成）。
    fn perform_upload(&self, stats: &Stats, elapsed: Duration) -> UploadState {
        if !self.logged_in && !self.api.is_logged_in() {
            // 未登录时成绩也不该丢失：先把统计复制到剪贴板。
            let share = format_stats_share_text(
                &self.text,
                stats,
                elapsed,
                &self.settings.input_method,
                None,
            );
            write_clipboard(&share);
            return UploadState::Failed {
                message: "未登录，无法上传成绩".to_string(),
                need_relogin: true,
                detail: None,
                copied_stats: Some(share),
            };
        }
        if !self.api.is_logged_in()
            && let Some(token) = &self.token
        {
            self.api.set_session(Some(AuthSession::from_token(token)));
        }
        match self
            .api
            .upload_session(&self.text, stats, elapsed, &self.settings.input_method)
        {
            Ok(outcome) => {
                // 分享文本只写入剪贴板：成绩视图顶部摘要已展示全部指标，不再重复渲染。
                write_clipboard(&outcome.share_text);
                UploadState::Success {
                    ranking: outcome.ranking,
                }
            }
            Err(e) => {
                let need_relogin = is_auth_failure(&e);
                if need_relogin {
                    self.api.logout();
                    let _ = self.token_store.clear();
                }
                // 登录失效：主文案用友好提示，原始服务器错误降级为次要信息。
                // 传输/解析失败：主文案保持友好，原始错误作为次要信息透出（供诊断，不再吞掉）。
                let (message, detail) = if need_relogin {
                    (
                        "登录已失效，请重新登录".to_string(),
                        Some(api_error_text(&e)),
                    )
                } else {
                    let detail = match &e {
                        ApiError::Transport(raw) | ApiError::Parse(raw) => Some(raw.clone()),
                        ApiError::Server(_) => None,
                    };
                    (api_error_text(&e), detail)
                };
                // 上传失败也把统计复制到剪贴板：跟打结果不因网络问题而丢失。
                let share = format_stats_share_text(
                    &self.text,
                    stats,
                    elapsed,
                    &self.settings.input_method,
                    None,
                );
                write_clipboard(&share);
                UploadState::Failed {
                    message,
                    need_relogin,
                    detail,
                    copied_stats: Some(share),
                }
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(arg) = args.get(1) {
        if arg == "--version" || arg == "-V" || arg == "-v" {
            println!("dazitui {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        if arg == "--help" || arg == "-h" {
            println!(
                "dazitui {} - 终端现代化沉浸式中文跟打练习工具 (TUI)\n\n\
                用法:\n  \
                dazitui [文件路径]       载入指定本地文本文件开始跟打\n  \
                dazitui                 默认启动并载入精选单字前五百\n  \
                dazitui --version, -V   查看当前版本号\n  \
                dazitui --help, -h      查看帮助信息\n",
                env!("CARGO_PKG_VERSION")
            );
            return;
        }
    }
    // 无参数：默认载入首套内置赛文（常用单字前五百）。
    let Some(path) = args.get(1) else {
        let text = load_builtin_text(BUILTIN_SETS[0]);
        if let Err(e) = run_tui(App::new(text)) {
            eprintln!("错误: {e}");
            std::process::exit(1);
        }
        return;
    };

    let text = match load_text_from_file(Path::new(path)) {
        Ok(text) => text,
        Err(err) => {
            let msg = match err {
                LoadError::NotFound => format!("错误: 文件不存在: {path}"),
                LoadError::Empty => format!("错误: 文件为空: {path}"),
                LoadError::ReadFailed => format!("错误: 无法读取文件: {path}"),
            };
            eprintln!("{msg}");
            std::process::exit(1);
        }
    };

    if let Err(e) = run_tui(App::new(text)) {
        eprintln!("错误: {e}");
        std::process::exit(1);
    }
}

fn run_tui(app: App) -> io::Result<()> {
    let mut terminal = ratatui::init();
    // bracketed paste：中文输入法（fcitx/ibus）上屏以 paste 事件到达，必须启用
    // 键盘增强协议（Kitty keyboard protocol）：支持终端准确发送 Ctrl+Enter 修饰键
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableBracketedPaste,
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    );
    let result = event_loop(&mut terminal, app);
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::PopKeyboardEnhancementFlags,
        crossterm::event::DisableBracketedPaste
    );
    ratatui::restore();
    result
}

/// 在收到单个字符事件后，迅速清空 crossterm 事件缓冲区中紧随其后的可打印字符（例如输入法整词上屏 "怎么"）。
fn drain_pending_chars(first_char: char) -> io::Result<(String, Option<KeyEvent>)> {
    let mut text = String::new();
    text.push(first_char);
    let mut pending_key = None;
    while event::poll(Duration::ZERO)? {
        if let Event::Key(next_key) = event::read()? {
            if is_quit(next_key) {
                return Ok((text, Some(next_key)));
            }
            if let KeyCode::Char(next_c) = next_key.code {
                if next_key.modifiers.is_empty() || next_key.modifiers == KeyModifiers::SHIFT {
                    text.push(next_c);
                    continue;
                }
            }
            pending_key = Some(next_key);
            break;
        }
    }
    Ok((text, pending_key))
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, mut app: App) -> io::Result<()> {
    let mut pending_key_event: Option<KeyEvent> = None;
    loop {
        // 消费后台方案加载结果（非阻塞，TUI 不冻结）。
        app.poll_scheme_loader();
        // 消费后台排行榜拉取结果（非阻塞，TUI 不冻结）。
        app.poll_rank_loader();
        // 检测方案源文件改动并（防抖后）热重载（issue #91/#94）。
        app.poll_scheme_hot_reload();
        app.advance_countdown_if_due();
        terminal.draw(|frame| ui(frame, &app))?;
        let event_to_process = if let Some(pk) = pending_key_event.take() {
            Event::Key(pk)
        } else {
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            event::read()?
        };
        match event_to_process {
            Event::Key(key) => {
                if is_quit(key) {
                    return Ok(());
                }
                // 登录模态框打开时优先处理其按键。
                if app.login_form.is_some() {
                    let action =
                        login_input(app.login_form.as_mut().expect("login_form open"), key);
                    match action {
                        LoginAction::Cancel => app.close_login(),
                        LoginAction::Submit => app.submit_login(),
                        LoginAction::None => {}
                    }
                    continue;
                }
                // 自定义设置文本弹窗（方案/输入法）打开时优先处理其按键。
                if let Some(modal) = app.text_setting_modal.as_mut() {
                    let action = text_setting_modal_input(modal, key);
                    match action {
                        TextSettingModalAction::Cancel => {
                            app.text_setting_modal = None;
                        }
                        TextSettingModalAction::Save(target, value) => {
                            app.text_setting_modal = None;
                            match target {
                                TextSettingTarget::Scheme => {
                                    app.settings.scheme = normalize_scheme_to_id(&value);
                                    let _ = app.settings_store.save(&app.settings);
                                    app.reload_scheme_dict();
                                }
                                TextSettingTarget::InputMethod => {
                                    app.settings.input_method = value;
                                    let _ = app.settings_store.save(&app.settings);
                                }
                            }
                        }
                        TextSettingModalAction::None => {}
                    }
                    continue;
                }
                // 自由发文模态框打开时优先处理其按键。
                if let Some(modal) = app.free_input_modal.as_mut() {
                    let action = free_input_modal_input(modal, key);
                    match action {
                        FreeInputAction::Cancel => app.close_free_input(),
                        FreeInputAction::Submit {
                            title,
                            content,
                            save,
                        } => {
                            app.submit_free_input(title, content, save);
                        }
                        FreeInputAction::None => {}
                    }
                    continue;
                }
                // 列定制弹窗打开时优先处理其按键（↑↓ 选择 / Space 切换 / Esc 完成）。
                if app.rank_column_modal.is_some() {
                    let action = rank_column_modal_input(
                        app.rank_column_modal
                            .as_mut()
                            .expect("rank_column_modal open"),
                        &mut app.settings.rank_columns,
                        key,
                    );
                    if matches!(action, RankColumnModalAction::Close) {
                        app.close_rank_column_modal();
                    }
                    continue;
                }
                if is_open_login(key) {
                    app.open_login();
                    continue;
                }
                let mut key = key;
                if !matches!(app.state, AppState::Typing) || app.session.is_empty() || app.paused {
                    normalize_key(&mut key);
                }
                match app.state {
                    AppState::Typing => {
                        if !app.session.is_empty() && !app.paused {
                            // 跟打进行中：Esc 或 Tab 暂停切入 Normal 菜单态
                            if key.code == KeyCode::Esc || key.code == KeyCode::Tab {
                                app.pause();
                                continue;
                            }
                            if key.code == KeyCode::Backspace {
                                app.touch_typing();
                                let elapsed = app.current_elapsed();
                                handle_key(
                                    &mut app.session,
                                    &mut app.live_keyboard,
                                    app.scheme_dict.as_ref(),
                                    key,
                                    elapsed,
                                    Instant::now(),
                                );
                                if app.session.is_complete() {
                                    finish_and_maybe_upload(&mut app, terminal)?;
                                }
                                app.persist_builtin_progress_if_changed();
                                continue;
                            }
                            if let KeyCode::Char(c) = key.code {
                                let (text, next_key) = drain_pending_chars(c)?;
                                pending_key_event = next_key;
                                app.touch_typing();
                                let elapsed = app.current_elapsed();
                                handle_text(
                                    &mut app.session,
                                    &mut app.live_keyboard,
                                    app.scheme_dict.as_ref(),
                                    &text,
                                    elapsed,
                                    Instant::now(),
                                );
                                if app.session.is_complete() {
                                    finish_and_maybe_upload(&mut app, terminal)?;
                                }
                                app.persist_builtin_progress_if_changed();
                                continue;
                            }
                            continue;
                        }

                        // 就绪态 (app.session.is_empty()) 或 暂停态 (app.paused) —— Normal 命令态
                        if key.code == KeyCode::Tab {
                            if app.paused {
                                app.enter_resume_countdown();
                            } else {
                                app.sidebar_visible = !app.sidebar_visible;
                            }
                            continue;
                        }

                        if app.paused {
                            if key.code == KeyCode::Esc
                                || key.code == KeyCode::Char('i')
                                || key.code == KeyCode::Char('I')
                            {
                                app.enter_resume_countdown();
                                continue;
                            }
                            if is_early_finish(key) {
                                finish_and_maybe_upload(&mut app, terminal)?;
                                continue;
                            }
                        }

                        // Vim jk 上下移动功能栏菜单
                        if key.code == KeyCode::Up
                            || key.code == KeyCode::Char('k')
                            || key.code == KeyCode::Char('K')
                        {
                            app.sidebar_selected = if app.sidebar_selected == 0 {
                                SIDEBAR_MENU_ITEMS.len() - 1
                            } else {
                                app.sidebar_selected - 1
                            };
                            continue;
                        }
                        if key.code == KeyCode::Down
                            || key.code == KeyCode::Char('j')
                            || key.code == KeyCode::Char('J')
                        {
                            app.sidebar_selected =
                                (app.sidebar_selected + 1) % SIDEBAR_MENU_ITEMS.len();
                            continue;
                        }
                        if key.code == KeyCode::Char('l') {
                            activate_sidebar_menu_item(&mut app, terminal)?;
                            continue;
                        }

                        // 助记快捷键直达
                        if is_open_browser(key) {
                            app.open_browser();
                            continue;
                        }
                        if is_open_builtin_browser(key) {
                            app.open_builtin_browser();
                            continue;
                        }
                        if is_open_free_input(key) {
                            app.open_free_input();
                            continue;
                        }
                        if is_load_clipboard(key) {
                            app.load_from_clipboard();
                            continue;
                        }
                        if is_open_stats(key) {
                            app.state =
                                AppState::Stats(StatsViewState::new(app.settings.heatmap_layout));
                            continue;
                        }
                        if is_open_settings(key) {
                            app.enter_settings();
                            continue;
                        }
                        if is_open_sponsor(key) {
                            app.open_sponsor();
                            continue;
                        }
                        if is_open_rank(key) {
                            let _ = app.open_online_rank();
                            continue;
                        }
                        if restart_allowed(key, app.text.is_online()) {
                            app.restart();
                            continue;
                        }
                        if let Some(competition_type) = online_shortcut(key) {
                            trigger_online_competition(&mut app, competition_type, terminal)?;
                            continue;
                        }

                        // 就绪态下输入非命令字符（如中文输入法上屏或英文首字）-> 自动切入跟打态
                        if app.session.is_empty() {
                            if key.code == KeyCode::Backspace {
                                app.touch_typing();
                                let elapsed = app.current_elapsed();
                                handle_key(
                                    &mut app.session,
                                    &mut app.live_keyboard,
                                    app.scheme_dict.as_ref(),
                                    key,
                                    elapsed,
                                    Instant::now(),
                                );
                                if app.session.is_complete() {
                                    finish_and_maybe_upload(&mut app, terminal)?;
                                }
                            } else if let KeyCode::Char(c) = key.code {
                                let (text, next_key) = drain_pending_chars(c)?;
                                pending_key_event = next_key;
                                app.touch_typing();
                                let elapsed = app.current_elapsed();
                                handle_text(
                                    &mut app.session,
                                    &mut app.live_keyboard,
                                    app.scheme_dict.as_ref(),
                                    &text,
                                    elapsed,
                                    Instant::now(),
                                );
                                if app.session.is_complete() {
                                    finish_and_maybe_upload(&mut app, terminal)?;
                                }
                            }
                        }
                    }
                    AppState::Finished { .. } => {
                        if handle_finished_key(&mut app, key) {
                            continue;
                        }
                    }
                    AppState::Browsing => {
                        if is_open_settings(key) {
                            app.enter_settings();
                            continue;
                        }
                        match key.code {
                            KeyCode::Up | KeyCode::Char('k') => {
                                app.browse_selection = app.browse_selection.saturating_sub(1);
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if !app.browse_files.is_empty() {
                                    app.browse_selection =
                                        (app.browse_selection + 1).min(app.browse_files.len() - 1);
                                }
                            }
                            KeyCode::Char('g') | KeyCode::Home => {
                                app.browse_selection = 0;
                            }
                            KeyCode::Char('G') | KeyCode::End => {
                                if !app.browse_files.is_empty() {
                                    app.browse_selection = app.browse_files.len() - 1;
                                }
                            }
                            KeyCode::Enter | KeyCode::Char('l') => app.load_selected(),
                            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') => {
                                app.state = AppState::Typing
                            }
                            _ => {}
                        }
                    }
                    AppState::BrowsingBuiltin => {
                        // 续打弹窗优先：拦截所有按键，处理「继续/重开/重置」选择。
                        if let Some((set, saved, total)) = app.resume_prompt {
                            match key.code {
                                KeyCode::Char('c')
                                | KeyCode::Char('C')
                                | KeyCode::Enter
                                | KeyCode::Char('l') => {
                                    // 已打完则从头开始新一轮；否则续到存档组。
                                    let completed = if saved >= total { 0 } else { saved };
                                    app.resume_prompt = None;
                                    app.start_builtin_set(set, completed);
                                }
                                KeyCode::Char('r') | KeyCode::Char('R') => {
                                    app.resume_prompt = None;
                                    app.start_builtin_set(set, 0);
                                }
                                KeyCode::Char('x')
                                | KeyCode::Char('d')
                                | KeyCode::Char('X')
                                | KeyCode::Char('D') => {
                                    app.clear_builtin_progress(set);
                                    app.resume_prompt = None;
                                    app.start_builtin_set(set, 0);
                                }
                                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') => {
                                    app.resume_prompt = None;
                                }
                                _ => {}
                            }
                            continue;
                        }
                        if is_open_settings(key) {
                            app.enter_settings();
                            continue;
                        }
                        match key.code {
                            KeyCode::Up | KeyCode::Char('k') => {
                                app.builtin_selection = app.builtin_selection.saturating_sub(1);
                                app.refresh_builtin_preview();
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                app.builtin_selection =
                                    (app.builtin_selection + 1).min(BUILTIN_SETS.len() - 1);
                                app.refresh_builtin_preview();
                            }
                            KeyCode::Home => {
                                app.builtin_selection = 0;
                                app.refresh_builtin_preview();
                            }
                            KeyCode::End => {
                                app.builtin_selection = BUILTIN_SETS.len().saturating_sub(1);
                                app.refresh_builtin_preview();
                            }
                            KeyCode::Char('g') | KeyCode::Char('G') => {
                                app.cycle_group_size();
                            }
                            KeyCode::Enter | KeyCode::Char('l') => app.load_selected_builtin(),
                            KeyCode::Char('s') | KeyCode::Char('S') => {
                                app.builtin_shuffle = !app.builtin_shuffle;
                                app.refresh_builtin_preview();
                            }
                            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') => {
                                app.state = AppState::Typing
                            }
                            _ => {}
                        }
                    }
                    AppState::Settings => match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.settings_focus = move_focus(app.settings_focus, -1)
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            app.settings_focus = move_focus(app.settings_focus, 1)
                        }
                        KeyCode::Left
                        | KeyCode::Char('h')
                        | KeyCode::Right
                        | KeyCode::Char('l') => {
                            let forward =
                                key.code == KeyCode::Right || key.code == KeyCode::Char('l');
                            match app.settings_focus {
                                FOCUS_THEME => {
                                    if forward {
                                        app.next_theme();
                                    } else {
                                        app.prev_theme();
                                    }
                                }
                                FOCUS_RATIO => app.adjust_ratio(if forward { 5 } else { -5 }),
                                FOCUS_BOLD => app.toggle_bold(),
                                FOCUS_CODE_HINT => app.toggle_code_hint(),
                                FOCUS_MONITOR_SCHEME => app.toggle_monitor_scheme(),
                                FOCUS_KEYBOARD => {
                                    if forward {
                                        app.next_keyboard_mode();
                                    } else {
                                        app.prev_keyboard_mode();
                                    }
                                }
                                FOCUS_SCHEME => {
                                    let opts = build_scheme_options(&app.discovered);
                                    let next = if forward {
                                        scheme_next_option(&opts, &app.settings.scheme)
                                    } else {
                                        scheme_prev_option(&opts, &app.settings.scheme)
                                    };
                                    app.settings.scheme = next;
                                    let _ = app.settings_store.save(&app.settings);
                                    app.reload_scheme_dict();
                                }
                                FOCUS_INPUT_METHOD => {
                                    let next = if forward {
                                        cycle_input_method_next(&app.settings.input_method)
                                    } else {
                                        cycle_input_method_prev(&app.settings.input_method)
                                    };
                                    app.settings.input_method = next;
                                    let _ = app.settings_store.save(&app.settings);
                                }
                                FOCUS_GROUP_SIZE => {
                                    let curr = app.settings.group_size;
                                    let next = if forward {
                                        (curr + 1).min(Settings::GROUP_SIZE_MAX)
                                    } else {
                                        (curr.saturating_sub(1)).max(Settings::GROUP_SIZE_MIN)
                                    };
                                    app.settings.group_size = next;
                                    let _ = app.settings_store.save(&app.settings);
                                    if app.text.source.is_builtin() && app.session.is_empty() {
                                        let wb = app.text.session_word_boundaries();
                                        app.session = Session::new_gated_with_words_and_size(
                                            &app.text.content,
                                            true,
                                            &wb,
                                            app.settings.group_size as usize,
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                        KeyCode::Enter => {
                            if app.settings_focus == FOCUS_SCHEME {
                                let opts = build_scheme_options(&app.discovered);
                                let idx = scheme_option_index(&opts, &app.settings.scheme);
                                if opts.get(idx) == Some(&SchemeOption::Custom) {
                                    app.text_setting_modal = Some(TextSettingModal::new(
                                        TextSettingTarget::Scheme,
                                        &app.settings.scheme,
                                    ));
                                }
                            } else if app.settings_focus == FOCUS_INPUT_METHOD
                                && input_method_preset_index(&app.settings.input_method)
                                    == INPUT_METHOD_PRESETS.len() - 1
                            {
                                app.text_setting_modal = Some(TextSettingModal::new(
                                    TextSettingTarget::InputMethod,
                                    &app.settings.input_method,
                                ));
                            }
                        }
                        KeyCode::Esc | KeyCode::Char('q') => app.state = AppState::Typing,
                        _ => {}
                    },
                    AppState::Stats(ref mut stats_state) => {
                        if is_open_settings(key) {
                            app.enter_settings();
                            continue;
                        }
                        match key.code {
                            KeyCode::Char('1') => stats_state.tab = StatsTab::WpmTrend,
                            KeyCode::Char('2') => stats_state.tab = StatsTab::Heatmap,
                            KeyCode::Char('3') => stats_state.tab = StatsTab::ErrorRanking,
                            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                                stats_state.tab = match stats_state.tab {
                                    StatsTab::WpmTrend => StatsTab::Heatmap,
                                    StatsTab::Heatmap => StatsTab::ErrorRanking,
                                    StatsTab::ErrorRanking => StatsTab::WpmTrend,
                                };
                            }
                            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                                stats_state.tab = match stats_state.tab {
                                    StatsTab::WpmTrend => StatsTab::ErrorRanking,
                                    StatsTab::Heatmap => StatsTab::WpmTrend,
                                    StatsTab::ErrorRanking => StatsTab::Heatmap,
                                };
                            }
                            KeyCode::Char('r') | KeyCode::Char('R') => {
                                stats_state.wpm_range = stats_state.wpm_range.next();
                            }
                            KeyCode::Char('v')
                            | KeyCode::Char('V')
                            | KeyCode::Char('s')
                            | KeyCode::Char('S') => {
                                if stats_state.tab == StatsTab::WpmTrend {
                                    stats_state.trend_metric = stats_state.trend_metric.next();
                                }
                            }
                            KeyCode::Char('L') => {
                                stats_state.heatmap_layout = stats_state.heatmap_layout.next();
                                app.settings.heatmap_layout = stats_state.heatmap_layout;
                                let _ = app.settings_store.save(&app.settings);
                            }
                            KeyCode::Char('m') | KeyCode::Char('M') => {
                                stats_state.heatmap_source = stats_state.heatmap_source.next();
                            }
                            KeyCode::Char('t') | KeyCode::Char('T') => {
                                stats_state.error_ranking_focus =
                                    stats_state.error_ranking_focus.toggle();
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if stats_state.tab == StatsTab::ErrorRanking {
                                    match stats_state.error_ranking_focus {
                                        ErrorRankingFocus::Chars => {
                                            stats_state.char_selected =
                                                stats_state.char_selected.saturating_sub(1);
                                            if stats_state.char_selected < stats_state.char_scroll {
                                                stats_state.char_scroll = stats_state.char_selected;
                                            }
                                        }
                                        ErrorRankingFocus::Words => {
                                            stats_state.word_selected =
                                                stats_state.word_selected.saturating_sub(1);
                                            if stats_state.word_selected < stats_state.word_scroll {
                                                stats_state.word_scroll = stats_state.word_selected;
                                            }
                                        }
                                    }
                                }
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if stats_state.tab == StatsTab::ErrorRanking {
                                    let db = StatsDb::with_default_path().ok();
                                    match stats_state.error_ranking_focus {
                                        ErrorRankingFocus::Chars => {
                                            let count = db
                                                .as_ref()
                                                .and_then(|d| d.get_top_mistyped_chars(50).ok())
                                                .map(|v| v.len())
                                                .unwrap_or(0);
                                            if count > 0 && stats_state.char_selected + 1 < count {
                                                stats_state.char_selected += 1;
                                                let visible_cap = 15;
                                                if stats_state.char_selected
                                                    >= stats_state.char_scroll + visible_cap
                                                {
                                                    stats_state.char_scroll =
                                                        stats_state.char_selected + 1 - visible_cap;
                                                }
                                            }
                                        }
                                        ErrorRankingFocus::Words => {
                                            let count = db
                                                .as_ref()
                                                .and_then(|d| d.get_top_mistyped_words(50).ok())
                                                .map(|v| v.len())
                                                .unwrap_or(0);
                                            if count > 0 && stats_state.word_selected + 1 < count {
                                                stats_state.word_selected += 1;
                                                let visible_cap = 15;
                                                if stats_state.word_selected
                                                    >= stats_state.word_scroll + visible_cap
                                                {
                                                    stats_state.word_scroll =
                                                        stats_state.word_selected + 1 - visible_cap;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            KeyCode::PageUp => {
                                if stats_state.tab == StatsTab::ErrorRanking {
                                    match stats_state.error_ranking_focus {
                                        ErrorRankingFocus::Chars => {
                                            stats_state.char_selected =
                                                stats_state.char_selected.saturating_sub(10);
                                            stats_state.char_scroll =
                                                stats_state.char_scroll.saturating_sub(10);
                                            if stats_state.char_selected < stats_state.char_scroll {
                                                stats_state.char_scroll = stats_state.char_selected;
                                            }
                                        }
                                        ErrorRankingFocus::Words => {
                                            stats_state.word_selected =
                                                stats_state.word_selected.saturating_sub(10);
                                            stats_state.word_scroll =
                                                stats_state.word_scroll.saturating_sub(10);
                                            if stats_state.word_selected < stats_state.word_scroll {
                                                stats_state.word_scroll = stats_state.word_selected;
                                            }
                                        }
                                    }
                                }
                            }
                            KeyCode::PageDown => {
                                if stats_state.tab == StatsTab::ErrorRanking {
                                    let db = StatsDb::with_default_path().ok();
                                    match stats_state.error_ranking_focus {
                                        ErrorRankingFocus::Chars => {
                                            let count = db
                                                .as_ref()
                                                .and_then(|d| d.get_top_mistyped_chars(50).ok())
                                                .map(|v| v.len())
                                                .unwrap_or(0);
                                            if count > 0 {
                                                stats_state.char_selected =
                                                    (stats_state.char_selected + 10).min(count - 1);
                                                stats_state.char_scroll =
                                                    stats_state.char_scroll.saturating_add(10);
                                            }
                                        }
                                        ErrorRankingFocus::Words => {
                                            let count = db
                                                .as_ref()
                                                .and_then(|d| d.get_top_mistyped_words(50).ok())
                                                .map(|v| v.len())
                                                .unwrap_or(0);
                                            if count > 0 {
                                                stats_state.word_selected =
                                                    (stats_state.word_selected + 10).min(count - 1);
                                                stats_state.word_scroll =
                                                    stats_state.word_scroll.saturating_add(10);
                                            }
                                        }
                                    }
                                }
                            }
                            KeyCode::Char('d')
                            | KeyCode::Char('D')
                            | KeyCode::Char('x')
                            | KeyCode::Char('X')
                            | KeyCode::Delete => {
                                if stats_state.tab == StatsTab::ErrorRanking {
                                    if let Ok(mut db) = StatsDb::with_default_path() {
                                        match stats_state.error_ranking_focus {
                                            ErrorRankingFocus::Chars => {
                                                if let Some(stat) = db
                                                    .get_top_mistyped_chars(50)
                                                    .ok()
                                                    .and_then(|list| {
                                                        list.get(stats_state.char_selected).cloned()
                                                    })
                                                {
                                                    let target = stat.target_char;
                                                    if let Ok(num) = db.delete_mistyped_char(target)
                                                    {
                                                        stats_state.status_msg = Some(format!(
                                                            "已删除错字 '{}'（共清除 {} 条记录）",
                                                            target, num
                                                        ));
                                                        let new_len = db
                                                            .get_top_mistyped_chars(50)
                                                            .unwrap_or_default()
                                                            .len();
                                                        if new_len > 0 {
                                                            stats_state.char_selected = stats_state
                                                                .char_selected
                                                                .min(new_len - 1);
                                                        } else {
                                                            stats_state.char_selected = 0;
                                                            stats_state.char_scroll = 0;
                                                        }
                                                    }
                                                }
                                            }
                                            ErrorRankingFocus::Words => {
                                                if let Some(stat) = db
                                                    .get_top_mistyped_words(50)
                                                    .ok()
                                                    .and_then(|list| {
                                                        list.get(stats_state.word_selected).cloned()
                                                    })
                                                {
                                                    let target = stat.target_word;
                                                    if let Ok(num) =
                                                        db.delete_mistyped_word(&target)
                                                    {
                                                        stats_state.status_msg = Some(format!(
                                                            "已删除错词 \"{}\"（共清除 {} 条记录）",
                                                            target, num
                                                        ));
                                                        let new_len = db
                                                            .get_top_mistyped_words(50)
                                                            .unwrap_or_default()
                                                            .len();
                                                        if new_len > 0 {
                                                            stats_state.word_selected = stats_state
                                                                .word_selected
                                                                .min(new_len - 1);
                                                        } else {
                                                            stats_state.word_selected = 0;
                                                            stats_state.word_scroll = 0;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    } else {
                                        stats_state.status_msg =
                                            Some("打开统计数据库失败".to_string());
                                    }
                                }
                            }
                            KeyCode::Esc | KeyCode::Char('q') => app.state = AppState::Typing,
                            _ => {}
                        }
                    }
                    AppState::Sponsor => match key.code {
                        KeyCode::Esc
                        | KeyCode::Char('q')
                        | KeyCode::Char('Q')
                        | KeyCode::Char('d')
                        | KeyCode::Char('D') => app.state = AppState::Typing,
                        _ => {}
                    },
                    AppState::OnlineRank(_) => match key.code {
                        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                            app.state = AppState::Typing;
                        }
                        KeyCode::Char('1') => app.switch_rank_tab(CompetitionType::Jisu),
                        KeyCode::Char('2') => app.switch_rank_tab(CompetitionType::Jinbiao),
                        KeyCode::Char('3') => app.switch_rank_tab(CompetitionType::Jianshen),
                        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                            let next = if let AppState::OnlineRank(s) = &app.state {
                                s.active_tab.next()
                            } else {
                                CompetitionType::Jisu
                            };
                            app.switch_rank_tab(next);
                        }
                        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                            let prev = if let AppState::OnlineRank(s) = &app.state {
                                s.active_tab.prev()
                            } else {
                                CompetitionType::Jisu
                            };
                            app.switch_rank_tab(prev);
                        }
                        KeyCode::Char('r') | KeyCode::Char('R') => app.refresh_rank(),
                        KeyCode::Up | KeyCode::Char('k') => app.rank_scroll(-1),
                        KeyCode::Down | KeyCode::Char('j') => app.rank_scroll(1),
                        KeyCode::Char('c') | KeyCode::Char('C') => app.open_rank_column_modal(),
                        _ => {}
                    },
                    AppState::Countdown { source, .. } => {
                        // 倒计时期间拦截所有输入，仅 Esc/q/Q 可取消返回进入前的浏览界面。
                        if key.code == KeyCode::Esc
                            || key.code == KeyCode::Char('q')
                            || key.code == KeyCode::Char('Q')
                        {
                            app.cancel_countdown(source);
                        }
                        continue;
                    }
                }
            }
            Event::Paste(committed) => {
                if let Some(modal) = app.free_input_modal.as_mut() {
                    match modal.focus {
                        FREE_INPUT_FOCUS_TITLE => {
                            modal.title.push_str(&committed);
                            modal.update_default_save_path();
                        }
                        FREE_INPUT_FOCUS_CONTENT => {
                            modal.content.push_str(&committed);
                        }
                        FREE_INPUT_FOCUS_SAVE_PATH => {
                            modal.save_path.push_str(&committed);
                        }
                        _ => {}
                    }
                    continue;
                }
                if let Some(modal) = app.text_setting_modal.as_mut() {
                    for c in committed.chars() {
                        modal.push_char(c);
                    }
                    continue;
                }
                if matches!(app.state, AppState::Finished { .. }) {
                    for c in committed.chars() {
                        let norm_c = normalize_char(c);
                        let key = KeyEvent::new(KeyCode::Char(norm_c), KeyModifiers::NONE);
                        if handle_finished_key(&mut app, key) {
                            break;
                        }
                    }
                    continue;
                }
                if matches!(app.state, AppState::Typing) {
                    app.touch_typing();
                    let elapsed = app.current_elapsed();
                    let now = Instant::now();
                    handle_text(
                        &mut app.session,
                        &mut app.live_keyboard,
                        app.scheme_dict.as_ref(),
                        &committed,
                        elapsed,
                        now,
                    );
                    if app.session.is_complete() {
                        finish_and_maybe_upload(&mut app, terminal)?;
                    }
                }
            }
            _ => {}
        }
    }
}

/// 触发在线比赛赛文下载（未登录时引导登录）。
fn trigger_online_competition<B: ratatui::backend::Backend>(
    app: &mut App,
    competition_type: CompetitionType,
    terminal: &mut ratatui::Terminal<B>,
) -> io::Result<()> {
    if !app.logged_in {
        app.online_error = Some("请先登录 52dazi 后再载入在线赛文".to_string());
        app.open_login();
    } else {
        app.online_loading = Some(competition_type);
        terminal
            .draw(|frame| ui(frame, app))
            .map_err(|e| io::Error::other(e.to_string()))?;
        app.download_online(competition_type);
    }
    Ok(())
}

/// 激活功能栏选中的菜单项。
fn activate_sidebar_menu_item<B: ratatui::backend::Backend>(
    app: &mut App,
    terminal: &mut ratatui::Terminal<B>,
) -> io::Result<()> {
    let item = SIDEBAR_MENU_ITEMS
        .get(app.sidebar_selected)
        .copied()
        .unwrap_or(SidebarMenuItem::LoadFile);
    match item {
        SidebarMenuItem::LoadFile => app.open_browser(),
        SidebarMenuItem::BuiltinText => app.open_builtin_browser(),
        SidebarMenuItem::FreeInput => app.open_free_input(),
        SidebarMenuItem::Clipboard => app.load_from_clipboard(),
        SidebarMenuItem::OnlineJisu => {
            trigger_online_competition(app, CompetitionType::Jisu, terminal)?;
        }
        SidebarMenuItem::OnlineJinbiao => {
            trigger_online_competition(app, CompetitionType::Jinbiao, terminal)?;
        }
        SidebarMenuItem::OnlineJianshen => {
            trigger_online_competition(app, CompetitionType::Jianshen, terminal)?;
        }
        SidebarMenuItem::OnlineRank => {
            app.open_online_rank()?;
        }
        SidebarMenuItem::Stats => {
            app.state = AppState::Stats(StatsViewState::new(app.settings.heatmap_layout));
        }
        SidebarMenuItem::Settings => app.enter_settings(),
        SidebarMenuItem::Sponsor => app.open_sponsor(),
        SidebarMenuItem::Login => app.open_login(),
    }
    Ok(())
}

/// 完成跟打：进入成绩视图；在线赛文先渲染「上传中」再同步上传成绩。
fn finish_and_maybe_upload<B: ratatui::backend::Backend>(
    app: &mut App,
    terminal: &mut ratatui::Terminal<B>,
) -> io::Result<()> {
    let result = app.finish_typing();
    if let Some((stats, elapsed)) = result {
        // 先渲染「上传中」，再同步上传（阻塞）。
        terminal
            .draw(|frame| ui(frame, app))
            .map_err(|e| io::Error::other(e.to_string()))?;
        app.do_upload(&stats, elapsed);
    }
    Ok(())
}

/// 收起/展开功能栏：Tab。
#[allow(dead_code)]
fn is_toggle_sidebar(key: KeyEvent) -> bool {
    key.code == KeyCode::Tab
}

/// 提前结束快捷键：d / D（Done）。
fn is_early_finish(key: KeyEvent) -> bool {
    key.modifiers.is_empty() && (key.code == KeyCode::Char('d') || key.code == KeyCode::Char('D'))
}

/// 重打快捷键：r / R（Restart）。
fn is_restart(key: KeyEvent) -> bool {
    key.modifiers.is_empty() && (key.code == KeyCode::Char('r') || key.code == KeyCode::Char('R'))
}

/// 是否允许重打：离线赛文按 r 重打；在线赛文禁用重打。
fn restart_allowed(key: KeyEvent, is_online: bool) -> bool {
    is_restart(key) && !is_online
}

/// 底部快捷键提示栏文案：按浏览状态与赛文来源动态切换。
fn hint_text(
    browsing: bool,
    browsing_builtin: bool,
    is_online: bool,
    paused: bool,
    is_ready: bool,
) -> &'static str {
    if browsing {
        " jk 选择 | Enter 载入 | g/G 首尾 | Esc/q 取消 | o 设置 | Ctrl-Q 退出"
    } else if browsing_builtin {
        " jk 选择 | Enter 载入 | s 乱序 | g/G 首尾 | Esc/q 取消 | o 设置 | Ctrl-Q 退出"
    } else if paused {
        " jk 菜单导航 | l 执行 | i/Esc 恢复跟打 | d 提前结算 | r 重打 | s 统计 | o 设置 | Ctrl-Q 退出"
    } else if is_ready {
        " jk 菜单导航 | l 执行 | f 载文 | b 内置 | i 自由发文 | p 剪贴板 | 1 极速杯 | 4 排行榜 | s 统计 | o 设置 | Ctrl-Q 退出"
    } else if is_online {
        " Esc 暂停/命令 | Tab 侧栏 | s 统计 | o 设置 | u 登录 | Ctrl-Q 退出"
    } else {
        " Esc 暂停/命令 | Tab 侧栏 | r 重打 | s 统计 | o 设置 | Ctrl-Q 退出"
    }
}

/// 将形如 `" ↑↓ 选择 | Enter 载入 | Esc 取消 "` 的快捷键文案解析为圆角键帽胶囊（Rounded Badge Pill）排版。
/// 使用 Unicode 左右半圆几何图形 `◖` 与 `◗` 作为按键胶囊两端的圆角外边框。
fn hint_bar_line(hint_str: &str, palette: &ThemePalette) -> Line<'static> {
    let cap_left_style = Style::default().fg(palette.selection).bg(palette.bg);
    let key_style = Style::default()
        .bg(palette.selection)
        .fg(palette.accent)
        .add_modifier(Modifier::BOLD);
    let cap_right_style = Style::default().fg(palette.selection).bg(palette.bg);
    let desc_style = Style::default().fg(palette.fg).bg(palette.bg);

    let mut spans = Vec::new();
    let items = hint_str.split('|');

    for (i, item) in items.enumerate() {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if i > 0 || !spans.is_empty() {
            spans.push(Span::styled(" ", Style::default().bg(palette.bg)));
        }
        if let Some((k, d)) = item.split_once(' ') {
            spans.push(Span::styled("◖", cap_left_style));
            spans.push(Span::styled(k.to_string(), key_style));
            spans.push(Span::styled("◗", cap_right_style));
            spans.push(Span::styled(format!(" {d} "), desc_style));
        } else {
            spans.push(Span::styled("◖", cap_left_style));
            spans.push(Span::styled(item.to_string(), key_style));
            spans.push(Span::styled("◗", cap_right_style));
        }
    }
    Line::from(spans)
}

/// 处理自由发文模态框按键，返回动作。
fn free_input_modal_input(modal: &mut FreeInputModal, key: KeyEvent) -> FreeInputAction {
    if key.code == KeyCode::Esc {
        return FreeInputAction::Cancel;
    }

    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let is_alt = key.modifiers.contains(KeyModifiers::ALT);

    let is_submit_shortcut = match key.code {
        KeyCode::Enter if is_ctrl || is_alt => true,
        KeyCode::Char('\n') | KeyCode::Char('\r') if is_ctrl || is_alt => true,
        KeyCode::Char('s') | KeyCode::Char('S') if is_ctrl => true,
        KeyCode::Char('d') | KeyCode::Char('D') if is_ctrl => true,
        KeyCode::Char('j') | KeyCode::Char('J') if is_ctrl => true,
        KeyCode::Char('m') | KeyCode::Char('M') if is_ctrl => true,
        _ => false,
    };

    if is_submit_shortcut {
        return try_submit_free_input(modal);
    }

    if key.code == KeyCode::Tab {
        modal.next_focus();
        return FreeInputAction::None;
    }
    if key.code == KeyCode::BackTab {
        modal.prev_focus();
        return FreeInputAction::None;
    }

    match modal.focus {
        FREE_INPUT_FOCUS_TITLE => match key.code {
            KeyCode::Char(c) if !is_ctrl && !is_alt => {
                modal.title.push(c);
                modal.update_default_save_path();
            }
            KeyCode::Backspace => {
                modal.title.pop();
                modal.update_default_save_path();
            }
            KeyCode::Down | KeyCode::Enter => {
                modal.focus = FREE_INPUT_FOCUS_CONTENT;
            }
            _ => {}
        },
        FREE_INPUT_FOCUS_CONTENT => match key.code {
            KeyCode::Char(c) if !is_ctrl && !is_alt => {
                modal.content.push(c);
                modal.error = None;
            }
            KeyCode::Backspace => {
                modal.content.pop();
            }
            KeyCode::Enter => {
                modal.content.push('\n');
            }
            KeyCode::Up if modal.content.is_empty() => {
                modal.focus = FREE_INPUT_FOCUS_TITLE;
            }
            _ => {}
        },
        FREE_INPUT_FOCUS_SAVE_CHECKBOX => match key.code {
            KeyCode::Char(' ') | KeyCode::Enter => {
                modal.save_to_file = !modal.save_to_file;
                modal.update_default_save_path();
            }
            KeyCode::Up => modal.focus = FREE_INPUT_FOCUS_CONTENT,
            KeyCode::Down => {
                if modal.save_to_file {
                    modal.focus = FREE_INPUT_FOCUS_SAVE_PATH;
                } else {
                    modal.focus = FREE_INPUT_FOCUS_SUBMIT_BTN;
                }
            }
            _ => {}
        },
        FREE_INPUT_FOCUS_SAVE_PATH => match key.code {
            KeyCode::Char(c) if !is_ctrl && !is_alt => {
                modal.save_path.push(c);
                modal.error = None;
            }
            KeyCode::Backspace => {
                modal.save_path.pop();
            }
            KeyCode::Up => modal.focus = FREE_INPUT_FOCUS_SAVE_CHECKBOX,
            KeyCode::Down | KeyCode::Enter => modal.focus = FREE_INPUT_FOCUS_SUBMIT_BTN,
            _ => {}
        },
        FREE_INPUT_FOCUS_SUBMIT_BTN => match key.code {
            KeyCode::Enter | KeyCode::Char(' ') => {
                return try_submit_free_input(modal);
            }
            KeyCode::Left | KeyCode::Up => {
                if modal.save_to_file {
                    modal.focus = FREE_INPUT_FOCUS_SAVE_PATH;
                } else {
                    modal.focus = FREE_INPUT_FOCUS_SAVE_CHECKBOX;
                }
            }
            KeyCode::Right | KeyCode::Down => {
                modal.focus = FREE_INPUT_FOCUS_CANCEL_BTN;
            }
            _ => {}
        },
        FREE_INPUT_FOCUS_CANCEL_BTN => match key.code {
            KeyCode::Enter | KeyCode::Char(' ') => {
                return FreeInputAction::Cancel;
            }
            KeyCode::Left | KeyCode::Up => {
                modal.focus = FREE_INPUT_FOCUS_SUBMIT_BTN;
            }
            KeyCode::Right | KeyCode::Down => {
                modal.focus = FREE_INPUT_FOCUS_TITLE;
            }
            _ => {}
        },
        _ => {}
    }

    FreeInputAction::None
}

/// 尝试提交自由发文。
fn try_submit_free_input(modal: &mut FreeInputModal) -> FreeInputAction {
    if modal.content.trim().is_empty() {
        modal.error = Some("赛文正文不能为空".to_string());
        return FreeInputAction::None;
    }
    let save_path = if modal.save_to_file {
        if modal.save_path.trim().is_empty() {
            modal.error = Some("保存路径不能为空".to_string());
            return FreeInputAction::None;
        }
        Some(PathBuf::from(modal.save_path.trim()))
    } else {
        None
    };
    FreeInputAction::Submit {
        title: modal.title.clone(),
        content: modal.content.clone(),
        save: save_path,
    }
}

/// 进入载文浏览快捷键：f / F（File）。
fn is_open_browser(key: KeyEvent) -> bool {
    key.modifiers.is_empty() && (key.code == KeyCode::Char('f') || key.code == KeyCode::Char('F'))
}

/// 进入内置赛文浏览快捷键：b / B（Builtin）。
fn is_open_builtin_browser(key: KeyEvent) -> bool {
    key.modifiers.is_empty() && (key.code == KeyCode::Char('b') || key.code == KeyCode::Char('B'))
}

/// 打开自由发文模态框快捷键：i / I（Insert / Input）。
fn is_open_free_input(key: KeyEvent) -> bool {
    key.modifiers.is_empty() && (key.code == KeyCode::Char('i') || key.code == KeyCode::Char('I'))
}

/// 剪贴板发文快捷键：p / P（Paste / Put）。
fn is_load_clipboard(key: KeyEvent) -> bool {
    key.modifiers.is_empty() && (key.code == KeyCode::Char('p') || key.code == KeyCode::Char('P'))
}

/// 打开统计视图快捷键：s / S（Stats）。
fn is_open_stats(key: KeyEvent) -> bool {
    key.modifiers.is_empty() && (key.code == KeyCode::Char('s') || key.code == KeyCode::Char('S'))
}

/// 打开赞赏&支持视图快捷键：d / D（Donate / 赞赏）。
fn is_open_sponsor(key: KeyEvent) -> bool {
    key.modifiers.is_empty() && (key.code == KeyCode::Char('d') || key.code == KeyCode::Char('D'))
}

/// 处理跟打文本上屏（支持单字符与多字词组，如输入法整词上屏 "怎么"）：
/// 记录按键频率与时序事件，并触发实时虚拟键盘（物理击键/汉字方案反查）高亮。
fn handle_text(
    session: &mut Session,
    live_kb: &mut LiveKeyboard,
    scheme_dict: Option<&SchemeDict>,
    text: &str,
    elapsed: Duration,
    now: Instant,
) {
    if text.is_empty() {
        return;
    }
    if let Some(dict) = scheme_dict {
        let (strokes, keys) = dict.resolve_strokes_and_keys(text);
        for c in text.chars() {
            session.record_key(&c.to_string());
        }
        session.type_text_with_strokes_at(text, strokes, elapsed);
        for k in &keys {
            live_kb.press_key(k, now);
        }
    } else {
        for c in text.chars() {
            session.record_key(&c.to_string());
        }
        let strokes = text.chars().count() as u32;
        session.type_text_with_strokes_at(text, strokes, elapsed);
        for c in text.chars() {
            if c == ' ' {
                live_kb.press_key("Space", now);
            } else if c.is_ascii() {
                live_kb.press_char(c, now);
            }
        }
    }
}

/// 处理跟打键：退格回改，可打印字符上屏；同时记录按键频率与时序事件，并触发实时虚拟键盘（物理击键/汉字方案反查）高亮。
fn handle_key(
    session: &mut Session,
    live_kb: &mut LiveKeyboard,
    scheme_dict: Option<&SchemeDict>,
    key: KeyEvent,
    elapsed: Duration,
    now: Instant,
) {
    match key.code {
        KeyCode::Backspace => {
            session.record_key("Backspace");
            session.backspace_at(elapsed);
            live_kb.press_key("Backspace", now);
        }
        KeyCode::Char(c) => {
            handle_text(session, live_kb, scheme_dict, &c.to_string(), elapsed, now);
        }
        _ => {}
    }
}

/// 处理成绩视图下的按键事件：
/// - f / F: 打开载文浏览
/// - b / B: 打开内置赛文浏览
/// - i / I: 打开自由发文
/// - p / P: 剪贴板发文
/// - s / S: 打开数据统计
/// - o / O: 打开设置视图
/// - u / U: 打开登录模态框
/// - 1/2/3: 在线比赛
/// - ↑/↓ / j/k: 在错字时间线中移动选中项；PgUp/PgDn 翻页；Home/End(g/G) 跳到首/末条
/// - Esc / q / Q: 返回主界面（重置会话为就绪状态；在线赛文重置回内置赛文）
/// - Enter / r / R: 重新开始跟打当前赛文（仅限离线/内置赛文）
fn handle_finished_key(app: &mut App, key: KeyEvent) -> bool {
    if is_open_browser(key) {
        app.open_browser();
        return true;
    }
    if is_open_builtin_browser(key) {
        app.open_builtin_browser();
        return true;
    }
    if is_open_free_input(key) {
        app.open_free_input();
        return true;
    }
    if is_load_clipboard(key) {
        app.load_from_clipboard();
        return true;
    }
    if is_open_stats(key) {
        app.state = AppState::Stats(StatsViewState::new(app.settings.heatmap_layout));
        return true;
    }
    if is_open_settings(key) {
        app.enter_settings();
        return true;
    }
    if is_open_login(key) {
        app.open_login();
        return true;
    }
    if is_open_rank(key) {
        let _ = app.open_online_rank();
        return true;
    }
    if let Some(_ct) = online_shortcut(key) {
        app.text = load_builtin_text(BUILTIN_SETS[0]);
        app.restart();
        return true;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.move_error_point(-1);
            true
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.move_error_point(1);
            true
        }
        KeyCode::PageUp => {
            app.move_error_point(-(ERROR_TIMELINE_VISIBLE as isize));
            true
        }
        KeyCode::PageDown => {
            app.move_error_point(ERROR_TIMELINE_VISIBLE as isize);
            true
        }
        KeyCode::Home | KeyCode::Char('g') => {
            app.select_error_point(0);
            true
        }
        KeyCode::End | KeyCode::Char('G') => {
            app.select_error_point(usize::MAX);
            true
        }
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
            if app.text.is_online() {
                app.text = load_builtin_text(BUILTIN_SETS[0]);
            }
            app.restart();
            true
        }
        KeyCode::Enter | KeyCode::Char('r') | KeyCode::Char('R') => {
            if !app.text.is_online() {
                app.restart();
            }
            true
        }
        _ => false,
    }
}

/// 将全角 ASCII 字符（U+FF01~U+FF5E）转换为标准半角 ASCII 字符，防止在输入法处于全角模式时快捷键失效。
fn normalize_char(c: char) -> char {
    let u = c as u32;
    if (0xFF01..=0xFF5E).contains(&u) {
        char::from_u32(u - 0xFEE0).unwrap_or(c)
    } else {
        c
    }
}

/// 规范化按键事件中的字符代码。
fn normalize_key(key: &mut KeyEvent) {
    if let KeyCode::Char(c) = key.code {
        key.code = KeyCode::Char(normalize_char(c));
    }
}

/// 退出快捷键：Ctrl-Q / Ctrl-C（防止单按 q 误触退出）。
fn is_quit(key: KeyEvent) -> bool {
    let is_ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
    let is_ctrl_q = key.modifiers.contains(KeyModifiers::CONTROL)
        && (key.code == KeyCode::Char('q') || key.code == KeyCode::Char('Q'));
    is_ctrl_q || is_ctrl_c
}

/// 打开登录模态框快捷键：u / U（User login）。
fn is_open_login(key: KeyEvent) -> bool {
    key.modifiers.is_empty() && (key.code == KeyCode::Char('u') || key.code == KeyCode::Char('U'))
}

/// 打开设置视图快捷键：o / O（Options）。
fn is_open_settings(key: KeyEvent) -> bool {
    key.modifiers.is_empty() && (key.code == KeyCode::Char('o') || key.code == KeyCode::Char('O'))
}

/// 打开在线排行榜快捷键：4（与 1/2/3 比赛入口并列）。
fn is_open_rank(key: KeyEvent) -> bool {
    key.modifiers.is_empty() && key.code == KeyCode::Char('4')
}

/// 三个比赛入口快捷键：1=极速杯、2=锦标赛、3=键神杯。
fn online_shortcut(key: KeyEvent) -> Option<CompetitionType> {
    if !key.modifiers.is_empty() {
        return None;
    }
    match key.code {
        KeyCode::Char('1') => Some(CompetitionType::Jisu),
        KeyCode::Char('2') => Some(CompetitionType::Jinbiao),
        KeyCode::Char('3') => Some(CompetitionType::Jianshen),
        _ => None,
    }
}

/// 把 API 错误转为友好文案。
fn api_error_text(err: &ApiError) -> String {
    match err {
        ApiError::Transport(_) => "网络连接失败".to_string(),
        ApiError::Server(msg) => msg.clone(),
        ApiError::Parse(_) => "服务器响应异常".to_string(),
    }
}

/// 通过 OSC 52 转义序列把文本写入系统剪贴板（终端转发，失败静默忽略）。
/// 测试构建直接跳过：避免单测向真实终端输出转义序列并覆盖开发者剪贴板。
fn write_clipboard(text: &str) {
    if cfg!(test) {
        return;
    }
    use crossterm::style::Print;
    let seq = osc52_clipboard(text);
    let _ = crossterm::execute!(std::io::stdout(), Print(seq));
}

/// 完成后自动复制统计结果到剪贴板的赛文来源：全部赛文来源。
fn copies_stats_to_clipboard(source: TextSource) -> bool {
    matches!(
        source,
        TextSource::Custom
            | TextSource::File
            | TextSource::Builtin { .. }
            | TextSource::Online { .. }
            | TextSource::Clipboard
    )
}

/// 焦点在设置项间循环移动（向上为负、向下为正）。
fn move_focus(current: usize, delta: i8) -> usize {
    ((current as isize + delta as isize).rem_euclid(SETTINGS_FOCUS_COUNT as isize)) as usize
}

/// 调整对照区占比并截断到合法范围（30–80%）。
fn adjust_ratio_value(current: u8, delta: i8) -> u8 {
    let next = (current as i16 + delta as i16).clamp(0, u8::MAX as i16) as u8;
    Settings::clamp_ratio(next)
}

/// 粗体开关派生的样式修饰符。
fn bold_modifier(bold: bool) -> Modifier {
    if bold {
        Modifier::BOLD
    } else {
        Modifier::empty()
    }
}

/// 派生对照区/跟打区占比：返回 `(对照区%, 跟打区%)`。
fn area_ratios(reference_ratio: u8) -> (u16, u16) {
    let ref_pct = Settings::clamp_ratio(reference_ratio) as u16;
    (ref_pct, 100 - ref_pct)
}

/// core 的 `Rgb` 转 ratatui `Color`。
fn color(rgb: Rgb) -> Color {
    Color::Rgb(rgb.0, rgb.1, rgb.2)
}

/// 带主题边框色、背景色与圆角的 Block 构建器。
/// - `is_active = true`：当前活动面板（例如正在打字时的跟打区，或暂停/就绪时的功能栏），边框采用 `palette.accent` 加粗高亮。
/// - `is_active = false`：非活动面板，边框采用 `palette.muted` 柔和暗色，不抢正文视觉。
/// - 面板底色统一填充为 `palette.bg`，正文基色为 `palette.fg`，保证任何终端环境下对比度一致且清晰。
fn themed_block(palette: &ThemePalette, is_active: bool) -> Block<'static> {
    let border_style = if is_active {
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.muted)
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .style(Style::default().bg(palette.bg).fg(palette.fg))
}

/// 处理登录模态框按键，返回动作。
fn login_input(form: &mut LoginForm, key: KeyEvent) -> LoginAction {
    match key.code {
        KeyCode::Esc => LoginAction::Cancel,
        KeyCode::Tab | KeyCode::Down => {
            form.focus = 1 - form.focus;
            LoginAction::None
        }
        KeyCode::BackTab | KeyCode::Up => {
            form.focus = 0;
            LoginAction::None
        }
        KeyCode::Enter => {
            if form.focus == 0 && !form.username.is_empty() && form.password.is_empty() {
                form.focus = 1;
                LoginAction::None
            } else {
                LoginAction::Submit
            }
        }
        KeyCode::Backspace => {
            let field = if form.focus == 0 {
                &mut form.username
            } else {
                &mut form.password
            };
            field.pop();
            LoginAction::None
        }
        KeyCode::Char(c) => {
            let field = if form.focus == 0 {
                &mut form.username
            } else {
                &mut form.password
            };
            field.push(c);
            LoginAction::None
        }
        _ => LoginAction::None,
    }
}

/// 处理自定义设置模态框按键，返回动作。
fn text_setting_modal_input(modal: &mut TextSettingModal, key: KeyEvent) -> TextSettingModalAction {
    match key.code {
        KeyCode::Esc => TextSettingModalAction::Cancel,
        KeyCode::Enter => TextSettingModalAction::Save(modal.target, modal.commit()),
        KeyCode::Backspace => {
            modal.pop_char();
            TextSettingModalAction::None
        }
        KeyCode::Char(c) => {
            modal.push_char(c);
            TextSettingModalAction::None
        }
        _ => TextSettingModalAction::None,
    }
}

/// 密码遮蔽：每个字符显示为 `*`。
fn mask_password(password: &str) -> String {
    "*".repeat(password.chars().count())
}

/// 列出目录下的文本文件（.txt/.md/.wenz），按名字排序。
fn list_text_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_text = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "txt" | "md" | "wenz"));
        if is_text {
            files.push(path);
        }
    }
    files.sort();
    files
}

/// 计算字符序列在指定内宽 `inner_width`（字符列数）下的折行后光标坐标 `(line, col)`。
///
/// 中文/全角字符宽度为 2 列，ASCII/半角字符为 1 列，换行符 `\n` 换行且列清零。
pub fn calculate_text_layout_position(
    chars: impl IntoIterator<Item = char>,
    inner_width: u16,
) -> (u16, u16) {
    if inner_width == 0 {
        return (0, 0);
    }
    let mut line = 0u16;
    let mut col = 0u16;

    for c in chars {
        if c == '\n' {
            line = line.saturating_add(1);
            col = 0;
        } else {
            let w = UnicodeWidthChar::width(c).unwrap_or(1) as u16;
            if w > 0 && col.saturating_add(w) > inner_width {
                line = line.saturating_add(1);
                col = w;
            } else {
                col = col.saturating_add(w);
            }
        }
    }
    (line, col)
}

/// 判断当前跟打页面是否未录入任何字符（处于空态或翻页后的初始就绪态）。
fn is_current_page_empty(session: &Session, text: &Text) -> bool {
    match text.source {
        TextSource::Builtin { set } if set.is_words() => {
            let start_word = builtin_page_start(session);
            let owned;
            let boundaries: &[(usize, usize)] = match &text.word_boundaries {
                Some(b) if !b.is_empty() => b,
                _ => {
                    owned = set.word_boundaries();
                    &owned
                }
            };
            if start_word < boundaries.len() {
                session.len() <= boundaries[start_word].0
            } else {
                session.is_empty()
            }
        }
        TextSource::Builtin { .. } => session.len() <= builtin_page_start(session),
        _ => session.is_empty(),
    }
}

/// 在线排行榜视图：标题 + 三比赛 Tab + 榜单区（加载/错误/空/表格四种态）。
///
/// 当前为 #104 骨架：榜单区展示加载中 / 失败 / 空数据占位；
/// #105 在此渲染四列（排名/用户名/速度/输入法）可滚动表格；
/// #106 增加「我第 N 名 / 共 M 人」名次条并高亮当前用户行。
fn render_online_rank_view(frame: &mut Frame, app: &App, rank_state: &OnlineRankState) {
    let palette = app.palette();
    let total_area = frame.area();

    // 全屏底色
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.bg).fg(palette.fg)),
        total_area,
    );

    let outer = themed_block(&palette, true).title(Line::from(vec![Span::styled(
        " 在线排行榜 ",
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )]));
    let inner = outer.inner(total_area);
    frame.render_widget(outer, total_area);

    // 主内容区 + 底部快捷键栏（占 3 行），与其他全屏视图（统计等）保持一致。
    let [content_area, hint_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(inner);

    // 三 Tab 行：极速杯 / 锦标赛 / 键神杯，当前 Tab 高亮（顺序由 `CompetitionType::ALL` 统一）。
    let mut tab_spans: Vec<Span> = Vec::new();
    for (i, ct) in CompetitionType::ALL.iter().enumerate() {
        let active = *ct == rank_state.active_tab;
        if i > 0 {
            tab_spans.push(Span::styled(" │ ", Style::default().fg(palette.selection)));
        }
        let style = if active {
            Style::default().fg(palette.bg).bg(palette.accent).bold()
        } else {
            Style::default().fg(palette.fg)
        };
        tab_spans.push(Span::styled(format!(" {} ", ct.name()), style));
    }

    // 榜单区内容（按当前 Tab 的缓存状态分支：加载 / 错误 / 空 / 表格）
    let board = rank_state.boards.get(&rank_state.active_tab);
    let [tab_area, body_area] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(content_area);
    frame.render_widget(
        Paragraph::new(Line::from(tab_spans))
            .alignment(ratatui::layout::Alignment::Center),
        tab_area,
    );

    match board {
        Some(b) if b.loading => render_rank_note(frame, body_area, "加载中…", palette.muted),
        Some(b) => match &b.error {
            Some(err) => {
                let line = Line::from(vec![
                    Span::styled(format!("加载失败：{err}"), Style::default().fg(palette.error)),
                    Span::styled("（按 R 重试）", Style::default().fg(palette.warning)),
                ]);
                frame.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), body_area);
            }
            None => match &b.data {
                Some(data) if data.rank_result.is_empty() => {
                    render_rank_note(frame, body_area, "暂无数据（今日该比赛尚未产生成绩）", palette.muted);
                }
                Some(data) => {
                    // 名次条：登录态下展示「我第 N 名 / 共 M 人」；未登录降级为公开榜提示。
                    let my_rank = data.my_rank_result.first().map(|r| r.rank);
                    let rankbar = match my_rank {
                        Some(r) => Line::from(Span::styled(
                            format!("我第 {} 名 / 共 {} 人", r, data.total),
                            Style::default().fg(palette.accent).bold(),
                        )),
                        None if !app.logged_in => Line::from(Span::styled(
                            "未登录：登录后可见个人名次（当前为公开榜）",
                            Style::default().fg(palette.warning),
                        )),
                        None => Line::from(Span::styled(
                            "暂无个人名次",
                            Style::default().fg(palette.muted),
                        )),
                    };
                    let [bar_area, table_area] =
                        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(body_area);
                    frame.render_widget(Paragraph::new(rankbar), bar_area);
                    let visible = app.settings.rank_columns.visible_ids();
                    render_rank_table(
                        frame,
                        &palette,
                        data,
                        my_rank,
                        b.scroll,
                        table_area,
                        &visible,
                    );
                }
                None => render_rank_note(frame, body_area, "加载中…", palette.muted),
            },
        },
        None => render_rank_note(frame, body_area, "加载中…", palette.muted),
    }

    // 底部快捷键提示栏（圆角边框 + 结构化标题），与统计视图一致。
    let hint = " 1/2/3 比赛 | Tab/←→ 切换 | ↑↓ 滚动 | c 列定制 | R 刷新 | Esc/q 返回 ";
    let hint_title = Line::from(vec![Span::styled(
        " 快捷键 ",
        Style::default().bold().fg(palette.accent),
    )]);
    frame.render_widget(
        Paragraph::new(hint_bar_line(hint, &palette))
            .block(themed_block(&palette, false).title(hint_title)),
        hint_area,
    );
}

/// 在线排行榜「自定义列」弹窗：居中列出四列，带勾选与选中高亮。
fn render_rank_column_modal(frame: &mut Frame, app: &App, palette: &ThemePalette) {
    let Some(modal) = app.rank_column_modal.as_ref() else {
        return;
    };
    let area = centered_rect(frame.area(), 40, 11);
    frame.render_widget(Clear, area);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(" 自定义展示列 ").bold().fg(palette.fg));
    lines.push(Line::from(""));
    for (i, id) in RankColumnId::ALL.iter().enumerate() {
        let checked = if app.settings.rank_columns.is_visible(*id) {
            "[x]"
        } else {
            "[ ]"
        };
        let mark = if i == modal.selected { "▸" } else { " " };
        let style = if i == modal.selected {
            Style::default().fg(palette.accent).bold()
        } else {
            Style::default().fg(palette.fg)
        };
        lines.push(Line::from(format!(" {mark} {checked} {}", id.title())).style(style));
    }
    lines.push(Line::from(""));
    lines.push(hint_bar_line(
        " ↑↓ 选择 | Space 切换 | Esc 完成 ",
        palette,
    ));
    let block = themed_block(palette, true)
        .title(" 自定义列 ")
        .style(Style::default().bg(palette.bg).fg(palette.fg));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// 榜单区居中提示（加载中 / 空数据等）。
fn render_rank_note(frame: &mut Frame, area: Rect, note: &str, color: Color) {
    frame.render_widget(
        Paragraph::new(Span::styled(note, Style::default().fg(color)))
            .alignment(ratatui::layout::Alignment::Center),
        area,
    );
}

/// 取某行在指定列上的展示文本。
fn rank_cell_value(id: RankColumnId, row: &CompetitionRankRow) -> String {
    match id {
        RankColumnId::Rank => row.rank.to_string(),
        RankColumnId::Username => row.username.clone(),
        RankColumnId::Speed => format!("{:.2}", row.speed),
        RankColumnId::InputMethod => row.input_method.clone(),
    }
}

/// 按可见列与可用宽度计算各列宽度：宽终端放大填满，窄终端等比缩小（每列下限 4）。
fn compute_rank_column_widths(visible: &[RankColumnId], avail: usize) -> Vec<usize> {
    let mins: Vec<usize> = visible.iter().map(|id| id.min_width()).collect();
    let total_min: usize = mins.iter().sum();
    if total_min == 0 {
        return mins;
    }
    if avail <= total_min {
        return mins.iter().map(|m| (*m).max(4)).collect();
    }
    // 放大到填满 avail：按比例分配余量，最少保留各列 min；舍入误差修正到最后一列。
    let slack = avail - total_min;
    let mut widths: Vec<usize> = mins.clone();
    let mut distributed: usize = 0;
    for i in 0..mins.len() {
        let add = ((mins[i] as f64 / total_min as f64) * slack as f64).round() as usize;
        widths[i] += add;
        distributed += add;
    }
    let diff = slack as isize - distributed as isize;
    let last = widths.len() - 1;
    widths[last] = (widths[last] as isize + diff).max(mins[last] as isize) as usize;
    widths
}

/// 渲染榜单：仅展示 `visible` 指定的列（顺序即 `RankColumnId::ALL` 中可见者），
/// 按 `scroll` 偏移可滚动；当前用户行高亮。列宽按可见列动态分配（#105/#108 列定制）。
fn render_rank_table(
    frame: &mut Frame,
    palette: &ThemePalette,
    data: &CompetitionRank,
    my_rank: Option<u32>,
    scroll: u16,
    area: Rect,
    visible: &[RankColumnId],
) {
    let accent = Style::default().fg(palette.accent).bold();
    let fg = Style::default().fg(palette.fg);
    // 相邻列之间插入固定间隔，避免右对齐列的值与下一列起点贴死
    // （如「排名↔用户名」「速度↔输入法」）。间隔计入可用宽度，防止溢出。
    let n = visible.len();
    let gap = 2usize;
    let gap_total = gap * n.saturating_sub(1);
    let content_avail = area.width.saturating_sub(gap_total as u16) as usize;
    let widths = compute_rank_column_widths(visible, content_avail);
    let mut header_spans: Vec<Span> = Vec::new();
    for (i, &id) in visible.iter().enumerate() {
        header_spans.push(Span::styled(
            pad_display(id.title(), widths[i], id.align_right()),
            accent,
        ));
        if i + 1 < n {
            header_spans.push(Span::styled(" ".repeat(gap), accent));
        }
    }
    let header = Line::from(header_spans);
    let available = area.height.saturating_sub(1) as usize;
    let rows = &data.rank_result;
    let max_scroll = rows.len().saturating_sub(available);
    let start = (scroll as usize).min(max_scroll);
    let mut lines = vec![header];
    for row in rows.iter().skip(start).take(available) {
        // 当前用户所在行高亮（与名次条呼应）。
        let row_style = if Some(row.rank) == my_rank {
            Style::default().fg(palette.accent).bold()
        } else {
            fg
        };
        let mut spans: Vec<Span> = Vec::new();
        for (i, &id) in visible.iter().enumerate() {
            let val = rank_cell_value(id, row);
            spans.push(Span::styled(
                pad_display(&val, widths[i], id.align_right()),
                row_style,
            ));
            if i + 1 < n {
                spans.push(Span::styled(" ".repeat(gap), row_style));
            }
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// 按显示宽度（而非字节数）对 `s` 做截断/补齐，兼容中文等宽与英文混排。
fn pad_display(s: &str, width: usize, align_right: bool) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        let mut out = String::new();
        let mut cur = 0;
        for c in s.chars() {
            let cw = UnicodeWidthChar::width(c).unwrap_or(0);
            if cur + cw > width {
                break;
            }
            out.push(c);
            cur += cw;
        }
        out
    } else {
        let pad = " ".repeat(width - w);
        if align_right {
            format!("{pad}{s}")
        } else {
            format!("{s}{pad}")
        }
    }
}

fn ui(frame: &mut Frame, app: &App) {
    if let AppState::Finished {
        stats,
        upload,
        elapsed,
    } = &app.state
    {
        render_result_view(frame, app, stats, upload, *elapsed);
        return;
    }
    if let AppState::Stats(stats_state) = &app.state {
        render_stats_view(frame, app, stats_state);
        return;
    }
    if matches!(app.state, AppState::Sponsor) {
        render_sponsor_view(frame, app);
        return;
    }
    if let AppState::OnlineRank(rank_state) = &app.state {
        render_online_rank_view(frame, app, rank_state);
        // 列定制弹窗为覆盖层，需在返回前渲染（否则被 OnlineRank 提前 return 跳过）。
        if app.rank_column_modal.is_some() {
            let palette = app.palette();
            render_rank_column_modal(frame, app, &palette);
        }
        return;
    }
    let palette = app.palette();
    // 渲染全屏底色，确保终端背景无论亮暗均统一呈现主题色彩与高对比度
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.bg).fg(palette.fg)),
        frame.area(),
    );

    let browsing = matches!(app.state, AppState::Browsing);
    let browsing_builtin = matches!(app.state, AppState::BrowsingBuiltin);
    // 整体：主区 + 底部快捷键 bar（带圆角边框，高度 3 行）
    let [main, help_bar] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(frame.area());
    // 主区：左侧功能栏 + 右侧内容区（功能栏收起时宽度为 0）
    let sidebar_width = if app.sidebar_visible { 24 } else { 0 };
    let [sidebar, content] =
        Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(0)]).areas(main);

    if app.sidebar_visible {
        render_sidebar(frame, app, sidebar, browsing, browsing_builtin);
    }

    if browsing {
        render_preview(frame, app, content);
    } else if browsing_builtin {
        render_builtin_preview(frame, app, content);
    } else {
        // 内容区：上对照区 + (中实时键盘) + 下跟打区（按设置占比分配）
        let (ref_pct, type_pct) = area_ratios(app.settings.reference_ratio);
        let kb_height = match app.settings.keyboard_mode {
            KeyboardMode::Staggered => 5,
            KeyboardMode::Ortholinear => 4,
            KeyboardMode::Off => 0,
        };

        let (ref_area, kb_area_opt, type_area) = if kb_height > 0 && content.height >= kb_height + 6
        {
            let [ref_area, kb_area, type_area] = Layout::vertical([
                Constraint::Percentage(ref_pct),
                Constraint::Length(kb_height),
                Constraint::Percentage(type_pct),
            ])
            .areas(content);
            (ref_area, Some(kb_area), type_area)
        } else {
            let [ref_area, type_area] = Layout::vertical([
                Constraint::Percentage(ref_pct),
                Constraint::Percentage(type_pct),
            ])
            .areas(content);
            (ref_area, None, type_area)
        };

        // 上：对照原文区（已跟打部分绿/红着色，非活动暗边框，复合双色标题，右下角实时复合指标）
        let ref_title = Line::from(vec![
            Span::styled(
                " 对照区 ",
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("— {} ", app.text.title),
                Style::default().fg(palette.fg),
            ),
        ]);
        let mut ref_block = themed_block(&palette, false).title(ref_title);
        if !app.session.is_empty() {
            let elapsed = app.current_elapsed();
            let metrics = app.session.realtime_metrics(elapsed);
            let (rolling_wpm_str, rolling_kps_str) = if app.paused {
                (" (0) ".to_string(), " (0.0) ".to_string())
            } else {
                (
                    format!(" ({:.0}) ", metrics.rolling_wpm),
                    format!(" ({:.1}) ", metrics.rolling_kps),
                )
            };
            let mut spans = vec![
                Span::styled(" WPM ", Style::default().fg(palette.muted)),
                Span::styled(
                    format!("{:.1}", metrics.cumulative_wpm),
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(rolling_wpm_str, Style::default().fg(palette.fg)),
                Span::styled("· ", Style::default().fg(palette.muted)),
                Span::styled("击键 ", Style::default().fg(palette.muted)),
                Span::styled(
                    format!("{:.1}", metrics.cumulative_kps),
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(rolling_kps_str, Style::default().fg(palette.fg)),
            ];
            if app.paused {
                spans.push(Span::styled(
                    "[暂停] ",
                    Style::default().fg(palette.warning),
                ));
            }
            ref_block = ref_block.title_bottom(Line::from(spans).right_aligned());
        }

        let is_builtin = matches!(app.text.source, TextSource::Builtin { .. });
        let dict_ok = code_hint_dict_usable(app.scheme_dict.as_ref());
        // 非内置长文 + 开启提示 + 已配置可用词典：走双行词格（按词边界锁步折行）路径。
        let use_code_hint_grid = app.settings.code_hint && !is_builtin && dict_ok;
        // 内置词组赛文 + 遍码提示：预计算词格列宽，供对照区正文行与跟打区行共用
        // （提示码宽于词时由正文补空格让位，保证两区与提示行三者词列一致）。
        let word_cell_widths = if app.settings.code_hint && dict_ok {
            builtin_words_cell_widths(&app.session, &app.text, app.scheme_dict.as_ref())
        } else {
            None
        };

        let ref_inner_width = ref_area.width.saturating_sub(2);
        let ref_inner_height = ref_area.height.saturating_sub(2);
        let ref_scroll_y = if !is_builtin && ref_inner_height > 0 {
            let (ref_target_line, _) = calculate_text_layout_position(
                app.text.content.chars().take(app.session.len()),
                ref_inner_width,
            );
            // 双行词格下正文行前各有一行提示，故折行后目标行号翻倍以对齐滚动。
            let target = if use_code_hint_grid {
                ref_target_line.saturating_mul(2)
            } else {
                ref_target_line
            };
            target.saturating_sub(ref_inner_height / 2)
        } else {
            0
        };

        let mut ref_text = if use_code_hint_grid {
            // 非内置长文双行词格：提示行与正文行已按词宽锁步预排版（无需 Paragraph 再折行）。
            code_hint_grid_text(
                &app.session,
                &app.text,
                app.scheme_dict.as_ref(),
                app.theme(),
                app.settings.bold,
                ref_inner_width as usize,
            )
            .unwrap_or_else(|| {
                original_line(
                    &app.session,
                    &app.text,
                    app.theme(),
                    app.settings.bold,
                    word_cell_widths.as_deref(),
                )
            })
        } else {
            original_line(
                &app.session,
                &app.text,
                app.theme(),
                app.settings.bold,
                word_cell_widths.as_deref(),
            )
        };
        // 遍码提示（编码提示）：开启时，有可用词典走正常提示路径，否则显示占位引导。
        if app.settings.code_hint {
            if dict_ok {
                // 内置词组赛文：正文行之上插入单行提示（单页，由 Paragraph 按词宽折行）。
                if let Some(hint_line) = code_hint_overlay_line(
                    &app.session,
                    &app.text,
                    app.scheme_dict.as_ref(),
                    app.theme(),
                ) {
                    ref_text.lines.insert(0, hint_line);
                }
            } else {
                // 无可用词典（未配置或仅 .schema.yaml 规则无 .dict.yaml 词条）：
                // 对照区顶部显示占位提示，不空白、不崩溃。
                ref_text
                    .lines
                    .insert(0, code_hint_placeholder_line(app.theme()));
            }
        }
        frame.render_widget(
            Paragraph::new(ref_text)
                .block(ref_block)
                .wrap(Wrap { trim: false })
                .scroll((ref_scroll_y, 0)),
            ref_area,
        );

        // 中：实时按键虚拟键盘（紧凑无边框）
        if let Some(kb_area) = kb_area_opt {
            render_live_keyboard(
                frame,
                &app.live_keyboard,
                app.settings.keyboard_mode,
                kb_area,
                &palette,
                Instant::now(),
            );
        }
        // 下：跟打区（实时绿/红渲染，打字活跃时高亮，复合双色标题与状态徽标）
        let typing_active = !app.paused && !app.session.is_empty();
        let mut typing_title_spans = vec![Span::styled(
            " 跟打区 ",
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        )];
        if app.paused {
            typing_title_spans.push(Span::styled(
                "[已暂停] ",
                Style::default()
                    .fg(palette.warning)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let is_words = matches!(app.text.source, TextSource::Builtin { set } if set.is_words());
        let unit_label = if is_words { "词/组" } else { "字/组" };
        let progress_str = if is_builtin {
            let curr_group = (app.session.completed_groups() + 1).min(app.session.total_groups());
            format!(
                "— 第 {}/{} 组 ({}{}) · {}/{} 字符 ",
                curr_group,
                app.session.total_groups(),
                app.session.group_size(),
                unit_label,
                app.session.len(),
                app.text.content.chars().count()
            )
        } else {
            format!(
                "— {}/{} 字符 ",
                app.session.len(),
                app.text.content.chars().count()
            )
        };
        typing_title_spans.push(Span::styled(progress_str, Style::default().fg(palette.fg)));
        let typing_title = Line::from(typing_title_spans);

        let type_inner_width = type_area.width.saturating_sub(2);
        let type_inner_height = type_area.height.saturating_sub(2);
        let rendered_type_lines = type_line(
            &app.session,
            &app.text,
            app.theme(),
            app.settings.bold,
            word_cell_widths.as_deref(),
        );

        let (type_cursor_line, type_cursor_col) = if is_current_page_empty(&app.session, &app.text)
        {
            (0, 0)
        } else {
            let rendered_chars = rendered_type_lines
                .lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .flat_map(|span| span.content.chars());
            calculate_text_layout_position(rendered_chars, type_inner_width)
        };
        let type_scroll_y = if !is_builtin && type_inner_height > 0 {
            type_cursor_line.saturating_sub(type_inner_height / 2)
        } else {
            0
        };

        frame.render_widget(
            Paragraph::new(rendered_type_lines)
                .block(themed_block(&palette, typing_active).title(typing_title))
                .wrap(Wrap { trim: false })
                .scroll((type_scroll_y, 0)),
            type_area,
        );

        if !app.paused && matches!(app.state, AppState::Typing) {
            let eff_line = if type_cursor_col >= type_inner_width {
                type_cursor_line.saturating_add(1)
            } else {
                type_cursor_line
            };
            let eff_col = if type_cursor_col >= type_inner_width {
                0
            } else {
                type_cursor_col
            };
            let cursor_inner_row = eff_line.saturating_sub(type_scroll_y);
            let cursor_x = type_area.x + 1 + eff_col;
            let cursor_y = type_area.y + 1 + cursor_inner_row;
            if cursor_y < type_area.y + type_area.height.saturating_sub(1)
                && cursor_x < type_area.x + type_area.width.saturating_sub(1)
            {
                frame.set_cursor_position((cursor_x, cursor_y));
            }
        }
    }

    // 底部快捷键提示 bar（带圆角边框与结构化标题）
    let hint = hint_text(
        browsing,
        browsing_builtin,
        app.text.is_online(),
        app.paused,
        app.session.is_empty(),
    );
    let hint_title = Line::from(vec![Span::styled(
        " 快捷键 ",
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )]);
    // 成功热重载后，状态栏底部右侧短暂闪现「方案已重载」（约 2s 淡出，issue #97）。
    let mut help_block = themed_block(&palette, false).title(hint_title);
    if let Some((msg, style)) = app.scheme_reload_status() {
        help_block = help_block.title_bottom(Line::from(Span::styled(msg, style)).right_aligned());
    }
    frame.render_widget(
        Paragraph::new(hint_bar_line(hint, &palette)).block(help_block),
        help_bar,
    );

    if matches!(app.state, AppState::Settings) {
        render_settings(frame, app);
    }

    // 模态框（覆盖层，必须在最顶层渲染）
    if let Some(form) = &app.login_form {
        render_login_modal(frame, form, &palette, app.theme());
    }
    if let Some(modal) = &app.free_input_modal {
        render_free_input_modal(frame, modal, &palette, app.theme());
    }
    if let Some(modal) = &app.text_setting_modal {
        render_text_setting_modal(frame, modal, &palette, app.theme());
    }
    // 续打选择弹窗：独立居中模态层（不再塞进侧边栏）
    if let Some((set, saved, total)) = app.resume_prompt {
        render_resume_prompt_popup(frame, set, saved, total, &palette, app.theme());
    }
    // 开始准备倒计时弹窗：覆盖层，倒计时结束由主循环自动关闭。
    if matches!(app.state, AppState::Countdown { .. }) {
        render_countdown_popup(frame, app, &palette);
    }

    // 非阻塞「方案加载中…」角标：右上角小标签，后台加载期间显示，跟打/设置照常进行。
    if let Some(loading_id) = &app.scheme_loading {
        let label = format!(" ◌ 方案加载中：{loading_id} ");
        let width = (label.width() as u16).min(frame.area().width);
        let badge_area = Rect {
            x: frame.area().x + frame.area().width.saturating_sub(width),
            y: frame.area().y,
            width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(palette.warning).bg(palette.bg)),
            badge_area,
        );
    }
}

/// 登录模态框：居中弹层，用户名 + 遮蔽密码。
fn render_login_modal(frame: &mut Frame, form: &LoginForm, palette: &ThemePalette, _theme: Theme) {
    let area = centered_rect(frame.area(), 62, 9);
    frame.render_widget(Clear, area);
    let mut lines = vec![
        Line::from(" 登录 52dazi ").bold().fg(palette.fg),
        Line::from(""),
    ];
    let user_label = if form.focus == 0 {
        "用户名 ▸ "
    } else {
        "用户名   "
    };
    lines.push(Line::from(format!(" {user_label}{}", form.username)).fg(palette.fg));
    let pass_label = if form.focus == 1 {
        "密码   ▸ "
    } else {
        "密码     "
    };
    lines
        .push(Line::from(format!(" {pass_label}{}", mask_password(&form.password))).fg(palette.fg));
    lines.push(Line::from(""));
    if form.busy {
        lines.push(Line::from(" 登录中…").fg(palette.warning));
    } else if let Some(err) = &form.error {
        lines.push(Line::from(format!(" 错误: {err}")).fg(palette.error));
    } else {
        lines.push(hint_bar_line(" Enter 登录 | Tab 切换 | Esc 取消 ", palette));
    }
    let block = themed_block(palette, true)
        .title(" 登录 ")
        .style(Style::default().bg(palette.bg).fg(palette.fg));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );

    let (field_x, field_y) = if form.focus == 0 {
        let w = UnicodeWidthStr::width(form.username.as_str()) as u16;
        (area.x + 1 + 10 + w, area.y + 1 + 2)
    } else {
        let w = UnicodeWidthStr::width(mask_password(&form.password).as_str()) as u16;
        (area.x + 1 + 10 + w, area.y + 1 + 3)
    };
    if field_y < area.y + area.height.saturating_sub(1)
        && field_x < area.x + area.width.saturating_sub(1)
    {
        frame.set_cursor_position((field_x, field_y));
    }
}

/// 自定义设置文本弹窗（方案路径 / 输入法名称）：居中弹层，单行文本输入。
fn render_text_setting_modal(
    frame: &mut Frame,
    modal: &TextSettingModal,
    palette: &ThemePalette,
    _theme: Theme,
) {
    let (title, hint_line) = match modal.target {
        TextSettingTarget::Scheme => (
            " 自定义反查方案/路径 ",
            Line::from(" 支持方案名或 .schema.yaml / .dict.yaml 文件绝对路径").fg(palette.muted),
        ),
        TextSettingTarget::InputMethod => {
            let remaining =
                Settings::INPUT_METHOD_MAX_CHARS.saturating_sub(modal.input.chars().count());
            (
                " 自定义上传输入法名称 ",
                Line::from(format!(" 还可输入 {remaining} 字（52dazi 上报展示）"))
                    .fg(palette.muted),
            )
        }
    };
    let area = centered_rect(frame.area(), 56, 8);
    frame.render_widget(Clear, area);
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(" ▸ ", Style::default().fg(palette.accent).bold()),
            Span::styled(&modal.input, Style::default().fg(palette.fg).bold()),
        ]),
        Line::from(""),
        hint_line,
        hint_bar_line(" Enter 保存 | Esc 取消 ", palette),
    ];
    let block = themed_block(palette, true)
        .title(title)
        .style(Style::default().bg(palette.bg).fg(palette.fg));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );

    let input_w = UnicodeWidthStr::width(modal.input.as_str()) as u16;
    let cursor_x = (area.x + 1 + 3 + input_w).min(area.x + area.width.saturating_sub(2));
    let cursor_y = area.y + 1 + 1;
    if cursor_y < area.y + area.height.saturating_sub(1)
        && cursor_x < area.x + area.width.saturating_sub(1)
    {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

/// 续打选择弹窗：从侧边栏剥离出来的居中模态层，展示进度并提供继续/重开/重置操作。
fn render_resume_prompt_popup(
    frame: &mut Frame,
    set: BuiltinSet,
    saved: usize,
    total: usize,
    palette: &ThemePalette,
    _theme: Theme,
) {
    let complete = total > 0 && saved >= total;
    let area = centered_rect(frame.area(), 48, 10);
    frame.render_widget(Clear, area);

    let block = themed_block(palette, true)
        .title(" 📚 续打进度 ")
        .style(Style::default().bg(palette.bg).fg(palette.fg));

    // 进度条：█ 已完 / ░ 未完
    let bar_w: u64 = 16;
    let pct = if total == 0 { 0 } else { saved * 100 / total };
    let filled = if total == 0 {
        0
    } else {
        (saved as u64 * bar_w / total as u64) as usize
    };
    let filled = filled.min(bar_w as usize);
    let bar = format!(
        "{}{}",
        "█".repeat(filled),
        "░".repeat((bar_w as usize) - filled)
    );

    let name = set.name();
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));
    if complete {
        lines.push(Line::from(vec![
            Span::styled(" 🎉 ", Style::default().fg(palette.success).bold()),
            Span::styled(
                format!("「{name}」已全部完成！"),
                Style::default().fg(palette.success).bold(),
            ),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled(" 📖 ", Style::default().fg(palette.accent)),
            Span::styled(
                format!("「{name}」"),
                Style::default().fg(palette.fg).bold(),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" 📊 已完成 ", Style::default().fg(palette.muted)),
        Span::styled(
            format!("{saved} / {total} 组"),
            Style::default().fg(palette.fg).bold(),
        ),
        Span::styled(format!("  {pct}%"), Style::default().fg(palette.accent)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" ", Style::default().fg(palette.muted)),
        Span::styled(bar, Style::default().fg(palette.accent)),
    ]));
    lines.push(Line::from(""));
    if complete {
        lines.push(hint_bar_line(
            " [r] 重新开始 🔄 | [x] 重置进度 🗑️ | [Esc] 返回 ↩️ ",
            palette,
        ));
    } else {
        lines.push(hint_bar_line(
            " [c] 继续 🚀 | [r] 重新开始 🔄 | [x] 重置 🗑️ | [Esc] 返回 ↩️ ",
            palette,
        ));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// 开始准备倒计时弹窗：居中显示 3-2-1 大数字，倒计时结束由主循环自动关闭并开始跟打。
fn render_countdown_popup(frame: &mut Frame, app: &App, palette: &ThemePalette) {
    let AppState::Countdown { deadline, source } = &app.state else {
        return;
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    // 剩余秒数向上取整：3.0~2.0 显示 3、2.0~1.0 显示 2、1.0~0.0 显示 1，每个数字约 1 秒。
    let secs = remaining.as_secs_f32().ceil().max(1.0) as u32;
    let (title, tip) = match source {
        CountdownSource::Resume => (" ⏱ 继续跟打 ", "手指就位，继续计时"),
        _ => (" ⏱ 准备开始 ", "手指就位，马上开始"),
    };
    let area = centered_rect(frame.area(), 36, 9);
    frame.render_widget(Clear, area);

    let block = themed_block(palette, true)
        .title(title)
        .style(Style::default().bg(palette.bg).fg(palette.fg));

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from("").centered());
    lines.push(
        Line::from(vec![Span::styled(
            format!("  {secs}  "),
            Style::default().fg(palette.accent).bold(),
        )])
        .centered(),
    );
    lines.push(Line::from("").centered());
    lines.push(Line::from(tip).centered());
    lines.push(Line::from("").centered());
    lines.push(hint_bar_line(" [Esc] 取消 ↩️ ", palette));

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// 自由发文模态框：居中弹层，标题 + 多行正文 + 保存选项。
fn render_free_input_modal(
    frame: &mut Frame,
    modal: &FreeInputModal,
    palette: &ThemePalette,
    _theme: Theme,
) {
    let area = centered_rect(frame.area(), 68, 20);
    frame.render_widget(Clear, area);

    let outer_block = themed_block(palette, true)
        .title(" 自由发文 ")
        .style(Style::default().bg(palette.bg).fg(palette.fg));
    frame.render_widget(outer_block, area);

    let inner_area = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };

    let [title_rect, content_rect, save_rect, button_rect, hint_rect] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(6),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(2),
    ])
    .areas(inner_area);

    // 1. 标题
    let is_title_focus = modal.focus == FREE_INPUT_FOCUS_TITLE;
    let title_prefix = if is_title_focus {
        "▸ 标题: "
    } else {
        "  标题: "
    };
    let mut title_spans = vec![Span::raw(title_prefix)];
    if is_title_focus {
        title_spans[0] = Span::styled(title_prefix, Style::default().fg(palette.accent).bold());
    }
    title_spans.push(Span::styled(&modal.title, Style::default().fg(palette.fg)));
    frame.render_widget(Paragraph::new(Line::from(title_spans)), title_rect);

    // 2. 正文
    let is_content_focus = modal.focus == FREE_INPUT_FOCUS_CONTENT;
    let content_border_style = if is_content_focus {
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.muted)
    };
    let content_title = format!(" 赛文正文（{} 字）", modal.content.chars().count());
    frame.render_widget(
        Paragraph::new(modal.content.as_str())
            .style(Style::default().fg(palette.fg))
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(content_title)
                    .border_style(content_border_style),
            )
            .wrap(Wrap { trim: false }),
        content_rect,
    );

    // 3. 保存复选框与路径
    let is_cb_focus = modal.focus == FREE_INPUT_FOCUS_SAVE_CHECKBOX;
    let is_path_focus = modal.focus == FREE_INPUT_FOCUS_SAVE_PATH;
    let cb_mark = if modal.save_to_file { "[x]" } else { "[ ]" };
    let mut save_lines = vec![Line::from(vec![if is_cb_focus {
        Span::styled(
            format!("▸ {cb_mark} 保存为本地文件（空格切换）"),
            Style::default().fg(palette.accent).bold(),
        )
    } else {
        Span::styled(
            format!("  {cb_mark} 保存为本地文件（空格切换）"),
            Style::default().fg(palette.fg),
        )
    }])];
    if modal.save_to_file {
        let path_prefix = if is_path_focus {
            "    ▸ 路径: "
        } else {
            "      路径: "
        };
        let mut path_spans = vec![Span::raw(path_prefix)];
        if is_path_focus {
            path_spans[0] = Span::styled(path_prefix, Style::default().fg(palette.accent).bold());
        }
        path_spans.push(Span::styled(
            &modal.save_path,
            Style::default().fg(palette.fg),
        ));
        save_lines.push(Line::from(path_spans));
    }
    frame.render_widget(Paragraph::new(save_lines), save_rect);

    // 4. 按钮行
    let is_submit_focus = modal.focus == FREE_INPUT_FOCUS_SUBMIT_BTN;
    let is_cancel_focus = modal.focus == FREE_INPUT_FOCUS_CANCEL_BTN;

    let submit_btn = if is_submit_focus {
        Span::styled(
            " [ 确认发文 (Ctrl-Enter) ] ",
            Style::default().reversed().fg(palette.accent).bold(),
        )
    } else {
        Span::styled(
            " [ 确认发文 (Ctrl-Enter) ] ",
            Style::default().fg(palette.accent).bold(),
        )
    };

    let cancel_btn = if is_cancel_focus {
        Span::styled(
            " [ 取消 (Esc) ] ",
            Style::default().reversed().fg(palette.fg).bold(),
        )
    } else {
        Span::styled(" [ 取消 (Esc) ] ", Style::default().fg(palette.fg))
    };

    let button_line = Line::from(vec![
        Span::raw("  "),
        submit_btn,
        Span::raw("   "),
        cancel_btn,
    ]);
    frame.render_widget(Paragraph::new(button_line), button_rect);

    // 5. 底部提示 / 错误
    let hint_lines = if let Some(err) = &modal.error {
        vec![Line::from(format!(" 错误: {err}")).fg(palette.error)]
    } else {
        vec![hint_bar_line(
            " Ctrl-Enter 发文 | Enter 换行 | Tab 切换 | Esc 取消 ",
            palette,
        )]
    };
    frame.render_widget(Paragraph::new(hint_lines), hint_rect);

    if is_title_focus {
        let w = UnicodeWidthStr::width(modal.title.as_str()) as u16;
        let c_x = (title_rect.x + 8 + w).min(title_rect.x + title_rect.width.saturating_sub(1));
        let c_y = title_rect.y;
        if c_y < area.y + area.height.saturating_sub(1)
            && c_x < area.x + area.width.saturating_sub(1)
        {
            frame.set_cursor_position((c_x, c_y));
        }
    } else if is_content_focus {
        let content_w = content_rect.width.saturating_sub(2);
        let (c_line, c_col) = calculate_text_layout_position(modal.content.chars(), content_w);
        let c_x =
            (content_rect.x + 1 + c_col).min(content_rect.x + content_rect.width.saturating_sub(1));
        let c_y = content_rect.y + 1 + c_line;
        if c_y < content_rect.y + content_rect.height.saturating_sub(1)
            && c_x < content_rect.x + content_rect.width.saturating_sub(1)
        {
            frame.set_cursor_position((c_x, c_y));
        }
    } else if is_path_focus {
        let w = UnicodeWidthStr::width(modal.save_path.as_str()) as u16;
        let c_x = (save_rect.x + 12 + w).min(save_rect.x + save_rect.width.saturating_sub(1));
        let c_y = save_rect.y + 1;
        if c_y < save_rect.y + save_rect.height.saturating_sub(1)
            && c_x < save_rect.x + save_rect.width.saturating_sub(1)
        {
            frame.set_cursor_position((c_x, c_y));
        }
    }
}

/// 计算居中矩形。
fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// 左侧功能栏：菜单列表。
fn render_sidebar(
    frame: &mut Frame,
    app: &App,
    area: ratatui::layout::Rect,
    browsing: bool,
    browsing_builtin: bool,
) {
    let _theme = app.theme();
    let palette = app.palette();
    let mut lines: Vec<Line> = Vec::new();
    if browsing {
        lines.push(Line::from(" 载入文件:").bold().fg(palette.fg));
        if app.browse_files.is_empty() {
            lines.push(Line::from("   （无文本文件）").fg(palette.fg));
        } else {
            for (i, path) in app.browse_files.iter().enumerate() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let prefix = if i == app.browse_selection {
                    " > "
                } else {
                    "   "
                };
                let mut line = Line::from(format!("{prefix}{name}"));
                if i == app.browse_selection {
                    line = line.fg(palette.accent).bold();
                } else {
                    line = line.fg(palette.fg);
                }
                lines.push(line);
            }
        }
        if let Some(err) = &app.browse_error {
            lines.push(Line::from(format!(" 错误: {err}")).fg(palette.error));
        }
    } else if browsing_builtin {
        lines.push(Line::from(" 内置赛文:").bold().fg(palette.fg));
        for (i, set) in BUILTIN_SETS.iter().enumerate() {
            let prefix = if i == app.builtin_selection {
                " > "
            } else {
                "   "
            };
            let label = set.name().to_string();
            let mut line = Line::from(format!("{prefix}{label}"));
            if i == app.builtin_selection {
                line = line.fg(palette.accent).bold();
            } else {
                line = line.fg(palette.fg);
            }
            lines.push(line);
        }
    } else {
        if app.paused {
            lines.push(Line::from(" [跟打已暂停]").bold().fg(palette.warning));
        }
        lines.push(Line::from(" 赛文来源:").bold().fg(palette.fg));

        for (idx, item) in SIDEBAR_MENU_ITEMS.iter().enumerate() {
            let is_sel = idx == app.sidebar_selected && (app.session.is_empty() || app.paused);
            let (key_badge, label, is_accent, is_warn) = match item {
                SidebarMenuItem::LoadFile => ("f", "载入文件", false, false),
                SidebarMenuItem::BuiltinText => ("b", "内置赛文", false, false),
                SidebarMenuItem::FreeInput => ("i", "自由发文", false, false),
                SidebarMenuItem::Clipboard => ("p", "剪贴板发文", false, false),
                SidebarMenuItem::OnlineJisu => ("1", "极速杯", false, false),
                SidebarMenuItem::OnlineJinbiao => ("2", "锦标赛", false, false),
                SidebarMenuItem::OnlineJianshen => ("3", "键神杯", false, false),
                SidebarMenuItem::OnlineRank => ("4", "排行榜", false, false),
                SidebarMenuItem::Stats => ("s", "数据统计", false, false),
                SidebarMenuItem::Settings => ("o", "设置", false, false),
                SidebarMenuItem::Sponsor => ("d", "赞赏支持", false, false),
                SidebarMenuItem::Login => {
                    if app.logged_in {
                        ("u", "已登录 52dazi", true, false)
                    } else {
                        ("u", "登录 52dazi", false, true)
                    }
                }
            };

            if *item == SidebarMenuItem::OnlineJisu {
                lines.push(Line::from(""));
                lines.push(Line::from(" 在线比赛:").bold().fg(palette.fg));
            } else if *item == SidebarMenuItem::Stats {
                lines.push(Line::from(""));
                lines.push(Line::from(" 统计与系统:").bold().fg(palette.fg));
            }

            let mut spans = Vec::new();
            if is_sel {
                spans.push(Span::styled(
                    " ▸ ",
                    Style::default().fg(palette.accent).bold(),
                ));
                spans.push(Span::styled("◖", Style::default().fg(palette.accent)));
                spans.push(Span::styled(
                    key_badge,
                    Style::default()
                        .fg(palette.bg)
                        .bg(palette.accent)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled("◗", Style::default().fg(palette.accent)));
                spans.push(Span::styled(
                    format!(" {label}"),
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled("   ", Style::default().fg(palette.fg)));
                spans.push(Span::styled("◖", Style::default().fg(palette.selection)));
                spans.push(Span::styled(
                    key_badge,
                    Style::default()
                        .fg(palette.accent)
                        .bg(palette.selection)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled("◗", Style::default().fg(palette.selection)));
                let label_style = if is_accent {
                    Style::default().fg(palette.accent)
                } else if is_warn {
                    Style::default().fg(palette.warning)
                } else {
                    Style::default().fg(palette.fg)
                };
                spans.push(Span::styled(format!(" {label}"), label_style));
            }
            lines.push(Line::from(spans));
        }
    }

    // 提示信息（加载中 / 通知 / 错误）
    if let Some(notice) = &app.sidebar_notice {
        lines.push(Line::from(""));
        lines.push(Line::from(format!(" {notice}")).fg(palette.accent));
    }
    if let Some(notice) = &app.login_notice {
        lines.push(Line::from(format!("  {notice}")).fg(palette.fg));
    }
    if let Some(ct) = app.online_loading {
        lines.push(Line::from(format!(" 正在载入{}...", ct.name())).fg(palette.accent));
    }
    if let Some(err) = &app.online_error {
        lines.push(Line::from(format!(" {err}")).fg(palette.error));
    }

    let is_active = browsing || browsing_builtin || app.paused || app.session.is_empty();
    let mut title_spans = vec![Span::styled(
        " 功能栏 ",
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )];
    if app.paused {
        title_spans.push(Span::styled(
            "[已暂停] ",
            Style::default()
                .fg(palette.warning)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let title = Line::from(title_spans);
    frame.render_widget(
        Paragraph::new(lines)
            .block(themed_block(&palette, is_active).title(title))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// 载文预览：右侧内容区显示选中文件的内容（前 400 字符）。
fn render_preview(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let _theme = app.theme();
    let palette = app.palette();
    let (title, body, style) = match app.browse_files.get(app.browse_selection) {
        Some(path) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            match fs::read_to_string(path) {
                Ok(raw) => {
                    let preview: String = raw.chars().take(400).collect();
                    let dot = if raw.chars().count() > 400 { "…" } else { "" };
                    (
                        name,
                        format!("{preview}{dot}"),
                        Style::default().fg(palette.fg),
                    )
                }
                Err(_) => (
                    name,
                    "（无法读取预览）".to_string(),
                    Style::default().fg(palette.error),
                ),
            }
        }
        None => (
            "预览".to_string(),
            "（无文件可选）".to_string(),
            Style::default().fg(palette.fg),
        ),
    };
    let preview_title = Line::from(vec![
        Span::styled(
            " 载文预览 ",
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("— {title} "), Style::default().fg(palette.fg)),
    ]);
    let lines = vec![
        Line::from(format!(" 载文预览 — {title} "))
            .bold()
            .fg(palette.fg),
        Line::from(""),
        Line::styled(body, style),
        Line::from(""),
        hint_bar_line(" Enter 载入 | Esc 取消 ", &palette),
    ];
    let block = themed_block(&palette, false)
        .title(preview_title)
        .style(Style::default().bg(palette.bg).fg(palette.fg));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// 词组赛文预览：取前 `group_size` 个词，词间加空格。
fn builtin_word_preview(
    boundaries: &[(usize, usize)],
    chars: &[char],
    group_size: usize,
) -> String {
    let preview_words = boundaries.len().min(group_size);
    let mut preview = String::new();
    for (i, &(ws, we)) in boundaries.iter().take(preview_words).enumerate() {
        if i > 0 {
            preview.push(' ');
        }
        for ch in chars[ws..we].iter() {
            preview.push(*ch);
        }
    }
    if boundaries.len() > group_size {
        preview.push_str(" …");
    }
    preview
}

/// 单字赛文预览：取前 400 字，超长则加省略号。
fn builtin_char_preview(content: &str) -> String {
    let chars: Vec<char> = content.chars().take(400).collect();
    let dot = if content.chars().count() > 400 {
        "…"
    } else {
        ""
    };
    format!("{}{dot}", chars.iter().collect::<String>())
}

/// 内置赛文预览：右侧内容区显示选中套题的内容预览。
fn render_builtin_preview(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let _theme = app.theme();
    let palette = app.palette();
    let (title, body) = app
        .builtin_preview
        .clone()
        .unwrap_or_else(|| ("预览".to_string(), "（无内置赛文）".to_string()));
    let group_size = app.settings.group_size as usize;
    let is_words = matches!(BUILTIN_SETS.get(app.builtin_selection), Some(set) if set.is_words());
    let unit_label = if is_words { "词" } else { "字" };

    let mut lines: Vec<Line> = vec![
        Line::from(format!(" 内置赛文 — {title} "))
            .bold()
            .fg(palette.fg),
        Line::from(""),
    ];
    if is_words {
        lines.push(Line::from(body).fg(palette.fg));
    } else {
        for chunk in body.chars().collect::<Vec<char>>().chunks(group_size) {
            lines.push(Line::from(chunk.iter().collect::<String>()).fg(palette.fg));
        }
    }
    lines.push(Line::from(""));
    let shuffle_label = if app.builtin_shuffle {
        "乱序(开)"
    } else {
        "乱序"
    };
    lines.push(hint_bar_line(
        &format!(" Enter 载入 | s {shuffle_label} | g 分组({group_size}{unit_label}) | Esc 取消 "),
        &palette,
    ));
    let builtin_preview_title = Line::from(vec![
        Span::styled(
            " 内置赛文预览 ",
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("— {title} "), Style::default().fg(palette.fg)),
        Span::styled(
            format!("[g] 分组: {group_size} {unit_label}/组 "),
            Style::default().bold().fg(palette.accent),
        ),
    ]);
    let block = themed_block(&palette, false)
        .title(builtin_preview_title)
        .style(Style::default().bg(palette.bg).fg(palette.fg));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// 赞赏与支持全局视图：展示极客幽默寄语与微信、支付宝赞赏二维码。
fn render_sponsor_view(frame: &mut Frame, app: &App) {
    let palette = app.palette();
    let total_area = frame.area();

    // 渲染全屏底色
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.bg).fg(palette.fg)),
        total_area,
    );

    // 垂直切分：顶部寄语标题 (高度 6) + 中间左右双二维码卡片 (Min 0) + 底部提示栏 (高度 3)
    let [header_area, body_area, hint_area] = Layout::vertical([
        Constraint::Length(6),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .areas(total_area);

    // 1. 顶部寄语 Banner
    let header_block = themed_block(&palette, true).title(Line::from(vec![Span::styled(
        " 💖 赞赏 & 支持开源开发 (Support & Sponsor) ",
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )]));
    let slogan_lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            "“键盘敲烂，码长砍半！给作者投喂一杯咖啡 ☕，继续用纯粹的 Rust 打造更好用的终端跟打神器 🦀。”",
            Style::default().fg(palette.fg).add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![Span::styled(
            "打字推坚持开源、纯粹、无广告。感谢每一位在指尖追求极致手速与韵律的跟打者！",
            Style::default().fg(palette.muted),
        )]),
    ];
    let header_paragraph = Paragraph::new(slogan_lines)
        .block(header_block)
        .alignment(ratatui::layout::Alignment::Center);
    frame.render_widget(header_paragraph, header_area);

    // 2. 中部二维码双卡片（左右水平分栏）
    let [left_card_area, right_card_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .areas(body_area);

    // 左侧：微信支付
    let wechat_title = Line::from(vec![Span::styled(
        " 微信支付 (WeChat Pay) ",
        Style::default()
            .fg(palette.success)
            .add_modifier(Modifier::BOLD),
    )]);
    let wechat_block = themed_block(&palette, false)
        .title(wechat_title)
        .border_style(Style::default().fg(palette.success));
    let inner_wechat = wechat_block.inner(left_card_area);
    frame.render_widget(wechat_block, left_card_area);

    // 右侧：支付宝
    let alipay_title = Line::from(vec![Span::styled(
        " 支付宝 (Alipay) ",
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )]);
    let alipay_block = themed_block(&palette, false)
        .title(alipay_title)
        .border_style(Style::default().fg(palette.accent));
    let inner_alipay = alipay_block.inner(right_card_area);
    frame.render_widget(alipay_block, right_card_area);

    // 延迟初始化图片渲染协议并绘制
    let mut sponsor_lock = app.sponsor_state.borrow_mut();
    if sponsor_lock.is_none() {
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        let wechat_img = image::load_from_memory(WECHAT_IMG_BYTES).expect("load wechat.png");
        let alipay_img = image::load_from_memory(ALIPAY_IMG_BYTES).expect("load alipay.jpg");
        *sponsor_lock = Some(SponsorViewState {
            wechat: picker.new_resize_protocol(wechat_img),
            alipay: picker.new_resize_protocol(alipay_img),
        });
    }

    if let Some(state) = sponsor_lock.as_mut() {
        frame.render_stateful_widget(StatefulImage::default(), inner_wechat, &mut state.wechat);
        frame.render_stateful_widget(StatefulImage::default(), inner_alipay, &mut state.alipay);
    }

    // 3. 底部提示栏
    let hint_spans = vec![
        Span::raw("  "),
        Span::styled(
            " [Esc / q / d] ",
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("返回跟打主页    "),
        Span::styled(
            "✨ 感谢您的每一份认可与支持！",
            Style::default().fg(palette.muted),
        ),
    ];
    let hint_block = themed_block(&palette, false);
    let hint_paragraph = Paragraph::new(Line::from(hint_spans)).block(hint_block);
    frame.render_widget(hint_paragraph, hint_area);
}

/// 统计数据中心全局视图：顶部三级 Tab 导航与内容区。
fn render_stats_view(frame: &mut Frame, app: &App, stats_state: &StatsViewState) {
    let palette = app.palette();
    let total_area = frame.area();

    // 渲染全屏底色
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.bg).fg(palette.fg)),
        total_area,
    );

    // 垂直切分：顶部导航 Tab (高度 3) + 主内容区 (Min 0) + 底部快捷键 (高度 3)
    let [header_area, body_area, hint_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .areas(total_area);

    // 1. 顶部 Tab 栏
    let tab_spans = vec![
        Span::raw("  "),
        if stats_state.tab == StatsTab::WpmTrend {
            Span::styled(
                " ◖ 1. 速度趋势 ◗ ",
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.selection)
                    .bold(),
            )
        } else {
            Span::styled(
                "   1. 速度趋势   ",
                Style::default().fg(palette.muted).bg(palette.bg),
            )
        },
        Span::raw("  "),
        if stats_state.tab == StatsTab::Heatmap {
            Span::styled(
                " ◖ 2. 键位热力图 ◗ ",
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.selection)
                    .bold(),
            )
        } else {
            Span::styled(
                "   2. 键位热力图   ",
                Style::default().fg(palette.muted).bg(palette.bg),
            )
        },
        Span::raw("  "),
        if stats_state.tab == StatsTab::ErrorRanking {
            Span::styled(
                " ◖ 3. 错字排行 ◗ ",
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.selection)
                    .bold(),
            )
        } else {
            Span::styled(
                "   3. 错字排行   ",
                Style::default().fg(palette.muted).bg(palette.bg),
            )
        },
    ];

    let header_title = Line::from(vec![Span::styled(
        " 统计数据中心 ",
        Style::default().bold().fg(palette.accent),
    )]);

    frame.render_widget(
        Paragraph::new(Line::from(tab_spans))
            .block(themed_block(&palette, true).title(header_title)),
        header_area,
    );

    // 2. 主体区内容（按当前选中的 Tab 渲染）
    match stats_state.tab {
        StatsTab::WpmTrend => render_wpm_trend_tab(
            frame,
            app,
            body_area,
            stats_state.trend_metric,
            stats_state.wpm_range,
            &palette,
        ),
        StatsTab::Heatmap => render_heatmap_tab(
            frame,
            app,
            body_area,
            stats_state.heatmap_layout,
            stats_state.heatmap_source,
            &palette,
        ),
        StatsTab::ErrorRanking => render_error_ranking_tab(
            frame,
            app,
            body_area,
            stats_state.error_ranking_focus,
            stats_state.char_scroll,
            stats_state.word_scroll,
            stats_state.char_selected,
            stats_state.word_selected,
            stats_state.status_msg.as_deref(),
            &palette,
        ),
    }

    // 3. 底部快捷键提示
    let hint_str = match stats_state.tab {
        StatsTab::WpmTrend => {
            " 1/2/3 Tab | hl 左右 | r 范围 | v/s 切换指标(WPM/KPS) | Esc/q 返回 | o 设置 | Ctrl-Q 退出 "
        }
        StatsTab::Heatmap => {
            " 1/2/3 Tab | hl 左右 | L 键盘布局 | m 视角 | Esc/q 返回 | o 设置 | Ctrl-Q 退出 "
        }
        StatsTab::ErrorRanking => {
            " 1/2/3 Tab | t 字/词焦点 | jk 选择 | PgUp/PgDn 翻页 | d/x 删除 | Esc/q 返回 | o 设置 | Ctrl-Q 退出 "
        }
    };
    let hint_title = Line::from(vec![Span::styled(
        " 快捷键 ",
        Style::default().bold().fg(palette.accent),
    )]);
    frame.render_widget(
        Paragraph::new(hint_bar_line(hint_str, &palette))
            .block(themed_block(&palette, false).title(hint_title)),
        hint_area,
    );
}

/// Tab 1: WPM / KPS 历史演进趋势图与历史概览卡片。
fn render_wpm_trend_tab(
    frame: &mut Frame,
    _app: &App,
    area: Rect,
    metric: TrendMetric,
    range: WpmChartRange,
    palette: &ThemePalette,
) {
    let db = StatsDb::with_default_path().ok();
    let summary = db
        .as_ref()
        .and_then(|d| d.get_global_summary().ok())
        .unwrap_or_default();
    let history_points = match metric {
        TrendMetric::Wpm => db
            .as_ref()
            .and_then(|d| d.get_rolling_wpm_history_with_limit(10, range.limit()).ok())
            .unwrap_or_default(),
        TrendMetric::Kps => db
            .as_ref()
            .and_then(|d| d.get_rolling_kps_history_with_limit(10, range.limit()).ok())
            .unwrap_or_default(),
    };

    let [summary_area, chart_area] =
        Layout::vertical([Constraint::Length(4), Constraint::Min(0)]).areas(area);

    let total_hrs = summary.total_duration_secs / 3600.0;
    let duration_str = if total_hrs >= 1.0 {
        format!("{total_hrs:.1} 小时")
    } else {
        format!("{:.1} 分钟", summary.total_duration_secs / 60.0)
    };

    let summary_lines = vec![
        Line::from(vec![
            Span::styled(" 总练习场次: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{} 场", summary.total_sessions),
                Style::default().bold().fg(palette.accent),
            ),
            Span::raw("    "),
            Span::styled(" 累计跟打用时: ", Style::default().fg(palette.muted)),
            Span::styled(duration_str, Style::default().bold().fg(palette.fg)),
            Span::raw("    "),
            Span::styled(" 累计输入: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{} 字", summary.total_typed_chars),
                Style::default().bold().fg(palette.success),
            ),
            Span::raw("    "),
            Span::styled(" 累计击数: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{} 击", summary.total_strokes),
                Style::default().bold().fg(palette.accent),
            ),
        ]),
        Line::from(vec![
            Span::styled(" 历史最高: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{:.1} WPM", summary.best_wpm),
                Style::default().bold().fg(palette.accent),
            ),
            Span::raw("    "),
            Span::styled(" 历史均速: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{:.1} WPM", summary.avg_wpm),
                Style::default().bold().fg(palette.fg),
            ),
            Span::raw("    "),
            Span::styled(" 平均击速: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{:.2} KPS", summary.avg_kps),
                Style::default().bold().fg(palette.accent),
            ),
            Span::raw("    "),
            Span::styled(" 平均码长: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{:.2}", summary.avg_key_length),
                Style::default().bold().fg(palette.success),
            ),
            Span::raw("    "),
            Span::styled(" 平均正确率: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{:.1}%", summary.avg_accuracy * 100.0),
                Style::default().bold().fg(palette.fg),
            ),
        ]),
    ];

    let summary_title = Line::from(vec![Span::styled(
        " 历史练习总览 ",
        Style::default().bold().fg(palette.accent),
    )]);
    frame.render_widget(
        Paragraph::new(summary_lines).block(themed_block(palette, true).title(summary_title)),
        summary_area,
    );

    let range_badge = range.label();
    let metric_badge = metric.label();
    let title_prefix = match metric {
        TrendMetric::Wpm => "WPM",
        TrendMetric::Kps => "KPS 击速",
    };
    let chart_title = Line::from(vec![
        Span::styled(
            format!(" {title_prefix} 历史演进趋势 "),
            Style::default().bold().fg(palette.accent),
        ),
        Span::styled(
            format!("— 指标: [{metric_badge} (按 v/s 切换)] · 范围: [{range_badge} (按 r 切换)] "),
            Style::default().fg(palette.muted),
        ),
    ]);

    let mut raw_points: Vec<(f64, f64)> = history_points
        .iter()
        .enumerate()
        .map(|(idx, (_time, val, _rolling))| (idx as f64 + 1.0, *val))
        .collect();

    let mut rolling_points: Vec<(f64, f64)> = history_points
        .iter()
        .enumerate()
        .map(|(idx, (_time, _val, rolling))| (idx as f64 + 1.0, *rolling))
        .collect();

    if raw_points.len() > 100 {
        raw_points = lttb_downsample(&raw_points, 100);
        rolling_points = lttb_downsample(&rolling_points, 100);
    }

    let (dataset_raw_name, dataset_rolling_name, y_title, min_y_bound) = match metric {
        TrendMetric::Wpm => ("单场 WPM", "10场滚动平均", "WPM", 30.0),
        TrendMetric::Kps => ("单场击速 (KPS)", "10场滚动平均", "KPS (击/秒)", 5.0),
    };

    let max_x = (history_points.len() as f64).max(5.0);
    let max_y_raw = raw_points.iter().map(|p| p.1).fold(0.0, f64::max);
    let max_y_rolling = rolling_points.iter().map(|p| p.1).fold(0.0, f64::max);
    let max_y = (max_y_raw.max(max_y_rolling).max(min_y_bound) * 1.15).ceil();

    let x_labels = vec![
        Span::styled("第 1 场", Style::default().fg(palette.muted).bg(palette.bg)),
        Span::styled(
            format!("第 {:.0} 场", max_x / 2.0),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
        Span::styled(
            format!("第 {:.0} 场", max_x),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
    ];

    let y_labels = match metric {
        TrendMetric::Wpm => vec![
            Span::styled("0", Style::default().fg(palette.muted).bg(palette.bg)),
            Span::styled(
                format!("{:.0}", max_y / 2.0),
                Style::default().fg(palette.muted).bg(palette.bg),
            ),
            Span::styled(
                format!("{max_y:.0}"),
                Style::default().fg(palette.muted).bg(palette.bg),
            ),
        ],
        TrendMetric::Kps => vec![
            Span::styled("0.0", Style::default().fg(palette.muted).bg(palette.bg)),
            Span::styled(
                format!("{:.1}", max_y / 2.0),
                Style::default().fg(palette.muted).bg(palette.bg),
            ),
            Span::styled(
                format!("{max_y:.1}"),
                Style::default().fg(palette.muted).bg(palette.bg),
            ),
        ],
    };

    let datasets = vec![
        Dataset::default()
            .name(dataset_raw_name)
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(palette.accent).bg(palette.bg))
            .data(&raw_points),
        Dataset::default()
            .name(dataset_rolling_name)
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(palette.success).bg(palette.bg))
            .data(&rolling_points),
    ];

    let chart = Chart::new(datasets)
        .block(themed_block(palette, true).title(chart_title))
        .style(Style::default().bg(palette.bg).fg(palette.fg))
        .x_axis(
            Axis::default()
                .title(Span::styled(
                    "场次序数",
                    Style::default().fg(palette.muted).bg(palette.bg),
                ))
                .style(Style::default().fg(palette.muted).bg(palette.bg))
                .bounds([1.0, max_x])
                .labels(x_labels),
        )
        .y_axis(
            Axis::default()
                .title(Span::styled(
                    y_title,
                    Style::default().fg(palette.muted).bg(palette.bg),
                ))
                .style(Style::default().fg(palette.muted).bg(palette.bg))
                .bounds([0.0, max_y])
                .labels(y_labels),
        );

    frame.render_widget(chart, chart_area);

    // 无历史数据时坐标轴（含单位标题）仍应渲染；在绘图区居中提示用户，不覆盖块标题。
    if history_points.is_empty() {
        let [_, center, _] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .areas(chart_area);
        let empty_msg = Paragraph::new(Line::from(Span::styled(
            "暂无有效跟打历史记录。完成跟打练习后，系统将在此自动绘制速度演进折线图。",
            Style::default().fg(palette.muted),
        )))
        .alignment(ratatui::layout::Alignment::Center);
        frame.render_widget(empty_msg, center);
    }
}

/// Tab 2: 键位热力图分析（标准斜列 ANSI 60% 与直列矩阵 4x12 切换，方案反查与物理击键切换，对数平滑着色）。
fn render_heatmap_tab(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    layout: HeatmapLayout,
    source: HeatmapSource,
    palette: &ThemePalette,
) {
    let db = StatsDb::with_default_path().ok();
    let mut scheme_dict_loaded = false;
    let mut loaded_scheme_name = String::new();
    let mut dict_path_display = String::new();

    let key_counts = match source {
        HeatmapSource::RawKeypress => db
            .as_ref()
            .and_then(|d| d.get_key_press_totals(Some(true)).ok())
            .unwrap_or_default(),
        HeatmapSource::SchemeProjected => {
            // 复用已加载/缓存的码表，避免每次绘制都重新解析（51MB 空明拳会卡死）。
            if let Some(dict) = &app.scheme_dict {
                scheme_dict_loaded = true;
                loaded_scheme_name = dict.name().unwrap_or(&app.settings.scheme).to_string();
                let scheme_id = &app.settings.scheme;
                dict_path_display = app
                    .discovered
                    .iter()
                    .find(|d| &d.id == scheme_id)
                    .map(|d| d.path.display().to_string())
                    .unwrap_or_else(|| scheme_id.clone());
                let sessions = db
                    .as_ref()
                    .and_then(|d| d.get_all_sessions().ok())
                    .unwrap_or_default();
                let mut counts = std::collections::HashMap::new();
                for s in sessions {
                    let proj = dict.project_text_to_keys(&s.text_title);
                    for (k, v) in proj {
                        *counts.entry(k).or_insert(0) += v;
                    }
                }
                if counts.is_empty() {
                    db.as_ref()
                        .and_then(|d| d.get_key_press_totals(Some(true)).ok())
                        .unwrap_or_default()
                } else {
                    counts
                }
            } else {
                db.as_ref()
                    .and_then(|d| d.get_key_press_totals(Some(true)).ok())
                    .unwrap_or_default()
            }
        }
    };

    let max_count = key_counts.values().copied().max().unwrap_or(0).max(1) as f64;
    let log_max = (1.0 + max_count).ln();
    let total_presses: u32 = key_counts.values().copied().sum();

    let [info_area, keyboard_area] =
        Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).areas(area);

    // 1. 顶部控制与方案状态卡片
    let layout_badge = layout.label();
    let source_badge = source.label();
    let scheme_status_str = if source == HeatmapSource::SchemeProjected {
        if scheme_dict_loaded {
            format!("已加载方案 [{loaded_scheme_name}]: {dict_path_display}")
        } else if app.settings.scheme.is_empty() {
            "未配置反查方案（按 Ctrl-E 设置）".to_string()
        } else {
            format!(
                "未找到方案 [{}] 码表文件 (可放至 ~/.config/dazitui/schemes/)，当前回退物理击键",
                app.settings.scheme
            )
        }
    } else {
        "物理击键模式（直接统计键盘输入事件）".to_string()
    };

    let info_lines = vec![
        Line::from(vec![
            Span::styled(" 当前布局: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("[{layout_badge} (按 l 切换)]"),
                Style::default().bold().fg(palette.accent),
            ),
            Span::raw("    "),
            Span::styled(" 数据视角: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("[{source_badge} (按 m 切换)]"),
                Style::default().bold().fg(palette.success),
            ),
            Span::raw("    "),
            Span::styled(" 累计击键: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{total_presses} 次"),
                Style::default().bold().fg(palette.fg),
            ),
            Span::raw("    "),
            Span::styled(" 峰值单键: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{:.0} 次", max_count),
                Style::default().bold().fg(palette.accent),
            ),
        ]),
        Line::from(vec![
            Span::styled(" 方案状态: ", Style::default().fg(palette.muted)),
            Span::styled(scheme_status_str, Style::default().fg(palette.fg)),
        ]),
        Line::from(vec![
            Span::styled(" 热力图例: ", Style::default().fg(palette.muted)),
            Span::styled(
                " [ 0次 ] ",
                Style::default().fg(palette.muted).bg(palette.bg),
            ),
            Span::styled(" [ 0-25% ] ", Style::default().fg(palette.muted)),
            Span::styled(" [ 25-50% ] ", Style::default().fg(palette.fg)),
            Span::styled(" [ 50-75% ] ", Style::default().fg(palette.success).bold()),
            Span::styled(
                " [ >75% ] ",
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.selection)
                    .bold(),
            ),
        ]),
    ];

    let info_title = Line::from(vec![Span::styled(
        " 键位热力图参数 ",
        Style::default().bold().fg(palette.accent),
    )]);
    frame.render_widget(
        Paragraph::new(info_lines).block(themed_block(palette, true).title(info_title)),
        info_area,
    );

    // 2. 键盘矩阵渲染
    let get_key_data = |key_name: &str| -> (Style, u32) {
        let count = *key_counts
            .get(key_name)
            .or_else(|| key_counts.get(&key_name.to_lowercase()))
            .unwrap_or(&0);
        if count == 0 {
            (Style::default().fg(palette.muted).bg(palette.bg), 0)
        } else {
            let intensity = (1.0 + count as f64).ln() / log_max;
            let style = if intensity > 0.75 {
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.selection)
                    .bold()
            } else if intensity > 0.50 {
                Style::default().fg(palette.success).bold()
            } else if intensity > 0.25 {
                Style::default().fg(palette.fg)
            } else {
                Style::default().fg(palette.muted)
            };
            (style, count)
        }
    };

    let mut keyboard_lines = Vec::new();
    keyboard_lines.push(Line::from(""));

    match layout {
        HeatmapLayout::Staggered => {
            // ANSI 60% 五行斜列紧凑布局（精简版：仅保留主键区、Bksp 与 Space，与实时键盘对齐）
            let rows: [&[(&str, &str)]; 5] = [
                &[
                    ("`", "~ `"),
                    ("1", "1"),
                    ("2", "2"),
                    ("3", "3"),
                    ("4", "4"),
                    ("5", "5"),
                    ("6", "6"),
                    ("7", "7"),
                    ("8", "8"),
                    ("9", "9"),
                    ("0", "0"),
                    ("-", "-"),
                    ("=", "="),
                    ("Backspace", "Bksp"),
                ],
                &[
                    ("q", "Q"),
                    ("w", "W"),
                    ("e", "E"),
                    ("r", "R"),
                    ("t", "T"),
                    ("y", "Y"),
                    ("u", "U"),
                    ("i", "I"),
                    ("o", "O"),
                    ("p", "P"),
                    ("[", "["),
                    ("]", "]"),
                    ("\\", "\\"),
                ],
                &[
                    ("a", "A"),
                    ("s", "S"),
                    ("d", "D"),
                    ("f", "F"),
                    ("g", "G"),
                    ("h", "H"),
                    ("j", "J"),
                    ("k", "K"),
                    ("l", "L"),
                    (";", ";"),
                    ("'", "'"),
                ],
                &[
                    ("z", "Z"),
                    ("x", "X"),
                    ("c", "C"),
                    ("v", "V"),
                    ("b", "B"),
                    ("n", "N"),
                    ("m", "M"),
                    (",", ","),
                    (".", "."),
                    ("/", "/"),
                ],
                &[("Space", "Space (空格)")],
            ];

            let row_indents = [
                "  ",
                "     ",
                "       ",
                "         ",
                "                           ",
            ];

            for (r_idx, row) in rows.iter().enumerate() {
                let mut spans = vec![Span::raw(row_indents[r_idx])];
                for (k_lookup, k_display) in *row {
                    let (style, count) = get_key_data(k_lookup);
                    let badge = if *k_lookup == "Space" {
                        format!(" [ {:^16} ({:>3}) ] ", k_display, count)
                    } else if k_display.len() > 1 {
                        format!(" [{:<4} {:>3}] ", k_display, count)
                    } else {
                        format!(" [{:^1} {:>3}] ", k_display, count)
                    };
                    spans.push(Span::styled(badge, style));
                }
                keyboard_lines.push(Line::from(spans));
                keyboard_lines.push(Line::from(""));
            }
        }
        HeatmapLayout::Ortholinear => {
            // Planck 4x12 直列网格紧凑布局（精简版：仅保留主键区、Bksp 与 Space，与实时键盘对齐）
            let rows: [&[(&str, &str)]; 4] = [
                &[
                    ("q", "Q"),
                    ("w", "W"),
                    ("e", "E"),
                    ("r", "R"),
                    ("t", "T"),
                    ("y", "Y"),
                    ("u", "U"),
                    ("i", "I"),
                    ("o", "O"),
                    ("p", "P"),
                    ("Backspace", "Bksp"),
                ],
                &[
                    ("a", "A"),
                    ("s", "S"),
                    ("d", "D"),
                    ("f", "F"),
                    ("g", "G"),
                    ("h", "H"),
                    ("j", "J"),
                    ("k", "K"),
                    ("l", "L"),
                    (";", ";"),
                    ("'", "'"),
                ],
                &[
                    ("z", "Z"),
                    ("x", "X"),
                    ("c", "C"),
                    ("v", "V"),
                    ("b", "B"),
                    ("n", "N"),
                    ("m", "M"),
                    (",", ","),
                    (".", "."),
                    ("/", "/"),
                ],
                &[("Space", "Space (空格)")],
            ];

            let row_indents = ["   ", "   ", "   ", "                         "];

            for (r_idx, row) in rows.iter().enumerate() {
                let mut spans = vec![Span::raw(row_indents[r_idx])];
                for (k_lookup, k_display) in *row {
                    let (style, count) = get_key_data(k_lookup);
                    let badge = if *k_lookup == "Space" {
                        format!(" [ {:^16} ({:>3}) ] ", k_display, count)
                    } else if k_display.len() > 1 {
                        format!(" [{:<4} {:>3}] ", k_display, count)
                    } else {
                        format!(" [{:^1} {:>3}] ", k_display, count)
                    };
                    spans.push(Span::styled(badge, style));
                }
                keyboard_lines.push(Line::from(spans));
                keyboard_lines.push(Line::from(""));
            }
        }
    }

    let keyboard_title = Line::from(vec![
        Span::styled(" 键盘热力矩阵 ", Style::default().bold().fg(palette.accent)),
        Span::styled(
            format!("— 视图: [{layout_badge}] · 视角: [{source_badge}] "),
            Style::default().fg(palette.muted),
        ),
    ]);

    frame.render_widget(
        Paragraph::new(keyboard_lines).block(themed_block(palette, true).title(keyboard_title)),
        keyboard_area,
    );
}

/// Tab 3: 高频错字与错词排行榜双列滚动表格。
#[allow(clippy::too_many_arguments)]
fn render_error_ranking_tab(
    frame: &mut Frame,
    _app: &App,
    area: Rect,
    focus: ErrorRankingFocus,
    char_scroll: usize,
    word_scroll: usize,
    char_selected: usize,
    word_selected: usize,
    status_msg: Option<&str>,
    palette: &ThemePalette,
) {
    let db = StatsDb::with_default_path().ok();
    let top_chars = db
        .as_ref()
        .and_then(|d| d.get_top_mistyped_chars(50).ok())
        .unwrap_or_default();
    let top_words = db
        .as_ref()
        .and_then(|d| d.get_top_mistyped_words(50).ok())
        .unwrap_or_default();

    let [info_area, table_area] =
        Layout::vertical([Constraint::Length(4), Constraint::Min(0)]).areas(area);

    // 1. 顶部概要卡片
    let total_unique_chars = top_chars.len();
    let total_unique_words = top_words.len();
    let total_char_errors: u32 = top_chars.iter().map(|c| c.error_count).sum();
    let total_word_errors: u32 = top_words.iter().map(|w| w.error_count).sum();

    let focus_badge = focus.label();
    let tip_line = if let Some(msg) = status_msg {
        Line::from(vec![
            Span::styled(" ▶ 状态: ", Style::default().bold().fg(palette.warning)),
            Span::styled(msg, Style::default().bold().fg(palette.accent)),
            Span::styled(
                "  (↑/↓ 移动, PgUp/PgDn 翻页, t 切换, d/x 删除)",
                Style::default().fg(palette.muted),
            ),
        ])
    } else {
        Line::from(vec![Span::styled(
            " 提示：↑ / ↓ 移动选中项，PgUp / PgDn 快速翻页；按 t 切换榜单；按 d 或 x 删除选中的错字/错词。",
            Style::default().fg(palette.muted),
        )])
    };

    let info_lines = vec![
        Line::from(vec![
            Span::styled(" 当前聚焦: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("[{focus_badge} (按 t 切换)]"),
                Style::default().bold().fg(palette.accent),
            ),
            Span::raw("    "),
            Span::styled(" 高频错字数: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{total_unique_chars} 种 (累计 {total_char_errors} 次)"),
                Style::default().bold().fg(palette.fg),
            ),
            Span::raw("    "),
            Span::styled(" 高频错词数: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{total_unique_words} 组 (累计 {total_word_errors} 次)"),
                Style::default().bold().fg(palette.success),
            ),
        ]),
        tip_line,
    ];

    let info_title = Line::from(vec![Span::styled(
        " 错字与错词数据总览 ",
        Style::default().bold().fg(palette.accent),
    )]);
    frame.render_widget(
        Paragraph::new(info_lines).block(themed_block(palette, true).title(info_title)),
        info_area,
    );

    // 2. 双列左右均分表格区
    let [left_col, right_col] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .areas(table_area);

    let is_char_focused = focus == ErrorRankingFocus::Chars;
    let is_word_focused = focus == ErrorRankingFocus::Words;

    // 左列：高频错字榜
    let char_title = Line::from(vec![Span::styled(
        if is_char_focused {
            " ▶ 高频错字排行榜 (Top 50) [d/x 删除] "
        } else {
            "   高频错字排行榜 (Top 50) "
        },
        if is_char_focused {
            Style::default().bold().fg(palette.accent)
        } else {
            Style::default().fg(palette.fg)
        },
    )]);

    let mut char_lines = Vec::new();
    // 表头
    char_lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("排名 ", Style::default().bold().fg(palette.muted)),
        Span::styled(" 错字 ", Style::default().bold().fg(palette.accent)),
        Span::styled("   高频误打 ", Style::default().bold().fg(palette.warning)),
        Span::styled("   累计错次 ", Style::default().bold().fg(palette.error)),
    ]));
    char_lines.push(Line::from(Span::styled(
        " ─────────────────────────────────────────────────────",
        Style::default().fg(palette.muted),
    )));

    if top_chars.is_empty() {
        char_lines.push(Line::from(""));
        char_lines.push(Line::from(Span::styled(
            "   暂无错字记录，继续保持！",
            Style::default().fg(palette.success),
        )));
    } else {
        let visible_capacity = left_col.height.saturating_sub(4) as usize;
        let max_scroll = top_chars.len().saturating_sub(visible_capacity);
        let actual_scroll = char_scroll.min(max_scroll);

        for (idx, stat) in top_chars
            .iter()
            .skip(actual_scroll)
            .take(visible_capacity)
            .enumerate()
        {
            let item_idx = actual_scroll + idx;
            let is_selected = is_char_focused && item_idx == char_selected;
            let rank = item_idx + 1;
            let rank_badge = match rank {
                1 => Span::styled(
                    " #1 ",
                    Style::default()
                        .bold()
                        .fg(palette.accent)
                        .bg(palette.selection),
                ),
                2 => Span::styled(" #2 ", Style::default().bold().fg(palette.warning)),
                3 => Span::styled(" #3 ", Style::default().bold().fg(palette.success)),
                _ => Span::styled(format!(" #{rank:<2}"), Style::default().fg(palette.muted)),
            };

            let actual_display = match stat.top_actual_char {
                Some(c) => format!("'{}'", c),
                None => "-".to_string(),
            };

            let cursor_span = if is_selected {
                Span::styled("▶", Style::default().bold().fg(palette.accent))
            } else {
                Span::raw(" ")
            };

            let row_style = if is_selected {
                Style::default().bg(palette.selection)
            } else {
                Style::default()
            };

            char_lines.push(
                Line::from(vec![
                    cursor_span,
                    rank_badge,
                    Span::styled(
                        format!("   '{}' ", stat.target_char),
                        if is_selected {
                            Style::default().bold().fg(palette.accent)
                        } else {
                            Style::default().bold().fg(palette.fg)
                        },
                    ),
                    Span::styled(
                        format!("      {:^4} ", actual_display),
                        Style::default().fg(palette.warning),
                    ),
                    Span::styled(
                        format!("      {:>4} 次", stat.error_count),
                        Style::default().bold().fg(palette.error),
                    ),
                ])
                .style(row_style),
            );
        }
    }

    frame.render_widget(
        Paragraph::new(char_lines).block(themed_block(palette, is_char_focused).title(char_title)),
        left_col,
    );

    // 右列：高频错词榜
    let word_title = Line::from(vec![Span::styled(
        if is_word_focused {
            " ▶ 高频错词排行榜 (Top 50) [d/x 删除] "
        } else {
            "   高频错词排行榜 (Top 50) "
        },
        if is_word_focused {
            Style::default().bold().fg(palette.accent)
        } else {
            Style::default().fg(palette.fg)
        },
    )]);

    let mut word_lines = Vec::new();
    // 表头
    word_lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("排名 ", Style::default().bold().fg(palette.muted)),
        Span::styled(" 归因错词 ", Style::default().bold().fg(palette.accent)),
        Span::styled(
            "      累计错误频次 ",
            Style::default().bold().fg(palette.fg),
        ),
        Span::styled(
            "   影响练习场次 ",
            Style::default().bold().fg(palette.warning),
        ),
    ]));
    word_lines.push(Line::from(Span::styled(
        " ─────────────────────────────────────────────────────",
        Style::default().fg(palette.muted),
    )));

    if top_words.is_empty() {
        word_lines.push(Line::from(""));
        word_lines.push(Line::from(Span::styled(
            "   暂无错词记录，词汇跟打非常准确！",
            Style::default().fg(palette.success),
        )));
    } else {
        let visible_capacity = right_col.height.saturating_sub(4) as usize;
        let max_scroll = top_words.len().saturating_sub(visible_capacity);
        let actual_scroll = word_scroll.min(max_scroll);

        for (idx, stat) in top_words
            .iter()
            .skip(actual_scroll)
            .take(visible_capacity)
            .enumerate()
        {
            let item_idx = actual_scroll + idx;
            let is_selected = is_word_focused && item_idx == word_selected;
            let rank = item_idx + 1;
            let rank_badge = match rank {
                1 => Span::styled(
                    " #1 ",
                    Style::default()
                        .bold()
                        .fg(palette.accent)
                        .bg(palette.selection),
                ),
                2 => Span::styled(" #2 ", Style::default().bold().fg(palette.warning)),
                3 => Span::styled(" #3 ", Style::default().bold().fg(palette.success)),
                _ => Span::styled(format!(" #{rank:<2}"), Style::default().fg(palette.muted)),
            };

            let cursor_span = if is_selected {
                Span::styled("▶", Style::default().bold().fg(palette.accent))
            } else {
                Span::raw(" ")
            };

            let row_style = if is_selected {
                Style::default().bg(palette.selection)
            } else {
                Style::default()
            };

            word_lines.push(
                Line::from(vec![
                    cursor_span,
                    rank_badge,
                    Span::styled(
                        format!("   {:<8}", stat.target_word),
                        if is_selected {
                            Style::default().bold().fg(palette.accent)
                        } else {
                            Style::default().bold().fg(palette.fg)
                        },
                    ),
                    Span::styled(
                        format!("          {:>4} 次", stat.error_count),
                        Style::default().bold().fg(palette.error),
                    ),
                    Span::styled(
                        format!("          {:>4} 场", stat.affected_sessions),
                        Style::default().fg(palette.warning),
                    ),
                ])
                .style(row_style),
            );
        }
    }

    frame.render_widget(
        Paragraph::new(word_lines).block(themed_block(palette, is_word_focused).title(word_title)),
        right_col,
    );
}

/// 设置视图：焦点行 + 左右调整（主题/占比/粗体/实时键盘/反查方案/上传名称）。
fn render_settings(frame: &mut Frame, app: &App) {
    let palette = app.palette();
    let focus = app.settings_focus;
    let mut lines = vec![Line::from(" 设置 ").bold(), Line::from("")];

    lines.push(settings_row(
        "主题",
        app.settings.theme.name(),
        focus == FOCUS_THEME,
        &palette,
    ));
    lines.push(settings_row(
        "对照区占比",
        &format!("{}%", app.settings.reference_ratio),
        focus == FOCUS_RATIO,
        &palette,
    ));
    lines.push(settings_row(
        "粗体",
        on_off(app.settings.bold),
        focus == FOCUS_BOLD,
        &palette,
    ));
    lines.push(settings_row(
        "实时键盘",
        app.settings.keyboard_mode.name(),
        focus == FOCUS_KEYBOARD,
        &palette,
    ));
    lines.push(settings_row(
        "反查方案",
        &scheme_current_label(app),
        focus == FOCUS_SCHEME,
        &palette,
    ));
    lines.push(settings_row(
        "上传名称",
        input_method_display(&app.settings.input_method),
        focus == FOCUS_INPUT_METHOD,
        &palette,
    ));
    lines.push(settings_row(
        "分组大小",
        &format!("{} 字/词", app.settings.group_size),
        focus == FOCUS_GROUP_SIZE,
        &palette,
    ));
    lines.push(settings_row(
        "遍码提示",
        on_off(app.settings.code_hint),
        focus == FOCUS_CODE_HINT,
        &palette,
    ));
    lines.push(settings_row(
        "方案热监控",
        on_off(app.settings.monitor_scheme),
        focus == FOCUS_MONITOR_SCHEME,
        &palette,
    ));

    lines.push(Line::from(""));
    // 主题预览：用当前主题的对/错色渲染示意文字。
    lines.push(Line::from(" 预览:").bold().fg(palette.fg));
    lines.push(Line::from("  对正确对正确").fg(palette.success));
    lines.push(Line::from("  错错误错错误").fg(palette.error));
    lines.push(Line::from(""));
    lines.push(hint_bar_line(" jk 选择 | hl 调整 | Esc/q 返回 ", &palette));

    let area = centered_rect(frame.area(), 60, 20);
    frame.render_widget(Clear, area);
    let settings_title = Line::from(vec![Span::styled(
        " 设置 ",
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )]);
    let block = themed_block(&palette, true)
        .title(settings_title)
        .style(Style::default().bg(palette.bg).fg(palette.fg));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// 设置项行：焦点项用 accent 色 + `>` 标记高亮。
fn settings_row(label: &str, value: &str, focused: bool, palette: &ThemePalette) -> Line<'static> {
    let marker = if focused { " > " } else { "   " };
    let line = Line::from(format!("{marker}{label}: {value}"));
    if focused {
        line.fg(palette.accent).bold()
    } else {
        line.fg(palette.fg)
    }
}

/// 布尔开关显示为「开/关」。
fn on_off(v: bool) -> &'static str {
    if v { "开" } else { "关" }
}

/// 生成实时虚拟键盘的渲染行（纯函数，易单测）。
pub fn generate_live_keyboard_lines(
    live_kb: &LiveKeyboard,
    mode: KeyboardMode,
    palette: &ThemePalette,
    now: Instant,
    target_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match mode {
        KeyboardMode::Staggered => {
            // ANSI 60% 五行斜列紧凑布局（精简版：仅保留主键区、Bksp 与 Space）
            let rows: [&[(&str, &str)]; 5] = [
                &[
                    ("`", "~ `"),
                    ("1", "1"),
                    ("2", "2"),
                    ("3", "3"),
                    ("4", "4"),
                    ("5", "5"),
                    ("6", "6"),
                    ("7", "7"),
                    ("8", "8"),
                    ("9", "9"),
                    ("0", "0"),
                    ("-", "-"),
                    ("=", "="),
                    ("Backspace", "Bksp"),
                ],
                &[
                    ("q", "Q"),
                    ("w", "W"),
                    ("e", "E"),
                    ("r", "R"),
                    ("t", "T"),
                    ("y", "Y"),
                    ("u", "U"),
                    ("i", "I"),
                    ("o", "O"),
                    ("p", "P"),
                    ("[", "["),
                    ("]", "]"),
                    ("\\", "\\"),
                ],
                &[
                    ("a", "A"),
                    ("s", "S"),
                    ("d", "D"),
                    ("f", "F"),
                    ("g", "G"),
                    ("h", "H"),
                    ("j", "J"),
                    ("k", "K"),
                    ("l", "L"),
                    (";", ";"),
                    ("'", "'"),
                ],
                &[
                    ("z", "Z"),
                    ("x", "X"),
                    ("c", "C"),
                    ("v", "V"),
                    ("b", "B"),
                    ("n", "N"),
                    ("m", "M"),
                    (",", ","),
                    (".", "."),
                    ("/", "/"),
                ],
                &[("Space", "Space")],
            ];

            let row_indents = ["", "     ", "       ", "         ", "                 "];
            let max_layout_width = 59u16;
            let center_pad = target_width.saturating_sub(max_layout_width) / 2;
            let pad_prefix = " ".repeat(center_pad as usize);

            for (r_idx, row) in rows.iter().enumerate() {
                let mut spans = vec![Span::raw(format!("{}{}", pad_prefix, row_indents[r_idx]))];
                for (k_idx, (k_lookup, k_display)) in row.iter().enumerate() {
                    let norm = LiveKeyboard::normalize_key(k_lookup);
                    let is_homing = *k_lookup == "f" || *k_lookup == "j";
                    let is_modifier = matches!(
                        *k_lookup,
                        "Backspace"
                            | "Bksp"
                            | "Tab"
                            | "Caps"
                            | "Shift"
                            | "Enter"
                            | "Ctrl"
                            | "Alt"
                            | "Esc"
                            | "Lower"
                            | "Raise"
                            | "Left"
                            | "Down"
                            | "Up"
                            | "Right"
                    );

                    let elapsed_opt = live_kb
                        .active_keys
                        .get(&norm)
                        .map(|&t| now.saturating_duration_since(t).as_millis());

                    if k_idx > 0 {
                        spans.push(Span::raw(" "));
                    }

                    if let Some(elapsed_ms) = elapsed_opt {
                        if elapsed_ms <= 100 {
                            // 强高亮 (0-100ms): 实体按键反色高亮
                            let active_style = Style::default()
                                .fg(palette.bg)
                                .bg(palette.accent)
                                .add_modifier(Modifier::BOLD);
                            let badge = if *k_lookup == "Space" {
                                format!("[ {:^20} ]", k_display)
                            } else {
                                format!("[{k_display}]")
                            };
                            spans.push(Span::styled(badge, active_style));
                        } else if elapsed_ms <= 250 {
                            // 余温衰减 (100-250ms): 强调色渐隐
                            let decay_style = Style::default()
                                .fg(palette.accent)
                                .add_modifier(Modifier::BOLD);
                            let badge = if *k_lookup == "Space" {
                                format!("[ {:^20} ]", k_display)
                            } else {
                                format!("[{k_display}]")
                            };
                            spans.push(Span::styled(badge, decay_style));
                        } else {
                            // 恢复常态
                            append_idle_key_spans(
                                &mut spans,
                                k_lookup,
                                k_display,
                                is_homing,
                                is_modifier,
                                palette,
                                20,
                            );
                        }
                    } else {
                        // 常态
                        append_idle_key_spans(
                            &mut spans,
                            k_lookup,
                            k_display,
                            is_homing,
                            is_modifier,
                            palette,
                            20,
                        );
                    }
                }
                lines.push(Line::from(spans));
            }
        }
        KeyboardMode::Ortholinear => {
            // Planck 4x12 直列网格紧凑布局（精简版：仅保留主键区、Bksp 与 Space）
            let rows: [&[(&str, &str)]; 4] = [
                &[
                    ("q", "Q"),
                    ("w", "W"),
                    ("e", "E"),
                    ("r", "R"),
                    ("t", "T"),
                    ("y", "Y"),
                    ("u", "U"),
                    ("i", "I"),
                    ("o", "O"),
                    ("p", "P"),
                    ("Backspace", "Bksp"),
                ],
                &[
                    ("a", "A"),
                    ("s", "S"),
                    ("d", "D"),
                    ("f", "F"),
                    ("g", "G"),
                    ("h", "H"),
                    ("j", "J"),
                    ("k", "K"),
                    ("l", "L"),
                    (";", ";"),
                    ("'", "'"),
                ],
                &[
                    ("z", "Z"),
                    ("x", "X"),
                    ("c", "C"),
                    ("v", "V"),
                    ("b", "B"),
                    ("n", "N"),
                    ("m", "M"),
                    (",", ","),
                    (".", "."),
                    ("/", "/"),
                ],
                &[("Space", "Space")],
            ];

            let row_indents = ["", "", "", "           "];
            let max_layout_width = 46u16;
            let center_pad = target_width.saturating_sub(max_layout_width) / 2;
            let pad_prefix = " ".repeat(center_pad as usize);

            for (r_idx, row) in rows.iter().enumerate() {
                let mut spans = vec![Span::raw(format!("{}{}", pad_prefix, row_indents[r_idx]))];
                for (k_idx, (k_lookup, k_display)) in row.iter().enumerate() {
                    let norm = LiveKeyboard::normalize_key(k_lookup);
                    let is_homing = *k_lookup == "f" || *k_lookup == "j";
                    let is_modifier = matches!(
                        *k_lookup,
                        "Backspace"
                            | "Bksp"
                            | "Tab"
                            | "Caps"
                            | "Shift"
                            | "Enter"
                            | "Ctrl"
                            | "Alt"
                            | "Esc"
                            | "Lower"
                            | "Raise"
                            | "Left"
                            | "Down"
                            | "Up"
                            | "Right"
                    );

                    let elapsed_opt = live_kb
                        .active_keys
                        .get(&norm)
                        .map(|&t| now.saturating_duration_since(t).as_millis());

                    if k_idx > 0 {
                        spans.push(Span::raw(" "));
                    }

                    if let Some(elapsed_ms) = elapsed_opt {
                        if elapsed_ms <= 100 {
                            let active_style = Style::default()
                                .fg(palette.bg)
                                .bg(palette.accent)
                                .add_modifier(Modifier::BOLD);
                            let badge = if *k_lookup == "Space" {
                                format!("[ {:^20} ]", k_display)
                            } else {
                                format!("[{k_display}]")
                            };
                            spans.push(Span::styled(badge, active_style));
                        } else if elapsed_ms <= 250 {
                            let decay_style = Style::default()
                                .fg(palette.accent)
                                .add_modifier(Modifier::BOLD);
                            let badge = if *k_lookup == "Space" {
                                format!("[ {:^20} ]", k_display)
                            } else {
                                format!("[{k_display}]")
                            };
                            spans.push(Span::styled(badge, decay_style));
                        } else {
                            append_idle_key_spans(
                                &mut spans,
                                k_lookup,
                                k_display,
                                is_homing,
                                is_modifier,
                                palette,
                                20,
                            );
                        }
                    } else {
                        append_idle_key_spans(
                            &mut spans,
                            k_lookup,
                            k_display,
                            is_homing,
                            is_modifier,
                            palette,
                            20,
                        );
                    }
                }
                lines.push(Line::from(spans));
            }
        }
        KeyboardMode::Off => {}
    }
    lines
}

/// 辅助函数：构造常态（未击键）下的键帽 Span 结构，实现主题配色层级化与盲打定位键强调。
fn append_idle_key_spans(
    spans: &mut Vec<Span<'static>>,
    k_lookup: &str,
    k_display: &str,
    is_homing: bool,
    is_modifier: bool,
    palette: &ThemePalette,
    space_width: usize,
) {
    let delim_style = Style::default().fg(palette.accent);
    if k_lookup == "Space" {
        spans.push(Span::styled("[", delim_style));
        spans.push(Span::styled(
            format!(" {:^width$} ", k_display, width = space_width),
            Style::default().fg(palette.muted),
        ));
        spans.push(Span::styled("]", delim_style));
    } else if is_homing {
        // 定位键 (F / J): 鲜明主题强调色 + 粗体，形成视觉瞄点
        spans.push(Span::styled("[", delim_style));
        spans.push(Span::styled(
            k_display.to_string(),
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled("]", delim_style));
    } else if is_modifier {
        // 修饰/功能键: 柔和次要色，避免喧宾夺主
        spans.push(Span::styled(
            format!("[{k_display}]"),
            Style::default().fg(palette.muted),
        ));
    } else {
        // 核心字母/符号键: 高对比度主题前景色，清晰易读
        spans.push(Span::styled("[", delim_style));
        spans.push(Span::styled(
            k_display.to_string(),
            Style::default().fg(palette.fg),
        ));
        spans.push(Span::styled("]", delim_style));
    }
}

/// 渲染实时虚拟键盘 Widget。
fn render_live_keyboard(
    frame: &mut Frame,
    live_kb: &LiveKeyboard,
    mode: KeyboardMode,
    area: Rect,
    palette: &ThemePalette,
    now: Instant,
) {
    let lines = generate_live_keyboard_lines(live_kb, mode, palette, now, area.width);
    if !lines.is_empty() {
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }
}

/// 全屏成绩视图：WPM/错字/用时摘要卡片 + WPM 速度折线图与打错标记 + 错字时间线明细 + 导航快捷键。
fn render_result_view(
    frame: &mut Frame,
    app: &App,
    stats: &Stats,
    upload: &UploadState,
    elapsed: Duration,
) {
    let theme = app.theme();
    let palette = app.palette();
    let total_area = frame.area();

    // 渲染全屏底色
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.bg).fg(palette.fg)),
        total_area,
    );

    // 1. 顶部成绩摘要
    let strokes = if stats.total_strokes > 0 {
        stats.total_strokes
    } else {
        stats.key_frequency.iter().map(|(_, n)| n).sum()
    };
    let accuracy = key_accuracy_pct(stats);
    let word_ratio = word_ratio_pct(&app.text, stats);
    let mut summary_lines = vec![Line::from(vec![
        Span::raw(" 🚀WPM: "),
        Span::styled(
            format!("{:.1}", stats.wpm),
            Style::default().bold().fg(palette.accent),
        ),
        Span::raw("   ⌨️击键: "),
        Span::styled(
            format!("{:.2}", stats.kps),
            Style::default().bold().fg(palette.accent),
        ),
        Span::raw("   📏码长: "),
        Span::styled(
            format!("{:.2}", stats.key_length),
            Style::default().bold().fg(palette.success),
        ),
        Span::raw("   ✅正确字数: "),
        Span::styled(
            format!(
                "{}/{}",
                stats.correct_chars,
                app.text.content.chars().count()
            ),
            Style::default().bold().fg(palette.success),
        ),
        Span::raw("   ❌错字: "),
        Span::styled(
            format!(
                "{} (不一致 {} + 回改 {})",
                stats.wrong_total, stats.wrong_chars, stats.edits
            ),
            Style::default().fg(if stats.wrong_total > 0 {
                palette.error
            } else {
                palette.success
            }),
        ),
        Span::raw("   ↩️回改: "),
        Span::styled(
            format!("{}", stats.edits),
            Style::default().bold().fg(palette.fg),
        ),
        Span::raw("   🔢键数: "),
        Span::styled(
            format!("{}", strokes),
            Style::default().bold().fg(palette.fg),
        ),
        Span::raw("   🎯键准: "),
        Span::styled(
            format!("{:.2}%", accuracy),
            Style::default().bold().fg(palette.accent),
        ),
        Span::raw("   💬打词率: "),
        Span::styled(
            format!("{:.2}%", word_ratio),
            Style::default().bold().fg(palette.accent),
        ),
        Span::raw("   ⏱️用时: "),
        Span::styled(format_time(elapsed), Style::default().bold().fg(palette.fg)),
    ])];
    if !stats.edit_details.is_empty() {
        let details: String = stats.edit_details.iter().collect();
        summary_lines.push(Line::from(format!(" 回改明细: {details}")));
    }
    // 上传状态（在线赛文）/ 统计复制状态（自由发文与离线赛文）
    summary_lines.extend(upload_lines(upload, theme, &app.settings.input_method));

    // 计算顶部高度：内容行按终端内宽折算换行（超宽行占多行）后的总行数 + 边框 2 行
    let inner_width = (total_area.width.saturating_sub(2)).max(1) as usize;
    let content_rows: usize = summary_lines
        .iter()
        .map(|line| line.width().div_ceil(inner_width).max(1))
        .sum();
    let summary_height = (content_rows + 2) as u16;

    // 2. 错字时间线行生成（按当前选中项滚动，↑/↓ 可翻看全部错字）
    let total_errors = stats.error_points.len();
    let visible_rows = total_errors.clamp(1, ERROR_TIMELINE_VISIBLE);
    let timeline_height = visible_rows as u16 + 2;

    // 3. 底部操作提示
    let hint_str = if app.text.is_online() {
        if let UploadState::Failed {
            need_relogin: true, ..
        } = upload
        {
            " Esc/q 返回 | s 统计 | u 登录并上传 | f 载文 | b 内置 | i 自由发文 | p 剪贴板 | o 设置 | Ctrl-Q 退出"
        } else {
            " Esc/q 返回 | s 统计 | f 载文 | b 内置 | i 自由发文 | p 剪贴板 | o 设置 | Ctrl-Q 退出"
        }
    } else {
        " Esc/q 返回 | Enter/r 重打 | s 统计 | f 载文 | b 内置 | i 自由发文 | p 剪贴板 | o 设置 | Ctrl-Q 退出"
    };
    let hint_line = hint_bar_line(hint_str, &palette);

    // 极小终端降级展示
    let result_title = Line::from(vec![
        Span::styled(
            " 成绩 ",
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("— {} ", app.text.title),
            Style::default().fg(palette.fg),
        ),
    ]);
    // 高度不足时图表会挤掉时间线（实测 14 行时可见区为 0 行），
    // 降级为「摘要 + 时间线」单块展示，保证错字条目始终可翻看。
    if total_area.height < 16 {
        let [top_area, bottom_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(total_area);
        let mut all_lines = summary_lines;
        all_lines.push(Line::from(""));
        all_lines.extend(error_timeline_lines(
            stats,
            app.error_point_selected,
            app.error_point_scroll,
            visible_rows.min(ERROR_TIMELINE_COMPACT_ROWS),
            &palette,
        ));
        frame.render_widget(
            Paragraph::new(all_lines)
                .block(themed_block(&palette, true).title(result_title))
                .wrap(Wrap { trim: false }),
            top_area,
        );
        let hint_title = Line::from(vec![Span::styled(
            " 快捷键 ",
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        )]);
        frame.render_widget(
            Paragraph::new(hint_line).block(themed_block(&palette, false).title(hint_title)),
            bottom_area,
        );
        return;
    }

    let [summary_area, chart_area, timeline_area, hint_area] = Layout::vertical([
        Constraint::Length(summary_height),
        Constraint::Min(6),
        Constraint::Length(timeline_height),
        Constraint::Length(3),
    ])
    .areas(total_area);

    frame.render_widget(
        Paragraph::new(summary_lines)
            .block(themed_block(&palette, true).title(result_title))
            .wrap(Wrap { trim: false }),
        summary_area,
    );

    // 绘制 Chart
    let speed_data = &stats.speed_samples;
    let error_data: Vec<(f64, f64)> = stats
        .error_points
        .iter()
        .map(|p| (p.time_secs, p.wpm))
        .collect();

    let max_x = speed_data
        .last()
        .map(|s| s.0)
        .unwrap_or(0.0)
        .max(elapsed.as_secs_f64())
        .max(5.0);
    let max_y_sample = speed_data.iter().map(|s| s.1).fold(0.0, f64::max);
    let max_y_error = error_data.iter().map(|s| s.1).fold(0.0, f64::max);
    let max_y = (max_y_sample.max(max_y_error).max(stats.wpm).max(30.0) * 1.15).ceil();

    let x_labels = vec![
        Span::styled("0s", Style::default().fg(palette.muted).bg(palette.bg)),
        Span::styled(
            format!("{:.1}s", max_x / 2.0),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
        Span::styled(
            format!("{:.1}s", max_x),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
    ];
    let y_labels = vec![
        Span::styled("0", Style::default().fg(palette.muted).bg(palette.bg)),
        Span::styled(
            format!("{:.0}", max_y / 2.0),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
        Span::styled(
            format!("{:.0}", max_y),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
    ];

    let datasets = vec![
        Dataset::default()
            .name("WPM 速度")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(palette.accent).bg(palette.bg))
            .data(speed_data),
    ];

    let max_y_label_width = y_labels
        .iter()
        .map(|s| s.content.chars().count())
        .max()
        .unwrap_or(3) as u16;

    let chart_title = Line::from(vec![Span::styled(
        " WPM 速度曲线 ",
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )]);
    let chart = Chart::new(datasets)
        .block(themed_block(&palette, true).title(chart_title))
        .style(Style::default().bg(palette.bg).fg(palette.fg))
        .x_axis(
            Axis::default()
                .title(Span::styled(
                    "时间",
                    Style::default().fg(palette.muted).bg(palette.bg),
                ))
                .style(Style::default().fg(palette.muted).bg(palette.bg))
                .bounds([0.0, max_x])
                .labels(x_labels),
        )
        .y_axis(
            Axis::default()
                .title(Span::styled(
                    "WPM",
                    Style::default().fg(palette.muted).bg(palette.bg),
                ))
                .style(Style::default().fg(palette.muted).bg(palette.bg))
                .bounds([0.0, max_y])
                .labels(y_labels),
        );

    frame.render_widget(chart, chart_area);

    overlay_error_chars_on_chart(
        frame.buffer_mut(),
        chart_area,
        stats,
        max_x,
        max_y,
        max_y_label_width,
        &palette,
    );

    let timeline_title = error_timeline_title(total_errors, app.error_point_selected, &palette);
    let capacity = (timeline_area.height.saturating_sub(2)) as usize;
    frame.render_widget(
        Paragraph::new(error_timeline_lines(
            stats,
            app.error_point_selected,
            app.error_point_scroll,
            capacity,
            &palette,
        ))
        .block(themed_block(&palette, true).title(timeline_title))
        .wrap(Wrap { trim: false }),
        timeline_area,
    );

    let hint_title = Line::from(vec![Span::styled(
        " 快捷键 ",
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )]);
    frame.render_widget(
        Paragraph::new(hint_line).block(themed_block(&palette, false).title(hint_title)),
        hint_area,
    );
}

/// 把错字时间线的滚动偏移修正到「刚好让 `selected` 可见」，`capacity` 为一屏可见条数。
///
/// 按键处理只掌握常量窗口大小，渲染时才知道终端实际容量；二者共用本函数，
/// 保证任何容量下选中项都不会滚出可见区。
fn clamp_error_scroll(selected: usize, scroll: usize, total: usize, capacity: usize) -> usize {
    if total == 0 || capacity == 0 {
        return 0;
    }
    let mut scroll = scroll.min(total.saturating_sub(capacity));
    if selected < scroll {
        scroll = selected;
    } else if selected >= scroll + capacity {
        scroll = selected + 1 - capacity;
    }
    scroll
}

/// 成绩视图「错字时间线」的区块标题：无错字时只显示标题，有错字时附上「第 n/m 处」与翻看提示。
///
/// 序号按总条数的位数右对齐补空，使标题渲染宽度恒定
/// （宽度逐帧变化会在宽字符后留下上一帧的残影）。
fn error_timeline_title(total: usize, selected: usize, palette: &ThemePalette) -> Line<'static> {
    let mut spans = vec![Span::styled(
        " 错字时间线 ",
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )];
    if total > 0 {
        let width = total.to_string().len();
        spans.push(Span::styled(
            format!(
                " 第 {:>width$}/{} 处 · ↑/↓ 翻看 ",
                selected.min(total - 1) + 1,
                total
            ),
            Style::default().fg(palette.muted),
        ));
    }
    Line::from(spans)
}

/// 生成成绩视图「错字时间线」的可见行（纯函数，供渲染与测试）。
///
/// 每个错字点占一行，选中项加 `▶` 光标与选中底色；返回行数不超过 `capacity`。
/// `selected` 越界时夹取到末条，`scroll` 会被修正到「刚好让选中项可见」的偏移。
fn error_timeline_lines(
    stats: &Stats,
    selected: usize,
    scroll: usize,
    capacity: usize,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    if stats.error_points.is_empty() {
        return vec![Line::from(" 全对无错字").fg(palette.success)];
    }
    if capacity == 0 {
        return Vec::new();
    }
    let total = stats.error_points.len();
    let selected = selected.min(total - 1);
    // 序号按总条数的位数定宽，滚动时各行渲染宽度保持不变。
    let index_width = total.to_string().len();
    // 终端高度变化时按真实容量重算偏移，保证选中项仍在可见窗口内。
    let scroll = clamp_error_scroll(selected, scroll, total, capacity);

    stats
        .error_points
        .iter()
        .enumerate()
        .skip(scroll)
        .take(capacity)
        .map(|(idx, ep)| {
            let is_selected = idx == selected;
            let (label, label_fg) = match &ep.error_type {
                ErrorType::Mismatch { typed, expected } => (
                    format!(
                        "错字: '{}' (期望'{}')",
                        typed,
                        expected
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "?".to_string())
                    ),
                    palette.error,
                ),
                ErrorType::Backspace { deleted } => (format!("回改: '{deleted}'"), palette.warning),
            };
            let meta_fg = if is_selected {
                palette.accent
            } else {
                palette.muted
            };
            Line::from(vec![
                Span::styled(
                    if is_selected { "▶" } else { " " },
                    Style::default().bold().fg(palette.accent),
                ),
                Span::styled(
                    format!("#{:<index_width$}", idx + 1),
                    Style::default().fg(meta_fg),
                ),
                Span::styled(
                    format!(" [{:04.1}s] ", ep.time_secs),
                    Style::default().fg(meta_fg),
                ),
                Span::styled(
                    label,
                    Style::default().fg(label_fg).add_modifier(if is_selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
                ),
                Span::styled(
                    format!(" · WPM {:.1}", ep.wpm),
                    Style::default().fg(meta_fg),
                ),
            ])
            .style(if is_selected {
                Style::default().bg(palette.selection)
            } else {
                Style::default()
            })
        })
        .collect()
}

/// 在速度折线图上直接标注打错点（红点）及对应的错字字符（标注在红点上方，支持多字词语组合连续展示与防碰撞）。
fn overlay_error_chars_on_chart(
    buf: &mut ratatui::buffer::Buffer,
    chart_area: Rect,
    stats: &Stats,
    max_x: f64,
    max_y: f64,
    max_y_label_width: u16,
    palette: &ThemePalette,
) {
    if stats.error_points.is_empty() || chart_area.width < 15 || chart_area.height < 6 {
        return;
    }

    let inner_x = chart_area.x + 1;
    let inner_y = chart_area.y + 1;
    let inner_w = chart_area.width.saturating_sub(2);
    let inner_h = chart_area.height.saturating_sub(2);

    let plot_x_start = inner_x + max_y_label_width + 1;
    let plot_x_end = inner_x + inner_w.saturating_sub(1);
    if plot_x_end <= plot_x_start {
        return;
    }
    let plot_width = (plot_x_end - plot_x_start) as f64;

    let plot_y_start = inner_y;
    let plot_y_end = inner_y + inner_h.saturating_sub(2);
    if plot_y_end <= plot_y_start {
        return;
    }
    let plot_height = (plot_y_end - plot_y_start) as f64;

    // 1. 将时间极其接近（如输入法同一词语一次性上屏）的打错点聚类为组
    struct ErrorCluster {
        time_secs: f64,
        wpm: f64,
        items: Vec<(char, bool)>, // (字符, 是否回改)
    }

    let mut clusters: Vec<ErrorCluster> = Vec::new();
    for ep in &stats.error_points {
        let (ch, is_backspace) = match &ep.error_type {
            ErrorType::Mismatch { typed, .. } => (*typed, false),
            ErrorType::Backspace { deleted } => (*deleted, true),
        };

        if let Some(last) = clusters.last_mut()
            && (ep.time_secs - last.time_secs).abs() < 0.35
        {
            last.items.push((ch, is_backspace));
            continue;
        }

        clusters.push(ErrorCluster {
            time_secs: ep.time_secs,
            wpm: ep.wpm,
            items: vec![(ch, is_backspace)],
        });
    }

    // 2. 依次渲染各个聚类，避免列覆盖
    let mut prev_cluster_end_col: u16 = 0;
    let mut prev_char_row: u16 = 0;

    for cluster in &clusters {
        let norm_x = (cluster.time_secs / max_x).clamp(0.0, 1.0);
        let norm_y = (cluster.wpm / max_y).clamp(0.0, 1.0);

        let base_col = (plot_x_start + (norm_x * (plot_width - 1.0)).round() as u16)
            .clamp(plot_x_start, plot_x_end.saturating_sub(1));
        let dot_row = plot_y_end
            .saturating_sub(1)
            .saturating_sub((norm_y * (plot_height - 1.0)).round() as u16)
            .clamp(plot_y_start, plot_y_end.saturating_sub(1));

        let char_row = if dot_row > plot_y_start {
            dot_row - 1
        } else {
            (dot_row + 1).min(plot_y_end.saturating_sub(1))
        };

        let total_char_width: u16 = cluster
            .items
            .iter()
            .map(|(c, _)| if c.is_ascii() { 1 } else { 2 })
            .sum();

        // 居中或锚定起始列，并防越界
        let mut start_col = base_col.saturating_sub(total_char_width / 2);
        if start_col < plot_x_start {
            start_col = plot_x_start;
        }
        if start_col + total_char_width > plot_x_end {
            start_col = plot_x_end.saturating_sub(total_char_width);
        }

        // 若与上一组在同一行发生列重叠，向后微调避免互相覆盖
        if char_row == prev_char_row && start_col < prev_cluster_end_col {
            start_col = prev_cluster_end_col.min(plot_x_end.saturating_sub(total_char_width));
        }

        let mut cur_col = start_col;
        for (ch, is_backspace) in &cluster.items {
            let char_w = if ch.is_ascii() { 1 } else { 2 };
            if cur_col + char_w > plot_x_end {
                break;
            }

            // 1. 在曲线上绘制打错点标记（红点/黄点），位于字符正下方
            let dot_col = cur_col + (char_w.saturating_sub(1) / 2);
            let dot_style = Style::default()
                .fg(if *is_backspace {
                    palette.warning
                } else {
                    palette.error
                })
                .bg(palette.bg)
                .bold();
            buf.set_string(dot_col, dot_row, "•", dot_style);

            // 2. 在红点上方绘制具体错字字符
            let char_style = if *is_backspace {
                Style::default()
                    .fg(palette.warning)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default()
                    .fg(palette.error)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            };
            buf.set_string(cur_col, char_row, ch.to_string(), char_style);

            cur_col += char_w;
        }

        prev_cluster_end_col = cur_col;
        prev_char_row = char_row;
    }
}

/// 成绩视图里的上传状态行（纯函数，供渲染与测试）。
///
/// `input_method` 为上传所用的输入法名（设置项「上传名称」），仅在线上传成功时展示。
fn upload_lines(upload: &UploadState, theme: Theme, input_method: &str) -> Vec<Line<'static>> {
    match upload {
        UploadState::NotApplicable { copied_stats } => match copied_stats {
            None => vec![],
            // 顶部摘要已完整展示各项指标（含 emoji），整段分享文本仅复制到剪贴板，
            // 此处不再重复展示，只保留「已复制」提示。
            Some(_) => vec![Line::from(" 已复制到剪贴板").fg(color(theme.muted))],
        },
        UploadState::Uploading => vec![
            Line::from(""),
            Line::from(" 成绩上传中…").fg(color(theme.warn)),
        ],
        UploadState::Success { ranking } => {
            let mut lines = vec![Line::from("")];
            match ranking {
                Some(r) => {
                    lines.push(
                        Line::from(format!(" 排名: 第{r}名 · 已上传")).fg(color(theme.accent)),
                    );
                }
                None => lines.push(Line::from(" 已上传").fg(color(theme.accent))),
            }
            // 上传名称是成绩提交到 52dazi 的身份标识，顶部摘要里没有，
            // 不随整段分享文本一起删掉，单独保留一行。
            if !input_method.is_empty() {
                lines
                    .push(Line::from(format!(" 上传名称: {input_method}")).fg(color(theme.accent)));
            }
            // 与离线赛文口径一致：顶部摘要已完整展示各项指标（含 emoji），
            // 整段分享文本仅复制到剪贴板，此处不再重复展示。
            lines.push(Line::from(" 已复制到剪贴板").fg(color(theme.muted)));
            lines
        }
        UploadState::Failed {
            message,
            need_relogin,
            detail,
            copied_stats,
        } => {
            let mut lines = vec![
                Line::from(""),
                Line::from(format!(" 上传失败: {message}")).fg(color(theme.wrong)),
            ];
            if let Some(d) = detail {
                lines.push(Line::from(format!(" 原始错误: {d}")).fg(color(theme.muted)));
            }
            if *need_relogin {
                lines.push(
                    Line::from(" 请按 Ctrl-O 重新登录（登录后自动重试上传）").fg(color(theme.warn)),
                );
            }
            if copied_stats.is_some() {
                // 同上：整段统计文本仅复制到剪贴板，不再重复展示。
                lines.push(Line::from(" 已复制到剪贴板").fg(color(theme.muted)));
            }
            lines
        }
    }
}

/// 单字赛文当前页的起始字符索引：基于已全对完成的组数。
fn builtin_page_start(session: &Session) -> usize {
    session.completed_groups() * session.group_size()
}

/// 对照区：将当前页指定数量词的原文按跟打状态着色，词间插入空格 span（不可打）。
///
/// `cell_widths` 为遍码提示开启时的词格列宽（`max(词宽, 提示码宽)`，按页内词条序）；
/// 为 `None` 时按纯词宽渲染。提示码宽于词时正文补尾随空格让位，保证提示行逐词对齐。
fn build_word_spans(
    session: &Session,
    word_boundaries: &[(usize, usize)],
    page_start_word: usize,
    page_end_word: usize,
    theme: Theme,
    bold: bool,
    cell_widths: Option<&[usize]>,
) -> Vec<Span<'static>> {
    let statuses = session.original_status();
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (word_i, &(ws, we)) in word_boundaries
        .iter()
        .enumerate()
        .skip(page_start_word)
        .take(page_end_word - page_start_word)
    {
        if word_i > page_start_word {
            // 词间空格（不可打的分隔符，用默认色）
            spans.push(Span::raw(" "));
        }
        let mut cell_width = 0usize;
        for &(c, status) in &statuses[ws..we] {
            let style = match status {
                Some(CharStatus::Correct) => Style::default().fg(color(theme.correct)),
                Some(CharStatus::Wrong) => Style::default().fg(color(theme.wrong)),
                None => Style::default(),
            };
            cell_width += c.width().unwrap_or(1);
            spans.push(Span::styled(
                c.to_string(),
                style.add_modifier(bold_modifier(bold)),
            ));
        }
        // 提示码宽于词 → 补尾随空格把词格撑到列宽，与上方提示行锁步。
        if let Some(extra) = cell_widths
            .and_then(|w| w.get(word_i - page_start_word))
            .and_then(|&cw| cw.checked_sub(cell_width))
            .filter(|&n| n > 0)
        {
            spans.push(Span::raw(" ".repeat(extra)));
        }
    }
    spans
}

/// 内置词组赛文开启遍码提示时，根据当前页词条与已载入方案反查，生成「提示行」。
///
/// 仅对内置词组赛文生效；未开启、非词组赛文或未配置方案时返回 `None`。
/// 提示行与正文行（同样以单空格分词、按词可视列宽对齐）逐词对齐。
fn code_hint_overlay_line(
    session: &Session,
    text: &Text,
    scheme_dict: Option<&SchemeDict>,
    theme: Theme,
) -> Option<Line<'static>> {
    let set = match text.source {
        TextSource::Builtin { set } => set,
        _ => return None,
    };
    if !set.is_words() {
        return None;
    }
    let dict = scheme_dict?;
    let boundaries: Vec<(usize, usize)> = match &text.word_boundaries {
        Some(b) if !b.is_empty() => b.clone(),
        _ => set.word_boundaries(),
    };
    let page_start = builtin_page_start(session);
    let page_end = (page_start + session.group_size()).min(boundaries.len());
    if page_start >= boundaries.len() {
        return None;
    }
    let statuses = session.original_status();
    let mut words: Vec<String> = Vec::new();
    let mut typed_mask: Vec<bool> = Vec::new();
    for &(ws, we) in boundaries[page_start..page_end].iter() {
        let word: String = statuses[ws..we].iter().map(|(c, _)| *c).collect();
        // T04：该词所有字符均已正确上屏 → 隐藏其上方提示（回改后随状态重现）。
        let all_correct = (ws..we).all(|i| {
            statuses
                .get(i)
                .is_some_and(|(_, s)| *s == Some(CharStatus::Correct))
        });
        words.push(word);
        typed_mask.push(all_correct);
    }
    let hints: Vec<CodeHint> = dict.build_code_hints(&words);
    let cells = layout_code_hint_line(&words, &hints, &typed_mask);
    Some(code_hint_line_from_cells(&cells, theme))
}

/// 短语感知合并（修复「经典造型」类自定义短语不显示整词码的根因）。
///
/// jieba 分词不认识用户词典里的自定义短语，会把「经典造型」拆成「经典」「造型」，
/// 导致提示显示逐词码 `RvFX mbfp` 而非整词码 `RFmf`。本函数在分词结果之上做最长匹配：
/// 若相邻若干词拼接后正好命中方案词典中的整词码，则合并为一个提示单元并反查整词码。
/// 仅当拼接命中时才合并；否则保持原分词不变，行为向后兼容。
///
/// 同时合并 `word_ranges` 与 `typed_mask`，使提示行、正文行的索引与合并后的词单元锁步。
fn merge_phrase_hints(
    words: &[String],
    typed_mask: &[bool],
    word_ranges: &[(usize, usize)],
    dict: &SchemeDict,
) -> (Vec<String>, Vec<bool>, Vec<(usize, usize)>) {
    let n = words.len();
    let mut out_w: Vec<String> = Vec::new();
    let mut out_m: Vec<bool> = Vec::new();
    let mut out_r: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        // 从最长拼接（覆盖余下全部词）向短回溯，命中词典整词码即采用。
        let mut best_k = 1usize;
        let mut merged = words[i].clone();
        let mut merged_range = word_ranges[i];
        for k in 2..=n - i {
            let cand: String = words[i..i + k].concat();
            if dict.get_primary_code(&cand).is_some() {
                merged = cand;
                best_k = k;
                merged_range = (word_ranges[i].0, word_ranges[i + k - 1].1);
            }
        }
        out_w.push(merged);
        // 合并单元的「已正确上屏」= 所有组成词均正确。
        out_m.push((i..i + best_k).all(|j| typed_mask.get(j).copied().unwrap_or(false)));
        out_r.push(merged_range);
        i += best_k;
    }
    (out_w, out_m, out_r)
}

/// 内置词组赛文开启遍码提示时，当前页词条的词格列宽（`max(词宽, 提示码宽)`）。
///
/// 提示码宽于词（如「腕间」4 列 vs 码 `HjYIw` 5 列）时词格需撑宽，对照区正文行与
/// 跟打区行共用本列宽补空格，两区词列才不会错位、提示行也才能逐词对齐。
/// 非词组赛文、无词典或当前页已越界时返回 `None`（退化为纯词宽，与关闭提示时一致）。
fn builtin_words_cell_widths(
    session: &Session,
    text: &Text,
    scheme_dict: Option<&SchemeDict>,
) -> Option<Vec<usize>> {
    let set = match text.source {
        TextSource::Builtin { set } => set,
        _ => return None,
    };
    if !set.is_words() {
        return None;
    }
    let dict = scheme_dict?;
    let owned_boundaries;
    let boundaries: &[(usize, usize)] = match &text.word_boundaries {
        Some(b) if !b.is_empty() => b,
        _ => {
            owned_boundaries = set.word_boundaries();
            &owned_boundaries
        }
    };
    let page_start_word = builtin_page_start(session);
    let page_end_word = (page_start_word + session.group_size()).min(boundaries.len());
    if page_start_word >= boundaries.len() {
        return None;
    }
    let statuses = session.original_status();
    let words: Vec<String> = boundaries[page_start_word..page_end_word]
        .iter()
        .map(|&(ws, we)| statuses[ws..we].iter().map(|(c, _)| *c).collect())
        .collect();
    let hints = dict.build_code_hints(&words);
    Some(hint_cell_widths(&words, &hints))
}

/// 将提示单元（已去皮手区前缀、携手区归属）拼为带色 `Line`：
/// 左手粉、右手黄，其余（双手并击/已打/未登录）用 muted。
fn code_hint_line_from_cells(cells: &[HintCell], theme: Theme) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(cells.len() * 2);
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            cell.text.clone(),
            code_hint_hand_style(cell.hand, theme),
        ));
    }
    Line::from(spans)
}

/// 遍码提示单元的手区配色：左手粉、右手黄、双手并击青（均取自当前主题，主题自适应）；
/// 其余（已打/未登录占位）用 muted。
fn code_hint_hand_style(hand: HintHand, theme: Theme) -> Style {
    let c = match hand {
        HintHand::Left => color(theme.hand_left),   // 粉
        HintHand::Right => color(theme.hand_right), // 黄
        HintHand::TwoHand => color(theme.hand_two), // 青（双手并击/全码）
        _ => color(theme.muted),
    };
    Style::default().fg(c)
}

/// 非内置长文（离线/自由/剪贴板/在线）开启遍码提示时，按词边界锁步折行渲染「双行词格」。
///
/// 以 `WordIndex` 词边界为最小换行单元打包：提示行（上方反查编码，已全对上屏词留空占位）
/// 与正文行（下方按跟打状态着色）折行点完全一致、永不错位。每词单元间以单空格分隔，
/// 使提示行与正文行列结构等同，从而逐词对齐。
///
/// 仅对非内置长文生效；内置词组赛文仍走 `code_hint_overlay_line` 单页路径。
/// 未配置方案（`dict` 为 `None`）或内置词组赛文或文本无词边界时返回 `None`，交由上层回退到普通渲染。
fn code_hint_grid_text(
    session: &Session,
    text: &Text,
    dict: Option<&SchemeDict>,
    theme: Theme,
    bold: bool,
    max_width: usize,
) -> Option<TextLines<'static>> {
    // 内置词组赛文由 code_hint_overlay_line 单页路径处理，不走双行词格。
    if matches!(text.source, TextSource::Builtin { .. }) {
        return None;
    }
    let dict = dict?;
    let index = text.build_word_index();
    let boundaries = index.word_boundaries();
    if boundaries.is_empty() {
        return None;
    }
    let content: Vec<char> = text.content.chars().collect();
    // 过滤空白单元（空格/换行仅作分隔），避免提示格里出现空白占位单元与双倍间距。
    let units: Vec<(usize, usize)> = boundaries
        .into_iter()
        .filter(|&(s, e)| {
            let e = e.min(content.len());
            s < e && !content[s..e].iter().all(|c| c.is_whitespace())
        })
        .collect();
    if units.is_empty() {
        return None;
    }

    let statuses = session.original_status();
    let mut words: Vec<String> = Vec::new();
    let mut word_ranges: Vec<(usize, usize)> = Vec::new();
    let mut typed_mask: Vec<bool> = Vec::new();
    for &(ws, we) in &units {
        let we = we.min(content.len());
        if ws >= we {
            continue;
        }
        let word: String = content[ws..we].iter().collect();
        let all_correct = (ws..we).all(|i| {
            statuses
                .get(i)
                .is_some_and(|(_, s)| *s == Some(CharStatus::Correct))
        });
        words.push(word);
        word_ranges.push((ws, we));
        typed_mask.push(all_correct);
    }
    if words.is_empty() {
        return None;
    }

    // 短语感知合并：相邻分词单元若拼接命中词典整词码（如自定义短语「经典造型」），
    // 合并为整词提示单元，反查显示整词码而非逐词码（issue 根因修复）。
    let (words, typed_mask, word_ranges) =
        merge_phrase_hints(&words, &typed_mask, &word_ranges, dict);

    let hints: Vec<CodeHint> = dict.build_code_hints(&words);
    // 词格列宽 = max(词宽, 提示码宽)：提示码宽于词时不截断提示，而由正文补空格让位。
    let widths: Vec<usize> = hint_cell_widths(&words, &hints);
    let rows = pack_words_by_width(&widths, max_width);

    let mut text_lines = TextLines::default();
    for row in rows {
        // 提示行：本行词单元的编码（按词宽居中/留空），muted 色。
        let row_words: Vec<String> = row.iter().map(|&i| words[i].clone()).collect();
        let row_hints: Vec<CodeHint> = row.iter().map(|&i| hints[i].clone()).collect();
        let row_typed: Vec<bool> = row.iter().map(|&i| typed_mask[i]).collect();
        let cells = layout_code_hint_line(&row_words, &row_hints, &row_typed);
        text_lines.push_line(code_hint_line_from_cells(&cells, theme));

        // 正文行：本行词单元按跟打状态着色（每词间单空格分隔，与提示行锁步）。
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (k, &wi) in row.iter().enumerate() {
            if k > 0 {
                spans.push(Span::raw(" "));
            }
            let (ws, we) = word_ranges[wi];
            let mut cell_width = 0usize;
            for ci in ws..we {
                if let Some((c, status)) = statuses.get(ci) {
                    let style = match status {
                        Some(CharStatus::Correct) => Style::default().fg(color(theme.correct)),
                        Some(CharStatus::Wrong) => Style::default().fg(color(theme.wrong)),
                        None => Style::default(),
                    };
                    cell_width += c.width().unwrap_or(1);
                    spans.push(Span::styled(
                        c.to_string(),
                        style.add_modifier(bold_modifier(bold)),
                    ));
                }
            }
            // 提示码宽于词 → 补尾随空格到词格宽，与上方提示行锁步。
            if let Some(extra) = widths[wi].checked_sub(cell_width).filter(|&n| n > 0) {
                spans.push(Span::raw(" ".repeat(extra)));
            }
        }
        text_lines.push_line(Line::from(spans));
    }
    Some(text_lines)
}

/// 当前方案是否具备可用词典（用于遍码提示）：须已载入且含至少一个词条。
///
/// `None`（未配置方案）或仅 `.schema.yaml` 规则而无 `.dict.yaml` 词条（`entry_count()==0`）
/// 均视为无可用词典，此时应显示占位提示而非空白/崩溃。
fn code_hint_dict_usable(dict: Option<&SchemeDict>) -> bool {
    dict.is_some_and(|d| d.entry_count() > 0)
}

/// 遍码提示开启但方案无可用词典时，对照区顶部显示的占位提示（引导用户载入词典）。
fn code_hint_placeholder_line(theme: Theme) -> Line<'static> {
    Line::from("遍码提示：未配置方案词典（请在设置中载入含词条的 .dict.yaml）")
        .fg(color(theme.muted))
}

/// 跟打区：将当前页指定数量词的已打字符按对/错着色，词间插入空格 span。
///
/// `cell_widths` 与 `build_word_spans` 同源：对照区正文行被提示码撑宽时，跟打区须同步补空格，
/// 否则两区词列错位。为 `None` 时按纯词宽渲染。
fn build_word_type_spans(
    display: &[(char, CharStatus)],
    word_boundaries: &[(usize, usize)],
    page_start_word: usize,
    page_end_word: usize,
    theme: Theme,
    bold: bool,
    cell_widths: Option<&[usize]>,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (word_i, &(ws, we)) in word_boundaries
        .iter()
        .enumerate()
        .skip(page_start_word)
        .take(page_end_word - page_start_word)
    {
        if display.len() <= ws {
            break;
        }
        if word_i > page_start_word {
            spans.push(Span::raw(" "));
        }
        let mut cell_width = 0usize;
        for ci in ws..we {
            if ci < display.len() {
                let (c, status) = display[ci];
                let style = match status {
                    CharStatus::Correct => Style::default().fg(color(theme.correct)),
                    CharStatus::Wrong => Style::default().fg(color(theme.wrong)),
                };
                cell_width += c.width().unwrap_or(1);
                spans.push(Span::styled(
                    c.to_string(),
                    style.add_modifier(bold_modifier(bold)),
                ));
            }
        }
        // 与对照区同源撑宽：未打完时补尾随空格保持两区词列一致。
        if let Some(extra) = cell_widths
            .and_then(|w| w.get(word_i - page_start_word))
            .and_then(|&cw| cw.checked_sub(cell_width))
            .filter(|&n| n > 0)
        {
            spans.push(Span::raw(" ".repeat(extra)));
        }
    }
    spans
}

/// 将对照区的字符按跟打状态着色：已打对=correct、已打错=wrong、未打到=默认。
///
/// 内置赛文只显示当前页：单字赛文每页 group_size 字、词组赛文每页 group_size 个词（词间加空格、去逗号）；
/// 打完当前页自动翻页；其余来源显示全文（由终端宽度自动折行）。
///
/// `cell_widths` 为遍码提示开启时内置词组赛文当前页的词格列宽（见 `build_word_spans`）；
/// 其余场景（单字赛文、非内置、提示关闭）传 `None`，按纯词宽渲染。
fn original_line(
    session: &Session,
    text: &Text,
    theme: Theme,
    bold: bool,
    cell_widths: Option<&[usize]>,
) -> TextLines<'static> {
    let source = text.source;
    let group_size = session.group_size();
    if let TextSource::Builtin { set } = source {
        if set.is_words() {
            let owned_boundaries;
            let boundaries: &[(usize, usize)] = match &text.word_boundaries {
                Some(b) if !b.is_empty() => b,
                _ => {
                    owned_boundaries = set.word_boundaries();
                    &owned_boundaries
                }
            };
            let page_start_word = builtin_page_start(session);
            let page_end_word = (page_start_word + group_size).min(boundaries.len());
            if page_start_word >= boundaries.len() {
                return TextLines::default();
            }
            let spans = build_word_spans(
                session,
                boundaries,
                page_start_word,
                page_end_word,
                theme,
                bold,
                cell_widths,
            );
            let mut text_lines = TextLines::default();
            text_lines.push_line(Line::from(spans));
            return text_lines;
        }
        // 单字赛文：每页 group_size 字
        let start = builtin_page_start(session);
        let statuses: Vec<_> = session
            .original_status()
            .into_iter()
            .skip(start)
            .take(group_size)
            .collect();
        let spans: Vec<Span<'static>> = statuses
            .into_iter()
            .map(|(c, status)| {
                let style = match status {
                    Some(CharStatus::Correct) => Style::default().fg(color(theme.correct)),
                    Some(CharStatus::Wrong) => Style::default().fg(color(theme.wrong)),
                    None => Style::default(),
                };
                Span::styled(c.to_string(), style.add_modifier(bold_modifier(bold)))
            })
            .collect();
        return group_spans(spans, source, group_size);
    }
    // 非内置赛文：全文单行
    let spans: Vec<Span<'static>> = session
        .original_status()
        .into_iter()
        .map(|(c, status)| {
            let style = match status {
                Some(CharStatus::Correct) => Style::default().fg(color(theme.correct)),
                Some(CharStatus::Wrong) => Style::default().fg(color(theme.wrong)),
                None => Style::default(),
            };
            Span::styled(c.to_string(), style.add_modifier(bold_modifier(bold)))
        })
        .collect();
    group_spans(spans, source, group_size)
}

/// 将跟打区的字符按对/错渲染为 correct/wrong。
///
/// 内置赛文只显示当前页：单字赛文每页 group_size 字、词组赛文每页 group_size 个词（词间加空格、去逗号）；
/// 打完当前页自动翻页；其余来源显示全文（由终端宽度自动折行）。
///
/// `cell_widths` 与 `original_line` 同源：内置词组赛文开启遍码提示时，跟打区须与对照区
/// 共用同一套词格列宽，否则两区词列错位。其余场景传 `None`。
fn type_line(
    session: &Session,
    text: &Text,
    theme: Theme,
    bold: bool,
    cell_widths: Option<&[usize]>,
) -> TextLines<'static> {
    let source = text.source;
    let group_size = session.group_size();
    if let TextSource::Builtin { set } = source {
        if set.is_words() {
            let owned_boundaries;
            let boundaries: &[(usize, usize)] = match &text.word_boundaries {
                Some(b) if !b.is_empty() => b,
                _ => {
                    owned_boundaries = set.word_boundaries();
                    &owned_boundaries
                }
            };
            let display = session.display();
            if display.is_empty() {
                return TextLines::from(
                    Line::from("（跟打区 — 输入法上屏文字将显示在这里）").fg(color(theme.muted)),
                );
            }
            let page_start_word = builtin_page_start(session);
            let page_end_word = (page_start_word + group_size).min(boundaries.len());
            if page_start_word >= boundaries.len() {
                return TextLines::default();
            }
            let spans = build_word_type_spans(
                &display,
                boundaries,
                page_start_word,
                page_end_word,
                theme,
                bold,
                cell_widths,
            );
            // 当前页无已打字符时（仅有词间空格 span），显示提示行。
            if spans.is_empty() || spans.iter().all(|s| s.content == " ") {
                return TextLines::from(
                    Line::from("（跟打区 — 输入法上屏文字将显示在这里）").fg(color(theme.muted)),
                );
            }
            let mut text_lines = TextLines::default();
            text_lines.push_line(Line::from(spans));
            return text_lines;
        }
        // 单字赛文：每页 group_size 字
        let start = builtin_page_start(session);
        let display: Vec<_> = session
            .display()
            .into_iter()
            .skip(start)
            .take(group_size)
            .collect();
        if display.is_empty() {
            return TextLines::from(
                Line::from("（跟打区 — 输入法上屏文字将显示在这里）").fg(color(theme.muted)),
            );
        }
        let spans: Vec<Span<'static>> = display
            .into_iter()
            .map(|(c, status)| {
                let style = match status {
                    CharStatus::Correct => Style::default().fg(color(theme.correct)),
                    CharStatus::Wrong => Style::default().fg(color(theme.wrong)),
                };
                Span::styled(c.to_string(), style.add_modifier(bold_modifier(bold)))
            })
            .collect();
        return group_spans(spans, source, group_size);
    }
    // 非内置赛文：全文单行
    let display = session.display();
    if display.is_empty() {
        return TextLines::from(
            Line::from("（跟打区 — 输入法上屏文字将显示在这里）").fg(color(theme.muted)),
        );
    }
    let spans: Vec<Span<'static>> = display
        .into_iter()
        .map(|(c, status)| {
            let style = match status {
                CharStatus::Correct => Style::default().fg(color(theme.correct)),
                CharStatus::Wrong => Style::default().fg(color(theme.wrong)),
            };
            Span::styled(c.to_string(), style.add_modifier(bold_modifier(bold)))
        })
        .collect();
    group_spans(spans, source, group_size)
}

/// 把已着色的 span 序列按赛文来源组织成多行文本：单字内置赛文每页 group_size 字一行，其余为单行。
/// 词组赛文已在调用方按页组装，不走此函数。
fn group_spans(
    spans: Vec<Span<'static>>,
    source: TextSource,
    group_size: usize,
) -> TextLines<'static> {
    let mut text = TextLines::default();
    if matches!(source, TextSource::Builtin { set } if !set.is_words()) {
        for chunk in spans.chunks(group_size) {
            text.push_line(Line::from(chunk.to_vec()));
        }
    } else {
        text.push_line(Line::from(spans));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use dazitui_core::{TextSource, ThemePreset};
    use std::fs;

    fn temp_dir(suffix: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dazitui-tui-{stamp}-{suffix}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 临时 token 存储：隔离本机 `~/.config/dazitui/token`（避免测试依赖真实 token 文件）。
    fn temp_token_store() -> TokenStore {
        let dir = temp_dir("token");
        TokenStore::new(dir.join("token"))
    }

    /// 临时设置存储：隔离本机 `~/.config/dazitui/settings`。
    fn temp_settings_store() -> SettingsStore {
        let dir = temp_dir("settings");
        SettingsStore::new(dir.join("settings"))
    }

    /// 测试用 App：临时 token/设置存储 + 不可达 API（无 token 文件时不发网络请求）。
    fn test_app(text: Text) -> App {
        let store = temp_token_store();
        App::new_with(
            text,
            store.clone(),
            ApiClient::with_base_url_and_store("http://127.0.0.1:1", Some(store)),
            temp_settings_store(),
            None,
        )
    }

    /// 起本地 mock HTTP 服务器：按请求路径返回固定响应（按 `responses` 长度 accept 次数）。
    /// 返回 `(端口, 线程句柄)`，`responses` 为 `(请求路径, 响应体)` 列表。
    fn mock_server(responses: &[(&str, &str)]) -> (u16, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let responses: Vec<(String, String)> = responses
            .iter()
            .map(|(p, b)| (p.to_string(), b.to_string()))
            .collect();
        let n = responses.len();
        let handle = std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Read, Write};
            for _ in 0..n {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                let _ = reader.read_line(&mut request_line);
                let path = request_line.split_whitespace().nth(1).unwrap_or("");
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" || line == "\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body_buf = vec![0u8; content_length];
                let _ = reader.read_exact(&mut body_buf);
                let body = responses
                    .iter()
                    .find(|(p, _)| p == path)
                    .map(|(_, b)| b.clone())
                    .unwrap_or_else(|| r#"{"error":1,"msg":"unexpected path"}"#.to_string());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (port, handle)
    }

    #[test]
    fn ctrl_q_quits() {
        assert!(is_quit(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL
        )));
        assert!(is_quit(KeyEvent::new(
            KeyCode::Char('Q'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn plain_q_does_not_quit() {
        assert!(!is_quit(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE
        )));
        assert!(!is_quit(KeyEvent::new(
            KeyCode::Char('Q'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn ctrl_c_quits() {
        assert!(is_quit(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn other_keys_do_not_quit() {
        assert!(!is_quit(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE
        )));
        assert!(!is_quit(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    }

    #[test]
    fn d_early_finishes() {
        assert!(is_early_finish(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::NONE
        )));
        assert!(is_early_finish(KeyEvent::new(
            KeyCode::Char('D'),
            KeyModifiers::NONE
        )));
        assert!(!is_early_finish(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::NONE
        )));
        assert!(!is_early_finish(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn r_restarts() {
        assert!(is_restart(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::NONE
        )));
        assert!(is_restart(KeyEvent::new(
            KeyCode::Char('R'),
            KeyModifiers::NONE
        )));
        assert!(!is_restart(KeyEvent::new(
            KeyCode::Char('t'),
            KeyModifiers::NONE
        )));
        assert!(!is_restart(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn restart_allowed_only_when_offline() {
        let r_key = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        // 离线赛文：r 允许重打。
        assert!(restart_allowed(r_key, false));
        // 在线赛文：r 被禁用。
        assert!(!restart_allowed(r_key, true));
        // 非重打键：无论在线与否都不触发。
        let other = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(!restart_allowed(other, false));
        assert!(!restart_allowed(other, true));
    }

    #[test]
    fn hint_text_hides_restart_when_online() {
        // 离线跟打：显示重打提示。
        assert!(hint_text(false, false, false, false, false).contains("重打"));
        // 在线跟打：不显示重打提示。
        assert!(!hint_text(false, false, true, false, false).contains("重打"));
        // 浏览态（载文选择）与来源无关，也不显示重打。
        assert!(!hint_text(true, false, false, false, false).contains("重打"));
        assert!(!hint_text(true, false, true, false, false).contains("重打"));
        // 内置赛文浏览态：不显示重打。
        assert!(!hint_text(false, true, false, false, false).contains("重打"));
        // 暂停态
        assert!(hint_text(false, false, false, true, false).contains("恢复跟打"));
        // 就绪态
        assert!(hint_text(false, false, false, false, true).contains("菜单导航"));
    }

    #[test]
    fn b_opens_builtin_browser() {
        assert!(is_open_builtin_browser(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::NONE
        )));
        assert!(is_open_builtin_browser(KeyEvent::new(
            KeyCode::Char('B'),
            KeyModifiers::NONE
        )));
        assert!(!is_open_builtin_browser(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE
        )));
        assert!(!is_open_builtin_browser(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn i_opens_free_input() {
        assert!(is_open_free_input(KeyEvent::new(
            KeyCode::Char('i'),
            KeyModifiers::NONE
        )));
        assert!(is_open_free_input(KeyEvent::new(
            KeyCode::Char('I'),
            KeyModifiers::NONE
        )));
        assert!(!is_open_free_input(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::NONE
        )));
        assert!(!is_open_free_input(KeyEvent::new(
            KeyCode::Char('i'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn p_loads_clipboard() {
        assert!(is_load_clipboard(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::NONE
        )));
        assert!(is_load_clipboard(KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::NONE
        )));
        assert!(!is_load_clipboard(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE
        )));
        assert!(!is_load_clipboard(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn s_opens_stats() {
        assert!(is_open_stats(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::NONE
        )));
        assert!(is_open_stats(KeyEvent::new(
            KeyCode::Char('S'),
            KeyModifiers::NONE
        )));
        assert!(!is_open_stats(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE
        )));
        assert!(!is_open_stats(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn no_arg_startup_loads_default_builtin() {
        // 默认载入首套内置赛文（常用单字前五百）：内容非空、来源为 Builtin、可重打。
        let text = load_builtin_text(BUILTIN_SETS[0]);
        assert!(!text.content.is_empty());
        assert!(matches!(text.source, TextSource::Builtin { .. }));
        assert!(!text.is_online());
        assert_eq!(text.title, "常用单字前五百");
    }

    #[test]
    fn builtin_sets_in_order() {
        assert_eq!(BUILTIN_SETS.len(), 7);
        assert_eq!(BUILTIN_SETS[0].name(), "常用单字前五百");
        assert_eq!(BUILTIN_SETS[1].name(), "常用单字中五百");
        assert_eq!(BUILTIN_SETS[2].name(), "常用单字后五百");
        assert_eq!(BUILTIN_SETS[3].name(), "常用词组前五百");
        assert_eq!(BUILTIN_SETS[4].name(), "常用词组中五百");
        assert_eq!(BUILTIN_SETS[5].name(), "常用词组后五百");
        assert_eq!(BUILTIN_SETS[6].name(), "yoyo 单字");
    }

    #[test]
    fn builtin_content_has_no_newlines() {
        // include_str! 后去换行：内容应为纯单字串，无换行。
        for set in BUILTIN_SETS {
            let text = load_builtin_text(set);
            assert!(
                !text.content.contains('\n') && !text.content.contains('\r'),
                "{} 含换行",
                set.name()
            );
        }
    }

    #[test]
    fn yoyo_chars_set_is_large_and_deduped() {
        let set = BUILTIN_SETS[6];
        assert_eq!(set.name(), "yoyo 单字");
        let text = load_builtin_text(set);
        let chars: Vec<char> = text.content.chars().collect();
        assert!(
            chars.len() > 6000,
            "yoyo 单字应约 6640 字，实际 {}",
            chars.len()
        );
        // 内容应无重复单字（即社区常说的「6636 单字无重」）。
        let mut sorted = chars.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), chars.len(), "yoyo 单字应无重复");
    }

    #[test]
    fn builtin_progress_persists_and_resume_prompt_appears() {
        let set = BUILTIN_SETS[6]; // yoyo 单字
        let text = load_builtin_text(set);
        let mut app = test_app(text);

        // 首次直接进入第 0 组跟打
        app.start_builtin_set(set, 0);
        assert!(matches!(app.state, AppState::Typing));

        // 打完前两组（每组 group_size 字），进度应增量落盘
        let gs = app.settings.group_size as usize;
        let chars: Vec<char> = app.text.content.chars().collect();
        let g1: String = chars[..gs].iter().collect();
        let g2: String = chars[gs..2 * gs].iter().collect();
        app.session.type_text(&g1);
        assert_eq!(app.session.completed_groups(), 1);
        app.session.type_text(&g2);
        assert_eq!(app.session.completed_groups(), 2);
        app.persist_builtin_progress_if_changed();

        // 存档中应记录已完成 2 组
        let prog = app.builtin_progress_for(set).expect("应已存档进度");
        assert_eq!(prog.completed_groups, 2);

        // 再次打开该内置赛文：应弹出续打选择而非直接开打
        app.resume_prompt = None;
        app.builtin_selection = 6;
        app.load_selected_builtin();
        assert!(app.resume_prompt.is_some(), "有存档进度时应弹出续打选择");

        // 选「继续」等价于从已完成组数续打：会话应被种到对应组
        app.start_builtin_set(set, prog.completed_groups as usize);
        assert_eq!(app.session.completed_groups(), 2);

        // 整本打完应记为全部完成（completed_groups == total）
        app.session.set_completed_groups(app.session.total_groups());
        app.save_builtin_progress(app.session.total_groups());
        let done = app.builtin_progress_for(set).unwrap();
        assert_eq!(done.completed_groups as usize, app.session.total_groups());
    }

    #[test]
    fn resume_prompt_popup_renders_in_separate_modal() {
        let set = BUILTIN_SETS[6]; // yoyo 单字
        let mut app = test_app(load_builtin_text(set));
        // 制造进度：打完前两组
        app.start_builtin_set(set, 0);
        let gs = app.settings.group_size as usize;
        let chars: Vec<char> = app.text.content.chars().collect();
        let g1: String = chars[..gs].iter().collect();
        let g2: String = chars[gs..2 * gs].iter().collect();
        app.session.type_text(&g1);
        app.session.type_text(&g2);
        app.persist_builtin_progress_if_changed();
        // 触发续打弹窗
        app.builtin_selection = 6;
        app.load_selected_builtin();
        assert!(app.resume_prompt.is_some(), "有存档时应弹出续打选择");

        // 弹窗应为独立模态层：含标题、赛文名、进度、按键提示（侧边栏不再内联展示）
        let buf = render_buffer_text(&app, 100, 30);
        assert!(buf.contains("续打进度"), "应渲染独立的续打弹窗标题");
        assert!(buf.contains("yoyo单字"), "应显示赛文名");
        assert!(buf.contains("已完成"), "应显示进度文案");
        assert!(buf.contains("继续"), "应提供继续选项");
        assert!(buf.contains("重置"), "应提供重置进度选项");
    }

    #[test]
    fn builtin_sets_have_no_overlap_and_unique() {
        // 单字三套各 500 字，互不重复。用 load_builtin_text（已去换行）取内容。
        let cq: Vec<char> = load_builtin_text(BUILTIN_SETS[0]).content.chars().collect();
        let cz: Vec<char> = load_builtin_text(BUILTIN_SETS[1]).content.chars().collect();
        let ch: Vec<char> = load_builtin_text(BUILTIN_SETS[2]).content.chars().collect();
        assert_eq!(cq.len(), 500);
        assert_eq!(cz.len(), 500);
        assert_eq!(ch.len(), 500);
        let cq_set: std::collections::HashSet<char> = cq.iter().copied().collect();
        let cz_set: std::collections::HashSet<char> = cz.iter().copied().collect();
        let ch_set: std::collections::HashSet<char> = ch.iter().copied().collect();
        assert_eq!(cq_set.len(), 500, "单字前五百有重复");
        assert_eq!(cz_set.len(), 500, "单字中五百有重复");
        assert_eq!(ch_set.len(), 500, "单字后五百有重复");
        assert!(cq_set.is_disjoint(&cz_set), "单字前/中五百有重叠");
        assert!(cq_set.is_disjoint(&ch_set), "单字前/后五百有重叠");
        assert!(cz_set.is_disjoint(&ch_set), "单字中/后五百有重叠");

        // 词组三套：内容非空，互不相同（词组以二字词为主，字符会复现，不做字符级唯一性检查）。
        let wq = load_builtin_text(BUILTIN_SETS[3]).content;
        let wz = load_builtin_text(BUILTIN_SETS[4]).content;
        let wh = load_builtin_text(BUILTIN_SETS[5]).content;
        assert!(!wq.is_empty() && !wz.is_empty() && !wh.is_empty());
        assert_ne!(wq, wz);
        assert_ne!(wq, wh);
        assert_ne!(wz, wh);
    }

    #[test]
    fn load_selected_builtin_replaces_text_and_starts_typing() {
        let mut app = test_app(Text {
            title: "old".into(),
            content: "旧赛文".into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        });
        // 选中第二套（中五百）。
        app.open_builtin_browser();
        app.builtin_selection = 1;
        app.load_selected_builtin();
        assert_eq!(app.text.title, "常用单字中五百");
        assert!(matches!(app.text.source, TextSource::Builtin { .. }));
        assert!(!app.text.is_online());
        assert!(matches!(app.state, AppState::Countdown { .. }));
        assert_eq!(app.session.len(), 0);
    }

    #[test]
    fn s_key_toggles_builtin_shuffle() {
        let mut app = test_app(file_text("旧赛文"));
        app.open_builtin_browser();
        assert!(!app.builtin_shuffle, "初始乱序开关应为关");
        // 模拟 s 键：切换乱序开关并刷新预览（事件循环中的处理逻辑）。
        app.builtin_shuffle = !app.builtin_shuffle;
        app.refresh_builtin_preview();
        assert!(app.builtin_shuffle, "s 键后乱序开关应为开");
        let (title, _) = app.builtin_preview.as_ref().unwrap();
        assert!(title.contains("乱序"), "乱序开时预览标题应含「（乱序）」");
        // 再按一次 s：关闭乱序。
        app.builtin_shuffle = !app.builtin_shuffle;
        app.refresh_builtin_preview();
        assert!(!app.builtin_shuffle, "再按 s 后乱序开关应为关");
        let (title, _) = app.builtin_preview.as_ref().unwrap();
        assert!(
            !title.contains("乱序"),
            "乱序关时预览标题不应含「（乱序）」"
        );
    }

    #[test]
    fn load_selected_builtin_with_shuffle_loads_shuffled_text() {
        let mut app = test_app(file_text("旧赛文"));
        app.open_builtin_browser();
        app.builtin_shuffle = true;
        app.builtin_selection = 0; // 常用单字前五百
        app.load_selected_builtin();
        assert!(app.text.shuffled, "乱序加载的 Text 应 shuffled=true");
        assert_eq!(app.text.title, "常用单字前五百（乱序）");
        assert!(matches!(app.text.source, TextSource::Builtin { .. }));
        assert!(matches!(app.state, AppState::Countdown { .. }));
    }

    #[test]
    fn restart_reshuffles_when_text_is_shuffled() {
        let mut app = test_app(load_builtin_text_shuffled(BUILTIN_SETS[0]));
        assert!(app.text.shuffled);
        let content_before = app.text.content.clone();
        app.restart();
        assert!(app.text.shuffled, "重打后仍应 shuffled=true");
        assert!(
            app.text.title.contains("乱序"),
            "重打后标题仍含「（乱序）」"
        );
        assert_ne!(app.text.content, content_before, "重打应产生新的乱序排列");
        assert_eq!(app.session.len(), 0, "重打后 session 应清空");
        assert!(matches!(app.state, AppState::Typing));
    }

    #[test]
    fn original_line_shuffled_word_set_uses_text_boundaries() {
        // 乱序词组 Text 携带自身 word_boundaries，original_line 应直接使用它们。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let text = load_builtin_text_shuffled(set);
        assert!(text.shuffled);
        let boundaries = text.word_boundaries.as_ref().unwrap();
        assert!(!boundaries.is_empty());
        let session = Session::new_gated_with_words(&text.content, true, boundaries);
        let rendered = original_line(&session, &text, theme, false, None);
        assert_eq!(rendered.lines.len(), 1, "乱序词组对照区应只有一行");
        let first_page_words = boundaries.len().min(session.group_size());
        let space_spans = rendered.lines[0]
            .spans
            .iter()
            .filter(|s| s.content == " ")
            .count();
        assert_eq!(space_spans, first_page_words - 1, "乱序第 1 页词间空格数");
        let word_chars: usize = boundaries
            .iter()
            .take(first_page_words)
            .map(|(s, e)| e - s)
            .sum();
        let non_space_spans = rendered.lines[0]
            .spans
            .iter()
            .filter(|s| s.content != " ")
            .count();
        assert_eq!(
            non_space_spans, word_chars,
            "乱序第 1 页非空格 span 数应等于词字符数"
        );
    }

    #[test]
    fn type_line_shuffled_word_set_uses_text_boundaries() {
        // 乱序词组 Text 携带自身 word_boundaries，type_line 应直接使用它们。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let text = load_builtin_text_shuffled(set);
        let boundaries = text.word_boundaries.as_ref().unwrap();
        let mut session = Session::new_gated_with_words(&text.content, true, boundaries);
        // 打第 1 个乱序词的全部字符
        let (ws, we) = boundaries[0];
        let first_word: String = text.content.chars().skip(ws).take(we - ws).collect();
        session.type_text(&first_word);
        let rendered = type_line(&session, &text, theme, false, None);
        assert_eq!(rendered.lines.len(), 1, "乱序词组跟打区应只有一行");
        for ch in first_word.chars() {
            let s = ch.to_string();
            assert!(
                rendered.lines[0].spans.iter().any(|sp| sp.content == s),
                "跟打区应含已打的「{ch}」字"
            );
        }
    }

    #[test]
    fn f_opens_browser() {
        assert!(is_open_browser(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::NONE
        )));
        assert!(is_open_browser(KeyEvent::new(
            KeyCode::Char('F'),
            KeyModifiers::NONE
        )));
        assert!(!is_open_browser(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE
        )));
        assert!(!is_open_browser(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn tab_toggles_sidebar() {
        assert!(is_toggle_sidebar(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE
        )));
        assert!(!is_toggle_sidebar(KeyEvent::new(
            KeyCode::Char('t'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn list_text_files_filters_and_sorts() {
        let dir = temp_dir("list");
        fs::write(dir.join("b.txt"), "乙").unwrap();
        fs::write(dir.join("a.txt"), "甲").unwrap();
        fs::write(dir.join("note.md"), "丙").unwrap();
        fs::write(dir.join("data.json"), "{}").unwrap();
        fs::write(dir.join("skip.rs"), "fn").unwrap();
        fs::create_dir_all(dir.join("sub.txt")).unwrap(); // 目录不算

        let files = list_text_files(&dir);
        let names: Vec<String> = files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "note.md"]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn handle_key_records_key_frequency() {
        let mut session = Session::new("你好世界");
        let mut live_kb = LiveKeyboard::new();
        let now = Instant::now();
        handle_key(
            &mut session,
            &mut live_kb,
            None,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            Duration::ZERO,
            now,
        );
        handle_key(
            &mut session,
            &mut live_kb,
            None,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            Duration::ZERO,
            now,
        );
        handle_key(
            &mut session,
            &mut live_kb,
            None,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            Duration::ZERO,
            now,
        );
        let stats = session.finish(Duration::from_secs(60));
        assert_eq!(stats.key_frequency[0], ("n".to_string(), 2));
        assert_eq!(stats.key_frequency[1], ("Backspace".to_string(), 1));
        assert!(live_kb.active_keys.contains_key("n"));
        assert!(live_kb.active_keys.contains_key("Backspace"));
    }

    #[test]
    fn backspace_key_edits_session() {
        let mut session = Session::new("你好世界");
        let mut live_kb = LiveKeyboard::new();
        let now = Instant::now();
        session.type_text("你好");
        let mut key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        handle_key(&mut session, &mut live_kb, None, key, Duration::ZERO, now);
        assert_eq!(session.len(), 1);
        assert_eq!(session.edit_count(), 1);
        key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        handle_key(&mut session, &mut live_kb, None, key, Duration::ZERO, now);
        assert_eq!(session.len(), 2);
    }

    #[test]
    fn type_line_colors_correct_green_wrong_red() {
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let mut session = Session::new("你好世界");
        session.type_text("你好四界");
        let text = type_line(
            &session,
            &Text {
                title: "t".into(),
                content: "你好世界".into(),
                source: TextSource::File,
                word_boundaries: None,
                shuffled: false,
            },
            theme,
            false,
            None,
        );
        let line = &text.lines[0];
        assert_eq!(line.spans.len(), 4);
        assert_eq!(line.spans[0].style.fg, Some(color(theme.correct)));
        assert_eq!(line.spans[2].style.fg, Some(color(theme.wrong)));
    }

    #[test]
    fn original_line_colors_green_red_default() {
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let mut session = Session::new("你好世界");
        session.type_text("你好四");
        let text = original_line(
            &session,
            &Text {
                title: "t".into(),
                content: "你好世界".into(),
                source: TextSource::File,
                word_boundaries: None,
                shuffled: false,
            },
            theme,
            false,
            None,
        );
        let line = &text.lines[0];
        assert_eq!(line.spans.len(), 4);
        assert_eq!(line.spans[0].style.fg, Some(color(theme.correct))); // 你 ✓
        assert_eq!(line.spans[2].style.fg, Some(color(theme.wrong))); // 世 ✗（打成四）
        assert_eq!(line.spans[3].style.fg, None); // 界：未打到，默认色
    }

    #[test]
    fn bold_modifier_switches_bold() {
        assert_eq!(bold_modifier(true), Modifier::BOLD);
        assert_eq!(bold_modifier(false), Modifier::empty());
    }

    #[test]
    fn type_line_applies_bold_modifier() {
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let mut session = Session::new("你好世界");
        session.type_text("你好");
        let file_text = Text {
            title: "t".into(),
            content: "你好世界".into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        };
        let text = type_line(&session, &file_text, theme, true, None);
        let line = &text.lines[0];
        assert_eq!(line.spans[0].style.add_modifier, Modifier::BOLD);
        assert_eq!(line.spans[1].style.add_modifier, Modifier::BOLD);
        let plain = type_line(&session, &file_text, theme, false, None);
        let plain_line = &plain.lines[0];
        assert_eq!(plain_line.spans[0].style.add_modifier, Modifier::empty());
    }

    #[test]
    fn original_line_applies_bold_modifier() {
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let mut session = Session::new("你好世界");
        session.type_text("你好");
        // 已打到（对）与未打到都加粗。
        let file_text = Text {
            title: "t".into(),
            content: "你好世界".into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        };
        let text = original_line(&session, &file_text, theme, true, None);
        let line = &text.lines[0];
        assert_eq!(line.spans[0].style.add_modifier, Modifier::BOLD);
        assert_eq!(line.spans[2].style.add_modifier, Modifier::BOLD);
        let plain = original_line(&session, &file_text, theme, false, None);
        let plain_line = &plain.lines[0];
        assert_eq!(plain_line.spans[0].style.add_modifier, Modifier::empty());
    }

    #[test]
    fn move_focus_wraps_around() {
        // SETTINGS_FOCUS_COUNT = 9（主题/占比/粗体/实时键盘/反查方案/上传名称/分组大小/遍码提示/方案热监控）
        assert_eq!(move_focus(0, -1), 8); // 第 0 项向前 → 末项（8）
        assert_eq!(move_focus(8, 1), 0); // 末项向后 → 第 0 项
        assert_eq!(move_focus(7, 1), 8); // 倒数第二项向后 → 末项
        assert_eq!(move_focus(0, 1), 1);
        assert_eq!(move_focus(5, 1), 6);
        assert_eq!(move_focus(2, -1), 1);
    }

    #[test]
    fn adjust_ratio_value_steps_and_clamps() {
        assert_eq!(adjust_ratio_value(62, 5), 67);
        assert_eq!(adjust_ratio_value(62, -5), 57);
        assert_eq!(adjust_ratio_value(78, 5), 80); // 越界截断到 80
        assert_eq!(adjust_ratio_value(32, -5), 30); // 越界截断到 30
    }

    #[test]
    fn area_ratios_derive_layout() {
        assert_eq!(area_ratios(62), (62, 38));
        assert_eq!(area_ratios(0), (30, 70)); // 越界截断
        assert_eq!(area_ratios(100), (80, 20)); // 越界截断
    }

    #[test]
    fn input_method_display_empty_shows_wu() {
        assert_eq!(input_method_display(""), "无");
        assert_eq!(input_method_display("虎码"), "虎码");
    }

    #[test]
    fn cycle_input_method_next_from_empty_is_first_preset() {
        // 空串（「无」）向后轮转 → 「虎码」（第 1 项）
        assert_eq!(cycle_input_method_next(""), "虎码");
    }

    #[test]
    fn cycle_input_method_next_wraps_last_to_empty() {
        // 末项「自定义」向后轮转 → 「无」（空串，即第 0 项）
        let last = INPUT_METHOD_CUSTOM;
        let result = cycle_input_method_next(last);
        assert_eq!(result, "");
    }

    #[test]
    fn app_keyboard_mode_cycling_and_persistence() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("dazitui-test-app-kb-{stamp}"));
        let store = SettingsStore::new(temp_dir.join("settings"));
        let token_store = temp_token_store();
        let mut app = App::new_with(
            load_builtin_text(BUILTIN_SETS[0]),
            token_store.clone(),
            ApiClient::with_base_url_and_store("http://127.0.0.1:1", Some(token_store)),
            store.clone(),
            None,
        );

        assert_eq!(app.settings.keyboard_mode, KeyboardMode::Off);
        app.next_keyboard_mode();
        assert_eq!(app.settings.keyboard_mode, KeyboardMode::Staggered);
        assert_eq!(store.load().keyboard_mode, KeyboardMode::Staggered);

        app.next_keyboard_mode();
        assert_eq!(app.settings.keyboard_mode, KeyboardMode::Ortholinear);
        assert_eq!(store.load().keyboard_mode, KeyboardMode::Ortholinear);

        app.next_keyboard_mode();
        assert_eq!(app.settings.keyboard_mode, KeyboardMode::Off);
        assert_eq!(store.load().keyboard_mode, KeyboardMode::Off);

        app.prev_keyboard_mode();
        assert_eq!(app.settings.keyboard_mode, KeyboardMode::Ortholinear);
        assert_eq!(store.load().keyboard_mode, KeyboardMode::Ortholinear);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn cycle_input_method_prev_from_empty_wraps_to_last() {
        // 「无」向前轮转 → 末项「自定义」
        let result = cycle_input_method_prev("");
        assert_eq!(result, INPUT_METHOD_CUSTOM);
    }

    #[test]
    fn cycle_input_method_prev_from_huma_is_empty() {
        // 「虎码」向前轮转 → 「无」（空串）
        assert_eq!(cycle_input_method_prev("虎码"), "");
    }

    #[test]
    fn cycle_input_method_unknown_falls_to_custom_slot_then_wraps() {
        // 自定义值（不在预设）→ 视为「自定义」末项下标，next → 第 0 项「无」
        let result = cycle_input_method_next("我的专属输入法");
        assert_eq!(result, "");
    }

    #[test]
    fn scheme_display_and_cycling_tests() {
        // 选项式轮转：无 + [yoyo-pure, kongmingma] + 自定义。
        let discovered = vec![
            SchemeInfo {
                id: "yoyo-pure".to_string(),
                display_name: "麓鸣纯形·六脉".to_string(),
                path: PathBuf::from("/x/yoyo-pure.schema.yaml"),
            },
            SchemeInfo {
                id: "kongmingma".to_string(),
                display_name: "空明码".to_string(),
                path: PathBuf::from("/x/kongmingma.schema.yaml"),
            },
        ];
        let opts = build_scheme_options(&discovered);
        assert_eq!(opts.len(), 4, "无 + 2 发现 + 自定义");
        assert!(matches!(opts[0], SchemeOption::None));
        assert!(matches!(opts[3], SchemeOption::Custom));

        // 空（无）→ 下一个是第一个发现
        assert_eq!(scheme_next_option(&opts, ""), "yoyo-pure");
        // 最后一个发现 → 下一个是自定义
        assert_eq!(scheme_next_option(&opts, "kongmingma"), SCHEME_CUSTOM);
        // 自定义 → 下一个回绕到无
        assert_eq!(scheme_next_option(&opts, SCHEME_CUSTOM), "");
        // 第一个发现 → 下一个是第二个发现
        assert_eq!(scheme_next_option(&opts, "yoyo-pure"), "kongmingma");
        // 第一个发现 → 上一个是无
        assert_eq!(scheme_prev_option(&opts, "yoyo-pure"), "");
        // 无 → 上一个回绕到自定义
        assert_eq!(scheme_prev_option(&opts, ""), SCHEME_CUSTOM);
        // 第二个发现 → 上一个是第一个发现
        assert_eq!(scheme_prev_option(&opts, "kongmingma"), "yoyo-pure");
        // 未知/自定义值 → 落到「自定义」项
        assert_eq!(scheme_option_index(&opts, "my_custom_scheme"), 3);
    }

    #[test]
    fn text_setting_modal_new_prefills_custom_and_clears_preset() {
        assert_eq!(
            TextSettingModal::new(TextSettingTarget::InputMethod, "").input,
            ""
        );
        assert_eq!(
            TextSettingModal::new(TextSettingTarget::InputMethod, "虎码").input,
            ""
        );
        assert_eq!(
            TextSettingModal::new(TextSettingTarget::InputMethod, "自定义").input,
            ""
        );
        assert_eq!(
            TextSettingModal::new(TextSettingTarget::InputMethod, "我的自定义码").input,
            "我的自定义码"
        );

        assert_eq!(
            TextSettingModal::new(TextSettingTarget::Scheme, "").input,
            ""
        );
        assert_eq!(
            TextSettingModal::new(TextSettingTarget::Scheme, "yoyo-pure").input,
            "yoyo-pure"
        );
        assert_eq!(
            TextSettingModal::new(TextSettingTarget::Scheme, SCHEME_CUSTOM).input,
            ""
        );
        assert_eq!(
            TextSettingModal::new(TextSettingTarget::Scheme, "/path/to/custom.schema.yaml").input,
            "/path/to/custom.schema.yaml"
        );
    }

    #[test]
    fn text_setting_modal_push_char_clamps() {
        let mut modal = TextSettingModal::new(TextSettingTarget::InputMethod, "");
        for _ in 0..25 {
            modal.push_char('字');
        }
        assert_eq!(modal.input.chars().count(), 20);
        assert_eq!(modal.input, "字".repeat(20));

        let mut scheme_modal = TextSettingModal::new(TextSettingTarget::Scheme, "");
        for _ in 0..50 {
            scheme_modal.push_char('a');
        }
        assert_eq!(scheme_modal.input.chars().count(), 50);
    }

    #[test]
    fn text_setting_modal_pop_char_removes_last_unicode_char() {
        let mut modal = TextSettingModal::new(TextSettingTarget::InputMethod, "虎码输入");
        modal.pop_char();
        assert_eq!(modal.input, "虎码输");
        modal.pop_char();
        assert_eq!(modal.input, "虎码");
        modal.pop_char();
        modal.pop_char();
        modal.pop_char(); // popping empty doesn't panic
        assert_eq!(modal.input, "");
    }

    #[test]
    fn text_setting_modal_commit_trims_and_returns_empty_for_blanks() {
        let mut modal = TextSettingModal::new(TextSettingTarget::InputMethod, "");
        assert_eq!(modal.commit(), "");
        modal.input = "   ".into();
        assert_eq!(modal.commit(), "");
        modal.input = "  小鹤双拼  ".into();
        assert_eq!(modal.commit(), "小鹤双拼");
    }

    #[test]
    fn text_setting_modal_input_actions() {
        let mut modal = TextSettingModal::new(TextSettingTarget::InputMethod, "");
        // 输入字符
        assert_eq!(
            text_setting_modal_input(
                &mut modal,
                KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)
            ),
            TextSettingModalAction::None
        );
        assert_eq!(
            text_setting_modal_input(
                &mut modal,
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE)
            ),
            TextSettingModalAction::None
        );
        assert_eq!(modal.input, "ab");

        // 退格
        assert_eq!(
            text_setting_modal_input(
                &mut modal,
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)
            ),
            TextSettingModalAction::None
        );
        assert_eq!(modal.input, "a");

        // 回车保存
        assert_eq!(
            text_setting_modal_input(
                &mut modal,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            ),
            TextSettingModalAction::Save(TextSettingTarget::InputMethod, "a".into())
        );

        // Esc 取消
        assert_eq!(
            text_setting_modal_input(&mut modal, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            TextSettingModalAction::Cancel
        );
    }

    #[test]
    fn adjust_ratio_persists_and_clamps() {
        let mut app = test_app(file_text("你好"));
        assert_eq!(app.settings.reference_ratio, 62); // 默认
        app.adjust_ratio(5);
        assert_eq!(app.settings.reference_ratio, 67);
        assert_eq!(app.settings_store.load().reference_ratio, 67);
        app.adjust_ratio(20); // 越界截断到 80
        assert_eq!(app.settings.reference_ratio, 80);
    }

    #[test]
    fn toggle_bold_persist() {
        let mut app = test_app(file_text("你好"));
        assert!(!app.settings.bold);
        app.toggle_bold();
        assert!(app.settings.bold);
        let loaded = app.settings_store.load();
        assert!(loaded.bold);
    }

    #[test]
    fn load_selected_loads_file_and_restarts() {
        let dir = temp_dir("load");
        let path = dir.join("a.txt");
        fs::write(&path, "你好， 世界。\n第二行").unwrap();
        let mut app = test_app(Text {
            title: "old".into(),
            content: "旧赛文".into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        });
        app.open_browser();
        // 打开浏览时扫描的是当前工作目录，手动指向临时目录
        app.browse_files = list_text_files(&dir);
        app.browse_selection = 0;

        app.load_selected();
        assert_eq!(app.text.title, "a.txt");
        assert_eq!(app.text.content, "你好， 世界。\n第二行");
        assert!(matches!(app.state, AppState::Countdown { .. }));
        assert_eq!(app.session.len(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn countdown_launches_into_typing_when_deadline_passed() {
        let mut app = test_app(file_text("旧赛文"));
        app.state = AppState::Countdown {
            deadline: Instant::now() - Duration::from_millis(1),
            source: CountdownSource::Browsing,
        };
        app.advance_countdown_if_due();
        assert!(matches!(app.state, AppState::Typing));
    }

    #[test]
    fn countdown_stays_until_deadline() {
        let mut app = test_app(file_text("旧赛文"));
        app.state = AppState::Countdown {
            deadline: Instant::now() + Duration::from_secs(10),
            source: CountdownSource::Browsing,
        };
        app.advance_countdown_if_due();
        assert!(matches!(app.state, AppState::Countdown { .. }));
    }

    #[test]
    fn resume_enters_countdown_then_continues_without_resetting_timer() {
        let mut app = test_app(file_text("旧赛文"));
        // 模拟已暂停且已累计 42 秒跟打
        app.accumulated_elapsed = Duration::from_secs(42);
        app.active_start = None;
        app.paused = true;
        app.state = AppState::Typing;

        // 按继续键：应进入续打倒计时，而非直接恢复
        app.enter_resume_countdown();
        assert!(matches!(
            app.state,
            AppState::Countdown {
                source: CountdownSource::Resume,
                ..
            }
        ));
        assert!(app.paused, "续打倒计时期间应仍保持暂停（计时冻结）");

        // 倒计时结束：从暂停处续接，已累计用时不丢、计时继续
        app.state = AppState::Countdown {
            deadline: Instant::now() - Duration::from_millis(1),
            source: CountdownSource::Resume,
        };
        app.advance_countdown_if_due();
        assert!(matches!(app.state, AppState::Typing));
        assert!(!app.paused, "倒计时结束后应恢复跟打");
        assert_eq!(
            app.accumulated_elapsed,
            Duration::from_secs(42),
            "续打不应清零已累计用时"
        );

        // 取消续打倒计时：回到暂停态
        app.paused = true;
        app.state = AppState::Countdown {
            deadline: Instant::now() + Duration::from_secs(10),
            source: CountdownSource::Resume,
        };
        app.cancel_countdown(CountdownSource::Resume);
        assert!(matches!(app.state, AppState::Typing));
        assert!(app.paused, "取消续打倒倒计时应回到暂停态");
    }

    #[test]
    fn load_selected_reports_error_without_panic() {
        let dir = temp_dir("loaderr");
        let path = dir.join("empty.txt");
        fs::write(&path, "").unwrap();
        let mut app = test_app(Text {
            title: "old".into(),
            content: "旧赛文".into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        });
        app.open_browser();
        app.browse_files = list_text_files(&dir);
        app.browse_selection = 0;

        app.load_selected();
        assert!(app.browse_error.is_some());
        assert!(matches!(app.state, AppState::Browsing));
        assert_eq!(app.text.title, "old"); // 旧赛文保留

        let _ = fs::remove_dir_all(&dir);
    }

    // ---- 登录 ----

    #[test]
    fn u_opens_login() {
        assert!(is_open_login(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::NONE
        )));
        assert!(is_open_login(KeyEvent::new(
            KeyCode::Char('U'),
            KeyModifiers::NONE
        )));
        assert!(!is_open_login(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE
        )));
        assert!(!is_open_login(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn o_opens_settings() {
        assert!(is_open_settings(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::NONE
        )));
        assert!(is_open_settings(KeyEvent::new(
            KeyCode::Char('O'),
            KeyModifiers::NONE
        )));
        assert!(!is_open_settings(KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::NONE
        )));
        assert!(!is_open_settings(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn next_prev_theme_cycles_and_persists() {
        let mut app = test_app(file_text("你好"));
        // 默认从 CatppuccinMocha 开始。
        assert_eq!(app.settings.theme, ThemePreset::CatppuccinMocha);
        // 切下一主题并持久化。
        app.next_theme();
        assert_eq!(app.settings.theme, ThemePreset::Cyberpunk);
        assert_eq!(app.settings_store.load().theme, ThemePreset::Cyberpunk);
        // 循环回绕：往前退回到 CatppuccinMocha。
        app.prev_theme();
        assert_eq!(app.settings.theme, ThemePreset::CatppuccinMocha);
        assert_eq!(
            app.settings_store.load().theme,
            ThemePreset::CatppuccinMocha
        );
        // 从 CatppuccinMocha 往上退绕到 OneDark。
        app.prev_theme();
        assert_eq!(app.settings.theme, ThemePreset::OneDark);
    }

    #[test]
    fn theme_palette_resolves_all_presets() {
        for preset in ThemePreset::ALL {
            let palette = theme_palette(preset);
            assert_ne!(palette.accent, palette.bg);
        }
    }

    #[test]
    fn themed_block_rounded_and_focus_highlight() {
        let palette = theme_palette(ThemePreset::CatppuccinMocha);
        let active_block = themed_block(&palette, true);
        let inactive_block = themed_block(&palette, false);
        // Both blocks use BorderType::Rounded and distinguishable border colors
        let _ = active_block;
        let _ = inactive_block;
    }

    #[test]
    fn composite_title_spans_rendering() {
        let palette = theme_palette(ThemePreset::CatppuccinMocha);
        let spans = vec![
            Span::styled(
                " 跟打区 ",
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "[已暂停] ",
                Style::default()
                    .fg(palette.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("— 0/10 字符 ", Style::default().fg(palette.fg)),
        ];
        let title = Line::from(spans);
        assert_eq!(title.spans.len(), 3);
        assert_eq!(title.spans[0].style.fg, Some(palette.accent));
        assert_eq!(title.spans[1].style.fg, Some(palette.warning));
        assert_eq!(title.spans[2].style.fg, Some(palette.fg));
    }

    #[test]
    fn hint_bar_line_badge_pill_formatting() {
        let palette = theme_palette(ThemePreset::CatppuccinMocha);
        let line = hint_bar_line(" Ctrl-Q 退出 | Ctrl-E 设置 ", &palette);
        assert!(!line.spans.is_empty());
        // Left rounded cap has selection fg and bg
        assert_eq!(line.spans[0].content, "◖");
        assert_eq!(line.spans[0].style.fg, Some(palette.selection));
        // Key text span has selection bg and accent fg
        assert_eq!(line.spans[1].content, "Ctrl-Q");
        assert_eq!(line.spans[1].style.bg, Some(palette.selection));
        assert_eq!(line.spans[1].style.fg, Some(palette.accent));
        // Right rounded cap has selection fg
        assert_eq!(line.spans[2].content, "◗");
        assert_eq!(line.spans[2].style.fg, Some(palette.selection));
        // Description span has palette.fg color
        assert!(line.spans[3].content.contains("退出"));
        assert_eq!(line.spans[3].style.fg, Some(palette.fg));
    }

    #[test]
    fn theme_switch_changes_correct_wrong_colors() {
        // 对/错颜色随主题切换改变（外部可观察行为：不是固定绿/红）。
        let catppuccin_theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let dracula_theme = Theme::preset(ThemePreset::Dracula);
        let mut session = Session::new("你好");
        session.type_text("你四");
        let file_text = Text {
            title: "t".into(),
            content: "你好".into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        };
        let catppuccin_text = original_line(&session, &file_text, catppuccin_theme, false, None);
        let dracula_text = original_line(&session, &file_text, dracula_theme, false, None);
        let catppuccin_line = &catppuccin_text.lines[0];
        let dracula_line = &dracula_text.lines[0];
        assert_eq!(
            catppuccin_line.spans[0].style.fg,
            Some(color(catppuccin_theme.correct))
        );
        assert_eq!(
            catppuccin_line.spans[1].style.fg,
            Some(color(catppuccin_theme.wrong))
        );
        assert_eq!(
            dracula_line.spans[0].style.fg,
            Some(color(dracula_theme.correct))
        );
        assert_eq!(
            dracula_line.spans[1].style.fg,
            Some(color(dracula_theme.wrong))
        );
        assert_ne!(
            catppuccin_line.spans[0].style.fg,
            dracula_line.spans[0].style.fg
        );
        assert_ne!(
            catppuccin_line.spans[1].style.fg,
            dracula_line.spans[1].style.fg
        );
    }

    #[test]
    fn type_line_builtin_shows_only_current_page() {
        // 25 字内置赛文：每页 10 字，当前组 10 字全对才翻到下一页，跟打区只显示当前页。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let content = "一二三四五六七八九十甲乙丙丁戊己庚辛壬癸子丑寅卯辰";
        let mut session = Session::new_gated(content, true);
        let text = builtin_text(content);
        // 打 5 字（全对）：仍在第一组，跟打区显示当前页已打的 5 字。
        session.type_text("一二三四五");
        let rendered = type_line(&session, &text, theme, false, None);
        assert_eq!(rendered.lines.len(), 1, "第一页应只有一行");
        assert_eq!(rendered.lines[0].spans.len(), 5, "打 5 字应显示 5 字");
        // 打到 10 字（全对）：第一组全对，completed_groups 推进，翻到第二组，跟打区显示提示行。
        session.type_text("六七八九十");
        let rendered = type_line(&session, &text, theme, false, None);
        assert_eq!(rendered.lines.len(), 1, "翻到第二组尚未打字时应显示提示行");
        // 打到 13 字（全对）：第二组已打 3 字，跟打区显示 3 字。
        session.type_text("甲乙丙");
        let rendered = type_line(&session, &text, theme, false, None);
        assert_eq!(rendered.lines.len(), 1, "第二组应只有一行");
        assert_eq!(
            rendered.lines[0].spans.len(),
            3,
            "打 13 字应翻页显示第 11-13 字"
        );
    }

    #[test]
    fn type_line_builtin_wrong_char_blocks_page_advance() {
        // 组内有错字 → completed_groups 不推进 → 不翻页。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let content = "一二三四五六七八九十甲乙丙丁戊己庚辛壬癸子丑寅卯辰";
        let mut session = Session::new_gated(content, true);
        let text = builtin_text(content);
        // 打 10 字但第 10 字打错 → 组未全对 → 不翻页
        session.type_text("一二三四五六七八九X");
        let rendered = type_line(&session, &text, theme, false, None);
        assert_eq!(
            rendered.lines[0].spans.len(),
            10,
            "组内打错仍应显示当前组 10 字"
        );
        assert_eq!(
            session.completed_groups(),
            0,
            "有错字不应推进 completed_groups"
        );
    }

    #[test]
    fn type_line_builtin_backspace_at_group_boundary_keeps_page() {
        // 退格到组首封顶 → 页起始不变、不翻回上一组。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let content = "一二三四五六七八九十甲乙丙丁戊己庚辛壬癸子丑寅卯辰";
        let mut session = Session::new_gated(content, true);
        let text = builtin_text(content);
        // 第一组 10 字全对 → 推进到第 2 组
        session.type_text("一二三四五六七八九十");
        assert_eq!(session.completed_groups(), 1);
        // 第 2 组打 3 字后回改 3 次回到组首
        session.type_text("甲乙丙");
        assert_eq!(session.len(), 13);
        assert!(session.backspace());
        assert!(session.backspace());
        assert!(session.backspace());
        // 回到组首，再回改应被封顶（返回 false）
        assert!(!session.backspace(), "组首回改应封顶");
        assert_eq!(session.len(), 10, "不应减过组首");
        assert_eq!(session.completed_groups(), 1, "已完成组不应回退");
        // 页起始仍为第 2 组
        assert_eq!(builtin_page_start(&session), 10);
        let rendered = type_line(&session, &text, theme, false, None);
        assert_eq!(rendered.lines.len(), 1, "未打字时应显示提示行");
    }

    #[test]
    fn original_line_builtin_shows_only_current_page() {
        // 25 字内置赛文：对照区只显示当前组 10 字，当前组全对才翻到下一组。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let content = "一二三四五六七八九十甲乙丙丁戊己庚辛壬癸子丑寅卯辰";
        let mut session = Session::new_gated(content, true);
        let text = builtin_text(content);
        // 打 5 字（全对）：对照区显示第一组 10 字（前 5 已打对，后 5 未打到）。
        session.type_text("一二三四五");
        let rendered = original_line(&session, &text, theme, false, None);
        assert_eq!(rendered.lines.len(), 1, "第一组应只有一行");
        assert_eq!(
            rendered.lines[0].spans.len(),
            10,
            "对照区第一组应显示 10 字"
        );
        // 打到 10 字（全对）：第一组全对，翻到第二组，对照区显示第 11-20 字（10 字）。
        session.type_text("六七八九十");
        let rendered = original_line(&session, &text, theme, false, None);
        assert_eq!(rendered.lines.len(), 1, "第二组应只有一行");
        assert_eq!(
            rendered.lines[0].spans.len(),
            10,
            "对照区第二组应显示 10 字"
        );
    }

    #[test]
    fn type_line_file_source_stays_single_line() {
        // 非内置赛文（File）保持单行：由终端宽度自动折行，不分多行 span。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let mut session = Session::new("一二三四五六七八九十十一十");
        session.type_text("一二三四五六七八九十十一十");
        let text = type_line(
            &session,
            &file_text("一二三四五六七八九十十一十"),
            theme,
            false,
            None,
        );
        assert_eq!(text.lines.len(), 1);
        assert_eq!(text.lines[0].spans.len(), 13);
    }

    #[test]
    fn type_line_empty_input_builtin_shows_placeholder() {
        // 空输入时显示提示行（不分多行、无空 span）。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let session = Session::new_gated("一二三四五六七八九十", true);
        let text = type_line(
            &session,
            &builtin_text("一二三四五六七八九十"),
            theme,
            false,
            None,
        );
        assert_eq!(text.lines.len(), 1, "空输入应只有一行提示");
    }

    #[test]
    fn type_line_word_set_shows_space_between_words() {
        // 词组赛文：词间显示空格 span，去逗号。每页 10 个词。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        // content_no_commas = "可以一个自己没有..."（词间无逗号）
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let no_commas = set.content_no_commas();
        let boundaries = set.word_boundaries();
        let mut session = Session::new_gated_with_words(no_commas.as_str(), true, &boundaries);
        let text = Text {
            title: set.name().into(),
            content: no_commas.clone(),
            source: TextSource::Builtin { set },
            word_boundaries: None,
            shuffled: false,
        };
        // 打第 1 个词「可以」（2 字）
        session.type_text("可以");
        let rendered = type_line(&session, &text, theme, false, None);
        assert_eq!(rendered.lines.len(), 1, "词组赛文应只有一行");
        // 第 1 个词 2 字 + 空格 + 第 2 个词的已打部分…跟打区只显示已打字符
        // 已打 2 字（第 1 词），应在 spans 中。第 2+ 词尚未打，跟打区无内容。
        let typed_spans: Vec<_> = rendered.lines[0]
            .spans
            .iter()
            .filter(|s| !s.content.is_empty())
            .collect();
        assert!(
            typed_spans.iter().any(|s| s.content == "可"),
            "跟打区应含已打的「可」字"
        );
        assert!(
            typed_spans.iter().any(|s| s.content == "以"),
            "跟打区应含已打的「以」字"
        );
    }

    #[test]
    fn original_line_word_set_shows_10_words_per_page() {
        // 词组赛文对照区：每页显示 10 个词，词间有 9 个空格 span。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let no_commas = set.content_no_commas();
        let boundaries = set.word_boundaries();
        let session = Session::new_gated_with_words(no_commas.as_str(), true, &boundaries);
        let text = Text {
            title: set.name().into(),
            content: no_commas.clone(),
            source: TextSource::Builtin { set },
            word_boundaries: None,
            shuffled: false,
        };
        let rendered = original_line(&session, &text, theme, false, None);
        assert_eq!(rendered.lines.len(), 1, "词组赛文应只有一行");
        // 第 1 页 10 个词，词间 9 个空格 span
        let first_page_words = boundaries.len().min(session.group_size());
        let word_chars: usize = boundaries
            .iter()
            .take(first_page_words)
            .map(|(s, e)| e - s)
            .sum();
        let space_spans = rendered.lines[0]
            .spans
            .iter()
            .filter(|s| s.content == " ")
            .count();
        assert_eq!(
            space_spans,
            first_page_words - 1,
            "第 1 页应有 {} 个词间空格",
            first_page_words - 1
        );
        // 非空格 span 数 = 第 1 页所有词的字符数
        let non_space_spans = rendered.lines[0]
            .spans
            .iter()
            .filter(|s| s.content != " ")
            .count();
        assert_eq!(
            non_space_spans, word_chars,
            "非空格 span 数应等于第 1 页词字符数"
        );
    }

    #[test]
    fn type_line_word_set_advances_page_after_10_words() {
        // 词组赛文打满 10 个词且全对后翻页。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let no_commas = set.content_no_commas();
        let boundaries = set.word_boundaries();
        let mut session = Session::new_gated_with_words(no_commas.as_str(), true, &boundaries);
        let text = Text {
            title: set.name().into(),
            content: no_commas.clone(),
            source: TextSource::Builtin { set },
            word_boundaries: None,
            shuffled: false,
        };
        // 打满第 1 组 10 个词的全部字符（全对）
        let first_page_char_count: usize = boundaries
            .iter()
            .take(session.group_size())
            .map(|(s, e)| e - s)
            .sum();
        let first_page_chars: String = no_commas.chars().take(first_page_char_count).collect();
        session.type_text(&first_page_chars);
        // 全对 → completed_groups 推进 → 翻到第 2 组
        assert_eq!(session.completed_groups(), 1, "第 1 组全对应推进到 1");
        let rendered = type_line(&session, &text, theme, false, None);
        assert_eq!(rendered.lines.len(), 1, "翻到第 2 组应只有一行");
        // 第 2 组尚未打字，应显示空输入提示
        let placeholder_spans: Vec<_> = rendered.lines[0]
            .spans
            .iter()
            .filter(|s| s.content.contains("跟打区"))
            .collect();
        assert!(
            !placeholder_spans.is_empty(),
            "翻到第 2 组未打字时应显示提示行"
        );
    }

    #[test]
    fn type_line_word_set_no_premature_advance_after_5_two_char_words() {
        // 回归：词组赛文每词 2 字，打 5 个词（= 10 字符）不应推进 completed_groups。
        // 现状 bug：completed_groups 以字符计（每 10 字符推进），而渲染以词计（每 10 词翻页），
        // 导致打 5 个词（10 字符）就翻页，跟打区变空白。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let no_commas = set.content_no_commas();
        let boundaries = set.word_boundaries();
        let mut session = Session::new_gated_with_words(no_commas.as_str(), true, &boundaries);
        let text = Text {
            title: set.name().into(),
            content: no_commas.clone(),
            source: TextSource::Builtin { set },
            word_boundaries: None,
            shuffled: false,
        };
        // 逐词打前 5 个词（模拟真实 IME 逐词上屏），每个词 2 字
        for &(ws, we) in boundaries.iter().take(5) {
            let word: String = no_commas.chars().skip(ws).take(we - ws).collect();
            session.type_text(&word);
        }
        // 打了 5 个词 = 10 字符，但词组赛文一组应为 10 个词，不应推进
        assert_eq!(
            session.completed_groups(),
            0,
            "打 5 个词（10 字符）不应推进 completed_groups，一组应为 10 词"
        );
        // 跟打区应仍显示当前页（第 1-10 词），不应空白
        let rendered = type_line(&session, &text, theme, false, None);
        let non_placeholder: Vec<_> = rendered.lines[0]
            .spans
            .iter()
            .filter(|s| !s.content.contains("跟打区") && s.content != " ")
            .collect();
        assert!(
            !non_placeholder.is_empty(),
            "打 5 个词后跟打区不应空白，应显示已打字符"
        );
    }

    #[test]
    fn code_hint_overlay_shows_hint_above_word_for_builtin_word_set() {
        // T03：内置词组赛文开启遍码提示且方案已载入时，提示行应出现在正文行之上且含首词编码。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let text = load_builtin_text(set);
        let boundaries = set.word_boundaries();
        let session = Session::new_gated_with_words_and_size(&text.content, true, &boundaries, 10);
        // yoyo-pure 风格字典夹具：首词「中国」→ lgy
        let tsv = "中国\tlgy\n中\tk\n国\tlgyi\n人民\twvww\n人\tw\n民\tnay\n";
        let dict = SchemeDict::parse(tsv);
        let line = code_hint_overlay_line(&session, &text, Some(&dict), theme);
        let line = line.expect("应生成提示行");
        let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            rendered.contains("lgy"),
            "提示行应含首词「中国」的编码 lgy，得到: {rendered:?}"
        );
        // 提示行为单行（与正文行逐词对齐，不自动折行）：不含换行符。
        assert!(
            !line.spans.iter().any(|s| s.content.contains('\n')),
            "提示行应为单行（无换行）"
        );

        // 未配置方案时不生成提示行
        assert!(
            code_hint_overlay_line(&session, &text, None, theme).is_none(),
            "未配置方案时不应生成提示行"
        );
        // 非词组（单字）赛文不生成提示行
        let char_text = load_builtin_text(BUILTIN_SETS[0]);
        let char_session = Session::new_gated(char_text.content.as_str(), true);
        assert!(
            code_hint_overlay_line(&char_session, &char_text, Some(&dict), theme).is_none(),
            "单字赛文不应生成提示行"
        );
    }

    #[test]
    fn code_hint_overlay_hides_typed_word_and_reveals_on_backspace() {
        // T04：已全对上屏的词，其上方提示隐藏；回改（删一个字符）后该词提示重现。
        // 对所有本页其他未打字词提示不产生影响。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let set = BUILTIN_SETS[3]; // 常用词组前五百，首词「可以」（双字·列宽 4）
        let text = load_builtin_text(set);
        let boundaries = set.word_boundaries();
        let mut session =
            Session::new_gated_with_words_and_size(&text.content, true, &boundaries, 10);
        // 字典涵盖首词「可以」及若干后续词（中国=lgy 用于验证其余提示不受影响）。
        let tsv =
            "可以\tkr\n一个\tyg\n自己\tvm\n没有\tei\n我们\twm\n这个\tvi\n问题\tuj\n中国\tlgy\n";
        let dict = SchemeDict::parse(tsv);

        let (fw_s, fw_e) = boundaries[0];
        let first_word: String = text.content.chars().skip(fw_s).take(fw_e - fw_s).collect();
        assert_eq!(first_word, "可以", "首词应为「可以」");

        // 未打字时首词「可以」的提示首格（4 列）应显示编码（非空白）。
        let untyped =
            code_hint_overlay_line(&session, &text, Some(&dict), theme).expect("应生成提示行");
        let untyped_str: String = untyped.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !untyped_str.chars().take(4).all(|c| c.is_whitespace()),
            "未打字时首词「可以」应显示编码，得到: {untyped_str:?}"
        );

        // 打满首词「可以」→ 其提示首格应隐藏（4 列空格占位），其余词提示不变。
        session.type_text(&first_word);
        let typed =
            code_hint_overlay_line(&session, &text, Some(&dict), theme).expect("应生成提示行");
        let typed_str: String = typed.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            typed_str.chars().take(4).all(|c| c.is_whitespace()),
            "首词已上屏后其提示应隐藏（4 列空格占位），得到: {typed_str:?}"
        );
        assert!(
            typed_str.contains("lgy"),
            "其余未打字词（如中国）提示应仍在，得到: {typed_str:?}"
        );

        // 回改：删除末字符 → 首词变为部分输入，提示随状态重现（首格不再全空白）。
        session.backspace();
        let reverted =
            code_hint_overlay_line(&session, &text, Some(&dict), theme).expect("应生成提示行");
        let reverted_str: String = reverted.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !reverted_str.chars().take(4).all(|c| c.is_whitespace()),
            "回改后首词提示应重现，得到: {reverted_str:?}"
        );
    }

    #[test]
    fn code_hint_grid_lockstep_wraps_long_text_narrow_width() {
        // T06：非内置长文（离线/自由/剪贴板/在线）开启提示时，双行词格按词边界锁步折行，
        // 提示行与正文行折行点完全一致、逐词对齐、永不错位。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let content = "我们看着这个美丽的世界，人民在静静地生活。社会主义事业发展得越来越好。";
        let text = load_text_from_string(
            "长文",
            content.to_string(),
            TextSource::File,
            &LoadOptions::default(),
        )
        .unwrap();
        // 未打字：全部词的提示都应可见。
        let session = Session::new(&text.content);
        // yoyo 风格字典夹具：覆盖正文中的词与单字。
        let tsv = "我们\twm\n看着\tva\n这个\tvi\n美丽\tmwi\n世界\twj\n人民\trvww\n\
                   生活\twvi\n社会\twpww\n主义\tuyit\n事业\tsira\n发展\tvzoi\n越来越好\tylyh\n";
        let dict = SchemeDict::parse(tsv);
        let max_width = 12; // 窄：约 2 个双字词一行
        let grid = code_hint_grid_text(&session, &text, Some(&dict), theme, false, max_width);
        let grid = grid.expect("非内置长文应生成双行词格");
        let lines: Vec<String> = grid
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(
            lines.len() >= 4,
            "应有若干 (提示,正文) 行对，实际 {} 行",
            lines.len()
        );
        assert_eq!(lines.len() % 2, 0, "行数应为偶数（提示+正文成对）");
        // 逐对校验：提示行与正文行总列宽一致（折行点锁步），且提示行在上方。
        for pair in lines.chunks(2) {
            let (hint, body) = (&pair[0], &pair[1]);
            assert_eq!(
                hint.width(),
                body.width(),
                "提示行与正文行应等宽对齐: hint={:?} body={:?}",
                hint,
                body
            );
        }
        // 至少应出现某个词的反查编码（如「我们」→ wm）。
        let joined: String = lines.concat();
        assert!(joined.contains("wm"), "提示格应含词码，得到: {:?}", joined);
    }

    #[test]
    fn code_hint_grid_returns_none_without_dict_or_builtin() {
        // T06：未配置方案或非内置词组赛文时，双行词格路径应回退（返回 None）。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let content = "我们看着这个美丽的世界。";
        let text = load_text_from_string(
            "长文",
            content.to_string(),
            TextSource::File,
            &LoadOptions::default(),
        )
        .unwrap();
        let session = Session::new(&text.content);
        // 未配置方案 → 不生成词格（交由上层回退到普通渲染）。
        assert!(
            code_hint_grid_text(&session, &text, None, theme, false, 20).is_none(),
            "未配置方案时不应生成词格"
        );

        // 内置词组赛文不在此路径（由 code_hint_overlay_line 处理）。
        let set = BUILTIN_SETS[3];
        let builtin_text = load_builtin_text(set);
        let builtin_session = Session::new_gated_with_words_and_size(
            &builtin_text.content,
            true,
            &set.word_boundaries(),
            10,
        );
        let dict = SchemeDict::parse("我们\twm\n");
        assert!(
            code_hint_grid_text(
                &builtin_session,
                &builtin_text,
                Some(&dict),
                theme,
                false,
                20
            )
            .is_none(),
            "内置词组赛文不应走双行词格路径"
        );
    }

    #[test]
    fn code_hint_grid_merges_adjacent_words_into_registered_phrase() {
        // 根因修复验证：jieba 把「经典造型」拆成「经典」「造型」，但词典含整词
        // 「经典造型→RFmf」时，提示应合并显示整词码 RFmf，无论参考文本是否含空格
        // （空格只是分隔符，真正分词靠 jieba）。码值取自真实 yoyo-pure-km 词库。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let tsv = "经典造型\tRFmf\n经典\tRvFX\n造型\tmbfp\n";
        let dict = SchemeDict::parse(tsv);

        for (label, content) in [("含空格", "经典 造型"), ("无空格", "经典造型")] {
            let text = load_text_from_string(
                "自由发文",
                content.to_string(),
                TextSource::File,
                &LoadOptions::default(),
            )
            .unwrap();
            let session = Session::new(&text.content);
            let grid = code_hint_grid_text(&session, &text, Some(&dict), theme, false, 40)
                .unwrap_or_else(|| panic!("[{label}] 应生成词格"));
            let joined: String = grid
                .lines
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
                .collect();
            assert!(
                joined.contains("RFmf"),
                "[{label}] 应显示整词码 RFmf, 得到: {:?}",
                joined
            );
            assert!(
                !joined.contains("RvFX"),
                "[{label}] 不应再出现拆解码 RvFX, 得到: {:?}",
                joined
            );
        }
    }

    #[test]
    fn code_hint_dict_usable_detects_none_and_empty() {
        // T07：未配置方案（None）或仅 .schema.yaml 规则无 .dict.yaml 词条（entry_count==0）
        // 均视为无可用词典，应显示占位而非空白/崩溃。
        assert!(!code_hint_dict_usable(None), "未配置方案应视为无可用词典");
        let empty = SchemeDict::parse("");
        assert_eq!(empty.entry_count(), 0);
        assert!(
            !code_hint_dict_usable(Some(&empty)),
            "空词典（无词条）应视为无可用词典"
        );
        let real = SchemeDict::parse("中国\tlgy\n中\tk\n");
        assert!(real.entry_count() > 0);
        assert!(code_hint_dict_usable(Some(&real)), "含词条的词典应可用");
    }

    #[test]
    fn code_hint_placeholder_shows_guide_text() {
        // T07：无可用词典时，对照区提示区应显示明确占位（提及词典并引导配置/载入）。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let line = code_hint_placeholder_line(theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("词典"), "占位应提及词典，得到: {:?}", text);
        assert!(
            text.contains("未配置") || text.contains("未载入") || text.contains("未加载"),
            "占位应引导配置/载入词典，得到: {:?}",
            text
        );
    }

    #[test]
    fn original_line_word_set_advances_page_after_10_words() {
        // 词组赛文对照区：10 个词全对后翻到第 2 组，显示第 11-20 词。
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let no_commas = set.content_no_commas();
        let boundaries = set.word_boundaries();
        let mut session = Session::new_gated_with_words(no_commas.as_str(), true, &boundaries);
        // 打满第 1 组 10 个词的全部字符（全对）
        let first_page_char_count: usize = boundaries
            .iter()
            .take(session.group_size())
            .map(|(s, e)| e - s)
            .sum();
        let first_page_chars: String = no_commas.chars().take(first_page_char_count).collect();
        session.type_text(&first_page_chars);
        // 全对 → completed_groups 推进
        assert_eq!(session.completed_groups(), 1, "第 1 组全对应推进到 1");
        // 对照区应显示第 2 组（第 11-20 词）
        let word_text = Text {
            title: set.name().into(),
            content: no_commas.clone(),
            source: TextSource::Builtin { set },
            word_boundaries: None,
            shuffled: false,
        };
        let rendered = original_line(&session, &word_text, theme, false, None);
        assert_eq!(rendered.lines.len(), 1, "第 2 组应只有一行");
        let second_page_words = boundaries.len().min(session.group_size());
        let space_spans = rendered.lines[0]
            .spans
            .iter()
            .filter(|s| s.content == " ")
            .count();
        assert_eq!(
            space_spans,
            second_page_words - 1,
            "第 2 页应有 {} 个词间空格",
            second_page_words - 1
        );
    }

    #[test]
    fn word_set_no_commas_in_original() {
        // 词组赛文原文（Session::original）无逗号：用户无需打逗号。
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let no_commas = set.content_no_commas();
        assert!(
            !no_commas.contains('，') && !no_commas.contains(','),
            "去逗号后的内容不应含逗号"
        );
        let session = Session::new(&no_commas);
        // original_len 等于去逗号后的字符数
        assert_eq!(session.original_len(), no_commas.chars().count());
    }

    #[test]
    fn login_input_appends_chars_to_focused_field() {
        let mut form = LoginForm::default();
        login_input(
            &mut form,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        login_input(
            &mut form,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
        );
        assert_eq!(form.username, "ab");
        // 切到密码字段
        login_input(&mut form, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        login_input(
            &mut form,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(form.username, "ab");
        assert_eq!(form.password, "x");
    }

    #[test]
    fn login_input_backspace_pops_focused_field() {
        let mut form = LoginForm {
            username: "ab".into(),
            password: "cd".into(),
            focus: 1,
            ..Default::default()
        };
        login_input(
            &mut form,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(form.password, "c");
        assert_eq!(form.username, "ab");
    }

    #[test]
    fn login_input_enter_submits_esc_cancels() {
        let mut form = LoginForm {
            username: "alice".into(),
            password: "secret".into(),
            focus: 1,
            ..Default::default()
        };
        assert_eq!(
            login_input(&mut form, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            LoginAction::Submit
        );
        assert_eq!(
            login_input(&mut form, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            LoginAction::Cancel
        );
    }

    #[test]
    fn login_input_enter_moves_to_password_when_empty() {
        let mut form = LoginForm {
            username: "alice".into(),
            password: "".into(),
            focus: 0,
            ..Default::default()
        };
        assert_eq!(
            login_input(&mut form, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            LoginAction::None
        );
        assert_eq!(form.focus, 1, "回车后应切换到密码输入框");
    }

    #[test]
    fn perform_upload_auth_failure_clears_stale_token() {
        let (port, handle) = mock_server(&[(
            "/Api/Rank/uploadResult",
            r#"{"error":1,"msg":"用户名不能为空！"}"#,
        )]);
        let store = temp_token_store();
        store.save("stale-tok").unwrap();
        let app = App::new_with(
            online_text("你好"),
            store.clone(),
            ApiClient::with_base_url_and_store(
                &format!("http://127.0.0.1:{port}"),
                Some(store.clone()),
            ),
            temp_settings_store(),
            None,
        );
        let stats = app.session.finish(Duration::from_secs(10));
        let up = app.perform_upload(&stats, Duration::from_secs(10));
        handle.join().unwrap();
        assert!(matches!(
            up,
            UploadState::Failed {
                need_relogin: true,
                ..
            }
        ));
        assert!(!app.api.is_logged_in(), "客户端会话应被清理");
        assert!(store.load().is_none(), "磁盘 token 应被清空");
    }

    #[test]
    fn mask_password_hides_every_char() {
        assert_eq!(mask_password("s3cret"), "******");
        assert_eq!(mask_password("密码123"), "*****");
    }

    #[test]
    fn api_error_text_maps_categories() {
        assert_eq!(
            api_error_text(&ApiError::Transport("x".into())),
            "网络连接失败"
        );
        assert_eq!(
            api_error_text(&ApiError::Server("您的用户名或密码错误！".into())),
            "您的用户名或密码错误！"
        );
        assert_eq!(
            api_error_text(&ApiError::Parse("x".into())),
            "服务器响应异常"
        );
    }

    #[test]
    fn submit_login_rejects_empty_fields_without_network() {
        let mut app = test_app(Text {
            title: "t".into(),
            content: "c".into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        });
        app.open_login();
        app.submit_login();
        // 空用户名/密码：提前返回错误，不发起网络请求。
        let form = app.login_form.as_ref().unwrap();
        assert_eq!(form.error.as_deref(), Some("用户名和密码不能为空"));
        assert!(!form.busy);
    }

    #[test]
    fn online_shortcut_maps_number_keys_to_competitions() {
        assert_eq!(
            online_shortcut(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)),
            Some(CompetitionType::Jisu)
        );
        assert_eq!(
            online_shortcut(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE)),
            Some(CompetitionType::Jinbiao)
        );
        assert_eq!(
            online_shortcut(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE)),
            Some(CompetitionType::Jianshen)
        );
        // 带修饰键（Ctrl-1 等）不触发，普通字符也不触发。
        assert_eq!(
            online_shortcut(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            online_shortcut(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn rank_loader_fetches_in_background_and_reports_result() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        // 紧凑 fixture：含 1 行公开榜，验证后台线程拉取 + 通道回传（非阻塞）。
        let fixture = r#"{"error":0,"msg":{"total":1,"textTitle":"测试赛文","textLength":100,"rankResult":[{"rank":1,"username":"alice","speed":"100.5","inputMethod":"虎码","jianShu":"500","huiGai":"2"}],"myRankResult":[]}}"#;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf).unwrap();
            conn.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    fixture.len(),
                    fixture
                )
                .as_bytes(),
            )
            .unwrap();
        });

        let client = ApiClient::with_base_url(&format!("http://{addr}"));
        let loader = RankLoader::new();
        loader.request(client, CompetitionType::Jisu, "2026-08-30".to_string());
        let result = loader
            .receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker 应在超时前回传结果");
        assert_eq!(result.competition_type, CompetitionType::Jisu);
        assert_eq!(result.date, "2026-08-30");
        let rank = result.result.expect("应解析成功");
        assert_eq!(rank.total, 1);
        assert_eq!(rank.rank_result[0].username, "alice");
        assert!((rank.rank_result[0].speed - 100.5).abs() < 1e-9);

        server.join().unwrap();
    }

    #[test]
    fn download_online_without_token_guides_login() {
        let mut app = test_app(Text {
            title: "t".into(),
            content: "c".into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        });
        // 默认未登录（无 token、无环境变量）。
        assert!(app.token.is_none());
        app.download_online(CompetitionType::Jisu);
        assert_eq!(
            app.online_error.as_deref(),
            Some("请先登录 52dazi（Ctrl-O）")
        );
        assert!(app.online_loading.is_none());
    }

    // ---- 成绩上传（T8）----

    fn online_text(content: &str) -> Text {
        Text {
            title: "锦标赛第3279期".into(),
            content: content.into(),
            source: TextSource::Online {
                competition_type: CompetitionType::Jinbiao,
            },
            word_boundaries: None,
            shuffled: false,
        }
    }

    #[test]
    fn upload_lines_renders_each_state() {
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        // 离线未复制：不显示上传状态。
        assert!(
            upload_lines(
                &UploadState::NotApplicable { copied_stats: None },
                theme,
                ""
            )
            .is_empty()
        );
        // 离线已复制统计（自由发文/离线赛文）：不再重复展示整段统计文本（顶部摘要已含），
        // 仅保留「已复制到剪贴板」提示。
        let lines = upload_lines(
            &UploadState::NotApplicable {
                copied_stats: Some("自由发文《日常》 · WPM 85.2".into()),
            },
            theme,
            "",
        );
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert!(!text.iter().any(|s| s.contains("统计: 自由发文《日常》")));
        assert!(text.iter().any(|s| s.contains("已复制到剪贴板")));
        // 上传中。
        let lines = upload_lines(&UploadState::Uploading, theme, "");
        assert!(lines.iter().any(|l| l.to_string().contains("上传中")));
        // 成功带排名：排名 + 上传名称 + 剪贴板（分享文本不再重复展示）。
        let lines = upload_lines(
            &UploadState::Success {
                ranking: Some("5".into()),
            },
            theme,
            "虎码",
        );
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert!(
            text.iter()
                .any(|s| s.contains("第5名") && s.contains("已上传"))
        );
        assert!(text.iter().all(|s| !s.contains("WPM 85.2")));
        assert!(text.iter().any(|s| s.contains("上传名称: 虎码")));
        assert!(text.iter().any(|s| s.contains("已复制到剪贴板")));
        // 成功无排名：仍显示已上传；未配置上传名称时不显示该行。
        let lines = upload_lines(&UploadState::Success { ranking: None }, theme, "");
        assert!(lines.iter().any(|l| l.to_string().contains("已上传")));
        assert!(lines.iter().all(|l| !l.to_string().contains("上传名称")));
        // 失败：显示原因（含原始错误详情）与已复制提示，不提示重新登录。
        let lines = upload_lines(
            &UploadState::Failed {
                message: "网络连接失败".into(),
                need_relogin: false,
                detail: Some("传输层错误详情".into()),
                copied_stats: Some("离线赛文《t》 · WPM 85.2".into()),
            },
            theme,
            "",
        );
        assert!(
            lines
                .iter()
                .any(|l| l.to_string().contains("上传失败: 网络连接失败"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.to_string().contains("原始错误: 传输层错误详情"))
        );
        assert!(lines.iter().all(|l| !l.to_string().contains("重新登录")));
        // 顶部摘要已含各项指标，失败分支同样不重复展示整段统计文本。
        assert!(
            lines
                .iter()
                .all(|l| !l.to_string().contains("离线赛文《t》"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.to_string().contains("已复制到剪贴板"))
        );
        // 失败且鉴权失效：提示重新登录；原始错误降级为次要信息。
        let lines = upload_lines(
            &UploadState::Failed {
                message: "登录已失效，请重新登录".into(),
                need_relogin: true,
                detail: Some("用户名不能为空！".into()),
                copied_stats: None,
            },
            theme,
            "",
        );
        assert!(lines.iter().any(|l| l.to_string().contains("重新登录")));
        assert!(
            lines
                .iter()
                .any(|l| l.to_string().contains("原始错误: 用户名不能为空！"))
        );
    }

    #[test]
    fn perform_upload_without_token_fails_with_relogin() {
        let mut app = test_app(online_text("你好世界"));
        app.token = None;
        let stats = app.session.finish(Duration::from_secs(10));
        let up = app.perform_upload(&stats, Duration::from_secs(10));
        match up {
            UploadState::Failed {
                message,
                need_relogin,
                detail,
                copied_stats,
            } => {
                assert_eq!(message, "未登录，无法上传成绩");
                assert!(need_relogin);
                assert_eq!(detail, None);
                let cs = copied_stats.expect("未登录时也应把统计复制到剪贴板");
                assert!(cs.contains("WPM"), "统计文本应含 WPM: {cs}");
            }
            other => panic!("期望 Failed，得到 {other:?}"),
        }
    }

    #[test]
    fn perform_upload_network_failure_is_not_relogin() {
        let mut app = test_app(online_text("你好世界"));
        app.token = Some("dead-token".into());
        app.logged_in = true;
        // 指向必然拒绝连接的地址，验证网络错误被友好化、原始错误透出、统计仍复制。
        app.api = ApiClient::with_base_url("http://127.0.0.1:1");
        let stats = app.session.finish(Duration::from_secs(10));
        let up = app.perform_upload(&stats, Duration::from_secs(10));
        match up {
            UploadState::Failed {
                message,
                need_relogin,
                detail,
                copied_stats,
            } => {
                assert_eq!(message, "网络连接失败");
                assert!(!need_relogin);
                let d = detail.expect("传输错误应透出原始错误详情供诊断");
                assert!(!d.is_empty(), "原始错误详情不应为空: {d}");
                let cs = copied_stats.expect("上传失败也应把统计复制到剪贴板");
                assert!(cs.contains("WPM"), "统计文本应含 WPM: {cs}");
            }
            other => panic!("期望 Failed，得到 {other:?}"),
        }
    }

    #[test]
    fn perform_upload_success_parses_rank_and_share() {
        // 起本地 mock 服务器，返回上传成功响应。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            use std::io::{BufRead, BufReader, Read, Write};
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            // 读请求头，解析 Content-Length。
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body_buf = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body_buf);
            let body = r#"{"error":0,"msg":{"ranking":5,"rankTips":"恭喜获得第5名"}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        });
        let mut app = test_app(online_text("你好世界"));
        app.token = Some("tok".into());
        app.logged_in = true;
        app.api = ApiClient::with_base_url(&format!("http://127.0.0.1:{port}"));
        let stats = app.session.finish(Duration::from_secs(40));
        let up = app.perform_upload(&stats, Duration::from_secs(40));
        handle.join().unwrap();
        assert!(matches!(
            &up,
            UploadState::Success { ranking } if ranking.as_deref() == Some("5")
        ));
    }

    #[test]
    fn finish_typing_offline_no_upload_online_uploading() {
        // 离线：直接进入成绩视图，无上传，统计结果已复制到剪贴板。
        let mut app = test_app(Text {
            title: "t".into(),
            content: "你好".into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        });
        app.session.type_text("你好");
        assert!(app.finish_typing().is_none());
        assert!(matches!(
            &app.state,
            AppState::Finished {
                upload: UploadState::NotApplicable { copied_stats: Some(s) },
                ..
            } if s.starts_with("离线赛文《t》") && s.contains("WPM")
        ));
        // 自由发文：同样复制统计结果。
        let mut app = test_app(Text {
            title: "随笔".into(),
            content: "你好".into(),
            source: TextSource::Custom,
            word_boundaries: None,
            shuffled: false,
        });
        app.session.type_text("你好");
        assert!(app.finish_typing().is_none());
        assert!(matches!(
            &app.state,
            AppState::Finished {
                upload: UploadState::NotApplicable { copied_stats: Some(s) },
                ..
            } if s.starts_with("自由发文《随笔》")
        ));
        // 内置/剪贴板来源：复制统计结果。
        let mut app = test_app(builtin_text("你好"));
        app.session.type_text("你好");
        app.finish_typing();
        assert!(matches!(
            &app.state,
            AppState::Finished {
                upload: UploadState::NotApplicable { copied_stats: Some(s) },
                ..
            } if s.starts_with("常用单字前五百") && s.contains("WPM")
        ));
        let mut app = test_app(Text {
            title: "剪贴板赛文".into(),
            content: "你好".into(),
            source: TextSource::Clipboard,
            word_boundaries: None,
            shuffled: false,
        });
        app.session.type_text("你好");
        app.finish_typing();
        assert!(matches!(
            &app.state,
            AppState::Finished {
                upload: UploadState::NotApplicable { copied_stats: Some(s) },
                ..
            } if s.starts_with("剪贴板") && s.contains("WPM")
        ));
        // 在线：进入成绩视图并置为「上传中」，返回成绩与用时。
        let mut app = test_app(online_text("你好"));
        app.session.type_text("你好");
        assert!(app.finish_typing().is_some());
        assert!(matches!(
            &app.state,
            AppState::Finished {
                upload: UploadState::Uploading,
                ..
            }
        ));
    }

    #[test]
    fn copies_stats_to_clipboard_scopes_to_custom_and_file() {
        assert!(copies_stats_to_clipboard(TextSource::File));
        assert!(copies_stats_to_clipboard(TextSource::Custom));
        assert!(copies_stats_to_clipboard(TextSource::Clipboard));
        assert!(copies_stats_to_clipboard(TextSource::Builtin {
            set: BUILTIN_SETS[0]
        }));
        assert!(copies_stats_to_clipboard(TextSource::Online {
            competition_type: CompetitionType::Jisu
        }));
    }

    // ---- T9 成绩上传可靠性：token 校验 + 自动重登 ----

    fn file_text(content: &str) -> Text {
        Text {
            title: "t".into(),
            content: content.into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        }
    }

    fn builtin_text(content: &str) -> Text {
        Text {
            title: "常用单字前五百".into(),
            content: content.into(),
            source: TextSource::Builtin {
                set: BUILTIN_SETS[0],
            },
            word_boundaries: None,
            shuffled: false,
        }
    }

    #[test]
    fn startup_with_saved_token_keeps_login() {
        // 本地有已保存 session/token → 直接保持登录，满足 US4（免重复登录）。
        let store = temp_token_store();
        store.save("saved-token").unwrap();
        let app = App::new_with(
            file_text("你好"),
            store.clone(),
            ApiClient::with_base_url_and_store("http://127.0.0.1:1", Some(store)), // 不发网络请求
            temp_settings_store(),
            None,
        );
        assert_eq!(app.token.as_deref(), Some("saved-token"));
        assert!(app.logged_in);
        assert!(app.login_notice.is_none());
    }

    #[test]
    fn download_online_with_token_loads_content() {
        // 有 token → 直接载文（不做 isLogin 预探测），getContent 成功后替换赛文进入跟打。
        let store = temp_token_store();
        store.save("tok").unwrap();
        let (port, handle) = mock_server(&[(
            "/Api/Text/getContent",
            r#"{"error":0,"msg":{"0":"你好世界内容","7":"极速杯第3280期"}}"#,
        )]);
        let mut app = App::new_with(
            online_text("旧赛文"),
            store.clone(),
            ApiClient::with_base_url_and_store(&format!("http://127.0.0.1:{port}"), Some(store)),
            temp_settings_store(),
            None,
        );
        app.logged_in = true;
        app.online_loading = Some(CompetitionType::Jisu);
        app.download_online(CompetitionType::Jisu);
        handle.join().unwrap();
        assert!(app.online_loading.is_none());
        assert!(app.online_error.is_none());
        assert_eq!(app.text.title, "极速杯第3280期");
        assert_eq!(app.text.content, "你好世界内容");
        assert!(matches!(app.text.source, TextSource::Online { .. }));
        // 在线赛文下载成功后应先进入三秒准备倒计时，而非直接开打。
        assert!(
            matches!(app.state, AppState::Countdown { source: CountdownSource::Online, .. }),
            "在线赛文应进入倒计时（CountdownSource::Online），当前为其他状态"
        );
    }

    #[test]
    fn download_online_strips_spaces_from_content() {
        // 52dazi 赛文内容带词间空格（如「经典 造型」）→ 载文后应自动去除，
        // 得到连续正文，避免用户误打空格、也避免破坏遍码提示分词。
        let store = temp_token_store();
        store.save("tok").unwrap();
        let (port, handle) = mock_server(&[(
            "/Api/Text/getContent",
            r#"{"error":0,"msg":{"0":"智能 手表 的 科技 感 ， 复古 腕表 以 简约 设计 、 精湛 机械 工艺 、 经典 造型","7":"极速杯第3281期"}}"#,
        )]);
        let mut app = App::new_with(
            online_text("旧赛文"),
            store.clone(),
            ApiClient::with_base_url_and_store(&format!("http://127.0.0.1:{port}"), Some(store)),
            temp_settings_store(),
            None,
        );
        app.logged_in = true;
        app.online_loading = Some(CompetitionType::Jisu);
        app.download_online(CompetitionType::Jisu);
        handle.join().unwrap();
        assert!(app.online_loading.is_none());
        assert!(app.online_error.is_none());
        assert_eq!(app.text.title, "极速杯第3281期");
        assert_eq!(
            app.text.content,
            "智能手表的科技感，复古腕表以简约设计、精湛机械工艺、经典造型"
        );
        // 标题作为元数据不受影响，仍保留服务端原始值。
        assert!(!app.text.content.contains(' '));
    }

    #[test]
    fn download_online_empty_after_strip_shows_error() {
        // 服务端返回纯空白赛文 → 去空格后为空 → 不应开打退化赛文，应报错并保留旧赛文。
        let store = temp_token_store();
        store.save("tok").unwrap();
        let (port, handle) = mock_server(&[(
            "/Api/Text/getContent",
            r#"{"error":0,"msg":{"0":"   \t\n   ","7":"极速杯第3282期"}}"#,
        )]);
        let mut app = App::new_with(
            online_text("旧赛文"),
            store.clone(),
            ApiClient::with_base_url_and_store(&format!("http://127.0.0.1:{port}"), Some(store)),
            temp_settings_store(),
            None,
        );
        app.logged_in = true;
        app.online_loading = Some(CompetitionType::Jisu);
        app.download_online(CompetitionType::Jisu);
        handle.join().unwrap();
        assert!(app.online_loading.is_none());
        assert_eq!(
            app.online_error.as_deref(),
            Some("赛文内容为空或仅含空白")
        );
        // 旧赛文保持不变，且未进入倒计时。
        assert_eq!(app.text.content, "旧赛文");
        assert!(!matches!(
            app.state,
            AppState::Countdown { .. }
        ));
    }

    #[test]
    fn download_online_network_error_shows_error() {
        // getContent 网络失败 → 按网络错误提示，不误报「登录已失效」。
        let store = temp_token_store();
        store.save("tok").unwrap();
        let mut app = App::new_with(
            online_text("你好世界"),
            store.clone(),
            ApiClient::with_base_url_and_store("http://127.0.0.1:1", Some(store)),
            temp_settings_store(),
            None,
        );
        app.logged_in = true;
        app.online_loading = Some(CompetitionType::Jisu);
        app.download_online(CompetitionType::Jisu);
        assert!(app.online_loading.is_none());
        assert_eq!(app.online_error.as_deref(), Some("网络连接失败"));
    }

    #[test]
    fn perform_upload_auth_failure_shows_friendly_message() {
        // 服务器返回「用户名不能为空！」→ 主文案友好化，原始错误降级为 detail。
        let (port, handle) = mock_server(&[(
            "/Api/Rank/uploadResult",
            r#"{"error":1,"msg":"用户名不能为空！"}"#,
        )]);
        let mut app = test_app(online_text("你好世界"));
        app.token = Some("dead-token".into());
        app.logged_in = true;
        app.api = ApiClient::with_base_url(&format!("http://127.0.0.1:{port}"));
        let stats = app.session.finish(Duration::from_secs(10));
        let up = app.perform_upload(&stats, Duration::from_secs(10));
        handle.join().unwrap();
        match up {
            UploadState::Failed {
                message,
                need_relogin,
                detail,
                copied_stats,
            } => {
                assert_eq!(message, "登录已失效，请重新登录");
                assert!(need_relogin);
                assert_eq!(detail.as_deref(), Some("用户名不能为空！"));
                let cs = copied_stats.expect("上传失败也应把统计复制到剪贴板");
                assert!(cs.contains("WPM"), "统计文本应含 WPM: {cs}");
            }
            other => panic!("期望 Failed，得到 {other:?}"),
        }
    }

    #[test]
    fn submit_login_retries_pending_upload_in_finished_state() {
        let (port, handle) = mock_server(&[
            (
                "/Api/User/login",
                r#"{"error":0,"msg":{"token":"fresh-token"}}"#,
            ),
            (
                "/Api/Rank/uploadResult",
                r#"{"error":0,"msg":{"ranking":1,"rankTips":"第1名"}}"#,
            ),
        ]);
        let mut app = test_app(online_text("你好世界"));
        app.api = ApiClient::with_base_url(&format!("http://127.0.0.1:{port}"));
        let stats = app.session.finish(Duration::from_secs(30));
        app.state = AppState::Finished {
            stats: stats.clone(),
            upload: UploadState::Failed {
                message: "登录已失效，请重新登录".to_string(),
                need_relogin: true,
                detail: Some("用户名不能为空！".to_string()),
                copied_stats: None,
            },
            elapsed: Duration::from_secs(30),
        };
        app.open_login();
        if let Some(form) = app.login_form.as_mut() {
            form.username = "alice".to_string();
            form.password = "secret".to_string();
        }
        app.submit_login();
        handle.join().unwrap();
        assert_eq!(app.token.as_deref(), Some("fresh-token"));
        assert!(matches!(
            &app.state,
            AppState::Finished {
                upload: UploadState::Success { ranking, .. },
                ..
            } if ranking.as_deref() == Some("1")
        ));
    }

    // ---- Issue #38 成绩视图快捷键导航测试 ----

    #[test]
    fn finished_key_esc_restarts_offline_to_typing() {
        let mut app = test_app(file_text("测试文本"));
        app.session.type_text("测试文本");
        app.finish_typing();
        assert!(matches!(app.state, AppState::Finished { .. }));

        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let handled = handle_finished_key(&mut app, esc);
        assert!(handled);
        assert!(matches!(app.state, AppState::Typing));
        assert_eq!(app.session.len(), 0);
    }

    #[test]
    fn finished_key_esc_restarts_online_to_typing() {
        let mut app = test_app(online_text("在线文本"));
        app.session.type_text("在线文本");
        app.finish_typing();
        assert!(matches!(app.state, AppState::Finished { .. }));

        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let handled = handle_finished_key(&mut app, esc);
        assert!(handled);
        assert!(matches!(app.state, AppState::Typing));
        assert!(!app.text.is_online());
        assert_eq!(app.text.title, "常用单字前五百");
    }

    #[test]
    fn finished_key_enter_and_r_restart_offline() {
        let mut app = test_app(file_text("离线文本"));
        app.session.type_text("离线文本");
        app.finish_typing();

        // Enter 重打
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(handle_finished_key(&mut app, enter));
        assert!(matches!(app.state, AppState::Typing));
        assert_eq!(app.session.len(), 0);

        // 打完后按 r 重打
        app.session.type_text("离线文本");
        app.finish_typing();
        let r_key = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(handle_finished_key(&mut app, r_key));
        assert!(matches!(app.state, AppState::Typing));

        // 打完后按 R 重打
        app.session.type_text("离线文本");
        app.finish_typing();
        let r_upper = KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE);
        assert!(handle_finished_key(&mut app, r_upper));
        assert!(matches!(app.state, AppState::Typing));
    }

    #[test]
    fn finished_key_enter_and_r_do_not_restart_online() {
        let mut app = test_app(online_text("在线比赛"));
        app.session.type_text("在线比赛");
        app.finish_typing();
        assert!(matches!(app.state, AppState::Finished { .. }));

        // 在线赛文按 Enter：handled 但不 restart（仍为 Finished）
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(handle_finished_key(&mut app, enter));
        assert!(matches!(app.state, AppState::Finished { .. }));

        // 在线赛文按 r：handled 但不 restart
        let r_key = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(handle_finished_key(&mut app, r_key));
        assert!(matches!(app.state, AppState::Finished { .. }));
    }

    #[test]
    fn finished_key_navigation_shortcuts() {
        let mut app = test_app(file_text("文本"));
        app.finish_typing();

        // f 打开载文浏览
        let f_key = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(handle_finished_key(&mut app, f_key));
        assert!(matches!(app.state, AppState::Browsing));

        // 回到 Finished 测试 b
        app.finish_typing();
        let b_key = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE);
        assert!(handle_finished_key(&mut app, b_key));
        assert!(matches!(app.state, AppState::BrowsingBuiltin));

        // 回到 Finished 测试 o (设置)
        app.finish_typing();
        let o_key = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
        assert!(handle_finished_key(&mut app, o_key));
        assert!(matches!(app.state, AppState::Settings));

        // 回到 Finished 测试 i (自由发文)
        app.finish_typing();
        let i_key = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE);
        assert!(handle_finished_key(&mut app, i_key));
        assert!(app.free_input_modal.is_some());
        app.close_free_input();

        // 回到 Finished 测试 p (剪贴板发文)
        app.finish_typing();
        let p_key = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        assert!(handle_finished_key(&mut app, p_key));

        // 回到 Finished 测试 s (数据统计)
        app.finish_typing();
        let s_key = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(handle_finished_key(&mut app, s_key));
        assert!(matches!(app.state, AppState::Stats(_)));

        // 回到 Finished 测试 u (登录)
        app.finish_typing();
        let u_key = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        assert!(handle_finished_key(&mut app, u_key));
        assert!(app.login_form.is_some());
        app.login_form = None;

        // 回到 Finished 测试 r (重打)
        app.finish_typing();
        let r_key = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(handle_finished_key(&mut app, r_key));
        assert!(matches!(app.state, AppState::Typing));
    }

    #[test]
    fn finished_key_unhandled_returns_false() {
        let mut app = test_app(file_text("文本"));
        app.finish_typing();

        let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!handle_finished_key(&mut app, space));
        assert!(matches!(app.state, AppState::Finished { .. }));
    }

    // ---- Issue #40 / #41 速度图表与错字时间线渲染测试 ----

    /// 渲染整个终端缓冲区并拼接为去除空格的纯文本（供结果视图断言）。
    fn render_buffer_text(app: &App, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        content.replace(' ', "")
    }

    #[test]
    fn render_result_view_shows_copied_stats_feedback() {
        // 离线赛文完成后：成绩视图不再重复展示整段统计文本（顶部摘要已含），
        // 但应保留「已复制到剪贴板」反馈行。
        let mut app = test_app(file_text("你好世界"));
        app.session.type_text_at("你", Duration::from_secs(1));
        app.session.type_text_at("好", Duration::from_secs(2));
        app.finish_typing();

        let content = render_buffer_text(&app, 100, 30);
        assert!(
            !content.contains("统计:离线赛文《t》"),
            "不应重复展示整段统计文本"
        );
        assert!(content.contains("已复制到剪贴板"), "应显示已复制提示");
    }

    #[test]
    fn render_result_view_hides_duplicated_online_share_text() {
        // 在线赛文上传成功后：整段分享文本不再重复展示（顶部摘要已含全部指标），
        // 但排名与「已复制到剪贴板」行仍应可见
        // （历史 bug：固定 summary 高度不足导致这两行被裁剪，宽度自适应高度后修复）。
        let mut app = test_app(online_text("你好世界"));
        app.session.type_text("你好世界");
        app.state = AppState::Finished {
            stats: app.session.finish(Duration::from_secs(4)),
            upload: UploadState::Success {
                ranking: Some("5".into()),
            },
            elapsed: Duration::from_secs(4),
        };

        let content = render_buffer_text(&app, 100, 30);
        assert!(!content.contains("WPM85.2"), "不应重复展示分享文本里的指标");
        assert!(content.contains("排名:第5名·已上传"), "应显示排名行");
        assert!(content.contains("已复制到剪贴板"), "应显示已复制提示");
    }

    /// 构造带 `n` 处错字的成绩视图：赛文由 `n` 个互不相同的汉字组成，逐字打错。
    fn finished_app_with_errors(n: usize) -> App {
        let content: String = "你好世界测试代码编程打字练习".chars().take(n).collect();
        let mut app = test_app(file_text(&content));
        for i in 0..n {
            app.session
                .type_text_at("四", Duration::from_secs_f64(i as f64 + 1.0));
        }
        app.finish_typing();
        app
    }

    /// 取出成绩视图里的统计副本（`AppState` 未实现 `Debug`，非成绩视图直接 panic）。
    fn finished_stats(app: &App) -> Stats {
        match &app.state {
            AppState::Finished { stats, .. } => stats.clone(),
            _ => panic!("expected finished state"),
        }
    }

    #[test]
    fn error_timeline_lines_renders_selection_and_scroll_window() {
        let app = finished_app_with_errors(12);
        let stats = finished_stats(&app);
        let palette = app.palette();
        assert_eq!(stats.error_points.len(), 12);

        // 无错字：单行提示。
        let mut empty = stats.clone();
        empty.error_points.clear();
        assert_eq!(
            error_timeline_lines(&empty, 0, 0, 8, &palette)
                .iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>(),
            vec![" 全对无错字".to_string()]
        );

        // 容量裁剪 + 选中项光标。
        let lines: Vec<String> = error_timeline_lines(&stats, 0, 0, 3, &palette)
            .iter()
            .map(|l| l.to_string())
            .collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("▶#1"), "首行应为选中态: {}", lines[0]);
        assert!(lines[1].starts_with(" #2"), "非选中行无光标: {}", lines[1]);

        // 选中项在窗口外时，滚动偏移自动修正到「刚好可见」。
        let lines: Vec<String> = error_timeline_lines(&stats, 5, 0, 3, &palette)
            .iter()
            .map(|l| l.to_string())
            .collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[2].starts_with("▶#6"), "应滚到选中项: {}", lines[2]);
        assert!(lines[0].starts_with(" #4"), "窗口应整体下移: {}", lines[0]);

        // 末尾：滚动偏移被总数夹取，选中项始终在末行。
        let lines: Vec<String> = error_timeline_lines(&stats, 11, 99, 3, &palette)
            .iter()
            .map(|l| l.to_string())
            .collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[2].starts_with("▶#12"), "末条应可见: {}", lines[2]);

        // 选中下标越界时夹取到末条，不 panic。
        let lines: Vec<String> = error_timeline_lines(&stats, 99, 0, 3, &palette)
            .iter()
            .map(|l| l.to_string())
            .collect();
        assert!(lines[2].starts_with("▶#12"), "越界下标应夹取: {}", lines[2]);
    }

    #[test]
    fn finished_key_navigates_error_timeline() {
        let mut app = finished_app_with_errors(12);
        assert_eq!(app.error_point_count(), Some(12));
        assert_eq!(app.error_point_selected, 0);

        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        for _ in 0..3 {
            assert!(handle_finished_key(&mut app, down));
        }
        assert_eq!(app.error_point_selected, 3);
        assert!(handle_finished_key(&mut app, up));
        assert_eq!(app.error_point_selected, 2);
        // j / k 与方向键等价。
        assert!(handle_finished_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)
        ));
        assert_eq!(app.error_point_selected, 3);
        assert!(handle_finished_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)
        ));
        assert_eq!(app.error_point_selected, 2);

        // Home/End（含 g/G）跳到首末条，越界保持不动。
        assert!(handle_finished_key(
            &mut app,
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE)
        ));
        assert_eq!(app.error_point_selected, 11);
        // 末条选中时滚动窗口跟到末屏（12 条 / 一屏 8 条 → 偏移 4）。
        assert_eq!(app.error_point_scroll, 4);
        assert!(handle_finished_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE)
        ));
        assert_eq!(app.error_point_selected, 11, "末条继续下移应保持不动");
        assert!(handle_finished_key(
            &mut app,
            KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)
        ));
        assert_eq!(app.error_point_selected, 0);
        assert_eq!(app.error_point_scroll, 0);
        assert!(handle_finished_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)
        ));
        assert_eq!(app.error_point_selected, 0, "首条继续上移应保持不动");

        // 翻页按一屏 8 条移动，末页夹取到末条。
        assert!(handle_finished_key(
            &mut app,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)
        ));
        assert_eq!(app.error_point_selected, 8);
        assert!(handle_finished_key(
            &mut app,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)
        ));
        assert_eq!(app.error_point_selected, 11);
        assert!(handle_finished_key(
            &mut app,
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)
        ));
        assert_eq!(app.error_point_selected, 3);
    }

    #[test]
    fn render_result_view_scrolls_error_timeline() {
        let mut app = finished_app_with_errors(12);
        // 一屏 8 条：首屏只显示第 1~8 处，标题里提示总数与当前位置。
        let content = render_buffer_text(&app, 100, 30);
        assert!(
            content.contains("#1") && content.contains("#8"),
            "{content}"
        );
        assert!(!content.contains("#9"), "首屏不应出现第 9 处: {content}");
        assert!(content.contains("1/12"), "标题应显示当前位置: {content}");

        // 跳到末条后窗口滚动，早期条目被移出可见区。
        assert!(handle_finished_key(
            &mut app,
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE)
        ));
        let content = render_buffer_text(&app, 100, 30);
        assert!(content.contains("12/12"), "标题应跟随选中项: {content}");
        assert!(content.contains("#12"), "末条应可见: {content}");
        assert!(!content.contains("#4"), "第 4 处应滚出可见区: {content}");
    }

    #[test]
    fn clamp_error_scroll_keeps_selection_visible() {
        // 一屏 3 条 / 共 12 条：偏移上限为 9。
        assert_eq!(clamp_error_scroll(0, 9, 12, 3), 0, "选中首条应回到顶部");
        assert_eq!(clamp_error_scroll(5, 0, 12, 3), 3, "选中项在窗口下方应下滚");
        assert_eq!(clamp_error_scroll(11, 99, 12, 3), 9, "偏移越界应夹到上限");
        assert_eq!(clamp_error_scroll(5, 4, 12, 3), 4, "已可见时保持不动");
        // 容量为 0（终端被挤压到只剩边框）与无错字时都退回 0。
        assert_eq!(clamp_error_scroll(5, 2, 12, 0), 0);
        assert_eq!(clamp_error_scroll(0, 3, 0, 3), 0);
    }

    #[test]
    fn render_result_view_keeps_timeline_visible_on_short_terminal() {
        // 15 行终端：图表会挤掉独立时间线区块，降级为「摘要 + 时间线」单块，
        // 错字条目仍须可见且可翻看（历史问题：可见区被压成 0 行，整块空白）。
        let mut app = finished_app_with_errors(12);
        let content = render_buffer_text(&app, 100, 15);
        assert!(content.contains("▶#1"), "应显示带光标的首条: {content}");
        assert!(content.contains("#3"), "降级视图应保留多条: {content}");

        // 降级视图下方向键依然能滚动选中项。
        assert!(handle_finished_key(
            &mut app,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)
        ));
        assert_eq!(app.error_point_selected, 1);
        let content = render_buffer_text(&app, 100, 15);
        assert!(content.contains("▶#2"), "降级视图应跟随选中项: {content}");
    }

    #[test]
    fn render_result_view_renders_chart_and_errors() {
        let mut app = test_app(file_text("你好世界"));
        app.session.type_text_at("你", Duration::from_secs(1));
        app.session.type_text_at("四", Duration::from_secs(2));
        app.session.backspace_at(Duration::from_secs_f64(2.5));
        app.session.type_text_at("好", Duration::from_secs(3));
        app.session.type_text_at("世", Duration::from_secs(4));
        app.session.type_text_at("界", Duration::from_secs(5));
        app.finish_typing();

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        let clean_content = content.replace(' ', "");
        assert!(clean_content.contains("成绩"));
        assert!(clean_content.contains("WPM速度曲线"));
        assert!(clean_content.contains("错字时间线"));
        assert!(clean_content.contains("回改:'四'"));
        assert!(clean_content.contains("四"));
        assert!(clean_content.contains("•"));
        assert!(clean_content.contains("Esc") && clean_content.contains("返回"));

        // 验证速度曲线图绘图区域（y: 5..17, x: 12..95）的所有单元格背景色与主题 palette.bg 一致
        let palette = app.palette();
        for y in 5..17 {
            for x in 12..95 {
                let cell = &buffer[(x, y)];
                // 跳过宽字符（如中文字符 '四'）的占位后导单元格
                let is_wide_char_tail = cell.symbol() == " "
                    && cell.fg == Color::Reset
                    && x > 0
                    && buffer[(x - 1, y)]
                        .symbol()
                        .chars()
                        .next()
                        .is_some_and(|c| !c.is_ascii());
                if !is_wide_char_tail {
                    assert_eq!(
                        cell.bg,
                        palette.bg,
                        "Chart cell at ({x}, {y}) bg mismatch with theme palette.bg, sym={:?}, fg={:?}, bg={:?}",
                        cell.symbol(),
                        cell.fg,
                        cell.bg
                    );
                }
            }
        }
    }

    #[test]
    fn render_result_view_shows_all_wrong_chars_when_multi_char_word_errors_occur() {
        let mut app = test_app(file_text("你好世界测试代码编程"));
        app.session.type_text_at("你好", Duration::from_secs(1));
        app.session.type_text_at("中华", Duration::from_secs(2));
        app.session.type_text_at("人民", Duration::from_secs(3));
        app.session.type_text_at("代码", Duration::from_secs(4));
        app.session.type_text_at("编程", Duration::from_secs(5));
        app.finish_typing();

        let stats = if let AppState::Finished { stats, .. } = &app.state {
            stats.clone()
        } else {
            panic!("expected finished state");
        };
        assert_eq!(stats.wrong_total, 4);
        assert_eq!(stats.wrong_chars, 4);
        assert_eq!(stats.edits, 0);

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let chart_content = (4..23)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        let clean_chart = chart_content.replace(' ', "");
        assert!(clean_chart.contains("中"), "图表区应包含错字'中'");
        assert!(clean_chart.contains("华"), "图表区应包含错字'华'");
        assert!(clean_chart.contains("人"), "图表区应包含错字'人'");
        assert!(clean_chart.contains("民"), "图表区应包含错字'民'");
        assert!(clean_chart.contains("•"), "图表区应包含打错点标记");
    }

    #[test]
    fn render_result_view_error_dot_count_matches_actual_error_count() {
        let mut app = test_app(file_text("一二三四五六"));
        app.session.type_text_at("一二", Duration::from_secs(1));
        app.session.type_text_at("错", Duration::from_secs(2));
        app.session.type_text_at("误", Duration::from_secs(3));
        app.session.type_text_at("五六", Duration::from_secs(4));
        app.finish_typing();

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let buffer = terminal.backend().buffer();

        let mut dot_count = 0;
        for y in 4..23 {
            for x in 0..buffer.area.width {
                if buffer[(x, y)].symbol() == "•" {
                    dot_count += 1;
                }
            }
        }
        assert_eq!(
            dot_count, 2,
            "打错 2 个字，图表区应恰好渲染 2 个打错红点，实际渲染了 {}",
            dot_count
        );
    }

    #[test]
    fn render_result_view_compact_mode_for_small_terminals() {
        let mut app = test_app(file_text("你好世界"));
        app.finish_typing();

        let backend = ratatui::backend::TestBackend::new(60, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        let clean_content = content.replace(' ', "");
        assert!(clean_content.contains("成绩"));
        assert!(clean_content.contains("Esc") && clean_content.contains("返回"));
    }

    // ---- Issues #45 / #46 / #47 自由发文、剪贴板发文与功能栏导航测试 ----

    #[test]
    fn free_input_modal_input_flow_and_submission() {
        let mut modal = FreeInputModal::new();
        assert_eq!(modal.focus, FREE_INPUT_FOCUS_CONTENT);

        // 默认按 Ctrl-Enter 提交空内容 -> 报错
        let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        let action = free_input_modal_input(&mut modal, ctrl_enter);
        assert_eq!(action, FreeInputAction::None);
        assert!(modal.error.is_some());

        // 输入正文
        let char_a = KeyEvent::new(KeyCode::Char('我'), KeyModifiers::NONE);
        free_input_modal_input(&mut modal, char_a);
        let char_b = KeyEvent::new(KeyCode::Char('打'), KeyModifiers::NONE);
        free_input_modal_input(&mut modal, char_b);
        assert_eq!(modal.content, "我打");

        // Tab 切换到保存勾选
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        free_input_modal_input(&mut modal, tab);
        assert_eq!(modal.focus, FREE_INPUT_FOCUS_SAVE_CHECKBOX);

        // 空格切换勾选
        let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        free_input_modal_input(&mut modal, space);
        assert!(modal.save_to_file);

        // Tab 切换到保存路径
        free_input_modal_input(&mut modal, tab);
        assert_eq!(modal.focus, FREE_INPUT_FOCUS_SAVE_PATH);
        assert_eq!(modal.save_path, "./自由发文.txt");

        // Tab 切换到确认发文按钮
        free_input_modal_input(&mut modal, tab);
        assert_eq!(modal.focus, FREE_INPUT_FOCUS_SUBMIT_BTN);

        // Tab 切换到取消按钮
        free_input_modal_input(&mut modal, tab);
        assert_eq!(modal.focus, FREE_INPUT_FOCUS_CANCEL_BTN);

        // 再次 Tab 循环回标题
        free_input_modal_input(&mut modal, tab);
        assert_eq!(modal.focus, FREE_INPUT_FOCUS_TITLE);

        // 修改标题
        let backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        free_input_modal_input(&mut modal, backspace);
        let char_c = KeyEvent::new(KeyCode::Char('章'), KeyModifiers::NONE);
        free_input_modal_input(&mut modal, char_c);
        assert_eq!(modal.title, "自由发章");

        // 测试多种发文快捷键：
        // 1. Ctrl-S (大写/小写)
        let ctrl_s = KeyEvent::new(KeyCode::Char('S'), KeyModifiers::CONTROL);
        let submit_action = free_input_modal_input(&mut modal, ctrl_s);
        assert_eq!(
            submit_action,
            FreeInputAction::Submit {
                title: "自由发章".to_string(),
                content: "我打".to_string(),
                save: Some(PathBuf::from("./自由发文.txt")),
            }
        );

        // 2. Ctrl-Enter 快捷键
        let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        assert_eq!(
            free_input_modal_input(&mut modal, ctrl_enter),
            FreeInputAction::Submit {
                title: "自由发章".to_string(),
                content: "我打".to_string(),
                save: Some(PathBuf::from("./自由发文.txt")),
            }
        );

        // 3. Alt-Enter 快捷键
        let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        assert_eq!(
            free_input_modal_input(&mut modal, alt_enter),
            FreeInputAction::Submit {
                title: "自由发章".to_string(),
                content: "我打".to_string(),
                save: Some(PathBuf::from("./自由发文.txt")),
            }
        );

        // 4. Ctrl-Enter 快捷键
        assert_eq!(
            free_input_modal_input(&mut modal, ctrl_enter),
            FreeInputAction::Submit {
                title: "自由发章".to_string(),
                content: "我打".to_string(),
                save: Some(PathBuf::from("./自由发文.txt")),
            }
        );

        // 5. 焦点移动到提交按钮并按 Enter 提交
        modal.focus = FREE_INPUT_FOCUS_SUBMIT_BTN;
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            free_input_modal_input(&mut modal, enter),
            FreeInputAction::Submit {
                title: "自由发章".to_string(),
                content: "我打".to_string(),
                save: Some(PathBuf::from("./自由发文.txt")),
            }
        );

        // 6. 焦点移动到取消按钮并按 Enter 取消
        modal.focus = FREE_INPUT_FOCUS_CANCEL_BTN;
        assert_eq!(
            free_input_modal_input(&mut modal, enter),
            FreeInputAction::Cancel
        );

        // 7. Esc 取消
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            free_input_modal_input(&mut modal, esc),
            FreeInputAction::Cancel
        );
    }

    #[test]
    fn app_submit_free_input_creates_custom_session() {
        let mut app = test_app(file_text("初始文本"));
        let dir = temp_dir("custom_save");
        let temp_file = dir.join("saved_custom.txt");

        app.open_free_input();
        assert!(app.free_input_modal.is_some());

        app.submit_free_input(
            "测试自定标题".to_string(),
            "自定义打字内容第一行\n第二行".to_string(),
            Some(temp_file.clone()),
        );

        assert!(app.free_input_modal.is_none());
        assert_eq!(app.text.title, "测试自定标题");
        assert_eq!(app.text.content, "自定义打字内容第一行\n第二行");
        assert_eq!(app.text.source, TextSource::Custom);
        assert_eq!(app.session.len(), 0);
        assert_eq!(app.sidebar_notice.as_deref(), Some("已载入: 测试自定标题"));

        // 验证文件是否已保存到本地
        let file_content = fs::read_to_string(&temp_file).expect("文件应存在");
        assert_eq!(file_content, "自定义打字内容第一行\n第二行");
        let _ = fs::remove_file(&temp_file);

        // 验证重打
        app.session.type_text("自定义打字内容");
        assert_eq!(app.session.len(), 7);
        app.restart();
        assert_eq!(app.session.len(), 0);
        assert_eq!(app.text.title, "测试自定标题");
    }

    #[test]
    fn app_pause_and_resume_freezes_timer() {
        let mut app = test_app(file_text("测试计时文本"));
        assert!(!app.paused);
        assert_eq!(app.current_elapsed(), Duration::ZERO);

        // 开始打字
        app.touch_typing();
        assert!(!app.paused);
        assert!(app.active_start.is_some());

        // 暂停
        app.pause();
        assert!(app.paused);
        assert!(app.active_start.is_none());
        let paused_elapsed = app.current_elapsed();

        // 恢复（续接计时，逻辑等价于倒计时结束后的恢复）
        app.complete_resume_countdown();
        assert!(!app.paused);
        assert!(app.active_start.is_some());
        assert!(app.current_elapsed() >= paused_elapsed);
    }

    #[test]
    fn sidebar_menu_navigation_and_activation() {
        let mut app = test_app(file_text("测试导航"));
        assert_eq!(app.sidebar_selected, 0);

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        // 激活 自由发文 (index 2)
        app.sidebar_selected = 2;
        assert_eq!(
            SIDEBAR_MENU_ITEMS[app.sidebar_selected],
            SidebarMenuItem::FreeInput
        );
        activate_sidebar_menu_item(&mut app, &mut terminal).unwrap();
        assert!(app.free_input_modal.is_some());
        app.close_free_input();

        // 激活 载入文件 (index 0)
        app.sidebar_selected = 0;
        activate_sidebar_menu_item(&mut app, &mut terminal).unwrap();
        assert!(matches!(app.state, AppState::Browsing));
        app.state = AppState::Typing;

        // 激活 内置赛文 (index 1)
        app.sidebar_selected = 1;
        activate_sidebar_menu_item(&mut app, &mut terminal).unwrap();
        assert!(matches!(app.state, AppState::BrowsingBuiltin));
        app.state = AppState::Typing;

        // 激活 在线排行榜 (index 7)：后台线程会拉取，测试中指向死地址避免真实网络。
        app.api = ApiClient::with_base_url("http://127.0.0.1:1");
        app.sidebar_selected = 7;
        activate_sidebar_menu_item(&mut app, &mut terminal).unwrap();
        assert!(matches!(app.state, AppState::OnlineRank(_)));
        app.state = AppState::Typing;

        // 激活 数据统计 (index 8)
        app.sidebar_selected = 8;
        activate_sidebar_menu_item(&mut app, &mut terminal).unwrap();
        assert!(matches!(app.state, AppState::Stats(_)));
        app.state = AppState::Typing;

        // 激活 设置 (index 9)
        app.sidebar_selected = 9;
        activate_sidebar_menu_item(&mut app, &mut terminal).unwrap();
        assert!(matches!(app.state, AppState::Settings));
        app.state = AppState::Typing;
    }

    #[test]
    fn render_online_rank_view_renders_four_column_table() {
        let data = CompetitionRank {
            rank_result: vec![
                dazitui_core::CompetitionRankRow {
                    rank: 1,
                    username: "虹".into(),
                    speed: 197.18,
                    input_method: "虎码".into(),
                    ..Default::default()
                },
                dazitui_core::CompetitionRankRow {
                    rank: 2,
                    username: "beiyi".into(),
                    speed: 190.31,
                    ..Default::default()
                },
            ],
            my_rank_result: vec![],
            total: 2,
            text_title: "t".into(),
            text_length: 100,
        };
        let state = OnlineRankState {
            active_tab: CompetitionType::Jisu,
            date: "2026-08-30".into(),
            boards: HashMap::from([(
                CompetitionType::Jisu,
                RankBoard {
                    data: Some(data),
                    loading: false,
                    error: None,
                    scroll: 0,
                },
            )]),
            error: None,
        };
        let mut app = test_app(file_text("x"));
        app.state = AppState::OnlineRank(state);

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let clean = content.replace(' ', "");
        assert!(clean.contains("排名"), "应包含表头「排名」");
        assert!(clean.contains("用户名"), "应包含表头「用户名」");
        assert!(clean.contains("速度"), "应包含表头「速度」");
        assert!(clean.contains("输入法"), "应包含表头「输入法」");
        assert!(clean.contains("虹"), "应包含榜首用户名「虹」");
        assert!(clean.contains("197.18"), "应渲染榜首速度");
    }

    #[test]
    fn render_online_rank_view_renders_bottom_help_bar() {
        let data = CompetitionRank {
            rank_result: vec![dazitui_core::CompetitionRankRow {
                rank: 1,
                username: "虹".into(),
                speed: 197.18,
                input_method: "虎码".into(),
                ..Default::default()
            }],
            my_rank_result: vec![],
            total: 1,
            text_title: "t".into(),
            text_length: 100,
        };
        let state = OnlineRankState {
            active_tab: CompetitionType::Jisu,
            date: "2026-08-30".into(),
            boards: HashMap::from([(
                CompetitionType::Jisu,
                RankBoard {
                    data: Some(data),
                    loading: false,
                    error: None,
                    scroll: 0,
                },
            )]),
            error: None,
        };
        let mut app = test_app(file_text("x"));
        app.state = AppState::OnlineRank(state);

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let clean = content.replace(' ', "");
        assert!(clean.contains("快捷键"), "排行榜应渲染底部快捷键栏标题「快捷键」");
        assert!(clean.contains("返回"), "快捷键栏应包含「返回」提示");
        assert!(clean.contains("刷新"), "快捷键栏应包含「刷新」提示");
        assert!(clean.contains("切换"), "快捷键栏应包含「切换」提示");
    }

    /// 列定制弹窗应列出全部四列，并据 `settings.rank_columns` 显隐渲染 `[x]`/`[ ]`。
    #[test]
    fn rank_column_modal_lists_columns_with_checkbox_state() {
        let data = CompetitionRank {
            rank_result: vec![dazitui_core::CompetitionRankRow {
                rank: 1,
                username: "虹".into(),
                speed: 197.18,
                input_method: "虎码".into(),
                ..Default::default()
            }],
            my_rank_result: vec![],
            total: 1,
            text_title: "t".into(),
            text_length: 100,
        };
        let state = OnlineRankState {
            active_tab: CompetitionType::Jisu,
            date: "2026-08-30".into(),
            boards: HashMap::from([(
                CompetitionType::Jisu,
                RankBoard {
                    data: Some(data),
                    loading: false,
                    error: None,
                    scroll: 0,
                },
            )]),
            error: None,
        };
        let mut app = test_app(file_text("x"));
        app.state = AppState::OnlineRank(state);
        // 默认全显：把「输入法」藏起来，验证勾选框反映显隐。
        app.settings.rank_columns.set_visible(RankColumnId::InputMethod, false);
        app.rank_column_modal = Some(RankColumnModal::default());

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let clean = content.replace(' ', "");
        assert!(clean.contains("自定义列"), "弹窗应显示标题「自定义列」");
        for title in ["排名", "用户名", "速度(WPM)", "输入法"] {
            assert!(clean.contains(title), "弹窗应列出列「{}」", title);
        }
        assert!(clean.contains("[x]"), "可见列应渲染为已勾选 `[x]`");
        // 注意：`[ ]` 中间带空格，去除空白后变为 `[]`。
        assert!(clean.contains("[]"), "隐藏列「输入法」应渲染为未勾选 `[ ]`");
        // 快捷键提示按 `◖key◗desc` 渲染，键与描述间有符号，故分开断言。
        assert!(clean.contains("Space"), "弹窗提示应含 Space 键");
        assert!(clean.contains("切换"), "弹窗提示应含「切换」描述");
        assert!(clean.contains("Esc"), "弹窗提示应含 Esc 键");
        assert!(clean.contains("完成"), "弹窗提示应含「完成」描述");
    }

    /// `rank_column_modal_input` 的 Space 应切换显隐写入 `config`，且至少保留 1 列；Esc 返回 Close。
    #[test]
    fn rank_column_modal_space_toggles_visibility_and_keeps_at_least_one() {
        let mut config = RankColumnConfig::default();
        assert_eq!(config.visible_count(), 4, "默认应四列全显");

        // 选中第 3 列（速度，下标 2），Space 隐藏它。
        let mut modal = RankColumnModal { selected: 2 };
        let action = rank_column_modal_input(
            &mut modal,
            &mut config,
            KeyEvent::from(KeyCode::Char(' ')),
        );
        assert!(matches!(action, RankColumnModalAction::None));
        assert!(!config.is_visible(RankColumnId::Speed), "Space 后应隐藏速度列");
        assert_eq!(config.visible_count(), 3);

        // 再按 Space 恢复显示。
        rank_column_modal_input(
            &mut modal,
            &mut config,
            KeyEvent::from(KeyCode::Char(' ')),
        );
        assert!(config.is_visible(RankColumnId::Speed), "再次 Space 应恢复显示");
        assert_eq!(config.visible_count(), 4);

        // 隐藏到仅剩 1 列后，Space 隐藏最后一列应被拒绝。
        config.set_visible(RankColumnId::Rank, false);
        config.set_visible(RankColumnId::Username, false);
        config.set_visible(RankColumnId::Speed, false);
        assert_eq!(config.visible_count(), 1, "应只剩输入法列");
        let mut last = RankColumnModal { selected: 3 };
        rank_column_modal_input(
            &mut last,
            &mut config,
            KeyEvent::from(KeyCode::Char(' ')),
        );
        assert!(
            config.is_visible(RankColumnId::InputMethod),
            "至少应保留 1 列可见，最后一列不允许被隐藏"
        );
        assert_eq!(config.visible_count(), 1);

        // ↑/↓ 在边界取模回绕。
        let mut wrap = RankColumnModal { selected: 0 };
        rank_column_modal_input(&mut wrap, &mut config, KeyEvent::from(KeyCode::Up));
        assert_eq!(wrap.selected, RankColumnId::ALL.len() - 1, "上移到顶应回绕到末列");
        rank_column_modal_input(&mut wrap, &mut config, KeyEvent::from(KeyCode::Down));
        assert_eq!(wrap.selected, 0, "下移到末应回绕到首列");

        // Esc 返回 Close 动作。
        let mut esc_modal = RankColumnModal::default();
        let close = rank_column_modal_input(
            &mut esc_modal,
            &mut config,
            KeyEvent::from(KeyCode::Esc),
        );
        assert!(matches!(close, RankColumnModalAction::Close));
    }

    /// 隐藏某列后，榜单表头不再渲染该列标题，其余可见列仍正常出现（#108 列定制）。
    #[test]
    fn render_online_rank_view_only_renders_visible_columns() {
        let data = CompetitionRank {
            rank_result: vec![dazitui_core::CompetitionRankRow {
                rank: 1,
                username: "虹".into(),
                speed: 197.18,
                input_method: "虎码".into(),
                ..Default::default()
            }],
            my_rank_result: vec![],
            total: 1,
            text_title: "t".into(),
            text_length: 100,
        };
        let state = OnlineRankState {
            active_tab: CompetitionType::Jisu,
            date: "2026-08-30".into(),
            boards: HashMap::from([(
                CompetitionType::Jisu,
                RankBoard {
                    data: Some(data),
                    loading: false,
                    error: None,
                    scroll: 0,
                },
            )]),
            error: None,
        };
        let mut app = test_app(file_text("x"));
        app.state = AppState::OnlineRank(state);
        // 隐藏「输入法」列，保留其余三列。
        app.settings.rank_columns.set_visible(RankColumnId::InputMethod, false);
        assert_eq!(app.settings.rank_columns.visible_count(), 3);

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let clean = content.replace(' ', "");
        assert!(
            !clean.contains("输入法"),
            "隐藏「输入法」列后表头不应再出现该标题"
        );
        assert!(clean.contains("用户名"), "其余可见列（用户名）应仍出现");
        assert!(clean.contains("速度(WPM)"), "其余可见列（速度）应仍出现");
    }

    #[test]
    fn render_online_rank_view_shows_my_rank_and_highlights_row() {
        let data = CompetitionRank {
            rank_result: vec![
                dazitui_core::CompetitionRankRow {
                    rank: 1,
                    username: "a".into(),
                    speed: 100.0,
                    ..Default::default()
                },
                dazitui_core::CompetitionRankRow {
                    rank: 2,
                    username: "我的账号".into(),
                    speed: 88.5,
                    ..Default::default()
                },
            ],
            my_rank_result: vec![dazitui_core::CompetitionRankRow {
                rank: 2,
                username: "我的账号".into(),
                speed: 88.5,
                ..Default::default()
            }],
            total: 59,
            text_title: "t".into(),
            text_length: 100,
        };
        let state = OnlineRankState {
            active_tab: CompetitionType::Jisu,
            date: "2026-08-30".into(),
            boards: HashMap::from([(
                CompetitionType::Jisu,
                RankBoard {
                    data: Some(data),
                    loading: false,
                    error: None,
                    scroll: 0,
                },
            )]),
            error: None,
        };
        let mut app = test_app(file_text("x"));
        app.logged_in = true;
        app.state = AppState::OnlineRank(state);

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let clean = content.replace(' ', "");
        assert!(clean.contains("我第2名"), "应显示「我第 2 名」名次条");
        assert!(clean.contains("共59人"), "应显示总人数");

        // 当前用户行（用户名「我的账号」）应高亮：其关键字形单元格前景为 accent。
        // 注意：宽字符在 TestBackend 中按单字占格、相邻空格为占位，故按字形而非 substring 校验；
        // 这些用户名字形仅出现在当前用户行，因此全缓冲范围内均应带 accent 前景。
        let palette = app.palette();
        let want_glyphs: &[&str] = &["我", "的", "账", "号"];
        let mut highlighted = true;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let sym = buffer[(x, y)].symbol();
                if want_glyphs.contains(&sym)
                    && buffer[(x, y)].style().fg != Some(palette.accent)
                {
                    highlighted = false;
                }
            }
        }
        assert!(highlighted, "当前用户行应高亮（前景 accent）");
    }

    // #107：网络失败态渲染「加载失败 …（按 R 重试）」提示。
    #[test]
    fn render_online_rank_view_shows_error_and_retry() {
        let state = OnlineRankState {
            active_tab: CompetitionType::Jisu,
            date: "2026-08-30".into(),
            boards: HashMap::from([(
                CompetitionType::Jisu,
                RankBoard {
                    data: None,
                    loading: false,
                    error: Some("连接超时".into()),
                    scroll: 0,
                },
            )]),
            error: None,
        };
        let mut app = test_app(file_text("x"));
        app.logged_in = false;
        app.state = AppState::OnlineRank(state);

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let clean: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .replace(' ', "");
        assert!(clean.contains("加载失败：连接超时"), "应展示具体错误");
        assert!(clean.contains("（按R重试）") || clean.contains("重试"), "应提示按 R 重试");
    }

    // #107：未登录降级为公开榜提示，且不展示个人名次条。
    #[test]
    fn render_online_rank_view_logged_out_shows_public_board_note() {
        let data = CompetitionRank {
            rank_result: vec![dazitui_core::CompetitionRankRow {
                rank: 1,
                username: "路人甲".into(),
                speed: 100.0,
                ..Default::default()
            }],
            my_rank_result: vec![], // 未登录：服务端不回填个人行
            total: 59,
            text_title: "t".into(),
            text_length: 100,
        };
        let state = OnlineRankState {
            active_tab: CompetitionType::Jisu,
            date: "2026-08-30".into(),
            boards: HashMap::from([(
                CompetitionType::Jisu,
                RankBoard {
                    data: Some(data),
                    loading: false,
                    error: None,
                    scroll: 0,
                },
            )]),
            error: None,
        };
        let mut app = test_app(file_text("x"));
        app.logged_in = false; // 未登录
        app.state = AppState::OnlineRank(state);

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let clean: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .replace(' ', "");
        assert!(clean.contains("未登录"), "应提示未登录");
        assert!(clean.contains("公开榜"), "应提示当前为公开榜");
        assert!(!clean.contains("我第"), "未登录不应展示个人名次条");
    }

    // #107：手动刷新跨天时更新期次日期。
    #[test]
    fn refresh_rank_updates_date_on_cross_day() {
        let mut app = test_app(file_text("x"));
        // 指向死亡地址，避免后台线程真实联网（连接被快速拒绝）。
        app.api = ApiClient::with_base_url("http://127.0.0.1:1");
        app.state = AppState::OnlineRank(OnlineRankState {
            active_tab: CompetitionType::Jisu,
            date: "2000-01-01".into(), // 旧期次
            boards: HashMap::new(),
            error: None,
        });
        app.refresh_rank();
        match &app.state {
            AppState::OnlineRank(s) => assert_eq!(s.date, today_ymd(), "刷新后日期应更新为今天"),
            _ => panic!("状态应保持为 OnlineRank"),
        }
    }

    // #107：榜单滚动偏移上滚不应越过 0（下界夹紧）。
    #[test]
    fn rank_scroll_clamps_at_top() {
        let mut app = test_app(file_text("x"));
        app.api = ApiClient::with_base_url("http://127.0.0.1:1");
        app.state = AppState::OnlineRank(OnlineRankState {
            active_tab: CompetitionType::Jisu,
            date: "2026-08-30".into(),
            boards: HashMap::from([(
                CompetitionType::Jisu,
                RankBoard {
                    data: None,
                    loading: false,
                    error: None,
                    scroll: 3,
                },
            )]),
            error: None,
        });
        app.rank_scroll(-10); // 远小于当前 scroll
        match &app.state {
            AppState::OnlineRank(s) => {
                let scroll = s.boards.get(&CompetitionType::Jisu).unwrap().scroll;
                assert_eq!(scroll, 0, "上滚不应越过 0");
            }
            _ => panic!("状态应保持为 OnlineRank"),
        }
        app.rank_scroll(2);
        match &app.state {
            AppState::OnlineRank(s) => {
                let scroll = s.boards.get(&CompetitionType::Jisu).unwrap().scroll;
                assert_eq!(scroll, 2, "正向下滚应在下界之上累加");
            }
            _ => panic!("状态应保持为 OnlineRank"),
        }
    }

    #[test]
    fn render_ui_with_free_input_modal_and_paused_sidebar() {
        let mut app = test_app(file_text("测试弹窗渲染"));
        app.open_free_input();

        let backend = ratatui::backend::TestBackend::new(90, 28);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        let clean = content.replace(' ', "");
        assert!(clean.contains("自由发文"));
        assert!(clean.contains("标题"));
        assert!(clean.contains("赛文正文"));
        assert!(clean.contains("保存为本地文件"));
        assert!(clean.contains("确认发文"));
        assert!(clean.contains("Ctrl-Enter"));

        // 测试暂停态功能栏渲染
        app.close_free_input();
        app.session.type_text("测");
        app.pause();
        let backend2 = ratatui::backend::TestBackend::new(90, 28);
        let mut terminal2 = ratatui::Terminal::new(backend2).unwrap();
        terminal2.draw(|f| ui(f, &app)).unwrap();

        let buffer2 = terminal2.backend().buffer();
        let content2 = (0..buffer2.area.height)
            .map(|y| {
                (0..buffer2.area.width)
                    .map(|x| buffer2[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        let clean2 = content2.replace(' ', "");
        assert!(clean2.contains("恢复跟打"));
    }

    #[test]
    fn settings_view_theme_cycling_and_preview_render() {
        let mut app = test_app(file_text("测试设置"));
        app.enter_settings();
        app.settings_focus = FOCUS_THEME;

        for preset in ThemePreset::ALL {
            let backend = ratatui::backend::TestBackend::new(90, 28);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| ui(f, &app)).unwrap();

            let buffer = terminal.backend().buffer();
            let content = (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");

            let clean = content.replace(' ', "");
            assert!(clean.contains("设置"));
            assert!(clean.contains("主题"));
            assert!(clean.contains(&preset.name().replace(' ', "")));
            assert!(clean.contains("对正确对正确"));
            assert!(clean.contains("错错误错错误"));

            // 验证持久化
            assert_eq!(app.settings_store.load().theme, preset);

            // 切到下一主题
            app.next_theme();
        }

        // 验证循环回绕回到了第一个
        assert_eq!(app.settings.theme, ThemePreset::CatppuccinMocha);

        // 验证向上反向循环
        app.prev_theme();
        assert_eq!(app.settings.theme, ThemePreset::OneDark);
    }

    #[test]
    fn settings_row_styling_focused() {
        let palette = theme_palette(ThemePreset::Cyberpunk);
        let focused = settings_row("主题", "Cyberpunk", true, &palette);
        let unfocused = settings_row("粗体", "关", false, &palette);

        assert!(focused.spans[0].content.contains('>'));
        assert_eq!(focused.style.fg, Some(palette.accent));
        assert!(focused.style.add_modifier.contains(Modifier::BOLD));

        assert!(!unfocused.spans[0].content.contains('>'));
        assert_eq!(unfocused.style.fg, Some(palette.fg));
    }

    #[test]
    fn settings_modal_overlay_clears_background_and_renders_cleanly() {
        let mut app = test_app(file_text("一二三四五六七八九十"));
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        // 1. 先渲染打字主界面
        terminal.draw(|f| ui(f, &app)).unwrap();

        // 2. 打开设置
        app.enter_settings();
        let backend2 = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal2 = ratatui::Terminal::new(backend2).unwrap();
        terminal2.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal2.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        let clean = content.replace(' ', "");
        assert!(clean.contains("设置"));
        assert!(clean.contains("主题:"));
        assert!(clean.contains("对照区占比:"));
        assert!(clean.contains("粗体:"));
        assert!(clean.contains("反查方案:"));
        assert!(clean.contains("上传名称:"));
    }

    #[test]
    fn nord_theme_hint_bar_and_preview_hints_high_contrast() {
        let palette = theme_palette(ThemePreset::Nord);
        let line = hint_bar_line(" ↑↓ 选择 | Enter 载入 | Esc 取消 ", &palette);

        // 验证 Badge Pill 中按键样式与描述文字对比度
        for span in &line.spans {
            if span.content.contains("选择")
                || span.content.contains("载入")
                || span.content.contains("取消")
            {
                assert_eq!(span.style.fg, Some(palette.fg));
            }
            if span.content.contains("Enter") || span.content.contains("Esc") {
                assert_eq!(span.style.fg, Some(palette.accent));
                assert_eq!(span.style.bg, Some(palette.selection));
            }
        }
    }

    #[test]
    fn ready_and_paused_hint_bar_no_longer_shows_enter_execute() {
        // 功能栏（就绪态/暂停态）不再用 Enter 执行菜单项，仅保留 l 与专用快捷键。
        let ready = hint_text(false, false, false, false, true);
        let paused = hint_text(false, false, false, true, false);
        assert!(
            !ready.contains("Enter 执行"),
            "就绪态提示栏不应再有「Enter 执行」：{ready}"
        );
        assert!(
            !paused.contains("Enter 执行"),
            "暂停态提示栏不应再有「Enter 执行」：{paused}"
        );
        // 仍应保留 l 作为执行键提示。
        assert!(ready.contains("l 执行"), "就绪态提示栏应保留「l 执行」：{ready}");
        assert!(paused.contains("l 执行"), "暂停态提示栏应保留「l 执行」：{paused}");
    }

    #[test]
    fn main_ui_renders_high_contrast_theme_background_and_sidebar_unselected_items_visible() {
        for preset in [
            ThemePreset::CatppuccinMocha,
            ThemePreset::Cyberpunk,
            ThemePreset::Nord,
            ThemePreset::Dracula,
            ThemePreset::Gruvbox,
            ThemePreset::RosePine,
            ThemePreset::Kanagawa,
            ThemePreset::OneDark,
        ] {
            let mut app = test_app(file_text("中文跟打测试赛文"));
            app.settings.theme = preset;
            let palette = theme_palette(preset);

            let backend = ratatui::backend::TestBackend::new(100, 30);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| ui(f, &app)).unwrap();

            let buffer = terminal.backend().buffer();

            // 1. 验证左侧功能栏未选中项（如 "F1 极速杯" 中的 "极"）具有 palette.fg 前景色和 palette.bg 背景色
            let mut found_f1 = false;
            for y in 0..buffer.area.height {
                for x in 0..24 {
                    let cell = &buffer[(x, y)];
                    if cell.symbol() == "极" {
                        assert_eq!(cell.fg, palette.fg, "Preset {:?} '极' fg mismatch", preset);
                        assert_eq!(cell.bg, palette.bg, "Preset {:?} '极' bg mismatch", preset);
                        found_f1 = true;
                        break;
                    }
                }
                if found_f1 {
                    break;
                }
            }
            assert!(found_f1, "应当在侧边栏找到 '极'");

            // 2. 验证底部快捷键栏带有圆角边框、快捷键标题与高对比度描述
            let mut found_nav = false;
            let mut found_key = false;
            let mut found_title = false;
            let mut found_rounded_border = false;

            for y in (buffer.area.height - 3)..buffer.area.height {
                for x in 0..buffer.area.width {
                    let cell = &buffer[(x, y)];
                    if cell.symbol() == "快" {
                        assert_eq!(
                            cell.fg, palette.accent,
                            "Preset {:?} '快' fg mismatch",
                            preset
                        );
                        found_title = true;
                    }
                    if cell.symbol() == "╭" || cell.symbol() == "╰" {
                        found_rounded_border = true;
                    }
                    if cell.symbol() == "菜" {
                        assert_eq!(cell.fg, palette.fg, "Preset {:?} '菜' fg mismatch", preset);
                        assert_eq!(cell.bg, palette.bg, "Preset {:?} '菜' bg mismatch", preset);
                        found_nav = true;
                    }
                    if cell.symbol() == "j" {
                        assert_eq!(
                            cell.fg, palette.accent,
                            "Preset {:?} 'j' fg mismatch",
                            preset
                        );
                        assert_eq!(
                            cell.bg, palette.selection,
                            "Preset {:?} 'j' bg mismatch",
                            preset
                        );
                        found_key = true;
                    }
                }
            }
            assert!(found_rounded_border, "底部快捷键栏应当有圆角边框 (╭/╰)");
            assert!(found_title, "底部快捷键栏应当包含标题 '快捷键'");
            assert!(found_nav, "应当在底部提示栏找到 '菜'");
            assert!(found_key, "应当在底部提示栏找到按键胶囊 'j'");
        }
    }

    #[test]
    fn finish_typing_persists_session_to_database_asynchronously() {
        let (worker, shared_db) = DbWorker::start_in_memory().unwrap();
        let store = temp_token_store();
        let mut app = App::new_with(
            file_text("你好世界"),
            store.clone(),
            ApiClient::with_base_url_and_store("http://127.0.0.1:1", Some(store)),
            temp_settings_store(),
            Some(worker),
        );
        app.text.title = "测试赛文".to_string();

        app.settings.input_method = "虎码".to_string();

        // 模拟打字：输入 "你好四界"，打错一个字，记录击键与错字
        let now = Instant::now();
        handle_key(
            &mut app.session,
            &mut app.live_keyboard,
            app.scheme_dict.as_ref(),
            KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE),
            Duration::from_secs(1),
            now,
        );
        handle_key(
            &mut app.session,
            &mut app.live_keyboard,
            app.scheme_dict.as_ref(),
            KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE),
            Duration::from_secs(2),
            now,
        );
        handle_key(
            &mut app.session,
            &mut app.live_keyboard,
            app.scheme_dict.as_ref(),
            KeyEvent::new(KeyCode::Char('四'), KeyModifiers::NONE),
            Duration::from_secs(3),
            now,
        );
        handle_key(
            &mut app.session,
            &mut app.live_keyboard,
            app.scheme_dict.as_ref(),
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            Duration::from_secs(4),
            now,
        );
        handle_key(
            &mut app.session,
            &mut app.live_keyboard,
            app.scheme_dict.as_ref(),
            KeyEvent::new(KeyCode::Char('世'), KeyModifiers::NONE),
            Duration::from_secs(5),
            now,
        );
        handle_key(
            &mut app.session,
            &mut app.live_keyboard,
            app.scheme_dict.as_ref(),
            KeyEvent::new(KeyCode::Char('界'), KeyModifiers::NONE),
            Duration::from_secs(6),
            now,
        );

        assert!(app.session.is_complete());
        app.accumulated_elapsed = Duration::from_secs(6);

        // 触发完成
        let _ = app.finish_typing();

        // 优雅停机刷盘
        if let Some(w) = app.db_worker.take() {
            w.flush_and_stop();
        }

        // 校验 SQLite 数据库中的记录
        let db = shared_db.lock().unwrap();
        assert_eq!(db.get_session_count().unwrap(), 1);

        let sessions = db.get_all_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].text_title, "测试赛文");
        assert_eq!(sessions[0].input_scheme, "虎码");
        assert_eq!(sessions[0].correct_chars, 4);
        assert_eq!(sessions[0].edits, 1);

        let key_totals = db.get_key_press_totals(Some(true)).unwrap();
        assert_eq!(key_totals.get("你"), Some(&1));
        assert_eq!(key_totals.get("Backspace"), Some(&1));

        let top_chars = db.get_top_mistyped_chars(5).unwrap();
        assert_eq!(top_chars.len(), 1);
        assert_eq!(top_chars[0].target_char, '世');
        assert_eq!(top_chars[0].error_count, 1);

        // 校验击键、码长与总击数的持久化
        assert!(sessions[0].kps > 0.0);
        assert!(sessions[0].key_length > 0.0);
        assert_eq!(sessions[0].total_strokes, 6);

        let summary = db.get_global_summary().unwrap();
        assert!(summary.avg_kps > 0.0);
        assert!(summary.avg_key_length > 0.0);
        assert_eq!(summary.total_strokes, 6);
    }

    #[test]
    fn wpm_chart_range_cycling() {
        assert_eq!(WpmChartRange::Recent30.next(), WpmChartRange::Recent100);
        assert_eq!(WpmChartRange::Recent100.next(), WpmChartRange::All);
        assert_eq!(WpmChartRange::All.next(), WpmChartRange::Recent30);

        assert_eq!(WpmChartRange::Recent30.limit(), Some(30));
        assert_eq!(WpmChartRange::Recent100.limit(), Some(100));
        assert_eq!(WpmChartRange::All.limit(), None);
    }

    #[test]
    fn finished_view_press_s_opens_stats_view() {
        let mut app = test_app(file_text("你好"));
        let stats = app.session.finish(Duration::from_secs(10));
        app.state = AppState::Finished {
            stats,
            upload: UploadState::NotApplicable { copied_stats: None },
            elapsed: Duration::from_secs(10),
        };

        let handled = handle_finished_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        );
        assert!(handled);
        assert!(matches!(app.state, AppState::Stats(_)));
        if let AppState::Stats(s) = &app.state {
            assert_eq!(s.tab, StatsTab::WpmTrend);
            assert_eq!(s.wpm_range, WpmChartRange::Recent30);
        }
    }

    #[test]
    fn render_stats_view_tabs_and_overview() {
        let mut app = test_app(file_text("测试赛文"));
        app.state = AppState::Stats(StatsViewState {
            tab: StatsTab::WpmTrend,
            wpm_range: WpmChartRange::Recent30,
            heatmap_layout: HeatmapLayout::Staggered,
            heatmap_source: HeatmapSource::SchemeProjected,
            ..Default::default()
        });

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal.draw(|f| ui(f, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();

        // 校验标题与 Tab 渲染
        let mut found_header = false;
        let mut found_tab1 = false;
        let mut found_tab2 = false;
        let mut found_tab3 = false;
        let mut found_overview = false;
        let mut found_chart_title = false;

        let full_text: String = (0..buffer.area.height)
            .map(|y| {
                let row: String = (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect();
                row + "\n"
            })
            .collect();

        let clean = full_text.replace(' ', "");

        if clean.contains("统计数据中心") {
            found_header = true;
        }
        if clean.contains("1.速度趋势") {
            found_tab1 = true;
        }
        if clean.contains("2.键位热力图") {
            found_tab2 = true;
        }
        if clean.contains("3.错字排行") {
            found_tab3 = true;
        }
        if clean.contains("历史练习总览") {
            found_overview = true;
        }
        if clean.contains("WPM历史演进趋势") {
            found_chart_title = true;
        }

        assert!(found_header, "Header should contain '统计数据中心'");
        assert!(found_tab1, "Tab 1 should be rendered");
        assert!(found_tab2, "Tab 2 should be rendered");
        assert!(found_tab3, "Tab 3 should be rendered");
        assert!(found_overview, "Overview summary card should be rendered");
        assert!(found_chart_title, "Chart title should be rendered");
        assert!(
            clean.contains("平均击速"),
            "Overview should contain '平均击速'"
        );
        assert!(
            clean.contains("平均码长"),
            "Overview should contain '平均码长'"
        );
    }

    #[test]
    fn render_stats_view_heatmap_and_error_tabs() {
        let mut app = test_app(file_text("测试赛文"));

        // Tab 2: Heatmap (Staggered Layout)
        app.state = AppState::Stats(StatsViewState {
            tab: StatsTab::Heatmap,
            wpm_range: WpmChartRange::Recent30,
            heatmap_layout: HeatmapLayout::Staggered,
            heatmap_source: HeatmapSource::SchemeProjected,
            ..Default::default()
        });
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let full_text: String = (0..terminal.backend().buffer().area.height)
            .map(|y| {
                (0..terminal.backend().buffer().area.width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        let clean2 = full_text.replace(' ', "");
        assert!(clean2.contains("键位热力图参数"));
        assert!(clean2.contains("键盘热力矩阵"));
        assert!(clean2.contains("标准斜列"));

        // Tab 2: Heatmap (Ortholinear Layout)
        app.state = AppState::Stats(StatsViewState {
            tab: StatsTab::Heatmap,
            wpm_range: WpmChartRange::Recent30,
            heatmap_layout: HeatmapLayout::Ortholinear,
            heatmap_source: HeatmapSource::RawKeypress,
            ..Default::default()
        });
        let backend_ortho = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal_ortho = ratatui::Terminal::new(backend_ortho).unwrap();
        terminal_ortho.draw(|f| ui(f, &app)).unwrap();
        let full_text_ortho: String = (0..terminal_ortho.backend().buffer().area.height)
            .map(|y| {
                (0..terminal_ortho.backend().buffer().area.width)
                    .map(|x| {
                        terminal_ortho.backend().buffer()[(x, y)]
                            .symbol()
                            .to_string()
                    })
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        let clean_ortho = full_text_ortho.replace(' ', "");
        assert!(clean_ortho.contains("直列矩阵"));
        assert!(clean_ortho.contains("物理击键视角"));

        // Tab 3: Error ranking
        app.state = AppState::Stats(StatsViewState {
            tab: StatsTab::ErrorRanking,
            wpm_range: WpmChartRange::Recent30,
            heatmap_layout: HeatmapLayout::Staggered,
            heatmap_source: HeatmapSource::SchemeProjected,
            error_ranking_focus: ErrorRankingFocus::Chars,
            char_scroll: 0,
            word_scroll: 0,
            ..Default::default()
        });
        let backend_err = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal_err = ratatui::Terminal::new(backend_err).unwrap();
        terminal_err.draw(|f| ui(f, &app)).unwrap();
        let full_text3: String = (0..terminal_err.backend().buffer().area.height)
            .map(|y| {
                (0..terminal_err.backend().buffer().area.width)
                    .map(|x| terminal_err.backend().buffer()[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        let clean3 = full_text3.replace(' ', "");
        assert!(clean3.contains("错字与错词数据总览"));
        assert!(clean3.contains("高频错字排行榜"));
        assert!(clean3.contains("高频错词排行榜"));
    }

    #[test]
    fn heatmap_layout_and_source_cycling() {
        assert_eq!(HeatmapLayout::Staggered.next(), HeatmapLayout::Ortholinear);
        assert_eq!(HeatmapLayout::Ortholinear.next(), HeatmapLayout::Staggered);

        assert_eq!(
            HeatmapSource::SchemeProjected.next(),
            HeatmapSource::RawKeypress
        );
        assert_eq!(
            HeatmapSource::RawKeypress.next(),
            HeatmapSource::SchemeProjected
        );
    }

    #[test]
    fn error_ranking_focus_toggle() {
        assert_eq!(ErrorRankingFocus::Chars.toggle(), ErrorRankingFocus::Words);
        assert_eq!(ErrorRankingFocus::Words.toggle(), ErrorRankingFocus::Chars);
    }

    #[test]
    fn live_keyboard_normalize_and_press() {
        assert_eq!(LiveKeyboard::normalize_key("A"), "a");
        assert_eq!(LiveKeyboard::normalize_key("Space (空格)"), "Space");
        assert_eq!(LiveKeyboard::normalize_key("Bksp"), "Backspace");
        assert_eq!(LiveKeyboard::normalize_key("tab"), "Tab");
        assert_eq!(LiveKeyboard::normalize_key("Enter"), "Enter");

        let mut kb = LiveKeyboard::new();
        let now = Instant::now();
        kb.press_char('w', now);
        kb.press_char(' ', now);
        assert!(kb.active_keys.contains_key("w"));
        assert!(kb.active_keys.contains_key("Space"));

        kb.clear();
        assert!(kb.active_keys.is_empty());

        kb.press_keys(["n", "i"], now);
        assert!(kb.active_keys.contains_key("n"));
        assert!(kb.active_keys.contains_key("i"));
    }

    #[test]
    fn live_keyboard_styles_and_decay() {
        let palette = theme_palette(ThemePreset::CatppuccinMocha);
        let mut kb = LiveKeyboard::new();
        let t0 = Instant::now();

        // 未按下的键 -> muted
        let idle_style = kb.get_key_style("a", &palette, t0);
        assert_eq!(idle_style.fg, Some(palette.muted));

        // 按下瞬间 -> 强高亮 (bg: accent, fg: bg)
        kb.press_key("a", t0);
        let active_style = kb.get_key_style("a", &palette, t0);
        assert_eq!(active_style.bg, Some(palette.accent));
        assert_eq!(active_style.fg, Some(palette.bg));
        assert!(active_style.add_modifier.contains(Modifier::BOLD));

        // 150ms 衰减 -> 次高亮 (fg: accent)
        let t_decay = t0 + Duration::from_millis(150);
        let decay_style = kb.get_key_style("a", &palette, t_decay);
        assert_eq!(decay_style.fg, Some(palette.accent));
        assert_eq!(decay_style.bg, None);

        // 300ms 后 -> 恢复常态 muted
        let t_end = t0 + Duration::from_millis(300);
        let end_style = kb.get_key_style("a", &palette, t_end);
        assert_eq!(end_style.fg, Some(palette.muted));
    }

    #[test]
    fn live_keyboard_generate_lines_rows_count() {
        let palette = theme_palette(ThemePreset::CatppuccinMocha);
        let kb = LiveKeyboard::new();
        let now = Instant::now();

        let staggered_lines =
            generate_live_keyboard_lines(&kb, KeyboardMode::Staggered, &palette, now, 80);
        assert_eq!(staggered_lines.len(), 5);

        let ortho_lines =
            generate_live_keyboard_lines(&kb, KeyboardMode::Ortholinear, &palette, now, 80);
        assert_eq!(ortho_lines.len(), 4);

        let off_lines = generate_live_keyboard_lines(&kb, KeyboardMode::Off, &palette, now, 80);
        assert_eq!(off_lines.len(), 0);
    }

    #[test]
    fn test_live_keyboard_centering_offsets() {
        let palette = theme_palette(ThemePreset::CatppuccinMocha);
        let kb = LiveKeyboard::new();
        let now = Instant::now();

        // 1. 宽度 60（刚好容纳 60% 键盘）：无居中额外填充
        let lines_60 =
            generate_live_keyboard_lines(&kb, KeyboardMode::Staggered, &palette, now, 60);
        let first_row_60 = lines_60[0].spans[0].content.as_ref();
        assert_eq!(first_row_60, ""); // Row 0 indent is ""

        // 2. 宽度 80：居中填充 (80 - 60) / 2 = 10 空格
        let lines_80 =
            generate_live_keyboard_lines(&kb, KeyboardMode::Staggered, &palette, now, 80);
        let first_row_80 = lines_80[0].spans[0].content.as_ref();
        assert_eq!(first_row_80, " ".repeat(10));

        // 3. 宽度 100：居中填充 (100 - 60) / 2 = 20 空格
        let lines_100 =
            generate_live_keyboard_lines(&kb, KeyboardMode::Staggered, &palette, now, 100);
        let first_row_100 = lines_100[0].spans[0].content.as_ref();
        assert_eq!(first_row_100, " ".repeat(20));

        // 验证各行阶梯缩进保持相对正确 (Row 1: +5 spaces, Row 2: +7 spaces, Row 3: +9 spaces, Row 4: +17 spaces)
        let row1_100 = lines_100[1].spans[0].content.as_ref();
        assert_eq!(row1_100, format!("{}     ", " ".repeat(20)));
        let row2_100 = lines_100[2].spans[0].content.as_ref();
        assert_eq!(row2_100, format!("{}       ", " ".repeat(20)));
        let row3_100 = lines_100[3].spans[0].content.as_ref();
        assert_eq!(row3_100, format!("{}         ", " ".repeat(20)));
        let row4_100 = lines_100[4].spans[0].content.as_ref();
        assert_eq!(row4_100, format!("{}                 ", " ".repeat(20)));

        // 4. 直列网格（Ortholinear）居中与缩进断言：最大宽度 46
        let ortho_80 =
            generate_live_keyboard_lines(&kb, KeyboardMode::Ortholinear, &palette, now, 80);
        // 居中填充 (80 - 46) / 2 = 17 空格
        assert_eq!(ortho_80[0].spans[0].content.as_ref(), " ".repeat(17));
        assert_eq!(ortho_80[1].spans[0].content.as_ref(), " ".repeat(17));
        assert_eq!(ortho_80[2].spans[0].content.as_ref(), " ".repeat(17));
        assert_eq!(
            ortho_80[3].spans[0].content.as_ref(),
            format!("{}           ", " ".repeat(17))
        );
    }

    #[test]
    fn test_live_keyboard_theme_hierarchy_spans() {
        let palette = theme_palette(ThemePreset::CatppuccinMocha);
        let mut kb = LiveKeyboard::new();
        let now = Instant::now();

        // 1. 常态（未击键）：测试主题颜色分层与按键边框跟随主题强调色
        let lines = generate_live_keyboard_lines(&kb, KeyboardMode::Staggered, &palette, now, 80);

        // 验证精简布局：除了 Bksp 和 Space，不含 Tab / Caps / Enter / Shift / Ctrl / Alt / Esc，且无中文“空格”
        let full_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(full_text.contains("[Bksp]"));
        assert!(full_text.contains("Space"));
        assert!(!full_text.contains("Tab"));
        assert!(!full_text.contains("Caps"));
        assert!(!full_text.contains("Enter"));
        assert!(!full_text.contains("Shift"));
        assert!(!full_text.contains("Ctrl"));
        assert!(!full_text.contains("Alt"));
        assert!(!full_text.contains("空格"));

        // 直列网格（Ortholinear）也同样不含已删除的功能键
        let ortho_lines =
            generate_live_keyboard_lines(&kb, KeyboardMode::Ortholinear, &palette, now, 80);
        let ortho_text: String = ortho_lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(ortho_text.contains("[Bksp]"));
        assert!(ortho_text.contains("Space"));
        assert!(!ortho_text.contains("Tab"));
        assert!(!ortho_text.contains("Esc"));
        assert!(!ortho_text.contains("Enter"));
        assert!(!ortho_text.contains("Shift"));
        assert!(!ortho_text.contains("Lower"));
        assert!(!ortho_text.contains("Raise"));
        assert!(!ortho_text.contains("空格"));

        // Row 0 Bksp 验证
        let row0 = &lines[0];
        let mut found_bksp = false;
        for span in &row0.spans {
            if span.content == "[Bksp]" {
                assert_eq!(span.style.fg, Some(palette.muted));
                found_bksp = true;
            }
        }
        assert!(found_bksp);

        // Row 2 包含 A, S, D, F, G, H, J, K, L, ;, '
        let row2 = &lines[2];

        // 验证定位键 F 与 J 在常态下被高亮为 accent + bold，字母键边框为 accent
        let mut found_f = false;
        let mut found_j = false;
        let mut found_a = false;
        for (i, span) in row2.spans.iter().enumerate() {
            if span.content == "F" {
                assert_eq!(span.style.fg, Some(palette.accent));
                assert!(span.style.add_modifier.contains(Modifier::BOLD));
                // F 键的左右括号应为主题强调色
                assert_eq!(row2.spans[i - 1].content, "[");
                assert_eq!(row2.spans[i - 1].style.fg, Some(palette.accent));
                assert_eq!(row2.spans[i + 1].content, "]");
                assert_eq!(row2.spans[i + 1].style.fg, Some(palette.accent));
                found_f = true;
            } else if span.content == "J" {
                assert_eq!(span.style.fg, Some(palette.accent));
                assert!(span.style.add_modifier.contains(Modifier::BOLD));
                found_j = true;
            } else if span.content == "A" {
                // 普通字母键为主要前景色 fg，但左右边框为主题强调色 accent
                assert_eq!(span.style.fg, Some(palette.fg));
                assert_eq!(row2.spans[i - 1].content, "[");
                assert_eq!(row2.spans[i - 1].style.fg, Some(palette.accent));
                assert_eq!(row2.spans[i + 1].content, "]");
                assert_eq!(row2.spans[i + 1].style.fg, Some(palette.accent));
                found_a = true;
            }
        }
        assert!(found_f && found_j && found_a);

        // Row 4 空格键验证：左右括号为 accent，内部文字为 muted，且标签为纯英文 Space
        let row4 = &lines[4];
        let mut found_space_brackets = false;
        for (i, span) in row4.spans.iter().enumerate() {
            if span.content.contains("Space") && !span.content.contains("[") {
                assert_eq!(span.style.fg, Some(palette.muted));
                assert_eq!(row4.spans[i - 1].content, "[");
                assert_eq!(row4.spans[i - 1].style.fg, Some(palette.accent));
                assert_eq!(row4.spans[i + 1].content, "]");
                assert_eq!(row4.spans[i + 1].style.fg, Some(palette.accent));
                found_space_brackets = true;
            }
        }
        assert!(
            found_space_brackets,
            "空格键外侧括号应为主题强调色且文本为纯英文 Space"
        );

        // 多主题预设联动验证：切换至 Dracula 主题，边框色彩随之变更
        let dracula_palette = theme_palette(ThemePreset::Dracula);
        let dracula_lines =
            generate_live_keyboard_lines(&kb, KeyboardMode::Staggered, &dracula_palette, now, 80);
        let dracula_row2 = &dracula_lines[2];
        for (i, span) in dracula_row2.spans.iter().enumerate() {
            if span.content == "A" {
                assert_eq!(
                    dracula_row2.spans[i - 1].style.fg,
                    Some(dracula_palette.accent)
                );
                assert_ne!(dracula_row2.spans[i - 1].style.fg, Some(palette.accent));
            }
        }

        // 2. 按键按下时：测试强高亮 (0-100ms) 反色填充 (bg: accent, fg: bg)
        kb.press_char('a', now);
        let active_lines =
            generate_live_keyboard_lines(&kb, KeyboardMode::Staggered, &palette, now, 80);
        let active_row2 = &active_lines[2];
        let mut found_active_a = false;
        for span in &active_row2.spans {
            if span.content == "[A]" {
                assert_eq!(span.style.bg, Some(palette.accent));
                assert_eq!(span.style.fg, Some(palette.bg));
                assert!(span.style.add_modifier.contains(Modifier::BOLD));
                found_active_a = true;
            }
        }
        assert!(found_active_a, "按下瞬间 'A' 键应渲染为反色实体高亮 [A]");
    }

    #[test]
    fn ui_renders_live_keyboard_when_enabled() {
        let store = temp_token_store();
        let mut app = App::new_with(
            file_text("你好世界"),
            store.clone(),
            ApiClient::with_base_url_and_store("http://127.0.0.1:1", Some(store)),
            temp_settings_store(),
            None,
        );
        app.settings.keyboard_mode = KeyboardMode::Staggered;

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let full_text: String = (0..terminal.backend().buffer().area.height)
            .map(|y| {
                (0..terminal.backend().buffer().area.width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(full_text.contains("[Bksp]"));
        assert!(full_text.contains("Space"));
        assert!(!full_text.contains("空格"));
    }

    #[test]
    fn handle_key_chinese_char_with_scheme_dict_activates_ripple_keys() {
        let dict = SchemeDict::parse("你\tvb\n好\tvr\n世\tvy\n界\tvj\n");
        let mut session = Session::new("你好世界");
        let mut live_kb = LiveKeyboard::new();
        let now = Instant::now();

        // 输入汉字 '你' -> 反查 'vb' -> 激活 'v' 和 'b'
        handle_key(
            &mut session,
            &mut live_kb,
            Some(&dict),
            KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE),
            Duration::from_millis(500),
            now,
        );
        assert_eq!(session.len(), 1);
        assert!(live_kb.active_keys.contains_key("v"));
        assert!(live_kb.active_keys.contains_key("b"));

        // 输入汉字 '好' -> 反查 'vr' -> 激活 'r'
        handle_key(
            &mut session,
            &mut live_kb,
            Some(&dict),
            KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE),
            Duration::from_millis(1000),
            now,
        );
        assert_eq!(session.len(), 2);
        assert!(live_kb.active_keys.contains_key("r"));
    }

    #[test]
    fn handle_key_chinese_char_without_scheme_or_missing_char_does_not_panic() {
        let dict = SchemeDict::parse("你\tvb\n");
        let mut session = Session::new("你好世界");
        let mut live_kb = LiveKeyboard::new();
        let now = Instant::now();

        // 1. 无 SchemeDict
        handle_key(
            &mut session,
            &mut live_kb,
            None,
            KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE),
            Duration::from_millis(500),
            now,
        );
        assert_eq!(session.len(), 1);

        // 2. 有 SchemeDict 但字典中不存在该字
        handle_key(
            &mut session,
            &mut live_kb,
            Some(&dict),
            KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE),
            Duration::from_millis(1000),
            now,
        );
        assert_eq!(session.len(), 2);
    }

    #[test]
    fn live_keyboard_multi_key_ripple_decay_styles() {
        let palette = theme_palette(ThemePreset::CatppuccinMocha);
        let mut kb = LiveKeyboard::new();
        let t0 = Instant::now();

        // 模拟汉字上屏瞬间同时激活多个字根键 (如 'v', 'b', 'g')
        kb.press_keys(["v", "b", "g"], t0);

        // 0-100ms 强高亮
        for k in &["v", "b", "g"] {
            let style = kb.get_key_style(k, &palette, t0 + Duration::from_millis(40));
            assert_eq!(style.bg, Some(palette.accent));
            assert_eq!(style.fg, Some(palette.bg));
            assert!(style.add_modifier.contains(Modifier::BOLD));
        }

        // 100-250ms 衰减次高亮
        for k in &["v", "b", "g"] {
            let style = kb.get_key_style(k, &palette, t0 + Duration::from_millis(180));
            assert_eq!(style.fg, Some(palette.accent));
            assert_eq!(style.bg, None);
            assert!(style.add_modifier.contains(Modifier::BOLD));
        }

        // >250ms 恢复常态
        for k in &["v", "b", "g"] {
            let style = kb.get_key_style(k, &palette, t0 + Duration::from_millis(300));
            assert_eq!(style.fg, Some(palette.muted));
            assert_eq!(style.bg, None);
        }
    }

    #[test]
    fn handle_key_chord_algebra_activates_all_physical_keys_simultaneously() {
        let mut dict = SchemeDict::default();
        dict.add_entry("到", "_.");
        dict.add_entry("是", "wCs");
        dict.add_entry("们", "aI");

        let rules = vec![
            "xform|xv|\\.|".to_string(),
            "xform|cf|C|".to_string(),
            "xform|eg|I|".to_string(),
            "xform|j|f|".to_string(),
            "xform|,|c|".to_string(),
            "xform|h|g|".to_string(),
            "xform|i|e|".to_string(),
        ];
        dict.set_chord_algebra(dazitui_core::ChordAlgebra::from_rules(&rules));

        let mut session = Session::new("到是们");
        let mut live_kb = LiveKeyboard::new();
        let now = Instant::now();

        // 键入 "到" (反查为 "_." -> 由 ChordAlgebra 逆向展开为 "x" 和 "v")
        handle_key(
            &mut session,
            &mut live_kb,
            Some(&dict),
            KeyEvent::new(KeyCode::Char('到'), KeyModifiers::NONE),
            Duration::from_millis(100),
            now,
        );

        assert!(live_kb.active_keys.contains_key("x"));
        assert!(live_kb.active_keys.contains_key("v"));
        assert_eq!(live_kb.active_keys.get("x"), Some(&now));
        assert_eq!(live_kb.active_keys.get("v"), Some(&now));

        // 键入 "是" (反查为 "wCs" -> 双手并击 w (左) + C (右手 cf 镜像为 , j) + 结构码 s)
        let t2 = now + Duration::from_millis(500);
        handle_key(
            &mut session,
            &mut live_kb,
            Some(&dict),
            KeyEvent::new(KeyCode::Char('是'), KeyModifiers::NONE),
            Duration::from_millis(600),
            t2,
        );

        assert!(live_kb.active_keys.contains_key("w"));
        assert!(live_kb.active_keys.contains_key(","));
        assert!(live_kb.active_keys.contains_key("j"));
        assert!(live_kb.active_keys.contains_key("s"));
        assert_eq!(live_kb.active_keys.get(","), Some(&t2));
        assert_eq!(live_kb.active_keys.get("j"), Some(&t2));

        // 键入 "们" (反查为 "aI" -> 双手并击 a (左) + I (右手 eg 镜像为 h i))
        let t3 = now + Duration::from_millis(1000);
        handle_key(
            &mut session,
            &mut live_kb,
            Some(&dict),
            KeyEvent::new(KeyCode::Char('们'), KeyModifiers::NONE),
            Duration::from_millis(1100),
            t3,
        );

        assert!(live_kb.active_keys.contains_key("a"));
        assert!(live_kb.active_keys.contains_key("h"));
        assert!(live_kb.active_keys.contains_key("i"));
        assert!(!live_kb.active_keys.contains_key("e"));
        assert!(!live_kb.active_keys.contains_key("g"));

        // 验证 session 的击数：到 (1击) + 是 (3击) + 们 (2击) = 6击
        assert_eq!(session.total_strokes(), 6);
    }

    #[test]
    fn handle_text_chord_word_zenme_activates_only_chord_keys() {
        let mut dict = SchemeDict::default();
        dict.add_entry("怎", "H:");
        dict.add_entry("么", "tB");
        dict.add_entry("怎么", "+H");

        let rules = vec![
            "xform|y|t|".to_string(),
            "xform|u|r|".to_string(),
            "xform|i|e|".to_string(),
            "xform|o|w|".to_string(),
            "xform|p|q|".to_string(),
            "xform|;|a|".to_string(),
            "xform|ar|H|".to_string(),
            "xform|as|:|".to_string(),
            "xform|qw|B|".to_string(),
        ];
        dict.set_chord_algebra(dazitui_core::ChordAlgebra::from_rules(&rules));

        let mut session = Session::new("怎么");
        let mut live_kb = LiveKeyboard::new();
        let now = Instant::now();

        // 键入词组 "怎么" (反查为 "+H" -> 右手镜像展开为 ";" 和 "u")
        handle_text(
            &mut session,
            &mut live_kb,
            Some(&dict),
            "怎么",
            Duration::from_millis(100),
            now,
        );

        // 应只激活右手镜像并击键 ";" 与 "u"
        assert!(live_kb.active_keys.contains_key(";"));
        assert!(live_kb.active_keys.contains_key("u"));

        // 绝对不能亮 "怎" (H: -> ars) 和 "么" (tB -> qwt) 的散键 (qwasrt)
        assert!(!live_kb.active_keys.contains_key("q"));
        assert!(!live_kb.active_keys.contains_key("w"));
        assert!(!live_kb.active_keys.contains_key("a"));
        assert!(!live_kb.active_keys.contains_key("s"));
        assert!(!live_kb.active_keys.contains_key("r"));
        assert!(!live_kb.active_keys.contains_key("t"));

        // 击数应为并击 1 击，而非 4 击
        assert_eq!(session.total_strokes(), 1);
    }

    #[test]
    fn reference_area_renders_realtime_compound_metrics_on_typing() {
        let mut app = test_app(file_text("你好世界"));
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        // 1. 就绪态（未开打）：右下角不应包含 WPM/击键 指标
        terminal.draw(|f| ui(f, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let ready_content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!ready_content.contains("击键"));

        // 2. 开打后（输入字符）：对照区底边框右侧应显示 WPM 和 击键 指标
        app.touch_typing();
        handle_key(
            &mut app.session,
            &mut app.live_keyboard,
            app.scheme_dict.as_ref(),
            KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE),
            Duration::from_secs(1),
            Instant::now(),
        );

        terminal.draw(|f| ui(f, &app)).unwrap();
        let buffer2 = terminal.backend().buffer();
        let typing_content = (0..buffer2.area.height)
            .map(|y| {
                (0..buffer2.area.width)
                    .map(|x| buffer2[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let clean_typing: String = typing_content
            .chars()
            .filter(|c| *c != ' ' && *c != '─')
            .collect();
        assert!(clean_typing.contains("WPM"));
        assert!(clean_typing.contains("击键"));

        // 3. 暂停态（按 Tab / 调用 pause()）：显示 [暂停] 徽标且即时瞬时值锁定为 (0)
        app.pause();
        let mut term_paused =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        term_paused.draw(|f| ui(f, &app)).unwrap();
        let buffer3 = term_paused.backend().buffer();
        let paused_content = (0..buffer3.area.height)
            .map(|y| {
                (0..buffer3.area.width)
                    .map(|x| buffer3[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let clean_paused: String = paused_content
            .chars()
            .filter(|c| *c != ' ' && *c != '─')
            .collect();
        assert!(clean_paused.contains("[暂停]"));
        assert!(clean_paused.contains("(0)"));

        // 4. 恢复打字（续接计时）：徽标消失，指标正常续接
        app.complete_resume_countdown();
        let mut term_resumed =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        term_resumed.draw(|f| ui(f, &app)).unwrap();
        let buffer4 = term_resumed.backend().buffer();
        let resumed_content = (0..buffer4.area.height)
            .map(|y| {
                (0..buffer4.area.width)
                    .map(|x| buffer4[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let clean_resumed: String = resumed_content
            .chars()
            .filter(|c| *c != ' ' && *c != '─')
            .collect();
        assert!(!clean_resumed.contains("[暂停]"));
        assert!(clean_resumed.contains("WPM"));
    }

    #[test]
    fn test_calculate_text_layout_position_cjk_ascii_newlines() {
        // 1. 空字符序列
        assert_eq!(calculate_text_layout_position("".chars(), 10), (0, 0));
        assert_eq!(calculate_text_layout_position("".chars(), 0), (0, 0));

        // 2. 纯 ASCII 字符
        // "abcde" 在宽 10 下占 5 列，行 0
        assert_eq!(calculate_text_layout_position("abcde".chars(), 10), (0, 5));
        // "abcdefghij" 满 10 列
        assert_eq!(
            calculate_text_layout_position("abcdefghij".chars(), 10),
            (0, 10)
        );
        // "abcdefghijk" 溢出换行：'k' 处于第 1 行第 1 列
        assert_eq!(
            calculate_text_layout_position("abcdefghijk".chars(), 10),
            (1, 1)
        );

        // 3. 中文字符（每个宽 2）
        // "中文" 占 4 列，行 0
        assert_eq!(calculate_text_layout_position("中文".chars(), 10), (0, 4));
        // "中文测试一" 占 10 列，行 0
        assert_eq!(
            calculate_text_layout_position("中文测试一".chars(), 10),
            (0, 10)
        );
        // "中文测试二号" 在宽 10 下，第 6 字 "号" 溢出到行 1 列 2
        assert_eq!(
            calculate_text_layout_position("中文测试二号".chars(), 10),
            (1, 2)
        );

        // 4. 换行符重置列
        assert_eq!(
            calculate_text_layout_position("abc\ndef".chars(), 10),
            (1, 3)
        );
        assert_eq!(
            calculate_text_layout_position("你好\n世界".chars(), 10),
            (1, 4)
        );
    }

    #[test]
    fn test_text_setting_modal_rendered_on_top_of_settings_with_cursor() {
        let text = load_text_from_string(
            "测试",
            "这是测试".into(),
            TextSource::Custom,
            &LoadOptions::default(),
        )
        .unwrap();
        let mut app = test_app(text);
        app.enter_settings();
        app.settings_focus = FOCUS_INPUT_METHOD;
        app.text_setting_modal = Some(TextSettingModal::new(
            TextSettingTarget::InputMethod,
            "我的自定义输入法",
        ));

        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        term.draw(|f| ui(f, &app)).unwrap();
        let buffer = term.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let clean: String = content.chars().filter(|c| *c != ' ' && *c != '─').collect();

        // 验证弹窗标题与自定义输入法内容在最顶层清晰可见（未被 settings 覆盖）
        assert!(clean.contains("自定义上传输入法名称"));
        assert!(clean.contains("我的自定义输入法"));

        // 验证硬件光标已定位在弹窗输入框处
        let (cx, cy): (u16, u16) = term.get_cursor_position().unwrap().into();
        assert!(cx > 0 && cy > 0);
        assert!(cy < 24 && cx < 80);
    }

    #[test]
    fn test_long_text_typing_scrolls_and_centers_cursor_within_bounds() {
        // 创建超过单页容量的超长赛文（500 字，在宽 60 的跟打区中折行超过 15 行）
        let long_raw =
            "天地玄黄宇宙洪荒日月盈昃辰宿列张寒来暑往秋收冬藏闰余成岁律吕调阳云腾致雨露结为霜"
                .repeat(6);
        let text = load_text_from_string(
            "长文本测试",
            long_raw.clone(),
            TextSource::Custom,
            &LoadOptions::default(),
        )
        .unwrap();
        let mut app = test_app(text);

        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();

        // 1. 就绪态（未输入）：光标在跟打区左上角，对照区未滚动
        term.draw(|f| ui(f, &app)).unwrap();
        let initial_cursor: (u16, u16) = term.get_cursor_position().unwrap().into();
        assert!(initial_cursor.0 > 0 && initial_cursor.1 > 0);

        // 2. 打入 200 个汉字（已推进到多行之后）
        let prefix: String = long_raw.chars().take(200).collect();
        app.session.type_text(&prefix);

        term.draw(|f| ui(f, &app)).unwrap();
        let (cx, cy): (u16, u16) = term.get_cursor_position().unwrap().into();

        // 3. 验证光标始终位于终端可视区域内，绝不会溢出或被下边框遮挡
        assert!(cy > 0 && cy < 23, "光标纵坐标 y={} 应在可视区域内部", cy);
        assert!(cx > 0 && cx < 79, "光标横坐标 x={} 应在可视区域内部", cx);

        // 4. 验证渲染缓冲区中能够找到当前最新的跟打字符（证明已自动向下滚动到当前打字处）
        let buffer = term.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // 第 200 个字附近的字符应当在缓冲区可见
        let recent_char = prefix.chars().last().unwrap();
        assert!(
            content.contains(recent_char),
            "缓冲区应包含当前打字处字符: {}",
            recent_char
        );
    }

    #[test]
    fn test_cursor_follows_typed_character_on_builtin_page_and_words() {
        // 1. 单字赛文：测试翻页后光标重置与紧跟
        let text = load_builtin_text(BUILTIN_SETS[0]);
        let mut app = test_app(text);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();

        // 初始第 1 页第 1 字前
        term.draw(|f| ui(f, &app)).unwrap();
        let (init_x, init_y): (u16, u16) = term.get_cursor_position().unwrap().into();

        // 打入第 1 页 10 个字（全对）
        let p1: String = BUILTIN_SETS[0].content().chars().take(10).collect();
        app.session.type_text(&p1);
        term.draw(|f| ui(f, &app)).unwrap();
        let (p1_end_x, p1_end_y): (u16, u16) = term.get_cursor_position().unwrap().into();
        assert_eq!(
            (p1_end_x, p1_end_y),
            (init_x, init_y),
            "翻页后未打字光标应在起始位置"
        );

        // 打入第 2 页第 1 个字（宽 2）
        let p2_c = BUILTIN_SETS[0].content().chars().nth(10).unwrap();
        app.session.type_text(&p2_c.to_string());
        term.draw(|f| ui(f, &app)).unwrap();
        let (p2_x, p2_y): (u16, u16) = term.get_cursor_position().unwrap().into();
        assert_eq!(
            (p2_x, p2_y),
            (init_x + 2, init_y),
            "第 2 页打了 1 个字后光标应在第 1 个字后面"
        );

        // 2. 词组赛文：词间空格与光标位置
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let no_commas = set.content_no_commas();
        let text_words = load_builtin_text(set);
        let mut app_words = test_app(text_words);

        term.draw(|f| ui(f, &app_words)).unwrap();
        let (w_init_x, w_init_y): (u16, u16) = term.get_cursor_position().unwrap().into();

        // 打入第 1 个词（2 字）
        let w0: String = no_commas.chars().take(2).collect();
        app_words.session.type_text(&w0);
        term.draw(|f| ui(f, &app_words)).unwrap();
        let (w0_x, w0_y): (u16, u16) = term.get_cursor_position().unwrap().into();
        assert_eq!(
            (w0_x, w0_y),
            (w_init_x + 4, w_init_y),
            "打了第 1 个词（2 字）光标应在第 4 列"
        );

        // 打入第 2 个词的第 1 个字（词间含空格，宽度: 2字*2 + 1空格 + 1字*2 = 7）
        let w1_c0 = no_commas.chars().nth(2).unwrap();
        app_words.session.type_text(&w1_c0.to_string());
        term.draw(|f| ui(f, &app_words)).unwrap();
        let (w1_x, w1_y): (u16, u16) = term.get_cursor_position().unwrap().into();
        assert_eq!(
            (w1_x, w1_y),
            (w_init_x + 7, w_init_y),
            "打第 2 词第 1 字后光标应在第 7 列（含空格）"
        );

        // 3. 单字赛文打到第 5 页（50 字以上），验证对照区不被误滚动而消失
        let mut app_p5 = test_app(load_builtin_text(BUILTIN_SETS[0]));
        let p5_chars: Vec<char> = app_p5.text.content.chars().take(45).collect();
        for chunk in p5_chars.chunks(10) {
            let s: String = chunk.iter().collect();
            app_p5.session.type_text(&s);
        }
        term.draw(|f| ui(f, &app_p5)).unwrap();
        let (p5_x, p5_y): (u16, u16) = term.get_cursor_position().unwrap().into();
        // 第 5 页打了 5 个字（宽 10），光标应在第 10 列
        assert_eq!(
            (p5_x, p5_y),
            (init_x + 10, init_y),
            "第 5 页打 5 字后光标应在第 10 列"
        );

        // 验证第 5 页文字在缓冲区中正常可见（未被误滚出视口）
        let buffer = term.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let current_char = app_p5.text.content.chars().nth(44).unwrap();
        assert!(
            content.contains(current_char),
            "第 5 页当前字符应在缓冲区可见"
        );
    }

    #[test]
    fn test_reference_area_scrolls_with_progress_on_long_text() {
        // 创建超过单页容量的超长赛文
        let long_raw = "甲乙丙丁戊己庚辛壬癸子丑寅卯辰巳午未申酉戌亥".repeat(10);
        let text = load_text_from_string(
            "对照区长文测试",
            long_raw.clone(),
            TextSource::Custom,
            &LoadOptions::default(),
        )
        .unwrap();
        let mut app = test_app(text);

        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 20)).unwrap();

        // 推进到第 150 个字符
        let typed_count = 150;
        let prefix: String = long_raw.chars().take(typed_count).collect();
        app.session.type_text(&prefix);

        term.draw(|f| ui(f, &app)).unwrap();
        let buffer = term.backend().buffer();
        let content = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        // 验证待打字符（第 150 个字符及其后续字符）在对照区缓冲区中清晰可见
        let target_char = long_raw.chars().nth(typed_count).unwrap();
        assert!(
            content.contains(target_char),
            "对照区应滚动并显示当前待打目标字符: {}",
            target_char
        );
    }

    #[test]
    fn test_heatmap_layout_persists_when_toggled_in_stats_view() {
        let text = load_text_from_string(
            "测试",
            "测试内容".into(),
            TextSource::Custom,
            &LoadOptions::default(),
        )
        .unwrap();
        let mut app = test_app(text);
        assert_eq!(app.settings.heatmap_layout, HeatmapLayout::Staggered);

        // 1. 打开统计视图
        app.state = AppState::Stats(StatsViewState::new(app.settings.heatmap_layout));
        if let AppState::Stats(ref s) = app.state {
            assert_eq!(s.heatmap_layout, HeatmapLayout::Staggered);
        }

        // 2. 模拟按下 'l' 切换为直列布局
        if let AppState::Stats(ref mut s) = app.state {
            s.heatmap_layout = s.heatmap_layout.next();
            app.settings.heatmap_layout = s.heatmap_layout;
            let _ = app.settings_store.save(&app.settings);
        }

        // 3. 验证内存中的 settings 和已保存到磁盘的 settings 均已变为 Ortholinear
        assert_eq!(app.settings.heatmap_layout, HeatmapLayout::Ortholinear);
        let loaded = app.settings_store.load();
        assert_eq!(loaded.heatmap_layout, HeatmapLayout::Ortholinear);

        // 4. 用户退出统计视图并重新打开，验证保留了直列矩阵状态
        app.state = AppState::Typing;
        app.state = AppState::Stats(StatsViewState::new(app.settings.heatmap_layout));
        if let AppState::Stats(ref s) = app.state {
            assert_eq!(s.heatmap_layout, HeatmapLayout::Ortholinear);
        }
    }

    #[test]
    fn test_sidebar_renders_keycaps_and_prominence() {
        let app = test_app(file_text("测试功能栏键帽"));
        let _palette = app.palette();

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let mut found_cap_left = false;
        let mut found_cap_right = false;
        let mut found_f = false;
        let mut found_b = false;
        let mut found_i = false;
        let mut found_p = false;
        let mut found_s = false;
        let mut found_o = false;
        let mut found_u = false;

        let mut all_lines = Vec::new();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..24 {
                line.push_str(buffer[(x, y)].symbol());
            }
            let clean = line.replace(' ', "");
            if clean.contains('◖') {
                found_cap_left = true;
            }
            if clean.contains('◗') {
                found_cap_right = true;
            }
            if clean.contains("◖f◗") && clean.contains("载入文件") {
                found_f = true;
            }
            if clean.contains("◖b◗") && clean.contains("内置赛文") {
                found_b = true;
            }
            if clean.contains("◖i◗") && clean.contains("自由发文") {
                found_i = true;
            }
            if clean.contains("◖p◗") && clean.contains("剪贴板发文") {
                found_p = true;
            }
            if clean.contains("◖s◗") && clean.contains("数据统计") {
                found_s = true;
            }
            if clean.contains("◖o◗") && clean.contains("设置") {
                found_o = true;
            }
            if clean.contains("◖u◗") && clean.contains("登录") {
                found_u = true;
            }
            all_lines.push(line);
        }

        assert!(found_cap_left, "功能栏应包含左圆角键帽 ◖: {:?}", all_lines);
        assert!(found_cap_right, "功能栏应包含右圆角键帽 ◗: {:?}", all_lines);
        assert!(found_f, "功能栏应高亮显示 f 载入文件: {:?}", all_lines);
        assert!(found_b, "功能栏应高亮显示 b 内置赛文: {:?}", all_lines);
        assert!(found_i, "功能栏应高亮显示 i 自由发文: {:?}", all_lines);
        assert!(found_p, "功能栏应高亮显示 p 剪贴板发文: {:?}", all_lines);
        assert!(found_s, "功能栏应高亮显示 s 数据统计: {:?}", all_lines);
        assert!(found_o, "功能栏应高亮显示 o 设置: {:?}", all_lines);
        assert!(found_u, "功能栏应高亮显示 u 登录: {:?}", all_lines);
    }

    #[test]
    fn test_all_shortcuts_without_ctrl_trigger_actions() {
        let keys_and_checkers: [(char, fn(KeyEvent) -> bool, &str); 9] = [
            ('f', is_open_browser, "f 载入文件"),
            ('b', is_open_builtin_browser, "b 内置赛文"),
            ('i', is_open_free_input, "i 自由发文"),
            ('p', is_load_clipboard, "p 剪贴板发文"),
            ('s', is_open_stats, "s 数据统计"),
            ('o', is_open_settings, "o 设置"),
            ('u', is_open_login, "u 登录"),
            ('d', is_early_finish, "d 提前结束"),
            ('r', is_restart, "r 重打"),
        ];

        for (code, checker, desc) in keys_and_checkers {
            let direct_key = KeyEvent::new(KeyCode::Char(code), KeyModifiers::NONE);
            assert!(checker(direct_key), "{desc} 在无 Ctrl 下应直接触发");

            let upper_key =
                KeyEvent::new(KeyCode::Char(code.to_ascii_uppercase()), KeyModifiers::NONE);
            assert!(checker(upper_key), "{desc} 大写在无 Ctrl 下亦应直接触发");

            let ctrl_key = KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL);
            assert!(!checker(ctrl_key), "{desc} 带 Ctrl 不应触发");
        }

        // 验证在线比赛快捷键 1/2/3
        assert_eq!(
            online_shortcut(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)),
            Some(CompetitionType::Jisu)
        );
        assert_eq!(
            online_shortcut(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE)),
            Some(CompetitionType::Jinbiao)
        );
        assert_eq!(
            online_shortcut(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE)),
            Some(CompetitionType::Jianshen)
        );

        // 验证退出使用 Ctrl-Q / Ctrl-C
        assert!(is_quit(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL
        )));
        assert!(is_quit(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_quit(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn plain_q_in_ready_state_types_char_instead_of_quitting() {
        let mut app = test_app(file_text("quick"));
        assert!(app.session.is_empty());

        // 模拟就绪态输入单键 'q'
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(!is_quit(key));
        if app.session.is_empty() && matches!(key.code, KeyCode::Backspace | KeyCode::Char(_)) {
            app.touch_typing();
            let elapsed = app.current_elapsed();
            handle_key(
                &mut app.session,
                &mut app.live_keyboard,
                app.scheme_dict.as_ref(),
                key,
                elapsed,
                Instant::now(),
            );
        }

        assert_eq!(app.session.len(), 1);
        assert!(!app.session.is_empty());
        assert_eq!(app.session.display(), vec![('q', CharStatus::Correct)]);
    }

    #[test]
    fn fullwidth_char_normalization_and_finished_key_handling() {
        assert_eq!(normalize_char('ｓ'), 's');
        assert_eq!(normalize_char('ｆ'), 'f');
        assert_eq!(normalize_char('ｂ'), 'b');
        assert_eq!(normalize_char('ｉ'), 'i');
        assert_eq!(normalize_char('１'), '1');
        assert_eq!(normalize_char('s'), 's');

        let mut app = test_app(file_text("测试"));
        app.state = AppState::Finished {
            stats: app.session.finish(Duration::from_secs(5)),
            upload: UploadState::NotApplicable { copied_stats: None },
            elapsed: Duration::from_secs(5),
        };

        // 测试全角 'ｓ' 触发进入统计视图
        let mut key_s = KeyEvent::new(KeyCode::Char('ｓ'), KeyModifiers::NONE);
        normalize_key(&mut key_s);
        assert!(handle_finished_key(&mut app, key_s));
        assert!(matches!(app.state, AppState::Stats(_)));

        // 返回成绩视图后测试全角 'ｆ' 触发进入文件浏览
        app.state = AppState::Finished {
            stats: app.session.finish(Duration::from_secs(5)),
            upload: UploadState::NotApplicable { copied_stats: None },
            elapsed: Duration::from_secs(5),
        };
        let mut key_f = KeyEvent::new(KeyCode::Char('ｆ'), KeyModifiers::NONE);
        normalize_key(&mut key_f);
        assert!(handle_finished_key(&mut app, key_f));
        assert!(matches!(app.state, AppState::Browsing));

        // 返回成绩视图后测试全角 'ｉ' 触发进入自由发文
        app.state = AppState::Finished {
            stats: app.session.finish(Duration::from_secs(5)),
            upload: UploadState::NotApplicable { copied_stats: None },
            elapsed: Duration::from_secs(5),
        };
        let mut key_i = KeyEvent::new(KeyCode::Char('ｉ'), KeyModifiers::NONE);
        normalize_key(&mut key_i);
        assert!(handle_finished_key(&mut app, key_i));
        assert!(app.free_input_modal.is_some());
    }

    #[test]
    fn test_vim_navigation_across_states() {
        let mut app = test_app(file_text("测试文本"));

        // 1. Browsing 状态 Vim 键位
        app.state = AppState::Browsing;
        app.browse_files = vec![
            PathBuf::from("a.txt"),
            PathBuf::from("b.txt"),
            PathBuf::from("c.txt"),
        ];
        app.browse_selection = 0;

        // j 下移
        app.browse_selection = (app.browse_selection + 1).min(app.browse_files.len() - 1);
        assert_eq!(app.browse_selection, 1);
        // G 首尾跳跃
        app.browse_selection = app.browse_files.len() - 1;
        assert_eq!(app.browse_selection, 2);
        // g 跳到首项
        app.browse_selection = 0;
        assert_eq!(app.browse_selection, 0);

        // 2. BrowsingBuiltin 状态 Vim 键位
        app.state = AppState::BrowsingBuiltin;
        app.builtin_selection = 0;
        app.builtin_selection = (app.builtin_selection + 1).min(BUILTIN_SETS.len() - 1);
        assert_eq!(app.builtin_selection, 1);
        app.builtin_selection = BUILTIN_SETS.len().saturating_sub(1);
        assert_eq!(app.builtin_selection, BUILTIN_SETS.len() - 1);
        app.builtin_selection = 0;
        assert_eq!(app.builtin_selection, 0);

        // 3. Settings 状态 Vim 键位 (j/k 移动焦点, h/l 调整数值)
        app.enter_settings();
        app.settings_focus = FOCUS_THEME;
        app.settings_focus = move_focus(app.settings_focus, 1);
        assert_eq!(app.settings_focus, FOCUS_RATIO);
        let ratio_before = app.settings.reference_ratio;
        app.adjust_ratio(5);
        assert_eq!(app.settings.reference_ratio, (ratio_before + 5).min(90));
        app.adjust_ratio(-5);
        assert_eq!(app.settings.reference_ratio, ratio_before);

        // 4. Stats 状态 Vim 键位
        let mut stats_state = StatsViewState::new(app.settings.heatmap_layout);
        assert_eq!(stats_state.tab, StatsTab::WpmTrend);
        stats_state.tab = StatsTab::Heatmap;
        assert_eq!(stats_state.tab, StatsTab::Heatmap);
        stats_state.tab = StatsTab::ErrorRanking;
        assert_eq!(stats_state.tab, StatsTab::ErrorRanking);

        // 错字排行光标移动与状态
        assert_eq!(stats_state.char_selected, 0);
        stats_state.char_selected += 1;
        assert_eq!(stats_state.char_selected, 1);
        stats_state.status_msg = Some("已删除错字 '你'".to_string());
        assert_eq!(stats_state.status_msg.as_deref(), Some("已删除错字 '你'"));
    }

    #[test]
    fn render_error_ranking_with_selection_and_status() {
        let mut app = test_app(file_text("测试文本"));
        let stats_state = StatsViewState {
            tab: StatsTab::ErrorRanking,
            error_ranking_focus: ErrorRankingFocus::Chars,
            char_selected: 0,
            word_selected: 0,
            status_msg: Some("已删除错字 '测'".to_string()),
            ..Default::default()
        };
        app.state = AppState::Stats(stats_state);

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let full_text: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        let clean = full_text.replace(' ', "");
        assert!(clean.contains("已删除错字'测'"));
        assert!(clean.contains("d/x删除"));
    }

    #[test]
    fn test_session_finish_punctuation_exclusion() {
        let (worker, shared_db) = DbWorker::start_in_memory().unwrap();
        let store = temp_token_store();
        let mut app = App::new_with(
            file_text("你好，世界！"),
            store.clone(),
            ApiClient::with_base_url_and_store("http://127.0.0.1:1", Some(store)),
            temp_settings_store(),
            Some(worker),
        );
        app.text.title = "标点排除测试".to_string();

        let now = Instant::now();
        // 输入 "你好。四界！"（打错标点 '，' 和汉字 '世'）
        handle_key(
            &mut app.session,
            &mut app.live_keyboard,
            app.scheme_dict.as_ref(),
            KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE),
            Duration::from_secs(1),
            now,
        );
        handle_key(
            &mut app.session,
            &mut app.live_keyboard,
            app.scheme_dict.as_ref(),
            KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE),
            Duration::from_secs(2),
            now,
        );
        handle_key(
            &mut app.session,
            &mut app.live_keyboard,
            app.scheme_dict.as_ref(),
            KeyEvent::new(KeyCode::Char('。'), KeyModifiers::NONE),
            Duration::from_secs(3),
            now,
        );
        handle_key(
            &mut app.session,
            &mut app.live_keyboard,
            app.scheme_dict.as_ref(),
            KeyEvent::new(KeyCode::Char('四'), KeyModifiers::NONE),
            Duration::from_secs(4),
            now,
        );
        handle_key(
            &mut app.session,
            &mut app.live_keyboard,
            app.scheme_dict.as_ref(),
            KeyEvent::new(KeyCode::Char('界'), KeyModifiers::NONE),
            Duration::from_secs(5),
            now,
        );
        handle_key(
            &mut app.session,
            &mut app.live_keyboard,
            app.scheme_dict.as_ref(),
            KeyEvent::new(KeyCode::Char('！'), KeyModifiers::NONE),
            Duration::from_secs(6),
            now,
        );

        app.accumulated_elapsed = Duration::from_secs(6);
        let _ = app.finish_typing();
        if let Some(w) = app.db_worker.take() {
            w.flush_and_stop();
        }

        let db = shared_db.lock().unwrap();
        let top_chars = db.get_top_mistyped_chars(10).unwrap();
        // 标点 '，' 虽被输错为 '。'，但被过滤；仅汉字 '世' 记录为错字
        assert_eq!(top_chars.len(), 1);
        assert_eq!(top_chars[0].target_char, '世');
    }

    #[test]
    fn test_heatmap_layout_simplified_keys() {
        let mut app = test_app(file_text("测试文本"));
        app.state = AppState::Stats(StatsViewState {
            tab: StatsTab::Heatmap,
            heatmap_layout: HeatmapLayout::Staggered,
            heatmap_source: HeatmapSource::RawKeypress,
            ..Default::default()
        });

        let backend = ratatui::backend::TestBackend::new(160, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let full_text: String = (0..terminal.backend().buffer().area.height)
            .map(|y| {
                (0..terminal.backend().buffer().area.width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        let clean = full_text.replace(' ', "");
        // 校验已精简掉 Caps, Tab, Ctrl, Alt
        assert!(!clean.contains("[Caps"));
        assert!(!clean.contains("[Tab"));
        assert!(!clean.contains("[Ctrl"));
        assert!(!clean.contains("[Alt"));
        // 校验仍包含 Bksp 与 Space
        assert!(clean.contains("[Bksp"));
        assert!(clean.contains("Space"));
    }

    #[test]
    fn test_trend_metric_toggle_and_kps_chart_rendering() {
        let mut app = test_app(file_text("测试文本"));
        let mut stats_state = StatsViewState {
            tab: StatsTab::WpmTrend,
            trend_metric: TrendMetric::Wpm,
            ..Default::default()
        };
        assert_eq!(stats_state.trend_metric, TrendMetric::Wpm);
        stats_state.trend_metric = stats_state.trend_metric.next();
        assert_eq!(stats_state.trend_metric, TrendMetric::Kps);
        assert_eq!(stats_state.trend_metric.label(), "KPS 击速");

        app.state = AppState::Stats(stats_state);

        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let full_text: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        let clean = full_text.replace(' ', "");
        assert!(clean.contains("KPS击速历史演进趋势"));
        assert!(clean.contains("KPS(击/秒)"));
        assert!(clean.contains("切换指标(WPM/KPS)"));
    }

    #[test]
    fn test_is_open_sponsor_shortcut() {
        let key_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        let key_upper_d = KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE);
        let key_ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        let key_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);

        assert!(is_open_sponsor(key_d));
        assert!(is_open_sponsor(key_upper_d));
        assert!(!is_open_sponsor(key_ctrl_d));
        assert!(!is_open_sponsor(key_o));
    }

    #[test]
    fn test_open_sponsor_and_render_sponsor_view() {
        let mut app = test_app(file_text("测试文本"));
        assert!(matches!(app.state, AppState::Typing));

        // 模拟通过 open_sponsor 进入赞赏页面
        app.open_sponsor();
        assert!(matches!(app.state, AppState::Sponsor));

        let backend = ratatui::backend::TestBackend::new(120, 35);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer().clone();
        let full_text: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        let clean = full_text.replace(' ', "");

        // 校验标题与幽默寄语文案
        assert!(clean.contains("赞赏&支持开源开发"));
        assert!(clean.contains("键盘敲烂，码长砍半！给作者投喂一杯咖啡"));
        assert!(clean.contains("微信支付(WeChatPay)"));
        assert!(clean.contains("支付宝(Alipay)"));
        assert!(clean.contains("[Esc/q/d]返回跟打主页"));
    }

    #[test]
    fn test_sidebar_sponsor_menu_item_activation() {
        let mut app = test_app(file_text("测试文本"));
        let sponsor_idx = SIDEBAR_MENU_ITEMS
            .iter()
            .position(|&item| item == SidebarMenuItem::Sponsor)
            .expect("SidebarMenuItem::Sponsor should exist");
        app.sidebar_selected = sponsor_idx;

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        activate_sidebar_menu_item(&mut app, &mut terminal).unwrap();

        assert!(matches!(app.state, AppState::Sponsor));
    }

    #[test]
    fn test_group_size_customization_and_cycle() {
        let dir = temp_dir("test_group_size_cycle");
        fs::create_dir_all(&dir).unwrap();
        let store = SettingsStore::new(dir.join("settings.json"));
        let settings = store.load();
        assert_eq!(settings.group_size, 10);

        let mut app = App::new_with(
            load_builtin_text(BUILTIN_SETS[0]),
            TokenStore::new(dir.join("token.json")),
            ApiClient::new(),
            store,
            None,
        );

        assert_eq!(app.settings.group_size, 10);
        assert_eq!(app.session.group_size(), 10);

        // g 键循环切换预设 (10 -> 15 -> 20 -> 25 -> 30 -> 50 -> 5 -> 10)
        app.cycle_group_size();
        assert_eq!(app.settings.group_size, 15);
        assert_eq!(app.session.group_size(), 15);

        app.cycle_group_size();
        assert_eq!(app.settings.group_size, 20);
        assert_eq!(app.session.group_size(), 20);

        // 重启或重新载入赛文后依然保留分组
        app.load_selected_builtin();
        assert_eq!(app.session.group_size(), 20);
        app.restart();
        assert_eq!(app.session.group_size(), 20);
    }

    #[test]
    fn test_settings_group_size_row_rendering_and_step_adjustment() {
        let dir = temp_dir("test_settings_group_size");
        fs::create_dir_all(&dir).unwrap();
        let store = SettingsStore::new(dir.join("settings.json"));
        let mut app = App::new_with(
            load_builtin_text(BUILTIN_SETS[0]),
            TokenStore::new(dir.join("token.json")),
            ApiClient::new(),
            store,
            None,
        );

        app.enter_settings();
        app.settings_focus = FOCUS_GROUP_SIZE;

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let buffer = terminal.backend().buffer().clone();
        let full_text: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        let clean = full_text.replace(' ', "");
        assert!(clean.contains("分组大小:10字/词"));
    }

    #[test]
    fn test_builtin_pagination_with_custom_group_size() {
        let theme = Theme::preset(ThemePreset::CatppuccinMocha);
        let text = load_builtin_text(BUILTIN_SETS[0]); // 常用单字前五百
        let wb = text.session_word_boundaries();

        // 构造 group_size = 5 的会话
        let mut session = Session::new_gated_with_words_and_size(&text.content, true, &wb, 5);
        assert_eq!(session.group_size(), 5);

        let rendered = original_line(&session, &text, theme, false, None);
        assert_eq!(
            rendered.lines[0].spans.len(),
            5,
            "分组为 5 时对照区首页只显示 5 字"
        );

        // 全对打完 5 个字翻页
        let first_5: String = text.content.chars().take(5).collect();
        session.type_text(&first_5);
        assert_eq!(session.completed_groups(), 1);

        let rendered_p2 = original_line(&session, &text, theme, false, None);
        assert_eq!(rendered_p2.lines[0].spans.len(), 5, "翻页后第二页也是 5 字");
    }
}

/// 方案热重载（issue #91/#94）端到端测试：编辑源文件 → 自动驱逐缓存并重载。
#[cfg(test)]
mod scheme_hot_reload_tests {
    use super::*;
    use dazitui_core::{ApiClient, SchemeInfo, SettingsStore, TextSource, TokenStore};
    use std::time::Duration;

    fn tmp(suffix: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dazitui-hot-{stamp}-{suffix}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_text(content: &str) -> Text {
        Text {
            title: "t".into(),
            content: content.into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        }
    }

    /// 构造最小 App（临时 token/设置存储 + 不可达 API），并指向 `discovered` 中的临时方案。
    fn app_with_scheme(schema_path: PathBuf) -> App {
        let token_dir = tmp("tok");
        let settings_dir = tmp("set");
        let mut app = App::new_with(
            file_text("练习"),
            TokenStore::new(token_dir.join("token")),
            ApiClient::with_base_url_and_store(
                "http://127.0.0.1:1",
                Some(TokenStore::new(token_dir.join("token"))),
            ),
            SettingsStore::new(settings_dir.join("settings")),
            None,
        );
        // 覆盖自动发现，指向临时方案，避免加载本机真实 fcitx5 方案。
        app.settings.scheme = "demo".to_string();
        app.discovered = vec![SchemeInfo {
            id: "demo".to_string(),
            display_name: "Demo".to_string(),
            path: schema_path,
        }];
        app
    }

    #[test]
    fn hot_reload_reloads_scheme_dict_after_source_file_change() {
        let dir = tmp("scheme");
        let schema = dir.join("demo.schema.yaml");
        let dict = dir.join("demo.dict.yaml");
        std::fs::write(
            &schema,
            "schema:\n  name: Demo\n  schema_id: demo\n",
        )
        .unwrap();
        // 初始词典：文→vw，化→ah
        std::fs::write(&dict, "---\nname: demo\n...\n\n文\tvw\n化\tah\n").unwrap();

        let mut app = app_with_scheme(schema.clone());

        // 首次加载
        app.reload_scheme_dict();
        let mut loaded = false;
        for _ in 0..30 {
            app.poll_scheme_loader();
            if app.scheme_dict.is_some() {
                loaded = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(loaded, "首次方案加载应成功");
        assert_eq!(
            app.scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("文")),
            Some("vw")
        );
        // 监控应已对源文件闭包建监控
        assert!(
            app.scheme_watch_paths
                .as_ref()
                .map(|p| p.contains(&dict))
                .unwrap_or(false),
            "热监控应覆盖方案源文件"
        );

        // 修改源文件：新增「新→xx」
        std::fs::write(&dict, "---\nname: demo\n...\n\n文\tvw\n化\tah\n新\txx\n").unwrap();

        // 驱动热重载循环（模拟每帧：先 hot_reload 检测，再 poll 消费异步结果）
        let mut reloaded = false;
        for _ in 0..60 {
            app.poll_scheme_hot_reload();
            std::thread::sleep(Duration::from_millis(50));
            app.poll_scheme_loader();
            if app
                .scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("新"))
                == Some("xx")
            {
                reloaded = true;
                break;
            }
        }
        assert!(reloaded, "改动源文件后应自动热重载，使「新→xx」生效");
    }

    #[test]
    fn hot_reload_disabled_when_monitor_scheme_off() {
        let dir = tmp("scheme-off");
        let schema = dir.join("demo.schema.yaml");
        let dict = dir.join("demo.dict.yaml");
        std::fs::write(&schema, "schema:\n  name: Demo\n  schema_id: demo\n").unwrap();
        std::fs::write(&dict, "---\nname: demo\n...\n\n文\tvw\n化\tah\n").unwrap();

        let mut app = app_with_scheme(schema.clone());
        // 关闭热监控总开关。
        app.settings.monitor_scheme = false;

        // 首次加载（开关不影响正常加载）
        app.reload_scheme_dict();
        let mut loaded = false;
        for _ in 0..30 {
            app.poll_scheme_loader();
            if app.scheme_dict.is_some() {
                loaded = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(loaded, "首次方案加载应成功（与开关无关）");
        assert_eq!(
            app.scheme_dict.as_ref().and_then(|d| d.get_primary_code("文")),
            Some("vw")
        );
        // 开关关闭时不应对源文件建监控。
        assert!(
            app.scheme_watch_paths.is_none(),
            "开关关闭时不应对源文件建监控"
        );

        // 修改源文件
        std::fs::write(&dict, "---\nname: demo\n...\n\n文\tvw\n化\tah\n新\txx\n").unwrap();

        // 驱动循环，确认不会热重载
        for _ in 0..40 {
            app.poll_scheme_hot_reload();
            std::thread::sleep(Duration::from_millis(50));
            app.poll_scheme_loader();
        }
        assert_eq!(
            app.scheme_dict.as_ref().and_then(|d| d.get_primary_code("新")),
            None,
            "开关关闭时改动源文件不应触发热重载"
        );
    }

    #[test]
    fn hot_reload_shows_flash_on_success_and_fades() {
        let dir = tmp("scheme-flash");
        let schema = dir.join("demo.schema.yaml");
        let dict = dir.join("demo.dict.yaml");
        std::fs::write(&schema, "schema:\n  name: Demo\n  schema_id: demo\n").unwrap();
        std::fs::write(&dict, "---\nname: demo\n...\n\n文\tvw\n化\the\n").unwrap();

        let mut app = app_with_scheme(schema.clone());

        // 首次加载（非热重载路径）不应触发热重载闪现。
        app.reload_scheme_dict();
        for _ in 0..30 {
            app.poll_scheme_loader();
            if app.scheme_dict.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !app.scheme_reload_flash_active(),
            "初始加载不应闪现热重载提示"
        );

        // 改动源文件：新增「新→xx」
        std::fs::write(&dict, "---\nname: demo\n...\n\n文\tvw\n化\the\n新\txx\n").unwrap();

        let mut reloaded = false;
        for _ in 0..60 {
            app.poll_scheme_hot_reload();
            std::thread::sleep(Duration::from_millis(50));
            app.poll_scheme_loader();
            if app
                .scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("新"))
                == Some("xx")
            {
                reloaded = true;
                break;
            }
        }
        assert!(reloaded, "改动源文件后应自动热重载，使「新→xx」生效");

        // 成功热重载后，状态栏应闪现「方案已重载」，且约 2s 后淡出（不堆叠为多条）。
        assert!(
            app.scheme_reload_flash_active(),
            "成功热重载后应闪现「方案已重载」"
        );
        let remaining = app
            .scheme_reload_flash_at
            .expect("热重载成功后应置位闪现截止时刻")
            .duration_since(std::time::Instant::now());
        assert!(
            remaining <= Duration::from_millis(2100) && remaining > Duration::from_millis(1500),
            "闪现应在约 2s 内淡出，实际剩余 {:?}",
            remaining
        );

        // 连续多次重载不堆叠：再次改动并热重载，仍是单条、未过期时间被重置（非叠加）。
        std::fs::write(
            &dict,
            "---\nname: demo\n...\n\n文\tvw\n化\the\n新\txx\n又\tzz\n",
        )
        .unwrap();
        let mut reloaded2 = false;
        for _ in 0..60 {
            app.poll_scheme_hot_reload();
            std::thread::sleep(Duration::from_millis(50));
            app.poll_scheme_loader();
            if app
                .scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("又"))
                == Some("zz")
            {
                reloaded2 = true;
                break;
            }
        }
        assert!(reloaded2, "第二次改动也应自动热重载");
        assert!(
            app.scheme_reload_flash_active(),
            "连续重载仍只闪现单条「方案已重载」"
        );
    }

    #[test]
    fn hot_reload_failure_keeps_old_scheme_and_reports_error() {
        let dir = tmp("scheme-fail");
        let schema = dir.join("demo.schema.yaml");
        let dict = dir.join("demo.dict.yaml");
        std::fs::write(&schema, "schema:\n  name: Demo\n  schema_id: demo\n").unwrap();
        std::fs::write(&dict, "---\nname: demo\n...\n\n文\tvw\n化\the\n").unwrap();

        let mut app = app_with_scheme(schema.clone());

        // 首次加载
        app.reload_scheme_dict();
        for _ in 0..30 {
            app.poll_scheme_loader();
            if app.scheme_dict.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            app.scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("文")),
            Some("vw"),
            "初始加载应成功"
        );

        // 模拟「坏部署」：把词典文件换成同名目录，使后台加载读取失败（解析/读取错误）。
        std::fs::remove_file(&dict).unwrap();
        std::fs::create_dir(&dict).unwrap();

        // 驱动热重载循环，等待失败结果
        let mut failed = false;
        for _ in 0..60 {
            app.poll_scheme_hot_reload();
            std::thread::sleep(Duration::from_millis(50));
            app.poll_scheme_loader();
            if app.scheme_reload_error.is_some() {
                failed = true;
                break;
            }
        }
        assert!(failed, "坏部署热重载应失败并置位错误提示");
        // 旧方案应被保留（不空白、不丢失词条）
        assert_eq!(
            app.scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("文")),
            Some("vw"),
            "重载失败应保留上一版方案"
        );
        assert!(
            app.scheme_reload_error
                .as_deref()
                .unwrap()
                .contains("方案重载失败"),
            "状态栏应报错说明方案重载失败：{:?}",
            app.scheme_reload_error
        );

        // 修复部署：还原为合法词典并新增「新→xx」
        std::fs::remove_dir(&dict).unwrap();
        std::fs::write(
            &dict,
            "---\nname: demo\n...\n\n文\tvw\n化\the\n新\txx\n",
        )
        .unwrap();

        let mut recovered = false;
        for _ in 0..60 {
            app.poll_scheme_hot_reload();
            std::thread::sleep(Duration::from_millis(50));
            app.poll_scheme_loader();
            if app
                .scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("新"))
                == Some("xx")
            {
                recovered = true;
                break;
            }
        }
        assert!(recovered, "修复部署后应热重载成功");
        // 成功重载应清除失败提示
        assert!(
            app.scheme_reload_error.is_none(),
            "成功重载后应清除失败提示"
        );
    }

    /// 真实场景复刻（yoyo-pure-km）：主表 `import` 用户表，用户表里的整词码
    /// （如用户在 yoyo-user.dict.yaml 加的「经典造型→RFmf」）应当被合并进词库；
    /// 改用户表后热重载应把新码吃进来（issue #94 在多文件 import 场景下的端到端验证）。
    #[test]
    fn hot_reload_picks_up_edit_in_imported_user_dict() {
        let dir = tmp("scheme-import");
        let schema = dir.join("demo.schema.yaml");
        let primary = dir.join("demo.dict.yaml");
        let user = dir.join("demo-user.dict.yaml");
        std::fs::write(
            &schema,
            "schema:\n  name: Demo\n  schema_id: demo\n  translator:\n    dictionary: demo\n",
        )
        .unwrap();
        std::fs::write(
            &primary,
            "---\nname: demo\nimport_tables:\n  - demo-user\n...\n\n经\tRv\n典\tFX\n造\tmbp\n型\tfpV\n",
        )
        .unwrap();
        std::fs::write(&user, "---\nname: demo-user\n...\n\n经典造型\tRFmf\t100\n").unwrap();

        let mut app = app_with_scheme(schema.clone());

        app.reload_scheme_dict();
        let mut loaded = false;
        for _ in 0..30 {
            app.poll_scheme_loader();
            if app.scheme_dict.is_some() {
                loaded = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(loaded, "首次加载应成功");
        assert_eq!(
            app.scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("经典造型")),
            Some("RFmf"),
            "初始加载应合并 import 进来的用户表整词码"
        );
        // 监控闭包应同时覆盖主表与用户表
        let paths = app.scheme_watch_paths.clone().unwrap_or_default();
        assert!(paths.contains(&primary), "应监控主表");
        assert!(paths.contains(&user), "应监控 import 进来的用户表");

        // 模拟用户改了 yoyo-user.dict.yaml：把 经典造型 的码改成 ZZZZ
        std::fs::write(&user, "---\nname: demo-user\n...\n\n经典造型\tZZZZ\t100\n").unwrap();

        let mut reloaded = false;
        for _ in 0..80 {
            app.poll_scheme_hot_reload();
            std::thread::sleep(Duration::from_millis(50));
            app.poll_scheme_loader();
            if app
                .scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("经典造型"))
                == Some("ZZZZ")
            {
                reloaded = true;
                break;
            }
        }
        assert!(
            reloaded,
            "改动 import 进来的用户表后，热重载应把新码吃进来"
        );
    }
}

/// 切换方案时重建监控闭包（issue #95）测试。
#[cfg(test)]
mod scheme_switch_watch_tests {
    use super::*;
    use dazitui_core::{ApiClient, SchemeInfo, SettingsStore, TextSource, TokenStore};
    use std::time::Duration;

    fn tmp(suffix: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dazitui-switch-{stamp}-{suffix}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_text(content: &str) -> Text {
        Text {
            title: "t".into(),
            content: content.into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        }
    }

    /// 构造最小 App，并指向 `discovered` 中的多个临时方案。
    fn app_with_schemes(schemas: &[(String, PathBuf)]) -> App {
        let token_dir = tmp("tok");
        let settings_dir = tmp("set");
        let mut app = App::new_with(
            file_text("练习"),
            TokenStore::new(token_dir.join("token")),
            ApiClient::with_base_url_and_store(
                "http://127.0.0.1:1",
                Some(TokenStore::new(token_dir.join("token"))),
            ),
            SettingsStore::new(settings_dir.join("settings")),
            None,
        );
        app.settings.scheme = String::new();
        app.discovered = schemas
            .iter()
            .map(|(id, p)| SchemeInfo {
                id: id.clone(),
                display_name: id.clone(),
                path: p.clone(),
            })
            .collect();
        app
    }

    /// 等待当前 scheme 加载完成且监控闭包包含 `expect_dict`。
    fn wait_loaded(app: &mut App, expect_dict: &PathBuf) -> bool {
        for _ in 0..30 {
            app.poll_scheme_loader();
            if app
                .scheme_watch_paths
                .as_ref()
                .map(|p| p.contains(expect_dict))
                .unwrap_or(false)
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn switching_scheme_rebuilds_watch_closure() {
        let dir = tmp("switch");
        let d_schema = dir.join("demo.schema.yaml");
        let d_dict = dir.join("demo.dict.yaml");
        std::fs::write(&d_schema, "schema:\n  name: Demo\n  schema_id: demo\n").unwrap();
        std::fs::write(&d_dict, "---\nname: demo\n...\n\n文\tvw\n").unwrap();
        let a_schema = dir.join("alt.schema.yaml");
        let a_dict = dir.join("alt.dict.yaml");
        std::fs::write(&a_schema, "schema:\n  name: Alt\n  schema_id: alt\n").unwrap();
        std::fs::write(&a_dict, "---\nname: alt\n...\n\n字\tqq\n").unwrap();

        let mut app = app_with_schemes(&[
            ("demo".to_string(), d_schema.clone()),
            ("alt".to_string(), a_schema.clone()),
        ]);

        // 加载 demo（首次，异步）
        app.settings.scheme = "demo".to_string();
        app.reload_scheme_dict();
        assert!(wait_loaded(&mut app, &d_dict), "demo 应加载并建监控");

        // 切到 alt（异步）
        app.settings.scheme = "alt".to_string();
        app.reload_scheme_dict();
        assert!(wait_loaded(&mut app, &a_dict), "切到 alt 应重建监控到 alt");
        assert!(
            !app.scheme_watch_paths.as_ref().unwrap().contains(&d_dict),
            "切换后不应再监控 demo 文件"
        );

        // 切回 demo：此时 demo 已在缓存中，走「缓存命中」分支——这里正是不重建监控的陷阱。
        app.settings.scheme = "demo".to_string();
        app.reload_scheme_dict();
        assert!(
            app.scheme_watch_paths.as_ref().unwrap().contains(&d_dict),
            "缓存命中切回 demo 也应重建监控到 demo"
        );
        assert!(
            !app.scheme_watch_paths.as_ref().unwrap().contains(&a_dict),
            "切回后不应再监控 alt 文件"
        );
    }
}

/// 热监控外部行为回归测试（issue #99）：全程使用临时目录 fixture，
/// 避开 live fcitx5/rime，保证 hermetic 与可移植；最高测试缝为每帧 `poll_scheme_loader`。
#[cfg(test)]
mod scheme_hot_reload_regression_tests {
    use super::*;
    use dazitui_core::{ApiClient, SchemeDict, SchemeInfo, SettingsStore, TextSource, TokenStore};
    use std::time::Duration;

    fn tmp(suffix: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dazitui-reg-{stamp}-{suffix}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_text(content: &str) -> Text {
        Text {
            title: "t".into(),
            content: content.into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        }
    }

    /// 最小 App，指向单个临时方案（避开本机真实 fcitx5 方案）。
    fn app_with_scheme(schema_path: PathBuf) -> App {
        let token_dir = tmp("tok");
        let settings_dir = tmp("set");
        let mut app = App::new_with(
            file_text("练习"),
            TokenStore::new(token_dir.join("token")),
            ApiClient::with_base_url_and_store(
                "http://127.0.0.1:1",
                Some(TokenStore::new(token_dir.join("token"))),
            ),
            SettingsStore::new(settings_dir.join("settings")),
            None,
        );
        app.settings.scheme = "demo".to_string();
        app.discovered = vec![SchemeInfo {
            id: "demo".to_string(),
            display_name: "Demo".to_string(),
            path: schema_path,
        }];
        app
    }

    /// 最小 App，指向多个临时方案（用于切方案场景）。
    fn app_with_schemes(schemas: &[(String, PathBuf)]) -> App {
        let token_dir = tmp("tok");
        let settings_dir = tmp("set");
        let mut app = App::new_with(
            file_text("练习"),
            TokenStore::new(token_dir.join("token")),
            ApiClient::with_base_url_and_store(
                "http://127.0.0.1:1",
                Some(TokenStore::new(token_dir.join("token"))),
            ),
            SettingsStore::new(settings_dir.join("settings")),
            None,
        );
        app.settings.scheme = String::new();
        app.discovered = schemas
            .iter()
            .map(|(id, p)| SchemeInfo {
                id: id.clone(),
                display_name: id.clone(),
                path: p.clone(),
            })
            .collect();
        app
    }

    /// 同步等待当前方案首次加载完成（不触发热监控）。
    fn load_initial(app: &mut App) -> bool {
        app.reload_scheme_dict();
        for _ in 0..30 {
            app.poll_scheme_loader();
            if app.scheme_dict.is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// 驱动热重载循环（每帧：先检测改动，再消费异步结果），直到 `pred` 满足。
    fn drive_hot_reload(app: &mut App, pred: impl Fn(&App) -> bool) -> bool {
        for _ in 0..80 {
            app.poll_scheme_hot_reload();
            std::thread::sleep(Duration::from_millis(50));
            app.poll_scheme_loader();
            if pred(app) {
                return true;
            }
        }
        false
    }

    // (1) 变更被 watch 文件 → 确实触发重新加载且 scheme_dict 更新
    #[test]
    fn regression_watched_file_change_triggers_reload() {
        let dir = tmp("r1");
        let schema = dir.join("demo.schema.yaml");
        let dict = dir.join("demo.dict.yaml");
        std::fs::write(&schema, "schema:\n  name: Demo\n  schema_id: demo\n").unwrap();
        std::fs::write(&dict, "---\nname: demo\n...\n\n文\tvw\n").unwrap();

        let mut app = app_with_scheme(schema.clone());
        assert!(load_initial(&mut app), "初始加载应成功");
        assert_eq!(
            app.scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("文")),
            Some("vw")
        );

        std::fs::write(&dict, "---\nname: demo\n...\n\n文\tvw\n新\txx\n").unwrap();

        let ok = drive_hot_reload(&mut app, |a| {
            a.scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("新"))
                == Some("xx")
        });
        assert!(
            ok,
            "(1) 变更被 watch 文件应触发热重载并使新词条生效"
        );
    }

    // (2) 部署坏 YAML → 旧方案被保留
    #[test]
    fn regression_bad_deploy_keeps_old_scheme() {
        let dir = tmp("r2");
        let schema = dir.join("demo.schema.yaml");
        let dict = dir.join("demo.dict.yaml");
        std::fs::write(&schema, "schema:\n  name: Demo\n  schema_id: demo\n").unwrap();
        std::fs::write(&dict, "---\nname: demo\n...\n\n文\tvw\n").unwrap();

        let mut app = app_with_scheme(schema.clone());
        assert!(load_initial(&mut app), "初始加载应成功");

        // 坏部署：词典文件换成同名目录，使后台读取失败
        std::fs::remove_file(&dict).unwrap();
        std::fs::create_dir(&dict).unwrap();

        let failed = drive_hot_reload(&mut app, |a| a.scheme_reload_error.is_some());
        assert!(failed, "(2) 坏部署应失败并报错");
        assert_eq!(
            app.scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("文")),
            Some("vw"),
            "(2) 失败应保留旧方案"
        );

        // 修复部署
        std::fs::remove_dir(&dict).unwrap();
        std::fs::write(&dict, "---\nname: demo\n...\n\n文\tvw\n新\txx\n").unwrap();
        let recovered = drive_hot_reload(&mut app, |a| {
            a.scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("新"))
                == Some("xx")
        });
        assert!(recovered, "(2) 修复后应成功重载");
        assert!(app.scheme_reload_error.is_none(), "(2) 成功应清除错误");
    }

    // (3) scheme_cache 驱逐确实强制了重新派发（非命中旧缓存）
    #[test]
    fn regression_cache_eviction_forces_redispatch() {
        let dir = tmp("r3");
        let schema = dir.join("demo.schema.yaml");
        let dict = dir.join("demo.dict.yaml");
        std::fs::write(&schema, "schema:\n  name: Demo\n  schema_id: demo\n").unwrap();
        std::fs::write(&dict, "---\nname: demo\n...\n\n文\tvw\n").unwrap();

        let mut app = app_with_scheme(schema.clone());
        assert!(load_initial(&mut app), "初始加载应成功");
        // 缓存已填充当前方案
        assert!(app.scheme_cache.contains_key("demo"));

        // 注入一条「陈旧伪造」缓存：若热重载命中旧缓存，scheme_dict 会错误包含该词条
        let mut stale = SchemeDict::default();
        stale.add_entry("缓存", "stale");
        app.scheme_cache.insert("demo".to_string(), stale);

        // 修改磁盘文件：新增「新→xx」
        std::fs::write(&dict, "---\nname: demo\n...\n\n文\tvw\n新\txx\n").unwrap();

        let ok = drive_hot_reload(&mut app, |a| {
            a.scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("新"))
                == Some("xx")
        });
        assert!(
            ok,
            "(3) 热重载应重新派发并从磁盘读取新内容"
        );

        // 断言 scheme_dict 来自磁盘（含新词条），且未命中陈旧伪造缓存
        assert_eq!(
            app.scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("新")),
            Some("xx")
        );
        assert!(
            app.scheme_dict
                .as_ref()
                .and_then(|d| d.get_primary_code("缓存"))
                .is_none(),
            "(3) 热重载不应命中被驱逐前的陈旧缓存"
        );

        // 断言缓存已被驱逐并重新派发填充为磁盘内容
        let cached = app
            .scheme_cache
            .get("demo")
            .expect("(3) 缓存应被重新填充");
        assert_eq!(cached.get_primary_code("新"), Some("xx"));
        assert!(
            cached.get_primary_code("缓存").is_none(),
            "(3) 重新填充的缓存不应含陈旧伪造词条"
        );
    }

    // (4) 切方案时 watch 闭包随之切换
    #[test]
    fn regression_switch_scheme_switches_watch_closure() {
        let dir = tmp("r4");
        let d_schema = dir.join("demo.schema.yaml");
        let d_dict = dir.join("demo.dict.yaml");
        std::fs::write(&d_schema, "schema:\n  name: Demo\n  schema_id: demo\n").unwrap();
        std::fs::write(&d_dict, "---\nname: demo\n...\n\n文\tvw\n").unwrap();
        let a_schema = dir.join("alt.schema.yaml");
        let a_dict = dir.join("alt.dict.yaml");
        std::fs::write(&a_schema, "schema:\n  name: Alt\n  schema_id: alt\n").unwrap();
        std::fs::write(&a_dict, "---\nname: alt\n...\n\n字\tqq\n").unwrap();

        let mut app = app_with_schemes(&[
            ("demo".to_string(), d_schema.clone()),
            ("alt".to_string(), a_schema.clone()),
        ]);

        app.settings.scheme = "demo".to_string();
        app.reload_scheme_dict();
        let mut demo_loaded = false;
        for _ in 0..30 {
            app.poll_scheme_loader();
            if app
                .scheme_watch_paths
                .as_ref()
                .map(|p| p.contains(&d_dict))
                .unwrap_or(false)
            {
                demo_loaded = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(demo_loaded, "(4) demo 应加载并监控其源文件");

        // 切到 alt
        app.settings.scheme = "alt".to_string();
        app.reload_scheme_dict();
        let mut alt_loaded = false;
        for _ in 0..30 {
            app.poll_scheme_loader();
            if app
                .scheme_watch_paths
                .as_ref()
                .map(|p| p.contains(&a_dict))
                .unwrap_or(false)
            {
                alt_loaded = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(alt_loaded, "(4) 切到 alt 应重建监控闭包到 alt 源文件");
        assert!(
            app.scheme_watch_paths
                .as_ref()
                .map(|p| !p.contains(&d_dict))
                .unwrap_or(false),
            "(4) 切方案后不应再监控原 demo 文件"
        );
    }
}
