use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use dazitui_core::{
    ApiClient, ApiError, CharStatus, CompetitionType, LoadError, LoadOptions, Session, Stats, Text,
    TextSource, TokenStore, env_credentials, load_text_from_file, load_text_from_file_with_options,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::prelude::Stylize;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

/// 跟打应用状态。
enum AppState {
    /// 跟打中。
    Typing,
    /// 已出成绩（成绩视图）。
    Finished(Stats),
    /// 载文浏览：功能栏显示文件列表，可预览与载入。
    Browsing,
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
    /// 载文选项。
    options: LoadOptions,
    /// 载文失败时的错误提示。
    browse_error: Option<String>,
    /// token 持久化存储。
    token_store: TokenStore,
    /// 当前登录 token（`None` = 未登录）。
    token: Option<String>,
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
        let session = Session::new(&text.content);
        let token_store = TokenStore::with_default_path();
        let api = ApiClient::new();
        // 免登录：加载已保存的 token。
        let saved_token = token_store.load();
        // 未保存 token 时，尝试环境变量自动登录。
        let (token, login_notice) = if saved_token.is_some() {
            (saved_token, None)
        } else if let Some((user, pass)) = env_credentials(|k| std::env::var(k).ok()) {
            match api.login(&user, &pass) {
                Ok(r) => {
                    let _ = token_store.save(&r.token);
                    (Some(r.token), Some("已通过环境变量登录".to_string()))
                }
                Err(e) => (None, Some(format!("自动登录失败: {}", api_error_text(&e)))),
            }
        } else {
            (None, None)
        };
        Self {
            text,
            session,
            start: Instant::now(),
            state: AppState::Typing,
            sidebar_visible: true,
            browse_files: Vec::new(),
            browse_selection: 0,
            options: LoadOptions::default(),
            browse_error: None,
            token_store,
            token,
            api,
            login_form: None,
            login_notice,
            online_loading: None,
            online_error: None,
        }
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
    fn restart(&mut self) {
        self.session = Session::new(&self.text.content);
        self.start = Instant::now();
        self.state = AppState::Typing;
        self.browse_error = None;
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

    /// 按比赛类型下载在线赛文并进入跟打。
    ///
    /// 调用前 `online_loading` 已由事件循环置为 `Some` 并渲染（保证「加载中...」可见）；
    /// 这里执行同步下载，成功后替换赛文，失败则回填错误提示。
    fn download_online(&mut self, competition_type: CompetitionType) {
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
                };
                self.session = Session::new(&self.text.content);
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
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("用法: dazitui <文件名>");
        std::process::exit(1);
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
                            app.state = AppState::Finished(app.session.finish(app.start.elapsed()));
                            continue;
                        }
                        if is_open_browser(key) {
                            app.open_browser();
                            continue;
                        }
                        if is_restart(key) {
                            app.restart();
                            continue;
                        }
                        if let Some(competition_type) = online_shortcut(key) {
                            if app.token.is_none() {
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
                            app.state = AppState::Finished(app.session.finish(app.start.elapsed()));
                        }
                    }
                    AppState::Finished(_) => {
                        // 任意键重打同一篇赛文
                        app.restart();
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
                }
            }
            Event::Paste(committed) => {
                if matches!(app.state, AppState::Typing) {
                    app.session.type_text(&committed);
                    if app.session.is_complete() {
                        app.state = AppState::Finished(app.session.finish(app.start.elapsed()));
                    }
                }
            }
            _ => {}
        }
    }
}

/// 提前结束快捷键：Ctrl-S（Stop）。
fn is_early_finish(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s')
}

/// 重打快捷键：Ctrl-R（Restart）。
fn is_restart(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r')
}

/// 进入载文浏览快捷键：Ctrl-F（File）。
fn is_open_browser(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('f')
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
    if let AppState::Finished(stats) = &app.state {
        render_result_view(frame, app, stats);
        return;
    }
    let browsing = matches!(app.state, AppState::Browsing);
    // 整体：主区 + 底部快捷键 bar
    let [main, help_bar] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    // 主区：左侧功能栏 + 右侧内容区（功能栏收起时宽度为 0）
    let sidebar_width = if app.sidebar_visible { 24 } else { 0 };
    let [sidebar, content] =
        Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(0)]).areas(main);

    if app.sidebar_visible {
        render_sidebar(frame, app, sidebar, browsing);
    }

    if browsing {
        render_preview(frame, app, content);
    } else {
        // 内容区：上对照区 + 下跟打区
        let [ref_area, type_area] =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(content);
        // 上：对照原文区（已跟打部分绿/红着色）
        frame.render_widget(
            Paragraph::new(original_line(&app.session))
                .block(Block::bordered().title(format!(" 对照区 — {} ", app.text.title)))
                .wrap(Wrap { trim: false }),
            ref_area,
        );
        // 下：跟打区（实时绿/红渲染）
        frame.render_widget(
            Paragraph::new(type_line(&app.session))
                .block(Block::bordered().title(format!(
                    " 跟打区 — {}/{} 字符 ",
                    app.session.len(),
                    app.text.content.chars().count()
                )))
                .wrap(Wrap { trim: false }),
            type_area,
        );
    }

    // 底部快捷键提示 bar
    let hint = if browsing {
        " ↑↓ 选择 | Enter 载入 | Esc 取消 | 1 去空格 | 2 去符号 | q 退出"
    } else {
        " q 退出 | Ctrl-S 结束 | Ctrl-R 重打 | Ctrl-F 载文 | Ctrl-O 登录 | Tab 收起栏 "
    };
    frame.render_widget(Paragraph::new(Line::from(hint)), help_bar);

    // 登录模态框（覆盖层）
    if let Some(form) = &app.login_form {
        render_login_modal(frame, form);
    }
}

/// 登录模态框：居中弹层，用户名 + 遮蔽密码。
fn render_login_modal(frame: &mut Frame, form: &LoginForm) {
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
        lines.push(Line::from(" 登录中…").yellow());
    } else if let Some(err) = &form.error {
        lines.push(Line::from(format!(" 错误: {err}")).red());
    } else {
        lines.push(Line::from(" Enter 登录 | Tab 切换 | Esc 取消").gray());
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(" 登录 "))
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
fn render_sidebar(frame: &mut Frame, app: &App, area: ratatui::layout::Rect, browsing: bool) {
    let mut lines: Vec<Line> = Vec::new();
    if browsing {
        lines.push(Line::from(" 载入文件:").bold());
        if app.browse_files.is_empty() {
            lines.push(Line::from("   （无文本文件）").gray());
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
            lines.push(Line::from(format!(" 错误: {err}")).red());
        }
    } else {
        lines.push(Line::from(" 载入文件（Ctrl-F）").gray());
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
    let login_entry = if app.token.is_some() {
        Line::from(" 已登录 52dazi").green()
    } else {
        Line::from(" 登录 52dazi（Ctrl-O）").yellow()
    };
    lines.push(login_entry);
    if let Some(notice) = &app.login_notice {
        lines.push(Line::from(format!("  {notice}")).gray());
    }
    // 三个比赛入口。
    lines.push(Line::from(" F1 极速杯"));
    lines.push(Line::from(" F2 锦标赛"));
    lines.push(Line::from(" F3 键神杯"));
    // 加载中 / 错误提示。
    if let Some(ct) = app.online_loading {
        lines.push(Line::from(format!(" 正在载入{}...", ct.name())).cyan());
    }
    if let Some(err) = &app.online_error {
        lines.push(Line::from(format!(" {err}")).red());
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(" 功能栏 "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// 载文预览：右侧内容区显示选中文件的内容（前 400 字符）。
fn render_preview(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
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
                    Style::default().fg(Color::Red),
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
        Line::from(" Enter 载入 | Esc 取消 ").gray(),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(" 预览 "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// 全屏成绩视图：WPM/错字/回改/按键频率。
fn render_result_view(frame: &mut Frame, app: &App, stats: &Stats) {
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
    all.push(Line::from(""));
    all.push(Line::from(" 按任意键重打 | q 退出").gray());
    frame.render_widget(
        Paragraph::new(all)
            .block(Block::bordered().title(" 成绩 "))
            .wrap(Wrap { trim: false }),
        frame.area(),
    );
}

/// 将对照区的字符按跟打状态着色：已打对=绿、已打错=红、未打到=默认。
fn original_line(session: &Session) -> Line<'static> {
    let spans: Vec<Span<'static>> = session
        .original_status()
        .into_iter()
        .map(|(c, status)| {
            let style = match status {
                Some(CharStatus::Correct) => Style::default().fg(Color::Green),
                Some(CharStatus::Wrong) => Style::default().fg(Color::Red),
                None => Style::default(),
            };
            Span::styled(c.to_string(), style)
        })
        .collect();
    Line::from(spans)
}

/// 将跟打区的字符按对/错渲染为绿/红一行。
fn type_line(session: &Session) -> Line<'static> {
    let display = session.display();
    if display.is_empty() {
        return Line::from("（跟打区 — 输入法上屏文字将显示在这里）").gray();
    }
    let spans: Vec<Span<'static>> = display
        .into_iter()
        .map(|(c, status)| {
            let style = match status {
                CharStatus::Correct => Style::default().fg(Color::Green),
                CharStatus::Wrong => Style::default().fg(Color::Red),
            };
            Span::styled(c.to_string(), style)
        })
        .collect();
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dazitui_core::TextSource;
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
        let mut session = Session::new("你好世界");
        session.type_text("你好四界");
        let line = type_line(&session);
        assert_eq!(line.spans.len(), 4);
        assert_eq!(line.spans[0].style.fg, Some(Color::Green));
        assert_eq!(line.spans[2].style.fg, Some(Color::Red));
    }

    #[test]
    fn original_line_colors_green_red_default() {
        let mut session = Session::new("你好世界");
        session.type_text("你好四");
        let line = original_line(&session);
        assert_eq!(line.spans.len(), 4);
        assert_eq!(line.spans[0].style.fg, Some(Color::Green)); // 你 ✓
        assert_eq!(line.spans[2].style.fg, Some(Color::Red)); // 世 ✗（打成四）
        assert_eq!(line.spans[3].style.fg, None); // 界：未打到，默认色
    }

    #[test]
    fn load_selected_applies_options_and_restarts() {
        let dir = temp_dir("load");
        let path = dir.join("a.txt");
        fs::write(&path, "你好， 世界。\n第二行").unwrap();
        let mut app = App::new(Text {
            title: "old".into(),
            content: "旧赛文".into(),
            source: TextSource::File,
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
        let mut app = App::new(Text {
            title: "old".into(),
            content: "旧赛文".into(),
            source: TextSource::File,
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
        let mut app = App::new(Text {
            title: "t".into(),
            content: "c".into(),
            source: TextSource::File,
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
        let mut app = App::new(Text {
            title: "t".into(),
            content: "c".into(),
            source: TextSource::File,
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
}
