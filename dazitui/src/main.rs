use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use dazitui_core::{CharStatus, LoadError, Session, Stats, Text, load_text_from_file};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::prelude::Stylize;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

/// 跟打应用状态：跟打中 / 已出成绩（成绩视图）。
enum AppState {
    Typing,
    Finished(Stats),
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

    if let Err(e) = run_tui(&text) {
        eprintln!("错误: {e}");
        std::process::exit(1);
    }
}

fn run_tui(text: &Text) -> io::Result<()> {
    let mut terminal = ratatui::init();
    // bracketed paste：中文输入法（fcitx/ibus）上屏以 paste 事件到达，必须启用
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
    let result = event_loop(&mut terminal, text);
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, text: &Text) -> io::Result<()> {
    let mut session = Session::new(&text.content);
    let mut start = Instant::now();
    let mut state = AppState::Typing;
    loop {
        terminal.draw(|frame| ui(frame, text, &session, &state))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                if is_quit(key) {
                    return Ok(());
                }
                match &state {
                    AppState::Typing => {
                        if is_early_finish(key) {
                            state = AppState::Finished(session.finish(start.elapsed()));
                            continue;
                        }
                        handle_key(&mut session, key);
                        // 打到原文末尾自动完成
                        if session.is_complete() {
                            state = AppState::Finished(session.finish(start.elapsed()));
                        }
                    }
                    AppState::Finished(_) => {
                        // 任意键重打同一篇赛文
                        session = Session::new(&text.content);
                        start = Instant::now();
                        state = AppState::Typing;
                    }
                }
            }
            Event::Paste(committed) => {
                session.type_text(&committed);
                if session.is_complete() {
                    state = AppState::Finished(session.finish(start.elapsed()));
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

fn ui(frame: &mut Frame, text: &Text, session: &Session, state: &AppState) {
    if let AppState::Finished(stats) = state {
        render_result_view(frame, text, stats);
        return;
    }
    // 整体：主区 + 底部快捷键 bar
    let [main, help_bar] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    // 主区：左侧功能栏 + 右侧内容区
    let [sidebar, content] =
        Layout::horizontal([Constraint::Length(20), Constraint::Min(0)]).areas(main);
    // 内容区：上对照区 + 下跟打区
    let [ref_area, type_area] =
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(content);

    // 左侧功能栏（T1 只显示占位，载文入口在 T4 实现）
    frame.render_widget(Block::bordered().title(" 功能栏 "), sidebar);
    frame.render_widget(
        Paragraph::new(Line::from("  载入文件（待实现）")).gray(),
        sidebar,
    );

    // 上：对照原文区
    frame.render_widget(
        Paragraph::new(text.content.as_str())
            .block(Block::bordered().title(format!(" 对照区 — {} ", text.title)))
            .wrap(Wrap { trim: false }),
        ref_area,
    );

    // 下：跟打区（实时绿/红渲染）
    frame.render_widget(
        Paragraph::new(type_line(session))
            .block(Block::bordered().title(format!(
                " 跟打区 — {}/{} 字符 ",
                session.len(),
                text.content.chars().count()
            )))
            .wrap(Wrap { trim: false }),
        type_area,
    );

    // 底部快捷键提示 bar
    frame.render_widget(
        Paragraph::new(Line::from(
            " q 退出   |   Ctrl-S 提前结束   |   重打（待实现）   |   载文（待实现）",
        )),
        help_bar,
    );
}

/// 全屏成绩视图：WPM/错字/回改/按键频率。
fn render_result_view(frame: &mut Frame, text: &Text, stats: &Stats) {
    let lines = vec![
        Line::from(format!(" 成绩 — {} ", text.title)).bold(),
        Line::from(""),
        Line::from(format!(" WPM:        {:.1}", stats.wpm)),
        Line::from(format!(
            " 正确字数:   {} / {}",
            stats.correct_chars,
            text.content.chars().count()
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
    use crossterm::event::KeyCode;

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
}
