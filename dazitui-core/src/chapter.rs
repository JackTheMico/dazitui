//! 分章赛文（小说/长篇文本）的章节解析与目录结构。

use std::path::Path;

use crate::{LoadError, LoadOptions, Text, TextSource, process_content};

/// 单个章节元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    /// 章节标题（如「第一章 师傅」）。
    pub title: String,
    /// 章节内容在全文本中的起始 byte 偏移量（不含标题行）。
    pub byte_start: usize,
    /// 章节内容在全文本中的结束 byte 偏移量。
    pub byte_end: usize,
    /// 估算的章节字符数。
    pub char_count: usize,
}

/// 整本书籍/长篇赛文的目录与元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookCatalog {
    /// 书名（从文本提取或默认为文件名）。
    pub book_title: String,
    /// 作者（若从文本提取到）。
    pub author: Option<String>,
    /// 简介（若从文本提取到）。
    pub description: Option<String>,
    /// 所有解析出的章节列表。
    pub chapters: Vec<Chapter>,
    /// 原始完整文本内容。
    pub raw_content: String,
}

impl BookCatalog {
    /// 章节总数。
    pub fn len(&self) -> usize {
        self.chapters.len()
    }

    /// 目录是否为空。
    pub fn is_empty(&self) -> bool {
        self.chapters.is_empty()
    }

    /// 获取指定章节元数据。
    pub fn chapter(&self, index: usize) -> Option<&Chapter> {
        self.chapters.get(index)
    }

    /// 获取指定章节的原始正文内容。
    pub fn chapter_raw_content(&self, index: usize) -> Option<&str> {
        let ch = self.chapters.get(index)?;
        self.raw_content.get(ch.byte_start..ch.byte_end)
    }

    /// 将指定章节转换为用于跟打的 `Text` 实例。
    pub fn chapter_text(
        &self,
        index: usize,
        _file_path: &Path,
        options: &LoadOptions,
    ) -> Result<Text, LoadError> {
        let ch = self.chapters.get(index).ok_or(LoadError::NotFound)?;
        let raw = self
            .chapter_raw_content(index)
            .ok_or(LoadError::ReadFailed)?;
        // 章节正文跟打必须去除换行与段落缩进等空白字符（与 normalize_online_content 口径一致），
        // 否则正文中的段落换行 \n 无法输入，导致后续段首文字（如“对话双引号”）整体错位变红。
        let mut chapter_options = *options;
        chapter_options.strip_whitespace = true;
        let content = process_content(raw.to_string(), &chapter_options)?;
        if content.is_empty() {
            return Err(LoadError::Empty);
        }
        Ok(Text {
            title: ch.title.clone(),
            content,
            source: TextSource::ChapteredFile {
                chapter_index: index,
            },
            word_boundaries: None,
            shuffled: false,
        })
    }
}

/// 检查一行是否为章节标题行。
pub fn is_chapter_heading(line: &str) -> bool {
    let trimmed = line.trim_matches(|c: char| c.is_whitespace() || c == '\u{3000}');
    if trimmed.is_empty() || trimmed.chars().count() > 60 {
        return false;
    }

    // 模式 1: 第[0-9一二三四五六七八九十百千万零两]+[章回卷节幕集话篇]
    if let Some(rest) = trimmed.strip_prefix('第') {
        let mut chars = rest.chars().peekable();
        let mut has_num = false;
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() || is_chinese_numeral(c) {
                has_num = true;
                chars.next();
            } else {
                break;
            }
        }
        if has_num {
            if let Some(unit) = chars.next() {
                if matches!(unit, '章' | '回' | '卷' | '节' | '幕' | '集' | '话' | '篇') {
                    // 确保后面接空格、标点或结尾，或直接是标题
                    return true;
                }
            }
        }
    }

    // 模式 2: 特殊通用章节：序、序言、楔子、引子、尾声、后记、番外
    for prefix in ["序言", "楔子", "引子", "尾声", "后记"] {
        if trimmed == prefix || trimmed.starts_with(&format!("{prefix} ")) || trimmed.starts_with(&format!("{prefix}：")) || trimmed.starts_with(&format!("{prefix}:")) {
            return true;
        }
    }
    if trimmed == "序" || trimmed.starts_with("序 ") || trimmed.starts_with("序：") || trimmed.starts_with("序:") {
        return true;
    }
    if let Some(rest) = trimmed.strip_prefix("番外") {
        if rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace() || c == ':' || c == '：' || c.is_ascii_digit() || is_chinese_numeral(c)) {
            return true;
        }
    }

    false
}

fn is_chinese_numeral(c: char) -> bool {
    matches!(
        c,
        '一' | '二' | '三' | '四' | '五' | '六' | '七' | '八' | '九' | '十' | '百' | '千' | '万' | '零' | '两'
    )
}

/// 从长文本中嗅探并解析分章赛文目录。
///
/// 若识别出的有效章节数 < 2，则返回 `None`（表示该文件不应作为分章赛文处理）。
pub fn detect_and_parse_book(content: String, file_name: &str) -> Option<BookCatalog> {
    let mut book_title: Option<String> = None;
    let mut author: Option<String> = None;
    let mut description: Option<String> = None;

    // 记录各章节的行首字节偏移与标题
    let mut headings: Vec<(usize, String)> = Vec::new();

    let mut byte_offset = 0;
    for line in content.split_inclusive('\n') {
        let line_len = line.len();
        let trimmed = line.trim_matches(|c: char| c.is_whitespace() || c == '\u{3000}');

        // 前置元信息提取（仅在尚未遇到第一章前有效）
        if headings.is_empty() {
            if let Some(val) = extract_meta(trimmed, &["书名：", "书名:"]) {
                book_title = Some(val.to_string());
            } else if let Some(val) = extract_meta(trimmed, &["作者：", "作者:"]) {
                author = Some(val.to_string());
            } else if let Some(val) = extract_meta(trimmed, &["简介：", "简介:"]) {
                description = Some(val.to_string());
            }
        }

        if is_chapter_heading(trimmed) {
            headings.push((byte_offset, trimmed.to_string()));
        }

        byte_offset += line_len;
    }

    // 至少需要 2 个章节才判定为分章赛文
    if headings.len() < 2 {
        return None;
    }

    // 默认书名
    let default_title = file_name
        .strip_suffix(".txt")
        .or_else(|| file_name.strip_suffix(".md"))
        .unwrap_or(file_name)
        .to_string();
    let final_title = book_title.unwrap_or(default_title);

    let mut chapters = Vec::new();

    // 检查第一章之前是否有实质性的前言/简介正文
    let first_heading_byte = headings[0].0;
    if first_heading_byte > 0 {
        let preamble_slice = &content[..first_heading_byte];
        // 过滤掉书名、作者等元数据行，看是否有实质正文
        let mut preamble_body = String::new();
        for line in preamble_slice.lines() {
            let t = line.trim();
            if !t.is_empty()
                && !t.starts_with("书名")
                && !t.starts_with("作者")
            {
                if let Some(desc) = t.strip_prefix("简介：").or_else(|| t.strip_prefix("简介:")) {
                    preamble_body.push_str(desc.trim());
                    preamble_body.push('\n');
                } else {
                    preamble_body.push_str(t);
                    preamble_body.push('\n');
                }
            }
        }
        let char_count = preamble_body.chars().count();
        if char_count >= 10 {
            chapters.push(Chapter {
                title: "前言与简介".to_string(),
                byte_start: 0,
                byte_end: first_heading_byte,
                char_count,
            });
        }
    }

    // 构建各章元数据
    for i in 0..headings.len() {
        let (start_byte, ref title) = headings[i];
        let end_byte = if i + 1 < headings.len() {
            headings[i + 1].0
        } else {
            content.len()
        };

        // 章节内容跳过标题行本身
        let chapter_slice = &content[start_byte..end_byte];
        let content_start = if let Some(newline_pos) = chapter_slice.find('\n') {
            start_byte + newline_pos + 1
        } else {
            start_byte
        };

        let body = &content[content_start..end_byte];
        let char_count = body.chars().filter(|c| !c.is_whitespace()).count();

        chapters.push(Chapter {
            title: title.clone(),
            byte_start: content_start,
            byte_end: end_byte,
            char_count,
        });
    }

    Some(BookCatalog {
        book_title: final_title,
        author,
        description,
        chapters,
        raw_content: content,
    })
}

fn extract_meta<'a>(line: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    for prefix in prefixes {
        if let Some(rest) = line.strip_prefix(prefix) {
            let val = rest.trim();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_chapter_heading() {
        assert!(is_chapter_heading("第一章 师傅"));
        assert!(is_chapter_heading("　　第一章 师傅"));
        assert!(is_chapter_heading("第10章 馒头"));
        assert!(is_chapter_heading("第一百二十三章 决战"));
        assert!(is_chapter_heading("第二卷 第五章 归来"));
        assert!(is_chapter_heading("楔子"));
        assert!(is_chapter_heading("序言"));
        assert!(is_chapter_heading("番外1 生日快乐"));
        assert!(is_chapter_heading("尾声"));

        assert!(!is_chapter_heading(""));
        assert!(!is_chapter_heading("他走进了房间。"));
        assert!(!is_chapter_heading("第一张桌子被搬走了"));
    }

    #[test]
    fn test_detect_and_parse_book_success() {
        let raw = "\
书名：测试小说
作者：张三
简介：这是一个测试小说。

第一章 出发
今天天气很好，李四踏上了旅程。

第二章 遇险
路途漫漫，遇到了风暴。
";
        let catalog = detect_and_parse_book(raw.to_string(), "test.txt").expect("应检测为分章赛文");
        assert_eq!(catalog.book_title, "测试小说");
        assert_eq!(catalog.author.as_deref(), Some("张三"));
        // 有简介前置正文 + 2章
        assert_eq!(catalog.chapters.len(), 3);
        assert_eq!(catalog.chapters[0].title, "前言与简介");
        assert_eq!(catalog.chapters[1].title, "第一章 出发");
        assert_eq!(catalog.chapters[2].title, "第二章 遇险");
    }

    #[test]
    fn test_detect_and_parse_book_single_chapter_returns_none() {
        let raw = "\
第一章 独木桥
只有这一章，不是多章节小说。
";
        assert!(detect_and_parse_book(raw.to_string(), "test.txt").is_none());
    }
}
