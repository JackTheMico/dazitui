use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use dazitui_core::{
    ApiClient, ApiError, BUILTIN_SETS, CharStatus, CompetitionType, FONT_SIZE_PT,
    LoadError, LoadOptions, Rgb, Session, Settings, SettingsStore, Stats, Text, TextSource,
    Theme, TokenStore, build_upload_payload, env_credentials, format_share_text,
    is_auth_failure, load_builtin_text, load_builtin_text_shuffled,
    load_text_from_file, load_text_from_file_with_options,
    osc_font_size_sequence, osc52_clipboard, should_auto_relogin, to_upload_stats,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::prelude::Stylize;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text as TextLines};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

/// 跟打应用状态。
enum AppState {
    /// 跟打中。
    Typing,
    /// 已出成绩（成绩视图），携带成绩与上传状态。
    Finished { stats: Stats, upload: UploadState },
    /// 载文浏览：功能栏显示文件列表，可预览与载入。
    Browsing,
    /// 内置赛文浏览：功能栏显示套题列表，可载入。
    BrowsingBuiltin,
    /// 设置视图：切换主题等外观设置。
    Settings,
}

/// 设置视图焦点项下标。
const FOCUS_THEME: usize = 0;
const FOCUS_RATIO: usize = 1;
const FOCUS_BOLD: usize = 2;
const FOCUS_FONT: usize = 3;
/// 设置视图焦点项总数。
const SETTINGS_FOCUS_COUNT: usize = 4;

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

/// 应用全部状态（TUI 层）。
struct App {
    /// 当前赛文（载文后替换）。
    text: Text,
    session: Session,
    start: Instant,
    state: AppState,
    /// 功能栏是否展开。
    sidebar_visible: bool,
    /// 载文浏览的文件列表。
    browse_files: Vec<PathBuf>,
    /// 文件列表当前选中下标。
    browse_selection: usize,
    /// 内置赛文浏览当前选中下标。
    builtin_selection: usize,
    /// 载文选项。
    options: LoadOptions,
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
    /// 设置视图当前焦点项（FOCUS_THEME/FOCUS_RATIO/FOCUS_BOLD/FOCUS_FONT）。
    settings_focus: usize,
    /// 内置赛文浏览中的乱序开关（`true` = 载入时打乱顺序）。
    builtin_shuffle: bool,
    /// 内置赛文浏览器预览缓存 `(title, body)`。
    /// 乱序开时存乱序版预览（避免每帧重新随机导致闪烁），关时存顺序版预览。
    /// 在 `open_builtin_browser` 与 Up/Down/s 按键时重新生成。
    builtin_preview: Option<(String, String)>,
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

impl App {
    fn new(text: Text) -> Self {
        Self::new_with(
            text,
            TokenStore::with_default_path(),
            ApiClient::new(),
            SettingsStore::with_default_path(),
        )
    }

    /// 指定 token 存储、API 客户端与设置存储（测试注入；生产用 `new`）。
    fn new_with(
        text: Text,
        token_store: TokenStore,
        api: ApiClient,
        settings_store: SettingsStore,
    ) -> Self {
        let session = Session::new_gated(&text.content, text.source.is_builtin());
        let settings = settings_store.load();
        // token 持久化仅用于请求携带；登录会话（session cookie）不持久化，
        // 故每次启动都需重新登录（方案 1）。即使加载到持久化 token 也不视为已登录。
        let saved_token = token_store.load();
        let (token, logged_in, login_notice) =
            if let Some((user, pass)) = env_credentials(|k| std::env::var(k).ok()) {
                match api.login(&user, &pass) {
                    Ok(r) => {
                        let _ = token_store.save(&r.token);
                        (Some(r.token), true, Some("已通过环境变量登录".to_string()))
                    }
                    Err(e) => {
                        (
                        saved_token,
                        false,
                        Some(format!("自动登录失败: {}", api_error_text(&e))),
                        )
                    }
                }
            } else {
                (saved_token, false, None)
            };
        Self {
            text,
            session,
            start: Instant::now(),
            state: AppState::Typing,
            sidebar_visible: true,
            browse_files: Vec::new(),
            browse_selection: 0,
            builtin_selection: 0,
            options: LoadOptions::default(),
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
        }
    }

    /// 当前主题的语义色板。
    fn theme(&self) -> Theme {
        Theme::preset(self.settings.theme)
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

    /// 打开登录模态框。
    fn open_login(&mut self) {
        self.login_form = Some(LoginForm::default());
        self.login_notice = None;
    }

    /// 关闭登录模态框（不改变登录状态）。
    fn close_login(&mut self) {
        self.login_form = None;
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
        if self.text.shuffled {
            if let TextSource::Builtin { set } = self.text.source {
                self.text = load_builtin_text_shuffled(set);
            }
        }
        self.session = Session::new_gated(&self.text.content, self.text.source.is_builtin());
        self.start = Instant::now();
        self.state = AppState::Typing;
        self.browse_error = None;
    }

    /// 完成跟打：计算成绩并进入成绩视图。
    ///
    /// 在线赛文置为「上传中」并返回 `Some((成绩, 用时))` 供调用方继续上传；
    /// 离线赛文直接进入成绩视图，返回 `None`。
    fn finish_typing(&mut self) -> Option<(Stats, Duration)> {
        let elapsed = self.start.elapsed();
        let stats = self.session.finish(elapsed);
        let is_online = self.text.is_online();
        if is_online {
            self.state = AppState::Finished {
                stats: stats.clone(),
                upload: UploadState::Uploading,
            };
            Some((stats, elapsed))
        } else {
            self.state = AppState::Finished {
                stats,
                upload: UploadState::NotApplicable,
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

    /// 载入当前选中的文件（应用载文选项），成功后开始新跟打。
    fn load_selected(&mut self) {
        let Some(path) = self.browse_files.get(self.browse_selection).cloned() else {
            return;
        };
        match load_text_from_file_with_options(&path, &self.options) {
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
                (set.name().to_string(), builtin_word_preview(&boundaries, &chars))
            }
            Some(&set) => {
                (set.name().to_string(), builtin_char_preview(set.content()))
            }
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
        self.session = Session::new_gated(&self.text.content, self.text.source.is_builtin());
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
        if !self.logged_in {
            self.online_loading = None;
            self.online_error = Some("请先登录 52dazi（Ctrl-O）".to_string());
            return;
        }
        let Some(token) = self.token.clone() else {
            self.online_loading = None;
            self.online_error = Some("请先登录 52dazi（Ctrl-O）".to_string());
            return;
        };
        match self.api.get_content(&token, competition_type) {
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
    ///
    /// 若上传失败因登录失效且配置了 `DAZITUI_USER`/`DAZITUI_PASS`，自动重新登录
    /// 并重试上传一次；重试仍失败按原失败路径提示。
    fn do_upload(&mut self, stats: &Stats, elapsed: Duration) {
        let mut upload = self.perform_upload(stats, elapsed);
        let need_relogin = matches!(
            &upload,
            UploadState::Failed {
                need_relogin: true,
                ..
            }
        );
        let credentials = env_credentials(|k| std::env::var(k).ok());
        if should_auto_relogin(need_relogin, credentials.is_some())
            && let Some((user, pass)) = credentials
        {
            upload = self.retry_after_relogin((user, pass), stats, elapsed);
        }
        self.state = AppState::Finished {
            stats: stats.clone(),
            upload,
        };
    }

    /// 用环境变量凭据重新登录并重试上传一次；重登失败时保留失败状态并附原始错误。
    fn retry_after_relogin(
        &mut self,
        credentials: (String, String),
        stats: &Stats,
        elapsed: Duration,
    ) -> UploadState {
        let (user, pass) = credentials;
        match self.api.login(&user, &pass) {
            Ok(r) => {
                let _ = self.token_store.save(&r.token);
                self.token = Some(r.token);
                self.logged_in = true;
                self.perform_upload(stats, elapsed)
            }
            Err(e) => UploadState::Failed {
                message: "自动重登失败，请手动重新登录".to_string(),
                need_relogin: true,
                detail: Some(api_error_text(&e)),
            },
        }
    }

    /// 执行上传：构造 payload → 调网关 → 处理结果（成功则写剪贴板）。纯状态产出，不修改自身。
    fn perform_upload(&self, stats: &Stats, elapsed: Duration) -> UploadState {
        if !self.logged_in {
            return UploadState::Failed {
                message: "未登录，无法上传成绩".to_string(),
                need_relogin: true,
                detail: None,
            };
        }
        let Some(token) = self.token.clone() else {
            return UploadState::Failed {
                message: "未登录，无法上传成绩".to_string(),
                need_relogin: true,
                detail: None,
            };
        };
        let upload = to_upload_stats(stats, elapsed);
        let payload = build_upload_payload(&self.text, stats, &upload, elapsed);
        match self.api.upload_result(&token, &payload) {
            Ok(rank) => {
                let ranking = rank.ranking.clone();
                let rank_num = ranking.as_deref().and_then(|s| s.parse::<u32>().ok());
                let share_text = format_share_text(&self.text.source, rank_num, &upload);
                write_clipboard(&share_text);
                UploadState::Success {
                    ranking,
                    share_text,
                }
            }
            Err(e) => {
                let need_relogin = is_auth_failure(&e);
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
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
    let result = event_loop(&mut terminal, app);
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
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
                if is_toggle_sidebar(key) {
                    app.sidebar_visible = !app.sidebar_visible;
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
                            if !app.logged_in {
                                // 未登录：引导先登录。
                                app.online_error =
                                    Some("请先登录 52dazi 后再载入在线赛文".to_string());
                                app.open_login();
                            } else {
                                app.online_loading = Some(competition_type);
                                // 先渲染「加载中...」，再同步下载。
                                terminal.draw(|frame| ui(frame, &app))?;
                                app.download_online(competition_type);
                            }
                            continue;
                        }
                    handle_key(&mut app.session, key);
                    if app.session.is_complete() {
                        finish_and_maybe_upload(&mut app, terminal)?;
                    }
                    }
                    AppState::Finished { .. } => {
                        // 离线赛文：任意键重打同一篇；在线赛文不支持重打。
                        if !app.text.is_online() {
                            app.restart();
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
                        KeyCode::Char('1') => {
                            app.options.strip_whitespace = !app.options.strip_whitespace
                        }
                        KeyCode::Char('2') => {
                            app.options.strip_punctuation = !app.options.strip_punctuation
                        }
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
                                _ => {}
                            }
                        }
                        KeyCode::Esc => app.state = AppState::Typing,
                        _ => {}
                    },
                }
            }
            Event::Paste(committed) => {
                if matches!(app.state, AppState::Typing) {
                    app.session.type_text(&committed);
                    if app.session.is_complete() {
                        finish_and_maybe_upload(&mut app, terminal)?;
                    }
                }
            }
            _ => {}
        }
    }
}

/// 完成跟打：进入成绩视图；在线赛文先渲染「上传中」再同步上传成绩。
fn finish_and_maybe_upload(
    app: &mut App,
    terminal: &mut ratatui::DefaultTerminal,
) -> io::Result<()> {
    let result = app.finish_typing();
    if let Some((stats, elapsed)) = result {
        // 先渲染「上传中」，再同步上传（阻塞）。
        terminal.draw(|frame| ui(frame, app))?;
        app.do_upload(&stats, elapsed);
    }
    Ok(())
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

/// 底部快捷键提示栏文案：按浏览状态与赛文来源动态切换（在线赛文不显示重打）。
fn hint_text(browsing: bool, browsing_builtin: bool, is_online: bool) -> &'static str {
    if browsing {
        " ↑↓ 选择 | Enter 载入 | Esc 取消 | 1 去空格 | 2 去符号 | Ctrl-E 设置 | q 退出"
    } else if browsing_builtin {
        " ↑↓ 选择 | Enter 载入 | s 乱序 | Esc 取消 | q 退出"
    } else if is_online {
        " q 退出 | Ctrl-S 结束 | Ctrl-B 内置赛文 | Ctrl-F 载文 | Ctrl-O 登录 | Ctrl-E 设置 | Tab 收起栏 "
    } else {
        " q 退出 | Ctrl-S 结束 | Ctrl-R 重打 | Ctrl-B 内置赛文 | Ctrl-F 载文 | Ctrl-O 登录 | Ctrl-E 设置 | Tab 收起栏 "
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

/// 收起/展开功能栏：Tab。
fn is_toggle_sidebar(key: KeyEvent) -> bool {
    key.code == KeyCode::Tab
}

/// 处理跟打键：退格回改，可打印字符上屏；同时记录按键频率。
fn handle_key(session: &mut Session, key: KeyEvent) {
    match key.code {
        KeyCode::Backspace => {
            session.record_key("Backspace");
            session.backspace();
        }
        KeyCode::Char(c) => {
            session.record_key(&c.to_string());
            session.type_text(&c.to_string());
        }
        _ => {}
    }
}

/// 退出快捷键：q / Q / Ctrl-C。
fn is_quit(key: KeyEvent) -> bool {
    let is_ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
    key.code == KeyCode::Char('q') || key.code == KeyCode::Char('Q') || is_ctrl_c
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

/// 带主题边框色（text 槽位）的边框块。
fn themed_block(theme: Theme) -> Block<'static> {
    Block::bordered().border_style(Style::default().fg(color(theme.text)))
}

/// 处理登录模态框按键，返回动作。
fn login_input(form: &mut LoginForm, key: KeyEvent) -> LoginAction {
    match key.code {
        KeyCode::Esc => LoginAction::Cancel,
        KeyCode::Tab => {
            form.focus = 1 - form.focus;
            LoginAction::None
        }
        KeyCode::Enter => LoginAction::Submit,
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
    if let AppState::Finished { stats, upload } = &app.state {
        render_result_view(frame, app, stats, upload);
        return;
    }
    if matches!(app.state, AppState::Settings) {
        render_settings(frame, app);
        return;
    }
    let browsing = matches!(app.state, AppState::Browsing);
    let browsing_builtin = matches!(app.state, AppState::BrowsingBuiltin);
    // 整体：主区 + 底部快捷键 bar
    let [main, help_bar] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
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
        // 内容区：上对照区 + 下跟打区（按设置占比分配）
        let (ref_pct, type_pct) = area_ratios(app.settings.reference_ratio);
        let [ref_area, type_area] = Layout::vertical([
            Constraint::Percentage(ref_pct),
            Constraint::Percentage(type_pct),
        ])
        .areas(content);
        // 上：对照原文区（已跟打部分绿/红着色）
        frame.render_widget(
            Paragraph::new(original_line(
                &app.session,
                &app.text,
                app.theme(),
                app.settings.bold,
            ))
            .block(themed_block(app.theme()).title(format!(" 对照区 — {} ", app.text.title)))
            .wrap(Wrap { trim: false }),
            ref_area,
        );
        // 下：跟打区（实时绿/红渲染）
        frame.render_widget(
            Paragraph::new(type_line(
                &app.session,
                &app.text,
                app.theme(),
                app.settings.bold,
            ))
            .block(themed_block(app.theme()).title(format!(
                " 跟打区 — {}/{} 字符 ",
                app.session.len(),
                app.text.content.chars().count()
            )))
            .wrap(Wrap { trim: false }),
            type_area,
        );
    }

    // 底部快捷键提示 bar
    let hint = hint_text(browsing, browsing_builtin, app.text.is_online());
    frame.render_widget(Paragraph::new(Line::from(hint)), help_bar);

    // 登录模态框（覆盖层）
    if let Some(form) = &app.login_form {
        render_login_modal(frame, form, app.theme());
    }
}

/// 登录模态框：居中弹层，用户名 + 遮蔽密码。
fn render_login_modal(frame: &mut Frame, form: &LoginForm, theme: Theme) {
    let area = centered_rect(frame.area(), 62, 9);
    frame.render_widget(Clear, area);
    let mut lines = vec![Line::from(" 登录 52dazi ").bold(), Line::from("")];
    let user_label = if form.focus == 0 {
        "用户名 ▸ "
    } else {
        "用户名   "
    };
    lines.push(Line::from(format!(" {user_label}{}", form.username)));
    let pass_label = if form.focus == 1 {
        "密码   ▸ "
    } else {
        "密码     "
    };
    lines.push(Line::from(format!(
        " {pass_label}{}",
        mask_password(&form.password)
    )));
    lines.push(Line::from(""));
    if form.busy {
        lines.push(Line::from(" 登录中…").fg(color(theme.warn)));
    } else if let Some(err) = &form.error {
        lines.push(Line::from(format!(" 错误: {err}")).fg(color(theme.wrong)));
    } else {
        lines.push(Line::from(" Enter 登录 | Tab 切换 | Esc 取消").fg(color(theme.muted)));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(themed_block(theme).title(" 登录 "))
            .wrap(Wrap { trim: false }),
        area,
    );
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

/// 左侧功能栏：文件列表 + 载文选项开关。
fn render_sidebar(
    frame: &mut Frame,
    app: &App,
    area: ratatui::layout::Rect,
    browsing: bool,
    browsing_builtin: bool,
) {
    let theme = app.theme();
    let mut lines: Vec<Line> = Vec::new();
    if browsing {
        lines.push(Line::from(" 载入文件:").bold());
        if app.browse_files.is_empty() {
            lines.push(Line::from("   （无文本文件）").fg(color(theme.muted)));
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
                lines.push(Line::from(format!("{prefix}{name}")));
            }
        }
        if let Some(err) = &app.browse_error {
            lines.push(Line::from(format!(" 错误: {err}")).fg(color(theme.wrong)));
        }
    } else if browsing_builtin {
        lines.push(Line::from(" 内置赛文:").bold());
        for (i, set) in BUILTIN_SETS.iter().enumerate() {
            let prefix = if i == app.builtin_selection {
                " > "
            } else {
                "   "
            };
            lines.push(Line::from(format!("{prefix}{}", set.name())));
        }
    } else {
        lines.push(Line::from(" 载入文件（Ctrl-F）").fg(color(theme.muted)));
        lines.push(Line::from(" 内置赛文（Ctrl-B）").fg(color(theme.muted)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(" 载文选项:").bold());
    let ws = if app.options.strip_whitespace {
        "[x]"
    } else {
        "[ ]"
    };
    let punct = if app.options.strip_punctuation {
        "[x]"
    } else {
        "[ ]"
    };
    lines.push(Line::from(format!(" {ws} 1 去空格")));
    lines.push(Line::from(format!(" {punct} 2 去符号")));
    lines.push(Line::from(""));
    lines.push(Line::from(" 在线:").bold());
    let login_entry = if app.logged_in {
        Line::from(" 已登录 52dazi").fg(color(theme.accent))
    } else {
        Line::from(" 登录 52dazi（Ctrl-O）").fg(color(theme.warn))
    };
    lines.push(login_entry);
    if let Some(notice) = &app.login_notice {
        lines.push(Line::from(format!("  {notice}")).fg(color(theme.muted)));
    }
    // 三个比赛入口。
    lines.push(Line::from(" F1 极速杯"));
    lines.push(Line::from(" F2 锦标赛"));
    lines.push(Line::from(" F3 键神杯"));
    // 加载中 / 错误提示。
    if let Some(ct) = app.online_loading {
        lines.push(Line::from(format!(" 正在载入{}...", ct.name())).fg(color(theme.accent)));
    }
    if let Some(err) = &app.online_error {
        lines.push(Line::from(format!(" {err}")).fg(color(theme.wrong)));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(themed_block(theme).title(" 功能栏 "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// 载文预览：右侧内容区显示选中文件的内容（前 400 字符）。
fn render_preview(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let theme = app.theme();
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
                    (name, format!("{preview}{dot}"), Style::default())
                }
                Err(_) => (
                    name,
                    "（无法读取预览）".to_string(),
                    Style::default().fg(color(theme.wrong)),
                ),
            }
        }
        None => (
            "预览".to_string(),
            "（无文件可选）".to_string(),
            Style::default(),
        ),
    };
    let lines = vec![
        Line::from(format!(" 载文预览 — {title} ")).bold(),
        Line::from(""),
        Line::styled(body, style),
        Line::from(""),
        Line::from(" Enter 载入 | Esc 取消 ").fg(color(theme.muted)),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(themed_block(theme).title(" 预览 "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// 词组赛文预览：取前 `BUILTIN_ITEMS_PER_PAGE` 个词，词间加空格。
fn builtin_word_preview(
    boundaries: &[(usize, usize)],
    chars: &[char],
) -> String {
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
    let dot = if content.chars().count() > 400 { "…" } else { "" };
    format!("{}{dot}", chars.iter().collect::<String>())
}

/// 内置赛文预览：右侧内容区显示选中套题的内容预览。
fn render_builtin_preview(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let theme = app.theme();
    let (title, body) = app
        .builtin_preview
        .clone()
        .unwrap_or_else(|| ("预览".to_string(), "（无内置赛文）".to_string()));
    // 预览按每 10 字一页（单字）或整行（词组），与实际跟打展示一致。
    let mut lines: Vec<Line> = vec![
        Line::from(format!(" 内置赛文 — {title} ")).bold(),
        Line::from(""),
    ];
    let is_words = matches!(BUILTIN_SETS.get(app.builtin_selection), Some(set) if set.is_words());
    if is_words {
        lines.push(Line::from(body));
    } else {
        for chunk in body.chars().collect::<Vec<char>>().chunks(BUILTIN_ITEMS_PER_PAGE) {
            lines.push(Line::from(chunk.iter().collect::<String>()));
        }
    }
    lines.push(Line::from(""));
    let shuffle_label = if app.builtin_shuffle { "[x] 乱序" } else { "[ ] 乱序" };
    lines.push(
        Line::from(format!(" Enter 载入 | s {shuffle_label} | Esc 取消 "))
            .fg(color(theme.muted)),
    );
    frame.render_widget(
        Paragraph::new(lines)
            .block(themed_block(theme).title(" 预览 "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// 设置视图：焦点行 + 左右调整（主题/占比/粗体/字体）。
fn render_settings(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let focus = app.settings_focus;
    let mut lines = vec![Line::from(" 设置 ").bold(), Line::from("")];

    lines.push(settings_row(
        "主题",
        app.settings.theme.name(),
        focus == FOCUS_THEME,
        theme,
    ));
    lines.push(settings_row(
        "对照区占比",
        &format!("{}%", app.settings.reference_ratio),
        focus == FOCUS_RATIO,
        theme,
    ));
    lines.push(settings_row(
        "粗体",
        on_off(app.settings.bold),
        focus == FOCUS_BOLD,
        theme,
    ));
    lines.push(settings_row(
        "字体",
        on_off(app.settings.font),
        focus == FOCUS_FONT,
        theme,
    ));

    lines.push(Line::from(""));
    // 主题预览：用当前主题的对/错色渲染示意文字。
    lines.push(Line::from(" 预览:").bold());
    lines.push(Line::from("  对正确对正确").fg(color(theme.correct)));
    lines.push(Line::from("  错错误错错误").fg(color(theme.wrong)));
    lines.push(Line::from(""));
    lines.push(Line::from(" ↑↓ 选择 | ←→ 调整 | Esc 返回").fg(color(theme.muted)));

    frame.render_widget(
        Paragraph::new(lines)
            .block(themed_block(theme).title(" 设置 "))
            .wrap(Wrap { trim: false }),
        centered_rect(frame.area(), 60, 14),
    );
}

/// 设置项行：焦点项用 accent 色 + `>` 标记高亮。
fn settings_row(label: &str, value: &str, focused: bool, theme: Theme) -> Line<'static> {
    let marker = if focused { " > " } else { "   " };
    let line = Line::from(format!("{marker}{label}: {value}"));
    if focused {
        line.fg(color(theme.accent))
    } else {
        line
    }
}

/// 布尔开关显示为「开/关」。
fn on_off(v: bool) -> &'static str {
    if v { "开" } else { "关" }
}

/// 全屏成绩视图：WPM/错字/回改/按键频率 + 上传状态（在线赛文）。
fn render_result_view(frame: &mut Frame, app: &App, stats: &Stats, upload: &UploadState) {
    let theme = app.theme();
    let lines = vec![
        Line::from(format!(" 成绩 — {} ", app.text.title)).bold(),
        Line::from(""),
        Line::from(format!(" WPM:        {:.1}", stats.wpm)),
        Line::from(format!(
            " 正确字数:   {} / {}",
            stats.correct_chars,
            app.text.content.chars().count()
        )),
        Line::from(format!(
            " 错字:       {}（不一致 {} + 回改 {}）",
            stats.wrong_total, stats.wrong_chars, stats.edits
        )),
        Line::from(format!(
            " 回改明细:   {}",
            if stats.edit_details.is_empty() {
                "无".to_string()
            } else {
                stats.edit_details.iter().collect::<String>()
            }
        )),
        Line::from(""),
        Line::from(" 按键频率:").bold(),
    ];
    let mut freq_lines = stats
        .key_frequency
        .iter()
        .map(|(k, n)| Line::from(format!("   {k:<12} {n}")))
        .collect::<Vec<_>>();
    if freq_lines.is_empty() {
        freq_lines.push(Line::from("   （无按键记录）"));
    }
    let mut all = lines;
    all.extend(freq_lines);
    // 上传状态（在线赛文；离线赛文不显示）。
    all.extend(upload_lines(upload, theme));
    all.push(Line::from(""));
    if app.text.is_online() {
        // 在线赛文不支持重打，只能退出或载入其他赛文。
        all.push(Line::from(" q 退出 | Ctrl-F 载文").fg(color(theme.muted)));
    } else {
        all.push(Line::from(" 按任意键重打 | q 退出").fg(color(theme.muted)));
    }
    frame.render_widget(
        Paragraph::new(all)
            .block(themed_block(theme).title(" 成绩 "))
            .wrap(Wrap { trim: false }),
        frame.area(),
    );
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
                lines.push(Line::from(" 请按 Ctrl-O 重新登录").fg(color(theme.warn)));
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
        for ci in ws..we {
            let (c, status) = statuses[ci];
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
fn original_line(
    session: &Session,
    text: &Text,
    theme: Theme,
    bold: bool,
) -> TextLines<'static> {
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
            let page_end_word =
                (page_start_word + BUILTIN_ITEMS_PER_PAGE).min(boundaries.len());
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
fn type_line(
    session: &Session,
    text: &Text,
    theme: Theme,
    bold: bool,
) -> TextLines<'static> {
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
            let page_end_word =
                (page_start_word + BUILTIN_ITEMS_PER_PAGE).min(boundaries.len());
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
        App::new_with(
            text,
            temp_token_store(),
            ApiClient::with_base_url("http://127.0.0.1:1"),
            temp_settings_store(),
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
    fn q_quits() {
        assert!(is_quit(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE
        )));
        assert!(is_quit(KeyEvent::new(
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
        assert!(hint_text(false, false, false).contains("重打"));
        // 在线跟打：不显示重打提示。
        assert!(!hint_text(false, false, true).contains("重打"));
        // 浏览态（载文选择）与来源无关，也不显示重打。
        assert!(!hint_text(true, false, false).contains("重打"));
        assert!(!hint_text(true, false, true).contains("重打"));
        // 内置赛文浏览态：不显示重打。
        assert!(!hint_text(false, true, false).contains("重打"));
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
        assert!(!title.contains("乱序"), "乱序关时预览标题不应含「（乱序）」");
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
        assert!(app.text.title.contains("乱序"), "重打后标题仍含「（乱序）」");
        assert_ne!(
            app.text.content, content_before,
            "重打应产生新的乱序排列"
        );
        assert_eq!(app.session.len(), 0, "重打后 session 应清空");
        assert!(matches!(app.state, AppState::Typing));
    }

    #[test]
    fn original_line_shuffled_word_set_uses_text_boundaries() {
        // 乱序词组 Text 携带自身 word_boundaries，original_line 应直接使用它们。
        let theme = Theme::preset(ThemePreset::Default);
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let text = load_builtin_text_shuffled(set);
        assert!(text.shuffled);
        let boundaries = text.word_boundaries.as_ref().unwrap();
        assert!(!boundaries.is_empty());
        let session = Session::new_gated(&text.content, true);
        let rendered = original_line(&session, &text, theme, false);
        assert_eq!(rendered.lines.len(), 1, "乱序词组对照区应只有一行");
        let first_page_words = boundaries.len().min(BUILTIN_ITEMS_PER_PAGE);
        let space_spans = rendered.lines[0]
            .spans
            .iter()
            .filter(|s| s.content == " ")
            .count();
        assert_eq!(space_spans, first_page_words - 1, "乱序第 1 页词间空格数");
        let word_chars: usize = boundaries.iter().take(first_page_words).map(|(s, e)| e - s).sum();
        let non_space_spans = rendered.lines[0]
            .spans
            .iter()
            .filter(|s| s.content != " ")
            .count();
        assert_eq!(non_space_spans, word_chars, "乱序第 1 页非空格 span 数应等于词字符数");
    }

    #[test]
    fn type_line_shuffled_word_set_uses_text_boundaries() {
        // 乱序词组 Text 携带自身 word_boundaries，type_line 应直接使用它们。
        let theme = Theme::preset(ThemePreset::Default);
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let text = load_builtin_text_shuffled(set);
        let boundaries = text.word_boundaries.as_ref().unwrap();
        let mut session = Session::new_gated(&text.content, true);
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
        handle_key(
            &mut session,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        handle_key(
            &mut session,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        handle_key(
            &mut session,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        let stats = session.finish(Duration::from_secs(60));
        assert_eq!(stats.key_frequency[0], ("n".to_string(), 2));
        assert_eq!(stats.key_frequency[1], ("Backspace".to_string(), 1));
    }

    #[test]
    fn backspace_key_edits_session() {
        let mut session = Session::new("你好世界");
        session.type_text("你好");
        let mut key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        handle_key(&mut session, key);
        assert_eq!(session.len(), 1);
        assert_eq!(session.edit_count(), 1);
        key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        handle_key(&mut session, key);
        assert_eq!(session.len(), 2);
    }

    #[test]
    fn type_line_colors_correct_green_wrong_red() {
        let theme = Theme::preset(ThemePreset::Default);
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
        let theme = Theme::preset(ThemePreset::Default);
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
        let theme = Theme::preset(ThemePreset::Default);
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
        let theme = Theme::preset(ThemePreset::Default);
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
        assert_eq!(move_focus(0, -1), 3);
        assert_eq!(move_focus(3, 1), 0);
        assert_eq!(move_focus(0, 1), 1);
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
    fn load_selected_applies_options_and_restarts() {
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
        app.options.strip_whitespace = true;
        app.options.strip_punctuation = true;

        app.load_selected();
        assert_eq!(app.text.title, "a.txt");
        assert_eq!(app.text.content, "你好世界第二行");
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
        // 默认从 Default 开始。
        assert_eq!(app.settings.theme, ThemePreset::Default);
        // 切下一主题并持久化。
        app.next_theme();
        assert_eq!(app.settings.theme, ThemePreset::Catppuccin);
        assert_eq!(app.settings_store.load().theme, ThemePreset::Catppuccin);
        // 循环回绕：往前退回到 Default。
        app.prev_theme();
        assert_eq!(app.settings.theme, ThemePreset::Default);
        assert_eq!(app.settings_store.load().theme, ThemePreset::Default);
        // 从 Default 往上退绕到 Gruvbox。
        app.prev_theme();
        assert_eq!(app.settings.theme, ThemePreset::Gruvbox);
    }

    #[test]
    fn theme_switch_changes_correct_wrong_colors() {
        // 对/错颜色随主题切换改变（外部可观察行为：不是固定绿/红）。
        let default_theme = Theme::preset(ThemePreset::Default);
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
        let default_text = original_line(&session, &file_text, default_theme, false);
        let dracula_text = original_line(&session, &file_text, dracula_theme, false);
        let default_line = &default_text.lines[0];
        let dracula_line = &dracula_text.lines[0];
        assert_eq!(
            default_line.spans[0].style.fg,
            Some(color(default_theme.correct))
        );
        assert_eq!(
            default_line.spans[1].style.fg,
            Some(color(default_theme.wrong))
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
            default_line.spans[0].style.fg,
            dracula_line.spans[0].style.fg
        );
        assert_ne!(
            default_line.spans[1].style.fg,
            dracula_line.spans[1].style.fg
        );
    }

    #[test]
    fn type_line_builtin_shows_only_current_page() {
        // 25 字内置赛文：每页 10 字，当前组 10 字全对才翻到下一页，跟打区只显示当前页。
        let theme = Theme::preset(ThemePreset::Default);
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
        assert_eq!(rendered.lines[0].spans.len(), 3, "打 13 字应翻页显示第 11-13 字");
    }

    #[test]
    fn type_line_builtin_wrong_char_blocks_page_advance() {
        // 组内有错字 → completed_groups 不推进 → 不翻页。
        let theme = Theme::preset(ThemePreset::Default);
        let content = "一二三四五六七八九十甲乙丙丁戊己庚辛壬癸子丑寅卯辰";
        let mut session = Session::new_gated(content, true);
        let text = builtin_text(content);
        // 打 10 字但第 10 字打错 → 组未全对 → 不翻页
        session.type_text("一二三四五六七八九X");
        let rendered = type_line(&session, &text, theme, false);
        assert_eq!(rendered.lines[0].spans.len(), 10, "组内打错仍应显示当前组 10 字");
        assert_eq!(session.completed_groups(), 0, "有错字不应推进 completed_groups");
    }

    #[test]
    fn type_line_builtin_backspace_at_group_boundary_keeps_page() {
        // 退格到组首封顶 → 页起始不变、不翻回上一组。
        let theme = Theme::preset(ThemePreset::Default);
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
        let theme = Theme::preset(ThemePreset::Default);
        let content = "一二三四五六七八九十甲乙丙丁戊己庚辛壬癸子丑寅卯辰";
        let mut session = Session::new_gated(content, true);
        let text = builtin_text(content);
        // 打 5 字（全对）：对照区显示第一组 10 字（前 5 已打对，后 5 未打到）。
        session.type_text("一二三四五");
        let rendered = original_line(&session, &text, theme, false);
        assert_eq!(rendered.lines.len(), 1, "第一组应只有一行");
        assert_eq!(rendered.lines[0].spans.len(), 10, "对照区第一组应显示 10 字");
        // 打到 10 字（全对）：第一组全对，翻到第二组，对照区显示第 11-20 字（10 字）。
        session.type_text("六七八九十");
        let rendered = original_line(&session, &text, theme, false);
        assert_eq!(rendered.lines.len(), 1, "第二组应只有一行");
        assert_eq!(rendered.lines[0].spans.len(), 10, "对照区第二组应显示 10 字");
    }

    #[test]
    fn type_line_file_source_stays_single_line() {
        // 非内置赛文（File）保持单行：由终端宽度自动折行，不分多行 span。
        let theme = Theme::preset(ThemePreset::Default);
        let mut session = Session::new("一二三四五六七八九十十一十");
        session.type_text("一二三四五六七八九十十一十");
        let text = type_line(&session, &file_text("一二三四五六七八九十十一十"), theme, false);
        assert_eq!(text.lines.len(), 1);
        assert_eq!(text.lines[0].spans.len(), 13);
    }

    #[test]
    fn type_line_empty_input_builtin_shows_placeholder() {
        // 空输入时显示提示行（不分多行、无空 span）。
        let theme = Theme::preset(ThemePreset::Default);
        let session = Session::new_gated("一二三四五六七八九十", true);
        let text = type_line(&session, &builtin_text("一二三四五六七八九十"), theme, false);
        assert_eq!(text.lines.len(), 1, "空输入应只有一行提示");
    }

    #[test]
    fn type_line_word_set_shows_space_between_words() {
        // 词组赛文：词间显示空格 span，去逗号。每页 10 个词。
        let theme = Theme::preset(ThemePreset::Default);
        // content_no_commas = "可以一个自己没有..."（词间无逗号）
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let no_commas = set.content_no_commas();
        let mut session = Session::new_gated(no_commas.as_str(), true);
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
        let theme = Theme::preset(ThemePreset::Default);
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let no_commas = set.content_no_commas();
        let boundaries = set.word_boundaries();
        let session = Session::new_gated(no_commas.as_str(), true);
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
        let word_chars: usize = boundaries.iter().take(first_page_words).map(|(s, e)| e - s).sum();
        let space_spans = rendered.lines[0]
            .spans
            .iter()
            .filter(|s| s.content == " ")
            .count();
        assert_eq!(space_spans, first_page_words - 1, "第 1 页应有 {} 个词间空格", first_page_words - 1);
        // 非空格 span 数 = 第 1 页所有词的字符数
        let non_space_spans = rendered.lines[0]
            .spans
            .iter()
            .filter(|s| s.content != " ")
            .count();
        assert_eq!(non_space_spans, word_chars, "非空格 span 数应等于第 1 页词字符数");
    }

    #[test]
    fn type_line_word_set_advances_page_after_10_words() {
        // 词组赛文打满 10 个词且全对后翻页。
        let theme = Theme::preset(ThemePreset::Default);
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let no_commas = set.content_no_commas();
        let boundaries = set.word_boundaries();
        let mut session = Session::new_gated(no_commas.as_str(), true);
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
        assert!(!placeholder_spans.is_empty(), "翻到第 2 组未打字时应显示提示行");
    }

    #[test]
    fn original_line_word_set_advances_page_after_10_words() {
        // 词组赛文对照区：10 个词全对后翻到第 2 组，显示第 11-20 词。
        let theme = Theme::preset(ThemePreset::Default);
        let set = BUILTIN_SETS[3]; // 常用词组前五百
        let no_commas = set.content_no_commas();
        let boundaries = set.word_boundaries();
        let mut session = Session::new_gated(no_commas.as_str(), true);
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
        assert_eq!(space_spans, second_page_words - 1, "第 2 页应有 {} 个词间空格", second_page_words - 1);
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
        let mut form = LoginForm::default();
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
        let theme = Theme::preset(ThemePreset::Default);
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
    fn startup_with_saved_token_keeps_token_but_requires_relogin() {
        // 持久化 token 仍被加载（用于请求携带），但会话不持久化——不视为已登录，需重新登录。
        let store = temp_token_store();
        store.save("saved-token").unwrap();
        let app = App::new_with(
            file_text("你好"),
            store,
            ApiClient::with_base_url("http://127.0.0.1:1"), // 不发网络请求
            temp_settings_store(),
        );
        assert_eq!(app.token.as_deref(), Some("saved-token"));
        assert!(!app.logged_in);
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
            store,
            ApiClient::with_base_url(&format!("http://127.0.0.1:{port}")),
            temp_settings_store(),
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
            store,
            ApiClient::with_base_url("http://127.0.0.1:1"),
            temp_settings_store(),
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
    fn retry_after_relogin_relogins_and_uploads() {
        // 自动重登成功：token 更新为新值，并重试上传成功。
        let (port, handle) = mock_server(&[
            (
                "/Api/User/login",
                r#"{"error":0,"msg":{"token":"new-tok"}}"#,
            ),
            (
                "/Api/Rank/uploadResult",
                r#"{"error":0,"msg":{"ranking":5,"rankTips":"恭喜获得第5名"}}"#,
            ),
        ]);
        let mut app = test_app(online_text("你好世界"));
        app.token = Some("old-tok".into());
        app.api = ApiClient::with_base_url(&format!("http://127.0.0.1:{port}"));
        let stats = app.session.finish(Duration::from_secs(40));
        let up = app.retry_after_relogin(
            ("user".into(), "pass".into()),
            &stats,
            Duration::from_secs(40),
        );
        handle.join().unwrap();
        assert_eq!(app.token.as_deref(), Some("new-tok"));
        assert!(matches!(
            &up,
            UploadState::Success { ranking, .. } if ranking.as_deref() == Some("5")
        ));
    }

    #[test]
    fn retry_after_relogin_failed_login_keeps_failure() {
        // 自动重登失败：保留失败状态、附重登原始错误，token 不变。
        let (port, handle) = mock_server(&[(
            "/Api/User/login",
            r#"{"error":1,"msg":"您的用户名或密码错误！"}"#,
        )]);
        let mut app = test_app(online_text("你好世界"));
        app.token = Some("old-tok".into());
        app.api = ApiClient::with_base_url(&format!("http://127.0.0.1:{port}"));
        let stats = app.session.finish(Duration::from_secs(10));
        let up = app.retry_after_relogin(
            ("user".into(), "pass".into()),
            &stats,
            Duration::from_secs(10),
        );
        handle.join().unwrap();
        assert_eq!(app.token.as_deref(), Some("old-tok"));
        assert_eq!(
            up,
            UploadState::Failed {
                message: "自动重登失败，请手动重新登录".to_string(),
                need_relogin: true,
                detail: Some("您的用户名或密码错误！".to_string()),
            }
        );
    }
}
