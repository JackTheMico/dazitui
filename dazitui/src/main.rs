use std::io;
use std::path::Path;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use dazitui_core::{load_text_from_file, LoadError, Text};
use ratatui::layout::{Constraint, Layout};
use ratatui::prelude::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;

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
    let result = event_loop(&mut terminal, text);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    text: &Text,
) -> io::Result<()> {
    loop {
        terminal.draw(|frame| ui(frame, text))?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && is_quit(key)
        {
            return Ok(());
        }
    }
}

/// 退出快捷键：q / Q / Ctrl-C。
fn is_quit(key: KeyEvent) -> bool {
    let is_ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
        && key.code == KeyCode::Char('c');
    key.code == KeyCode::Char('q') || key.code == KeyCode::Char('Q') || is_ctrl_c
}

fn ui(frame: &mut Frame, text: &Text) {
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

    // 下：跟打区
    frame.render_widget(
        Paragraph::new(Line::from("（跟打区 — 输入法上屏文字将显示在这里）"))
            .block(Block::bordered().title(" 跟打区 "))
            .gray(),
        type_area,
    );

    // 底部快捷键提示 bar
    frame.render_widget(
        Paragraph::new(Line::from(" q 退出   |   重打（待实现）   |   载文（待实现）   |   功能栏（待实现）")),
        help_bar,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    #[test]
    fn q_quits() {
        assert!(is_quit(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(is_quit(KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::NONE)));
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
        assert!(!is_quit(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)));
        assert!(!is_quit(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    }
}
