use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use dazitui_core::ThemePreset;
use dazitui_core::{
    ApiClient, ApiError, AuthSession, BUILTIN_SETS, CharStatus, CompetitionType, DbTask, DbWorker,
    ErrorRecordItem, ErrorType, FONT_SIZE_PT, KeyboardMode, KeypressRecordItem, LoadError,
    LoadOptions, Rgb, SchemeDict, Session, SessionRecord, Settings, SettingsStore, Stats, StatsDb,
    Text, TextSource, Theme, TokenStore, env_credentials, format_time, is_auth_failure,
    load_builtin_text, load_builtin_text_shuffled, load_text_from_clipboard, load_text_from_file,
    load_text_from_string, lttb_downsample, osc_font_size_sequence, osc52_clipboard,
    save_text_to_file,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::prelude::Stylize;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span, Text as TextLines};
use ratatui::widgets::{
    Axis, Block, BorderType, Chart, Clear, Dataset, GraphType, Paragraph, Wrap,
};
use ratatui_themes::{ThemeName, ThemePalette};

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

/// 键位热力图布局模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum HeatmapLayout {
    #[default]
    Staggered, // 标准斜列 (ANSI 60%)
    Ortholinear, // 直列矩阵 (Planck 4x12)
}

impl HeatmapLayout {
    fn next(self) -> Self {
        match self {
            Self::Staggered => Self::Ortholinear,
            Self::Ortholinear => Self::Staggered,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Staggered => "标准斜列 (ANSI 60%)",
            Self::Ortholinear => "直列矩阵 (4x12)",
        }
    }
}

/// 键位热力图数据视角。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum HeatmapSource {
    #[default]
    SchemeProjected, // 方案反查击键
    RawKeypress,     // 物理捕获击键
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
    wpm_range: WpmChartRange,
    heatmap_layout: HeatmapLayout,
    heatmap_source: HeatmapSource,
    error_ranking_focus: ErrorRankingFocus,
    char_scroll: usize,
    word_scroll: usize,
}

/// 将 core 的 ThemePreset 映射为 ratatui_themes 的 ThemePalette。
pub fn theme_palette(preset: ThemePreset) -> ThemePalette {
    let name = match preset {
        ThemePreset::CatppuccinMocha => ThemeName::CatppuccinMocha,
        ThemePreset::TokyoNight => ThemeName::TokyoNight,
        ThemePreset::Nord => ThemeName::Nord,
        ThemePreset::Dracula => ThemeName::Dracula,
        ThemePreset::Gruvbox => ThemeName::GruvboxDark,
        ThemePreset::RosePine => ThemeName::RosePine,
        ThemePreset::Kanagawa => ThemeName::Kanagawa,
        ThemePreset::OneDark => ThemeName::OneDarkPro,
    };
    name.palette()
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
}

/// 设置视图焦点项下标。
const FOCUS_THEME: usize = 0;
const FOCUS_RATIO: usize = 1;
const FOCUS_BOLD: usize = 2;
const FOCUS_FONT: usize = 3;
const FOCUS_KEYBOARD: usize = 4;
const FOCUS_INPUT_METHOD: usize = 5;
/// 设置视图焦点项总数。
const SETTINGS_FOCUS_COUNT: usize = 6;

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
    "麓鸣并击",
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
    Stats,
    Settings,
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
    SidebarMenuItem::Stats,
    SidebarMenuItem::Settings,
    SidebarMenuItem::Login,
];

/// 成绩视图里的成绩上传状态（在线赛文完成跟打后自动上传）。
#[derive(Debug, Clone, PartialEq)]
enum UploadState {
    /// 离线赛文：无需上传。
    NotApplicable,
    /// 上传中（同步网络请求期间）。
    Uploading,
    /// 上传成功：结构化排名（`None` = 服务器未返回）+ 已复制的分享文本。
    Success {
        ranking: Option<String>,
        share_text: String,
    },
    /// 上传失败：友好文案 + 是否需要重新登录 + 原始服务器错误（次要信息）。
    Failed {
        message: String,
        need_relogin: bool,
        detail: Option<String>,
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
    /// 设置视图当前焦点项（FOCUS_THEME/FOCUS_RATIO/FOCUS_BOLD/FOCUS_FONT/FOCUS_KEYBOARD/FOCUS_INPUT_METHOD）。
    settings_focus: usize,
    /// 内置赛文浏览中的乱序开关（`true` = 载入时打乱顺序）。
    builtin_shuffle: bool,
    /// 内置赛文浏览器预览缓存 `(title, body)`。
    /// 乱序开时存乱序版预览（避免每帧重新随机导致闪烁），关时存顺序版预览。
    /// 在 `open_builtin_browser` 与 Up/Down/s 按键时重新生成。
    builtin_preview: Option<(String, String)>,
    /// 自定义输入法名称弹窗（`None` = 未打开）。
    input_method_modal: Option<InputMethodModal>,
    /// 自由发文编辑弹窗（`None` = 未打开）。
    free_input_modal: Option<FreeInputModal>,
    /// 实时虚拟键盘状态。
    live_keyboard: LiveKeyboard,
    /// 后台数据库异步写入 Worker。
    db_worker: Option<DbWorker>,
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

/// 自定义输入法模态框按键动作。
#[derive(Debug, PartialEq, Eq)]
enum InputMethodModalAction {
    None,
    Save(String),
    Cancel,
}

/// 自定义输入法名称弹窗状态。
#[derive(Debug, Default)]
struct InputMethodModal {
    /// 当前正在编辑的文本。
    input: String,
}

impl InputMethodModal {
    /// 新建弹窗，预填当前自定义值（若为「无」或预设，则置空）。
    fn new(current: &str) -> Self {
        let prefill = if INPUT_METHOD_PRESETS.contains(&current) {
            String::new()
        } else {
            current.to_string()
        };
        Self { input: prefill }
    }

    /// 追加字符（自动截断到 20 字符）。
    fn push_char(&mut self, c: char) {
        if self.input.chars().count() < Settings::INPUT_METHOD_MAX_CHARS {
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
        Settings::clamp_input_method(&self.input)
    }
}

impl App {
    fn new(text: Text) -> Self {
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
        let session = {
            let wb = text.session_word_boundaries();
            Session::new_gated_with_words(&text.content, text.source.is_builtin(), &wb)
        };
        let settings = settings_store.load();
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
        Self {
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
            input_method_modal: None,
            free_input_modal: None,
            live_keyboard: LiveKeyboard::new(),
            db_worker,
        }
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

    /// 恢复跟打计时。
    fn resume(&mut self) {
        if self.paused {
            self.active_start = Some(Instant::now());
            self.paused = false;
        }
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

    /// 切换字体开关并即时持久化（OSC 序列由事件层在开启时输出）。
    fn toggle_font(&mut self) {
        self.settings.font = !self.settings.font;
        let _ = self.settings_store.save(&self.settings);
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
        self.session =
            Session::new_gated_with_words(&self.text.content, self.text.source.is_builtin(), &wb);
        self.start = Instant::now();
        self.accumulated_elapsed = Duration::ZERO;
        self.active_start = None;
        self.paused = false;
        self.live_keyboard.clear();
        self.state = AppState::Typing;
        self.browse_error = None;
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

        // 异步持久化有效练习流水到 SQLite 数据库
        if let Some(worker) = &self.db_worker {
            let accuracy = if stats.typed_chars == 0 {
                1.0
            } else {
                stats.correct_chars as f64 / stats.typed_chars as f64
            };
            let session_record = SessionRecord::new(
                elapsed.as_secs_f64(),
                stats.wpm,
                accuracy,
                stats.correct_chars as u32,
                stats.wrong_chars as u32,
                stats.edits,
                stats.typed_chars as u32,
                &self.text.title,
                &self.settings.input_method,
            );
            let session_id = session_record.id.clone();
            let word_index = self.text.build_word_index();
            let errors: Vec<ErrorRecordItem> = stats
                .error_points
                .iter()
                .enumerate()
                .map(|(idx, ep)| {
                    let (target_char, actual_char, error_type_str) = match &ep.error_type {
                        ErrorType::Mismatch { typed, expected } => {
                            (*expected, Some(*typed), "Mismatch")
                        }
                        ErrorType::Backspace { deleted } => (None, Some(*deleted), "Backspace"),
                    };
                    let target_word = target_char
                        .and_then(|ch| word_index.find_word_containing_char(ch))
                        .or_else(|| word_index.get_word_at(idx))
                        .map(|w| w.to_string());
                    ErrorRecordItem::new(
                        &session_id,
                        ep.time_secs,
                        idx as u32,
                        target_char,
                        actual_char,
                        target_word,
                        error_type_str,
                    )
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
        if is_online {
            self.state = AppState::Finished {
                stats: stats.clone(),
                upload: UploadState::Uploading,
                elapsed,
            };
            Some((stats, elapsed))
        } else {
            self.state = AppState::Finished {
                stats,
                upload: UploadState::NotApplicable,
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
                self.restart();
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
        self.builtin_preview = Some(match BUILTIN_SETS.get(self.builtin_selection) {
            Some(&set) if self.builtin_shuffle => {
                let text = load_builtin_text_shuffled(set);
                let body = if set.is_words() {
                    let boundaries = text.word_boundaries.as_ref().unwrap();
                    let chars: Vec<char> = text.content.chars().collect();
                    builtin_word_preview(boundaries, &chars)
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
                    builtin_word_preview(&boundaries, &chars),
                )
            }
            Some(&set) => (set.name().to_string(), builtin_char_preview(set.content())),
            None => ("预览".to_string(), "（无内置赛文）".to_string()),
        });
    }

    /// 载入当前选中的内置赛文，进入新跟打。
    fn load_selected_builtin(&mut self) {
        let Some(set) = BUILTIN_SETS.get(self.builtin_selection).copied() else {
            return;
        };
        self.text = if self.builtin_shuffle {
            load_builtin_text_shuffled(set)
        } else {
            load_builtin_text(set)
        };
        let wb = self.text.session_word_boundaries();
        self.session =
            Session::new_gated_with_words(&self.text.content, self.text.source.is_builtin(), &wb);
        self.start = Instant::now();
        self.state = AppState::Typing;
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
                self.text = Text {
                    title: comp.title,
                    content: comp.content,
                    source: TextSource::Online { competition_type },
                    word_boundaries: None,
                    shuffled: false,
                };
                self.session =
                    Session::new_gated(&self.text.content, self.text.source.is_builtin());
                self.start = Instant::now();
                self.state = AppState::Typing;
                self.online_loading = None;
                self.online_error = None;
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
            return UploadState::Failed {
                message: "未登录，无法上传成绩".to_string(),
                need_relogin: true,
                detail: None,
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
                write_clipboard(&outcome.share_text);
                UploadState::Success {
                    ranking: outcome.ranking,
                    share_text: outcome.share_text,
                }
            }
            Err(e) => {
                let need_relogin = is_auth_failure(&e);
                if need_relogin {
                    self.api.logout();
                    let _ = self.token_store.clear();
                }
                // 登录失效：主文案用友好提示，原始服务器错误降级为次要信息。
                let (message, detail) = if need_relogin {
                    (
                        "登录已失效，请重新登录".to_string(),
                        Some(api_error_text(&e)),
                    )
                } else {
                    (api_error_text(&e), None)
                };
                UploadState::Failed {
                    message,
                    need_relogin,
                    detail,
                }
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
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
    // 启动时若字体开关已开启，输出 OSC 字体尝试序列（尽力而为）。
    if app.settings.font {
        emit_font_osc();
    }
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

fn event_loop(terminal: &mut ratatui::DefaultTerminal, mut app: App) -> io::Result<()> {
    loop {
        terminal.draw(|frame| ui(frame, &app))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
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
                // 自定义输入法弹窗打开时优先处理其按键。
                if let Some(modal) = app.input_method_modal.as_mut() {
                    let action = input_method_modal_input(modal, key);
                    match action {
                        InputMethodModalAction::Cancel => {
                            app.input_method_modal = None;
                        }
                        InputMethodModalAction::Save(value) => {
                            app.input_method_modal = None;
                            app.settings.input_method = value;
                            let _ = app.settings_store.save(&app.settings);
                        }
                        InputMethodModalAction::None => {}
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
                if is_open_login(key) {
                    app.open_login();
                    continue;
                }
                match app.state {
                    AppState::Typing => {
                        if is_early_finish(key) {
                            finish_and_maybe_upload(&mut app, terminal)?;
                            continue;
                        }
                        if is_open_browser(key) {
                            app.open_browser();
                            continue;
                        }
                        if is_open_builtin_browser(key) {
                            app.open_builtin_browser();
                            continue;
                        }
                        if is_open_settings(key) {
                            app.state = AppState::Settings;
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

                        // Tab 键：打字中途暂停/恢复；就绪态收起/展开功能栏
                        if key.code == KeyCode::Tab {
                            if !app.session.is_empty() {
                                if app.paused {
                                    app.resume();
                                } else {
                                    app.pause();
                                }
                            } else {
                                app.sidebar_visible = !app.sidebar_visible;
                            }
                            continue;
                        }

                        // 就绪态或暂停态下，上下键在功能栏导航，Enter 激活菜单项
                        let is_menu_navigating = app.session.is_empty() || app.paused;
                        if is_menu_navigating {
                            match key.code {
                                KeyCode::Up => {
                                    app.sidebar_selected = if app.sidebar_selected == 0 {
                                        SIDEBAR_MENU_ITEMS.len() - 1
                                    } else {
                                        app.sidebar_selected - 1
                                    };
                                    continue;
                                }
                                KeyCode::Down => {
                                    app.sidebar_selected =
                                        (app.sidebar_selected + 1) % SIDEBAR_MENU_ITEMS.len();
                                    continue;
                                }
                                KeyCode::Enter => {
                                    activate_sidebar_menu_item(&mut app, terminal)?;
                                    continue;
                                }
                                KeyCode::Esc if app.paused => {
                                    app.resume();
                                    continue;
                                }
                                _ => {}
                            }
                        }

                        if matches!(key.code, KeyCode::Backspace | KeyCode::Char(_)) {
                            app.touch_typing();
                            let elapsed = app.current_elapsed();
                            handle_key(
                                &mut app.session,
                                &mut app.live_keyboard,
                                key,
                                elapsed,
                                Instant::now(),
                            );
                            if app.session.is_complete() {
                                finish_and_maybe_upload(&mut app, terminal)?;
                            }
                        }
                    }
                    AppState::Finished { .. } => {
                        if handle_finished_key(&mut app, key) {
                            continue;
                        }
                    }
                    AppState::Browsing => match key.code {
                        KeyCode::Up => {
                            app.browse_selection = app.browse_selection.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            if !app.browse_files.is_empty() {
                                app.browse_selection =
                                    (app.browse_selection + 1).min(app.browse_files.len() - 1);
                            }
                        }
                        KeyCode::Enter => app.load_selected(),
                        KeyCode::Esc => app.state = AppState::Typing,
                        _ => {}
                    },
                    AppState::BrowsingBuiltin => match key.code {
                        KeyCode::Up => {
                            app.builtin_selection = app.builtin_selection.saturating_sub(1);
                            app.refresh_builtin_preview();
                        }
                        KeyCode::Down => {
                            app.builtin_selection =
                                (app.builtin_selection + 1).min(BUILTIN_SETS.len() - 1);
                            app.refresh_builtin_preview();
                        }
                        KeyCode::Enter => app.load_selected_builtin(),
                        KeyCode::Char('s') | KeyCode::Char('S') => {
                            app.builtin_shuffle = !app.builtin_shuffle;
                            app.refresh_builtin_preview();
                        }
                        KeyCode::Esc => app.state = AppState::Typing,
                        _ => {}
                    },
                    AppState::Settings => match key.code {
                        KeyCode::Up => app.settings_focus = move_focus(app.settings_focus, -1),
                        KeyCode::Down => app.settings_focus = move_focus(app.settings_focus, 1),
                        KeyCode::Left | KeyCode::Right => {
                            let forward = key.code == KeyCode::Right;
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
                                FOCUS_FONT => {
                                    app.toggle_font();
                                    if app.settings.font {
                                        emit_font_osc();
                                    }
                                }
                                FOCUS_KEYBOARD => {
                                    if forward {
                                        app.next_keyboard_mode();
                                    } else {
                                        app.prev_keyboard_mode();
                                    }
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
                                _ => {}
                            }
                        }
                        KeyCode::Enter => {
                            if app.settings_focus == FOCUS_INPUT_METHOD
                                && input_method_preset_index(&app.settings.input_method)
                                    == INPUT_METHOD_PRESETS.len() - 1
                            {
                                app.input_method_modal =
                                    Some(InputMethodModal::new(&app.settings.input_method));
                            }
                        }
                        KeyCode::Esc => app.state = AppState::Typing,
                        _ => {}
                    },
                    AppState::Stats(ref mut stats_state) => {
                        if is_open_settings(key) {
                            app.state = AppState::Settings;
                            continue;
                        }
                        match key.code {
                            KeyCode::Char('1') => stats_state.tab = StatsTab::WpmTrend,
                            KeyCode::Char('2') => stats_state.tab = StatsTab::Heatmap,
                            KeyCode::Char('3') => stats_state.tab = StatsTab::ErrorRanking,
                            KeyCode::Tab | KeyCode::Right => {
                                stats_state.tab = match stats_state.tab {
                                    StatsTab::WpmTrend => StatsTab::Heatmap,
                                    StatsTab::Heatmap => StatsTab::ErrorRanking,
                                    StatsTab::ErrorRanking => StatsTab::WpmTrend,
                                };
                            }
                            KeyCode::BackTab | KeyCode::Left => {
                                stats_state.tab = match stats_state.tab {
                                    StatsTab::WpmTrend => StatsTab::ErrorRanking,
                                    StatsTab::Heatmap => StatsTab::WpmTrend,
                                    StatsTab::ErrorRanking => StatsTab::Heatmap,
                                };
                            }
                            KeyCode::Char('r') | KeyCode::Char('R') => {
                                stats_state.wpm_range = stats_state.wpm_range.next();
                            }
                            KeyCode::Char('l') | KeyCode::Char('L') => {
                                stats_state.heatmap_layout = stats_state.heatmap_layout.next();
                            }
                            KeyCode::Char('m') | KeyCode::Char('M') => {
                                stats_state.heatmap_source = stats_state.heatmap_source.next();
                            }
                            KeyCode::Char('t') | KeyCode::Char('T') => {
                                stats_state.error_ranking_focus =
                                    stats_state.error_ranking_focus.toggle();
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                match stats_state.error_ranking_focus {
                                    ErrorRankingFocus::Chars => {
                                        stats_state.char_scroll =
                                            stats_state.char_scroll.saturating_sub(1);
                                    }
                                    ErrorRankingFocus::Words => {
                                        stats_state.word_scroll =
                                            stats_state.word_scroll.saturating_sub(1);
                                    }
                                }
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                match stats_state.error_ranking_focus {
                                    ErrorRankingFocus::Chars => {
                                        stats_state.char_scroll =
                                            stats_state.char_scroll.saturating_add(1);
                                    }
                                    ErrorRankingFocus::Words => {
                                        stats_state.word_scroll =
                                            stats_state.word_scroll.saturating_add(1);
                                    }
                                }
                            }
                            KeyCode::PageUp => match stats_state.error_ranking_focus {
                                ErrorRankingFocus::Chars => {
                                    stats_state.char_scroll =
                                        stats_state.char_scroll.saturating_sub(10);
                                }
                                ErrorRankingFocus::Words => {
                                    stats_state.word_scroll =
                                        stats_state.word_scroll.saturating_sub(10);
                                }
                            },
                            KeyCode::PageDown => match stats_state.error_ranking_focus {
                                ErrorRankingFocus::Chars => {
                                    stats_state.char_scroll =
                                        stats_state.char_scroll.saturating_add(10);
                                }
                                ErrorRankingFocus::Words => {
                                    stats_state.word_scroll =
                                        stats_state.word_scroll.saturating_add(10);
                                }
                            },
                            KeyCode::Esc => app.state = AppState::Typing,
                            _ => {}
                        }
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
                if let Some(modal) = app.input_method_modal.as_mut() {
                    for c in committed.chars() {
                        modal.push_char(c);
                    }
                    continue;
                }
                if matches!(app.state, AppState::Typing) {
                    app.touch_typing();
                    let elapsed = app.current_elapsed();
                    app.session.type_text_at(&committed, elapsed);
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
        SidebarMenuItem::Stats => app.state = AppState::Stats(StatsViewState::default()),
        SidebarMenuItem::Settings => app.state = AppState::Settings,
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

/// 提前结束快捷键：Ctrl-S（Stop）。
fn is_early_finish(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s')
}

/// 重打快捷键：Ctrl-R（Restart）。
fn is_restart(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r')
}

/// 是否允许重打：离线赛文按 Ctrl-R 重打；在线赛文禁用重打。
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
        " ↑↓ 选择 | Enter 载入 | Esc 取消 | Ctrl-E 设置 | Ctrl-Q 退出"
    } else if browsing_builtin {
        " ↑↓ 选择 | Enter 载入 | s 乱序 | Esc 取消 | Ctrl-Q 退出"
    } else if paused {
        " ↑↓ 选择菜单 | Enter 激活 | Esc/Tab 恢复跟打 | Ctrl-Q 退出"
    } else if is_ready {
        " ↑↓ 菜单导航 | Enter 执行 | 打字 自动聚焦 | Ctrl-B 内置 | Ctrl-F 载文 | Ctrl-Q 退出"
    } else if is_online {
        " Ctrl-Q 退出 | Ctrl-S 结束 | Tab 暂停 | Ctrl-B 内置赛文 | Ctrl-F 载文 | Ctrl-O 登录 | Ctrl-E 设置 "
    } else {
        " Ctrl-Q 退出 | Ctrl-S 结束 | Ctrl-R 重打 | Tab 暂停 | Ctrl-B 内置赛文 | Ctrl-F 载文 | Ctrl-O 登录 | Ctrl-E 设置 "
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
        KeyCode::F(2) | KeyCode::F(10) => true,
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

/// 进入载文浏览快捷键：Ctrl-F（File）。
fn is_open_browser(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('f')
}

/// 进入内置赛文浏览快捷键：Ctrl-B（Builtin）。
fn is_open_builtin_browser(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('b')
}

/// 处理跟打键：退格回改，可打印字符上屏；同时记录按键频率与时序事件，并触发实时虚拟键盘高亮。
fn handle_key(
    session: &mut Session,
    live_kb: &mut LiveKeyboard,
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
            session.record_key(&c.to_string());
            session.type_text_at(&c.to_string(), elapsed);
            if c == ' ' {
                live_kb.press_key("Space", now);
            } else if c.is_ascii() {
                live_kb.press_char(c, now);
            }
        }
        _ => {}
    }
}

/// 处理成绩视图下的按键事件：
/// - Ctrl-F: 打开载文浏览
/// - Ctrl-B: 打开内置赛文浏览
/// - Ctrl-E: 打开设置视图
/// - Esc: 返回主界面（重置会话为就绪状态；在线赛文重置回内置赛文）
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
    if is_open_settings(key) {
        app.state = AppState::Settings;
        return true;
    }
    match key.code {
        KeyCode::Char('s') | KeyCode::Char('S') => {
            app.state = AppState::Stats(StatsViewState::default());
            true
        }
        KeyCode::Esc => {
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

/// 退出快捷键：Ctrl-Q / Ctrl-C（防止单按 q 误触退出）。
fn is_quit(key: KeyEvent) -> bool {
    let is_ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
    let is_ctrl_q = key.modifiers.contains(KeyModifiers::CONTROL)
        && (key.code == KeyCode::Char('q') || key.code == KeyCode::Char('Q'));
    is_ctrl_q || is_ctrl_c
}

/// 打开登录模态框快捷键：Ctrl-O。
fn is_open_login(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('o')
}

/// 打开设置视图快捷键：Ctrl-E（Edit settings）。
fn is_open_settings(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('e')
}

/// 三个比赛入口快捷键：F1=极速杯、F2=锦标赛、F3=键神杯。
fn online_shortcut(key: KeyEvent) -> Option<CompetitionType> {
    if !key.modifiers.is_empty() {
        return None;
    }
    match key.code {
        KeyCode::F(1) => Some(CompetitionType::Jisu),
        KeyCode::F(2) => Some(CompetitionType::Jinbiao),
        KeyCode::F(3) => Some(CompetitionType::Jianshen),
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
fn write_clipboard(text: &str) {
    use crossterm::style::Print;
    let seq = osc52_clipboard(text);
    let _ = crossterm::execute!(std::io::stdout(), Print(seq));
}

/// 输出 kitty 兼容的 OSC 字体设置序列（尽力而为，不支持/失败静默忽略）。
fn emit_font_osc() {
    use crossterm::style::Print;
    let seq = osc_font_size_sequence(FONT_SIZE_PT);
    let _ = crossterm::execute!(std::io::stdout(), Print(seq));
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

/// 处理自定义输入法模态框按键，返回动作。
fn input_method_modal_input(modal: &mut InputMethodModal, key: KeyEvent) -> InputMethodModalAction {
    match key.code {
        KeyCode::Esc => InputMethodModalAction::Cancel,
        KeyCode::Enter => InputMethodModalAction::Save(modal.commit()),
        KeyCode::Backspace => {
            modal.pop_char();
            InputMethodModalAction::None
        }
        KeyCode::Char(c) => {
            modal.push_char(c);
            InputMethodModalAction::None
        }
        _ => InputMethodModalAction::None,
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

        let (ref_area, kb_area_opt, type_area) =
            if kb_height > 0 && content.height >= kb_height + 6 {
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

        // 上：对照原文区（已跟打部分绿/红着色，非活动暗边框，复合双色标题）
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
        frame.render_widget(
            Paragraph::new(original_line(
                &app.session,
                &app.text,
                app.theme(),
                app.settings.bold,
            ))
            .block(themed_block(&palette, false).title(ref_title))
            .wrap(Wrap { trim: false }),
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
        typing_title_spans.push(Span::styled(
            format!(
                "— {}/{} 字符 ",
                app.session.len(),
                app.text.content.chars().count()
            ),
            Style::default().fg(palette.fg),
        ));
        let typing_title = Line::from(typing_title_spans);
        frame.render_widget(
            Paragraph::new(type_line(
                &app.session,
                &app.text,
                app.theme(),
                app.settings.bold,
            ))
            .block(themed_block(&palette, typing_active).title(typing_title))
            .wrap(Wrap { trim: false }),
            type_area,
        );
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
    frame.render_widget(
        Paragraph::new(hint_bar_line(hint, &palette))
            .block(themed_block(&palette, false).title(hint_title)),
        help_bar,
    );

    // 模态框（覆盖层）
    if let Some(form) = &app.login_form {
        render_login_modal(frame, form, &palette, app.theme());
    }
    if let Some(modal) = &app.input_method_modal {
        render_input_method_modal(frame, modal, &palette, app.theme());
    }
    if let Some(modal) = &app.free_input_modal {
        render_free_input_modal(frame, modal, &palette, app.theme());
    }
    if matches!(app.state, AppState::Settings) {
        render_settings(frame, app);
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
}

/// 自定义输入法名称弹窗：居中弹层，单行文本输入。
fn render_input_method_modal(
    frame: &mut Frame,
    modal: &InputMethodModal,
    palette: &ThemePalette,
    _theme: Theme,
) {
    let area = centered_rect(frame.area(), 50, 7);
    frame.render_widget(Clear, area);
    let remaining = Settings::INPUT_METHOD_MAX_CHARS - modal.input.chars().count();
    let lines = vec![
        Line::from(" 自定义输入法 ").bold().fg(palette.fg),
        Line::from(""),
        Line::from(format!(" ▸ {}", modal.input))
            .fg(palette.accent)
            .bold(),
        Line::from(""),
        Line::from(format!(" 还可输入 {remaining} 字")).fg(palette.fg),
        hint_bar_line(" Enter 保存 | Esc 取消 ", palette),
    ];
    let block = themed_block(palette, true)
        .title(" 自定义输入法 ")
        .style(Style::default().bg(palette.bg).fg(palette.fg));
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
            " [ 确认发文 (Ctrl-Enter/F2) ] ",
            Style::default().reversed().fg(palette.accent).bold(),
        )
    } else {
        Span::styled(
            " [ 确认发文 (Ctrl-Enter/F2) ] ",
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
            let mut line = Line::from(format!("{prefix}{}", set.name()));
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
            let prefix = if is_sel { " > " } else { "   " };
            let (label, is_accent, is_warn) = match item {
                SidebarMenuItem::LoadFile => ("载入文件（Ctrl-F）", false, false),
                SidebarMenuItem::BuiltinText => ("内置赛文（Ctrl-B）", false, false),
                SidebarMenuItem::FreeInput => ("自由发文", false, false),
                SidebarMenuItem::Clipboard => ("剪贴板发文", false, false),
                SidebarMenuItem::OnlineJisu => ("F1 极速杯", false, false),
                SidebarMenuItem::OnlineJinbiao => ("F2 锦标赛", false, false),
                SidebarMenuItem::OnlineJianshen => ("F3 键神杯", false, false),
                SidebarMenuItem::Stats => ("数据统计（s）", false, false),
                SidebarMenuItem::Settings => ("设置（Ctrl-E）", false, false),
                SidebarMenuItem::Login => {
                    if app.logged_in {
                        ("已登录 52dazi", true, false)
                    } else {
                        ("登录 52dazi（Ctrl-O）", false, true)
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

            let mut line = Line::from(format!("{prefix}{label}"));
            if is_sel {
                line = line.fg(palette.accent).bold();
            } else if is_accent {
                line = line.fg(palette.accent);
            } else if is_warn {
                line = line.fg(palette.warning);
            } else {
                line = line.fg(palette.fg);
            }
            lines.push(line);
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

/// 词组赛文预览：取前 `BUILTIN_ITEMS_PER_PAGE` 个词，词间加空格。
fn builtin_word_preview(boundaries: &[(usize, usize)], chars: &[char]) -> String {
    let preview_words = boundaries.len().min(BUILTIN_ITEMS_PER_PAGE);
    let mut preview = String::new();
    for (i, &(ws, we)) in boundaries.iter().take(preview_words).enumerate() {
        if i > 0 {
            preview.push(' ');
        }
        for ch in chars[ws..we].iter() {
            preview.push(*ch);
        }
    }
    if boundaries.len() > BUILTIN_ITEMS_PER_PAGE {
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
    // 预览按每 10 字一页（单字）或整行（词组），与实际跟打展示一致。
    let mut lines: Vec<Line> = vec![
        Line::from(format!(" 内置赛文 — {title} "))
            .bold()
            .fg(palette.fg),
        Line::from(""),
    ];
    let is_words = matches!(BUILTIN_SETS.get(app.builtin_selection), Some(set) if set.is_words());
    if is_words {
        lines.push(Line::from(body).fg(palette.fg));
    } else {
        for chunk in body
            .chars()
            .collect::<Vec<char>>()
            .chunks(BUILTIN_ITEMS_PER_PAGE)
        {
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
        &format!(" Enter 载入 | s {shuffle_label} | Esc 取消 "),
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
        StatsTab::WpmTrend => {
            render_wpm_trend_tab(frame, app, body_area, stats_state.wpm_range, &palette)
        }
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
            &palette,
        ),
    }

    // 3. 底部快捷键提示
    let hint_str = match stats_state.tab {
        StatsTab::WpmTrend => {
            " 1/2/3 切换选项卡 | r 切换时间范围 | Esc 返回跟打 | Ctrl-E 设置 | Ctrl-Q 退出 "
        }
        StatsTab::Heatmap => {
            " 1/2/3 切换选项卡 | l 切换键盘布局(斜列/直列) | m 切换数据视角(方案/物理) | Esc 返回跟打 | Ctrl-E 设置 | Ctrl-Q 退出 "
        }
        StatsTab::ErrorRanking => {
            " 1/2/3 切换选项卡 | t 切换字/词焦点 | ↑↓/PgUp/PgDn 滚动浏览 | Esc 返回跟打 | Ctrl-E 设置 | Ctrl-Q 退出 "
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

/// Tab 1: WPM 历史演进趋势图与历史概览卡片。
fn render_wpm_trend_tab(
    frame: &mut Frame,
    _app: &App,
    area: Rect,
    range: WpmChartRange,
    palette: &ThemePalette,
) {
    let db = StatsDb::with_default_path().ok();
    let summary = db
        .as_ref()
        .and_then(|d| d.get_global_summary().ok())
        .unwrap_or_default();
    let history_points = db
        .as_ref()
        .and_then(|d| d.get_rolling_wpm_history_with_limit(10, range.limit()).ok())
        .unwrap_or_default();

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
            Span::styled(" 累计输入字符: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{} 字", summary.total_typed_chars),
                Style::default().bold().fg(palette.success),
            ),
        ]),
        Line::from(vec![
            Span::styled(" 历史最高速度: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{:.1} WPM", summary.best_wpm),
                Style::default().bold().fg(palette.accent),
            ),
            Span::raw("    "),
            Span::styled(" 历史平均速度: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{:.1} WPM", summary.avg_wpm),
                Style::default().bold().fg(palette.fg),
            ),
            Span::raw("    "),
            Span::styled(" 近10场均速: ", Style::default().fg(palette.muted)),
            Span::styled(
                format!("{:.1} WPM", summary.recent_10_avg_wpm),
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
    let chart_title = Line::from(vec![
        Span::styled(
            " WPM 历史演进趋势 ",
            Style::default().bold().fg(palette.accent),
        ),
        Span::styled(
            format!("— 当前范围: [{range_badge} (按 r 切换)] "),
            Style::default().fg(palette.muted),
        ),
    ]);

    if history_points.is_empty() {
        let empty_msg = vec![
            Line::from(""),
            Line::from(Span::styled(
                " 暂无有效跟打历史记录。完成跟打练习并进入成绩视图后，系统将在此自动绘制速度演进折线图与平滑趋势线。",
                Style::default().fg(palette.muted),
            )),
        ];
        frame.render_widget(
            Paragraph::new(empty_msg).block(themed_block(palette, true).title(chart_title)),
            chart_area,
        );
        return;
    }

    let mut raw_points: Vec<(f64, f64)> = history_points
        .iter()
        .enumerate()
        .map(|(idx, (_time, wpm, _rolling))| (idx as f64 + 1.0, *wpm))
        .collect();

    let mut rolling_points: Vec<(f64, f64)> = history_points
        .iter()
        .enumerate()
        .map(|(idx, (_time, _wpm, rolling))| (idx as f64 + 1.0, *rolling))
        .collect();

    if raw_points.len() > 100 {
        raw_points = lttb_downsample(&raw_points, 100);
        rolling_points = lttb_downsample(&rolling_points, 100);
    }

    let max_x = (history_points.len() as f64).max(5.0);
    let max_y_raw = raw_points.iter().map(|p| p.1).fold(0.0, f64::max);
    let max_y_rolling = rolling_points.iter().map(|p| p.1).fold(0.0, f64::max);
    let max_y = (max_y_raw.max(max_y_rolling).max(30.0) * 1.15).ceil();

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

    let y_labels = vec![
        Span::styled("0", Style::default().fg(palette.muted).bg(palette.bg)),
        Span::styled(
            format!("{:.0}", max_y / 2.0),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
        Span::styled(
            format!("{max_y:.0}"),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
    ];

    let datasets = vec![
        Dataset::default()
            .name("单场 WPM")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(palette.accent).bg(palette.bg))
            .data(&raw_points),
        Dataset::default()
            .name("10场滚动平均")
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
                    "WPM",
                    Style::default().fg(palette.muted).bg(palette.bg),
                ))
                .style(Style::default().fg(palette.muted).bg(palette.bg))
                .bounds([0.0, max_y])
                .labels(y_labels),
        );

    frame.render_widget(chart, chart_area);
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
    let mut dict_path_display = String::new();

    let key_counts = match source {
        HeatmapSource::RawKeypress => db
            .as_ref()
            .and_then(|d| d.get_key_press_totals(Some(true)).ok())
            .unwrap_or_default(),
        HeatmapSource::SchemeProjected => {
            let custom_paths = &app.settings.scheme_dict_paths;
            let scheme_name = &app.settings.input_method;
            if let Some(path) = SchemeDict::resolve_scheme_path(scheme_name, custom_paths) {
                if let Ok(dict) = SchemeDict::load_from_file(&path) {
                    scheme_dict_loaded = true;
                    dict_path_display = path.display().to_string();
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
            format!("已加载码表: {dict_path_display}")
        } else if app.settings.input_method.is_empty() {
            "未配置输入法方案（按 Ctrl-E 设置）".to_string()
        } else {
            format!(
                "未找到方案 [{}] 码表文件 (可放至 ~/.config/dazitui/schemes/)，当前回退物理击键",
                app.settings.input_method
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
            Span::styled(" [ 0次 ] ", Style::default().fg(palette.muted).bg(palette.bg)),
            Span::styled(" [ 0-25% ] ", Style::default().fg(palette.muted)),
            Span::styled(" [ 25-50% ] ", Style::default().fg(palette.fg)),
            Span::styled(
                " [ 50-75% ] ",
                Style::default().fg(palette.success).bold(),
            ),
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
            // ANSI 60% 五行斜列布局
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
                    ("Tab", "Tab"),
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
                    ("Caps", "Caps"),
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
                    ("Enter", "Enter"),
                ],
                &[
                    ("Shift", "Shift"),
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
                    ("Shift", "Shift"),
                ],
                &[
                    ("Ctrl", "Ctrl"),
                    ("Alt", "Alt"),
                    ("Space", "Space (空格)"),
                    ("Alt", "Alt"),
                    ("Ctrl", "Ctrl"),
                ],
            ];

            let row_indents = ["  ", "   ", "    ", "      ", "        "];

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
            // Planck 4x12 直列网格布局
            let rows: [&[(&str, &str)]; 4] = [
                &[
                    ("Tab", "Tab"),
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
                    ("Esc", "Esc"),
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
                    ("Shift", "Shift"),
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
                    ("Enter", "Enter"),
                ],
                &[
                    ("Ctrl", "Ctrl"),
                    ("Alt", "Alt"),
                    ("Lower", "Lower"),
                    ("Space", "Space (空格)"),
                    ("Raise", "Raise"),
                    ("Left", "←"),
                    ("Down", "↓"),
                    ("Up", "↑"),
                    ("Right", "→"),
                ],
            ];

            for row in rows {
                let mut spans = vec![Span::raw("   ")];
                for (k_lookup, k_display) in row {
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
        Span::styled(
            " 键盘热力矩阵 ",
            Style::default().bold().fg(palette.accent),
        ),
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
fn render_error_ranking_tab(
    frame: &mut Frame,
    _app: &App,
    area: Rect,
    focus: ErrorRankingFocus,
    char_scroll: usize,
    word_scroll: usize,
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
        Line::from(vec![Span::styled(
            " 提示：支持使用 ↑ / ↓ 单行滚动，PgUp / PgDn 快速翻页；按 t 切换左右榜单焦点。",
            Style::default().fg(palette.muted),
        )]),
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
            " ▶ 高频错字排行榜 (Top 50) "
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
        Span::styled(" 排名 ", Style::default().bold().fg(palette.muted)),
        Span::styled(" 错字 ", Style::default().bold().fg(palette.accent)),
        Span::styled("   高频误打 ", Style::default().bold().fg(palette.warning)),
        Span::styled("   累计错次 ", Style::default().bold().fg(palette.error)),
    ]));
    char_lines.push(Line::from(Span::styled(
        " ───────────────────────────────────────────────────",
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
            let rank = actual_scroll + idx + 1;
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

            char_lines.push(Line::from(vec![
                rank_badge,
                Span::styled(
                    format!("   '{}' ", stat.target_char),
                    Style::default().bold().fg(palette.fg),
                ),
                Span::styled(
                    format!("      {:^4} ", actual_display),
                    Style::default().fg(palette.warning),
                ),
                Span::styled(
                    format!("      {:>4} 次", stat.error_count),
                    Style::default().bold().fg(palette.error),
                ),
            ]));
        }
    }

    frame.render_widget(
        Paragraph::new(char_lines).block(themed_block(palette, is_char_focused).title(char_title)),
        left_col,
    );

    // 右列：高频错词榜
    let word_title = Line::from(vec![Span::styled(
        if is_word_focused {
            " ▶ 高频错词排行榜 (Top 50) "
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
        Span::styled(" 排名 ", Style::default().bold().fg(palette.muted)),
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
        " ───────────────────────────────────────────────────",
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
            let rank = actual_scroll + idx + 1;
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

            word_lines.push(Line::from(vec![
                rank_badge,
                Span::styled(
                    format!("   {:<8}", stat.target_word),
                    Style::default().bold().fg(palette.fg),
                ),
                Span::styled(
                    format!("          {:>4} 次", stat.error_count),
                    Style::default().bold().fg(palette.error),
                ),
                Span::styled(
                    format!("          {:>4} 场", stat.affected_sessions),
                    Style::default().fg(palette.warning),
                ),
            ]));
        }
    }

    frame.render_widget(
        Paragraph::new(word_lines).block(themed_block(palette, is_word_focused).title(word_title)),
        right_col,
    );
}

/// 设置视图：焦点行 + 左右调整（主题/占比/粗体/字体）。
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
        "字体",
        on_off(app.settings.font),
        focus == FOCUS_FONT,
        &palette,
    ));
    lines.push(settings_row(
        "实时键盘",
        app.settings.keyboard_mode.name(),
        focus == FOCUS_KEYBOARD,
        &palette,
    ));
    lines.push(settings_row(
        "输入法",
        input_method_display(&app.settings.input_method),
        focus == FOCUS_INPUT_METHOD,
        &palette,
    ));

    lines.push(Line::from(""));
    // 主题预览：用当前主题的对/错色渲染示意文字。
    lines.push(Line::from(" 预览:").bold().fg(palette.fg));
    lines.push(Line::from("  对正确对正确").fg(palette.success));
    lines.push(Line::from("  错错误错错误").fg(palette.error));
    lines.push(Line::from(""));
    lines.push(hint_bar_line(" ↑↓ 选择 | ←→ 调整 | Esc 返回 ", &palette));

    let area = centered_rect(frame.area(), 60, 17);
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
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match mode {
        KeyboardMode::Staggered => {
            // ANSI 60% 五行斜列紧凑布局
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
                    ("Tab", "Tab"),
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
                    ("Caps", "Caps"),
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
                    ("Enter", "Enter"),
                ],
                &[
                    ("Shift", "Shift"),
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
                    ("Shift", "Shift"),
                ],
                &[
                    ("Ctrl", "Ctrl"),
                    ("Alt", "Alt"),
                    ("Space", "Space (空格)"),
                    ("Alt", "Alt"),
                    ("Ctrl", "Ctrl"),
                ],
            ];

            let row_indents = ["  ", "   ", "    ", "     ", "        "];

            for (r_idx, row) in rows.iter().enumerate() {
                let mut spans = vec![Span::raw(row_indents[r_idx])];
                for (k_idx, (k_lookup, k_display)) in row.iter().enumerate() {
                    let style = live_kb.get_key_style(k_lookup, palette, now);
                    let badge = if *k_lookup == "Space" {
                        format!("[ {:^14} ]", k_display)
                    } else {
                        format!("[{k_display}]")
                    };
                    if k_idx > 0 {
                        spans.push(Span::raw(" "));
                    }
                    spans.push(Span::styled(badge, style));
                }
                lines.push(Line::from(spans));
            }
        }
        KeyboardMode::Ortholinear => {
            // Planck 4x12 直列网格紧凑布局
            let rows: [&[(&str, &str)]; 4] = [
                &[
                    ("Tab", "Tab"),
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
                    ("Esc", "Esc"),
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
                    ("Shift", "Shift"),
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
                    ("Enter", "Enter"),
                ],
                &[
                    ("Ctrl", "Ctrl"),
                    ("Alt", "Alt"),
                    ("Lower", "Lower"),
                    ("Space", "Space (空格)"),
                    ("Raise", "Raise"),
                    ("Left", "←"),
                    ("Down", "↓"),
                    ("Up", "↑"),
                    ("Right", "→"),
                ],
            ];

            let row_indents = ["    ", "    ", "    ", "    "];

            for (r_idx, row) in rows.iter().enumerate() {
                let mut spans = vec![Span::raw(row_indents[r_idx])];
                for (k_idx, (k_lookup, k_display)) in row.iter().enumerate() {
                    let style = live_kb.get_key_style(k_lookup, palette, now);
                    let badge = if *k_lookup == "Space" {
                        format!("[ {:^12} ]", k_display)
                    } else {
                        format!("[{k_display}]")
                    };
                    if k_idx > 0 {
                        spans.push(Span::raw(" "));
                    }
                    spans.push(Span::styled(badge, style));
                }
                lines.push(Line::from(spans));
            }
        }
        KeyboardMode::Off => {}
    }
    lines
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
    let lines = generate_live_keyboard_lines(live_kb, mode, palette, now);
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
    let mut summary_lines = vec![Line::from(vec![
        Span::raw(" WPM: "),
        Span::styled(
            format!("{:.1}", stats.wpm),
            Style::default().bold().fg(palette.accent),
        ),
        Span::raw("   正确字数: "),
        Span::styled(
            format!(
                "{}/{}",
                stats.correct_chars,
                app.text.content.chars().count()
            ),
            Style::default().bold().fg(palette.success),
        ),
        Span::raw("   错字: "),
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
        Span::raw("   用时: "),
        Span::styled(format_time(elapsed), Style::default().bold().fg(palette.fg)),
    ])];
    if !stats.edit_details.is_empty() {
        let details: String = stats.edit_details.iter().collect();
        summary_lines.push(Line::from(format!(" 回改明细: {details}")));
    }
    // 上传状态（在线赛文；离线赛文不显示）
    summary_lines.extend(upload_lines(upload, theme));

    // 计算顶部高度
    let summary_height = if matches!(upload, UploadState::NotApplicable) {
        if stats.edit_details.is_empty() { 3 } else { 4 }
    } else {
        if stats.edit_details.is_empty() { 5 } else { 6 }
    };

    // 2. 错字时间线行生成
    let mut timeline_lines = Vec::new();
    if stats.error_points.is_empty() {
        timeline_lines.push(Line::from(" 全对无错字").fg(palette.success));
    } else {
        let max_show = 4;
        for ep in stats.error_points.iter().take(max_show) {
            match &ep.error_type {
                ErrorType::Mismatch { typed, expected } => {
                    timeline_lines.push(
                        Line::from(format!(
                            "   [{:04.1}s] 错字: '{}' (期望'{}') · WPM {:.1}",
                            ep.time_secs,
                            typed,
                            expected
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| "?".into()),
                            ep.wpm
                        ))
                        .fg(palette.error),
                    );
                }
                ErrorType::Backspace { deleted } => {
                    timeline_lines.push(
                        Line::from(format!(
                            "   [{:04.1}s] 回改: '{}' · WPM {:.1}",
                            ep.time_secs, deleted, ep.wpm
                        ))
                        .fg(palette.warning),
                    );
                }
            }
        }
        if stats.error_points.len() > max_show {
            timeline_lines.push(
                Line::from(format!(
                    "   ... 共有 {} 处错字记录",
                    stats.error_points.len()
                ))
                .fg(palette.muted),
            );
        }
    }
    let timeline_height = (timeline_lines.len() as u16 + 2).min(8);

    // 3. 底部操作提示
    let hint_str = if app.text.is_online() {
        if let UploadState::Failed {
            need_relogin: true, ..
        } = upload
        {
            " Esc 返回 | s 数据统计 | Ctrl-O 登录并上传 | Ctrl-F 载文 | Ctrl-B 内置赛文 | Ctrl-Q 退出"
        } else {
            " Esc 返回 | s 数据统计 | Ctrl-F 载文 | Ctrl-B 内置赛文 | Ctrl-Q 退出"
        }
    } else {
        " Esc 返回 | Enter/r 重打 | s 数据统计 | Ctrl-F 载文 | Ctrl-B 内置赛文 | Ctrl-Q 退出"
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
    if total_area.height < 14 {
        let [top_area, bottom_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(total_area);
        let mut all_lines = summary_lines;
        all_lines.push(Line::from(""));
        all_lines.extend(timeline_lines);
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

    let timeline_title = Line::from(vec![Span::styled(
        " 错字时间线 ",
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )]);
    frame.render_widget(
        Paragraph::new(timeline_lines)
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
fn upload_lines(upload: &UploadState, theme: Theme) -> Vec<Line<'static>> {
    match upload {
        UploadState::NotApplicable => vec![],
        UploadState::Uploading => vec![
            Line::from(""),
            Line::from(" 成绩上传中…").fg(color(theme.warn)),
        ],
        UploadState::Success {
            ranking,
            share_text,
        } => {
            let mut lines = vec![Line::from("")];
            match ranking {
                Some(r) => {
                    lines.push(
                        Line::from(format!(" 排名: 第{r}名 · 已上传")).fg(color(theme.accent)),
                    );
                }
                None => lines.push(Line::from(" 已上传").fg(color(theme.accent))),
            }
            lines.push(Line::from(format!(" 分享: {share_text}")).fg(color(theme.accent)));
            lines.push(Line::from(" 已复制到剪贴板").fg(color(theme.muted)));
            lines
        }
        UploadState::Failed {
            message,
            need_relogin,
            detail,
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
            lines
        }
    }
}

/// 内置赛文每页显示的单位数（单字赛文每页 10 字，词组赛文每页 10 个词）。
const BUILTIN_ITEMS_PER_PAGE: usize = dazitui_core::GROUP_SIZE;

/// 单字赛文当前页的起始字符索引：基于已全对完成的组数。
fn builtin_page_start(session: &Session) -> usize {
    session.completed_groups() * BUILTIN_ITEMS_PER_PAGE
}

/// 对照区：将当前页 10 个词的原文按跟打状态着色，词间插入空格 span（不可打）。
fn build_word_spans(
    session: &Session,
    word_boundaries: &[(usize, usize)],
    page_start_word: usize,
    page_end_word: usize,
    theme: Theme,
    bold: bool,
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
        for &(c, status) in &statuses[ws..we] {
            let style = match status {
                Some(CharStatus::Correct) => Style::default().fg(color(theme.correct)),
                Some(CharStatus::Wrong) => Style::default().fg(color(theme.wrong)),
                None => Style::default(),
            };
            spans.push(Span::styled(
                c.to_string(),
                style.add_modifier(bold_modifier(bold)),
            ));
        }
    }
    spans
}

/// 跟打区：将当前页 10 个词的已打字符按对/错着色，词间插入空格 span。
fn build_word_type_spans(
    display: &[(char, CharStatus)],
    word_boundaries: &[(usize, usize)],
    page_start_word: usize,
    page_end_word: usize,
    theme: Theme,
    bold: bool,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (word_i, &(ws, we)) in word_boundaries
        .iter()
        .enumerate()
        .skip(page_start_word)
        .take(page_end_word - page_start_word)
    {
        if word_i > page_start_word {
            spans.push(Span::raw(" "));
        }
        for ci in ws..we {
            if ci < display.len() {
                let (c, status) = display[ci];
                let style = match status {
                    CharStatus::Correct => Style::default().fg(color(theme.correct)),
                    CharStatus::Wrong => Style::default().fg(color(theme.wrong)),
                };
                spans.push(Span::styled(
                    c.to_string(),
                    style.add_modifier(bold_modifier(bold)),
                ));
            }
        }
    }
    spans
}

/// 将对照区的字符按跟打状态着色：已打对=correct、已打错=wrong、未打到=默认。
///
/// 内置赛文只显示当前页：单字赛文每页 10 字、词组赛文每页 10 个词（词间加空格、去逗号）；
/// 打完当前页自动翻页；其余来源显示全文（由终端宽度自动折行）。
fn original_line(session: &Session, text: &Text, theme: Theme, bold: bool) -> TextLines<'static> {
    let source = text.source;
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
            let page_end_word = (page_start_word + BUILTIN_ITEMS_PER_PAGE).min(boundaries.len());
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
            );
            let mut text_lines = TextLines::default();
            text_lines.push_line(Line::from(spans));
            return text_lines;
        }
        // 单字赛文：每页 10 字
        let start = builtin_page_start(session);
        let statuses: Vec<_> = session
            .original_status()
            .into_iter()
            .skip(start)
            .take(BUILTIN_ITEMS_PER_PAGE)
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
        return group_spans(spans, source);
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
    group_spans(spans, source)
}

/// 将跟打区的字符按对/错渲染为 correct/wrong。
///
/// 内置赛文只显示当前页：单字赛文每页 10 字、词组赛文每页 10 个词（词间加空格、去逗号）；
/// 打完当前页自动翻页；其余来源显示全文（由终端宽度自动折行）。
fn type_line(session: &Session, text: &Text, theme: Theme, bold: bool) -> TextLines<'static> {
    let source = text.source;
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
            let page_end_word = (page_start_word + BUILTIN_ITEMS_PER_PAGE).min(boundaries.len());
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
        // 单字赛文：每页 10 字
        let start = builtin_page_start(session);
        let display: Vec<_> = session
            .display()
            .into_iter()
            .skip(start)
            .take(BUILTIN_ITEMS_PER_PAGE)
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
        return group_spans(spans, source);
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
    group_spans(spans, source)
}

/// 把已着色的 span 序列按赛文来源组织成多行文本：单字内置赛文每页 10 字一行，其余为单行。
/// 词组赛文已在调用方按页组装，不走此函数。
fn group_spans(spans: Vec<Span<'static>>, source: TextSource) -> TextLines<'static> {
    let mut text = TextLines::default();
    if matches!(source, TextSource::Builtin { set } if !set.is_words()) {
        for chunk in spans.chunks(BUILTIN_ITEMS_PER_PAGE) {
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
    fn ctrl_s_early_finishes() {
        assert!(is_early_finish(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_early_finish(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::NONE
        )));
        assert!(!is_early_finish(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn ctrl_r_restarts() {
        assert!(is_restart(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_restart(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn restart_allowed_only_when_offline() {
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        // 离线赛文：Ctrl-R 允许重打。
        assert!(restart_allowed(ctrl_r, false));
        // 在线赛文：Ctrl-R 被禁用。
        assert!(!restart_allowed(ctrl_r, true));
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
    fn ctrl_b_opens_builtin_browser() {
        assert!(is_open_builtin_browser(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_open_builtin_browser(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::NONE
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
        assert_eq!(BUILTIN_SETS.len(), 6);
        assert_eq!(BUILTIN_SETS[0].name(), "常用单字前五百");
        assert_eq!(BUILTIN_SETS[1].name(), "常用单字中五百");
        assert_eq!(BUILTIN_SETS[2].name(), "常用单字后五百");
        assert_eq!(BUILTIN_SETS[3].name(), "常用词组前五百");
        assert_eq!(BUILTIN_SETS[4].name(), "常用词组中五百");
        assert_eq!(BUILTIN_SETS[5].name(), "常用词组后五百");
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
        assert!(matches!(app.state, AppState::Typing));
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
        assert!(matches!(app.state, AppState::Typing));
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
        let rendered = original_line(&session, &text, theme, false);
        assert_eq!(rendered.lines.len(), 1, "乱序词组对照区应只有一行");
        let first_page_words = boundaries.len().min(BUILTIN_ITEMS_PER_PAGE);
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
        let rendered = type_line(&session, &text, theme, false);
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
    fn ctrl_f_opens_browser() {
        assert!(is_open_browser(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_open_browser(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::NONE
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
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            Duration::ZERO,
            now,
        );
        handle_key(
            &mut session,
            &mut live_kb,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            Duration::ZERO,
            now,
        );
        handle_key(
            &mut session,
            &mut live_kb,
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
        handle_key(&mut session, &mut live_kb, key, Duration::ZERO, now);
        assert_eq!(session.len(), 1);
        assert_eq!(session.edit_count(), 1);
        key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        handle_key(&mut session, &mut live_kb, key, Duration::ZERO, now);
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
        let text = type_line(&session, &file_text, theme, true);
        let line = &text.lines[0];
        assert_eq!(line.spans[0].style.add_modifier, Modifier::BOLD);
        assert_eq!(line.spans[1].style.add_modifier, Modifier::BOLD);
        let plain = type_line(&session, &file_text, theme, false);
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
        let text = original_line(&session, &file_text, theme, true);
        let line = &text.lines[0];
        assert_eq!(line.spans[0].style.add_modifier, Modifier::BOLD);
        assert_eq!(line.spans[2].style.add_modifier, Modifier::BOLD);
        let plain = original_line(&session, &file_text, theme, false);
        let plain_line = &plain.lines[0];
        assert_eq!(plain_line.spans[0].style.add_modifier, Modifier::empty());
    }

    #[test]
    fn move_focus_wraps_around() {
        // SETTINGS_FOCUS_COUNT = 6（主题/占比/粗体/字体/实时键盘/输入法）
        assert_eq!(move_focus(0, -1), 5); // 第 0 项向前 → 末项（5）
        assert_eq!(move_focus(5, 1), 0); // 末项向后 → 第 0 项
        assert_eq!(move_focus(0, 1), 1);
        assert_eq!(move_focus(4, 1), 5);
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
        let mut app = App::new(load_builtin_text(BUILTIN_SETS[0]));
        app.settings_store = store.clone();

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
    fn input_method_modal_new_prefills_custom_and_clears_preset() {
        assert_eq!(InputMethodModal::new("").input, "");
        assert_eq!(InputMethodModal::new("虎码").input, "");
        assert_eq!(InputMethodModal::new("自定义").input, "");
        assert_eq!(InputMethodModal::new("我的自定义码").input, "我的自定义码");
    }

    #[test]
    fn input_method_modal_push_char_clamps_to_20_chars() {
        let mut modal = InputMethodModal::default();
        for _ in 0..25 {
            modal.push_char('字');
        }
        assert_eq!(modal.input.chars().count(), 20);
        assert_eq!(modal.input, "字".repeat(20));
    }

    #[test]
    fn input_method_modal_pop_char_removes_last_unicode_char() {
        let mut modal = InputMethodModal::new("虎码输入");
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
    fn input_method_modal_commit_trims_and_returns_empty_for_blanks() {
        let mut modal = InputMethodModal::default();
        assert_eq!(modal.commit(), "");
        modal.input = "   ".into();
        assert_eq!(modal.commit(), "");
        modal.input = "  小鹤双拼  ".into();
        assert_eq!(modal.commit(), "小鹤双拼");
    }

    #[test]
    fn input_method_modal_input_actions() {
        let mut modal = InputMethodModal::default();
        // 输入字符
        assert_eq!(
            input_method_modal_input(
                &mut modal,
                KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)
            ),
            InputMethodModalAction::None
        );
        assert_eq!(
            input_method_modal_input(
                &mut modal,
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE)
            ),
            InputMethodModalAction::None
        );
        assert_eq!(modal.input, "ab");

        // 退格
        assert_eq!(
            input_method_modal_input(
                &mut modal,
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)
            ),
            InputMethodModalAction::None
        );
        assert_eq!(modal.input, "a");

        // 回车保存
        assert_eq!(
            input_method_modal_input(
                &mut modal,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            ),
            InputMethodModalAction::Save("a".into())
        );

        // Esc 取消
        assert_eq!(
            input_method_modal_input(&mut modal, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            InputMethodModalAction::Cancel
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
    fn toggle_bold_and_font_persist() {
        let mut app = test_app(file_text("你好"));
        assert!(!app.settings.bold);
        assert!(!app.settings.font);
        app.toggle_bold();
        app.toggle_font();
        assert!(app.settings.bold);
        assert!(app.settings.font);
        let loaded = app.settings_store.load();
        assert!(loaded.bold);
        assert!(loaded.font);
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
        assert!(matches!(app.state, AppState::Typing));
        assert_eq!(app.session.len(), 0);

        let _ = fs::remove_dir_all(&dir);
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
    fn ctrl_o_opens_login() {
        assert!(is_open_login(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_open_login(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn ctrl_e_opens_settings() {
        assert!(is_open_settings(KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_open_settings(KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::NONE
        )));
        assert!(!is_open_settings(KeyEvent::new(
            KeyCode::Char('s'),
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
        assert_eq!(app.settings.theme, ThemePreset::TokyoNight);
        assert_eq!(app.settings_store.load().theme, ThemePreset::TokyoNight);
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
        let catppuccin_text = original_line(&session, &file_text, catppuccin_theme, false);
        let dracula_text = original_line(&session, &file_text, dracula_theme, false);
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
        let rendered = type_line(&session, &text, theme, false);
        assert_eq!(rendered.lines.len(), 1, "第一页应只有一行");
        assert_eq!(rendered.lines[0].spans.len(), 5, "打 5 字应显示 5 字");
        // 打到 10 字（全对）：第一组全对，completed_groups 推进，翻到第二组，跟打区显示提示行。
        session.type_text("六七八九十");
        let rendered = type_line(&session, &text, theme, false);
        assert_eq!(rendered.lines.len(), 1, "翻到第二组尚未打字时应显示提示行");
        // 打到 13 字（全对）：第二组已打 3 字，跟打区显示 3 字。
        session.type_text("甲乙丙");
        let rendered = type_line(&session, &text, theme, false);
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
        let rendered = type_line(&session, &text, theme, false);
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
        let rendered = type_line(&session, &text, theme, false);
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
        let rendered = original_line(&session, &text, theme, false);
        assert_eq!(rendered.lines.len(), 1, "第一组应只有一行");
        assert_eq!(
            rendered.lines[0].spans.len(),
            10,
            "对照区第一组应显示 10 字"
        );
        // 打到 10 字（全对）：第一组全对，翻到第二组，对照区显示第 11-20 字（10 字）。
        session.type_text("六七八九十");
        let rendered = original_line(&session, &text, theme, false);
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
        let rendered = type_line(&session, &text, theme, false);
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
        let rendered = original_line(&session, &text, theme, false);
        assert_eq!(rendered.lines.len(), 1, "词组赛文应只有一行");
        // 第 1 页 10 个词，词间 9 个空格 span
        let first_page_words = boundaries.len().min(BUILTIN_ITEMS_PER_PAGE);
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
            .take(BUILTIN_ITEMS_PER_PAGE)
            .map(|(s, e)| e - s)
            .sum();
        let first_page_chars: String = no_commas.chars().take(first_page_char_count).collect();
        session.type_text(&first_page_chars);
        // 全对 → completed_groups 推进 → 翻到第 2 组
        assert_eq!(session.completed_groups(), 1, "第 1 组全对应推进到 1");
        let rendered = type_line(&session, &text, theme, false);
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
        let rendered = type_line(&session, &text, theme, false);
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
            .take(BUILTIN_ITEMS_PER_PAGE)
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
        let rendered = original_line(&session, &word_text, theme, false);
        assert_eq!(rendered.lines.len(), 1, "第 2 组应只有一行");
        let second_page_words = boundaries.len().min(BUILTIN_ITEMS_PER_PAGE);
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
    fn online_shortcut_maps_f_keys_to_competitions() {
        assert_eq!(
            online_shortcut(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)),
            Some(CompetitionType::Jisu)
        );
        assert_eq!(
            online_shortcut(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)),
            Some(CompetitionType::Jinbiao)
        );
        assert_eq!(
            online_shortcut(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE)),
            Some(CompetitionType::Jianshen)
        );
        // 带修饰键（Ctrl-F1 等）不触发，普通字符也不触发。
        assert_eq!(
            online_shortcut(KeyEvent::new(KeyCode::F(1), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            online_shortcut(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            None
        );
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
        // 离线：不显示上传状态。
        assert!(upload_lines(&UploadState::NotApplicable, theme).is_empty());
        // 上传中。
        let lines = upload_lines(&UploadState::Uploading, theme);
        assert!(lines.iter().any(|l| l.to_string().contains("上传中")));
        // 成功带排名：排名 + 已上传 + 分享 + 剪贴板。
        let lines = upload_lines(
            &UploadState::Success {
                ranking: Some("5".into()),
                share_text: "极速杯 第5名 · WPM 85.2".into(),
            },
            theme,
        );
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert!(
            text.iter()
                .any(|s| s.contains("第5名") && s.contains("已上传"))
        );
        assert!(text.iter().any(|s| s.contains("极速杯 第5名")));
        assert!(text.iter().any(|s| s.contains("已复制到剪贴板")));
        // 成功无排名：仍显示已上传。
        let lines = upload_lines(
            &UploadState::Success {
                ranking: None,
                share_text: "x".into(),
            },
            theme,
        );
        assert!(lines.iter().any(|l| l.to_string().contains("已上传")));
        // 失败：显示原因，不提示重新登录。
        let lines = upload_lines(
            &UploadState::Failed {
                message: "网络连接失败".into(),
                need_relogin: false,
                detail: None,
            },
            theme,
        );
        assert!(
            lines
                .iter()
                .any(|l| l.to_string().contains("上传失败: 网络连接失败"))
        );
        assert!(lines.iter().all(|l| !l.to_string().contains("重新登录")));
        // 失败且鉴权失效：提示重新登录；原始错误降级为次要信息。
        let lines = upload_lines(
            &UploadState::Failed {
                message: "登录已失效，请重新登录".into(),
                need_relogin: true,
                detail: Some("用户名不能为空！".into()),
            },
            theme,
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
        assert_eq!(
            up,
            UploadState::Failed {
                message: "未登录，无法上传成绩".to_string(),
                need_relogin: true,
                detail: None,
            }
        );
    }

    #[test]
    fn perform_upload_network_failure_is_not_relogin() {
        let mut app = test_app(online_text("你好世界"));
        app.token = Some("dead-token".into());
        app.logged_in = true;
        // 指向必然拒绝连接的地址，验证网络错误被友好化。
        app.api = ApiClient::with_base_url("http://127.0.0.1:1");
        let stats = app.session.finish(Duration::from_secs(10));
        let up = app.perform_upload(&stats, Duration::from_secs(10));
        assert_eq!(
            up,
            UploadState::Failed {
                message: "网络连接失败".to_string(),
                need_relogin: false,
                detail: None,
            }
        );
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
            UploadState::Success { ranking, share_text }
                if ranking.as_deref() == Some("5") && share_text.contains("第5名")
        ));
    }

    #[test]
    fn finish_typing_offline_no_upload_online_uploading() {
        // 离线：直接进入成绩视图，无上传。
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
                upload: UploadState::NotApplicable,
                ..
            }
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
        assert!(matches!(app.state, AppState::Typing));
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
        assert_eq!(
            up,
            UploadState::Failed {
                message: "登录已失效，请重新登录".to_string(),
                need_relogin: true,
                detail: Some("用户名不能为空！".to_string()),
            }
        );
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
    fn finished_key_ctrl_f_b_e_navigate() {
        let mut app = test_app(file_text("文本"));
        app.finish_typing();

        // Ctrl-F 打开载文浏览
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert!(handle_finished_key(&mut app, ctrl_f));
        assert!(matches!(app.state, AppState::Browsing));

        // 回到 Finished 测试 Ctrl-B
        app.finish_typing();
        let ctrl_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        assert!(handle_finished_key(&mut app, ctrl_b));
        assert!(matches!(app.state, AppState::BrowsingBuiltin));

        // 回到 Finished 测试 Ctrl-E
        app.finish_typing();
        let ctrl_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert!(handle_finished_key(&mut app, ctrl_e));
        assert!(matches!(app.state, AppState::Settings));
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

        // 2. F2 快捷键
        let f2 = KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE);
        assert_eq!(
            free_input_modal_input(&mut modal, f2),
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

        // 恢复
        app.resume();
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

        // 激活 数据统计 (index 7)
        app.sidebar_selected = 7;
        activate_sidebar_menu_item(&mut app, &mut terminal).unwrap();
        assert!(matches!(app.state, AppState::Stats(_)));
        app.state = AppState::Typing;

        // 激活 设置 (index 8)
        app.sidebar_selected = 8;
        activate_sidebar_menu_item(&mut app, &mut terminal).unwrap();
        assert!(matches!(app.state, AppState::Settings));
        app.state = AppState::Typing;
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
        app.state = AppState::Settings;
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
        let palette = theme_palette(ThemePreset::TokyoNight);
        let focused = settings_row("主题", "Tokyo Night", true, &palette);
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
        app.state = AppState::Settings;
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
        assert!(clean.contains("字体:"));
        assert!(clean.contains("输入法:"));
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
    fn main_ui_renders_high_contrast_theme_background_and_sidebar_unselected_items_visible() {
        for preset in [
            ThemePreset::CatppuccinMocha,
            ThemePreset::TokyoNight,
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
                    if cell.symbol() == "↑" {
                        assert_eq!(
                            cell.fg, palette.accent,
                            "Preset {:?} '↑' fg mismatch",
                            preset
                        );
                        assert_eq!(
                            cell.bg, palette.selection,
                            "Preset {:?} '↑' bg mismatch",
                            preset
                        );
                        found_key = true;
                    }
                }
            }
            assert!(found_rounded_border, "底部快捷键栏应当有圆角边框 (╭/╰)");
            assert!(found_title, "底部快捷键栏应当包含标题 '快捷键'");
            assert!(found_nav, "应当在底部提示栏找到 '菜'");
            assert!(found_key, "应当在底部提示栏找到按键胶囊 '↑'");
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
        handle_key(&mut app.session, &mut app.live_keyboard, KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE), Duration::from_secs(1), now);
        handle_key(&mut app.session, &mut app.live_keyboard, KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE), Duration::from_secs(2), now);
        handle_key(&mut app.session, &mut app.live_keyboard, KeyEvent::new(KeyCode::Char('四'), KeyModifiers::NONE), Duration::from_secs(3), now);
        handle_key(&mut app.session, &mut app.live_keyboard, KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), Duration::from_secs(4), now);
        handle_key(&mut app.session, &mut app.live_keyboard, KeyEvent::new(KeyCode::Char('世'), KeyModifiers::NONE), Duration::from_secs(5), now);
        handle_key(&mut app.session, &mut app.live_keyboard, KeyEvent::new(KeyCode::Char('界'), KeyModifiers::NONE), Duration::from_secs(6), now);

        assert!(app.session.is_complete());

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
            upload: UploadState::NotApplicable,
            elapsed: Duration::from_secs(10),
        };

        let handled = handle_finished_key(&mut app, KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
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
                    .map(|x| terminal_ortho.backend().buffer()[(x, y)].symbol().to_string())
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

        assert_eq!(HeatmapSource::SchemeProjected.next(), HeatmapSource::RawKeypress);
        assert_eq!(HeatmapSource::RawKeypress.next(), HeatmapSource::SchemeProjected);
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

        let staggered_lines = generate_live_keyboard_lines(&kb, KeyboardMode::Staggered, &palette, now);
        assert_eq!(staggered_lines.len(), 5);

        let ortho_lines = generate_live_keyboard_lines(&kb, KeyboardMode::Ortholinear, &palette, now);
        assert_eq!(ortho_lines.len(), 4);

        let off_lines = generate_live_keyboard_lines(&kb, KeyboardMode::Off, &palette, now);
        assert_eq!(off_lines.len(), 0);
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
        assert!(full_text.contains("[Space (空格)]") || full_text.contains("Space"));
    }
}
