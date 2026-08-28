//! 编码提示（遍码提示）渲染布局：将每词最优编码提示排版到正文行之上，逐词对齐。

use unicode_width::UnicodeWidthChar;

use crate::scheme::CodeHint;

/// 单个字符的可视列宽（CJK 等宽字符记 2，其余记 1）。
pub fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(1).max(1)
}

/// 字符串的可视列宽（按字符累加，CJK 记 2）。
pub fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// 将单个提示码按目标列宽（对应正文词宽）截断/居中，返回定宽字符串。
fn format_hint_cell(code: &str, target_width: usize) -> String {
    let code_width = display_width(code);
    if code_width == target_width {
        return code.to_string();
    }
    if code_width > target_width {
        // 超宽截断到目标列宽（编码多为 ASCII，按字符截断等价）。
        let mut out = String::new();
        let mut w = 0;
        for c in code.chars() {
            let cw = char_width(c);
            if w + cw > target_width {
                break;
            }
            out.push(c);
            w += cw;
        }
        return out;
    }
    // 居中补空格使总长度等于目标列宽。
    let pad = target_width - code_width;
    let left = pad / 2;
    let right = pad - left;
    format!(
        "{}{}{}",
        " ".repeat(left),
        code,
        " ".repeat(right)
    )
}

/// 内置赛文对照区双行词格：生成「提示行」字符串。
///
/// `words` 为本页正文词（与 `hints` 同序），每个提示按对应词的可视列宽截断/居中，
/// 词间以单空格分隔，使提示行与正文行（同样以单空格分词）逐词对齐。
pub fn layout_code_hint_line(words: &[String], hints: &[CodeHint]) -> String {
    words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let target = display_width(w);
            let code = hints.get(i).map(|h| h.code.as_str()).unwrap_or("");
            format_hint_cell(code, target)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheme::CodeHint;

    fn hint(code: &str) -> CodeHint {
        CodeHint {
            word: String::new(),
            code: code.to_string(),
            strokes: 0,
            is_oov: false,
        }
    }

    #[test]
    fn layout_single_char_centered() {
        // 单字「中」可视宽 2，提示 "k" 居中 → 定宽 2、右侧补 1 空格。
        let words = vec!["中".to_string()];
        let hints = vec![hint("k")];
        assert_eq!(layout_code_hint_line(&words, &hints), "k ");
    }

    #[test]
    fn layout_word_truncated_and_joined() {
        // 「中」(2) + 「中国」(4)；「中国」提示 lgyinay(7)>4 截断为 lgyi；
        // 词间单空格分隔，提示行与正文行逐词对齐。
        let words = vec!["中".to_string(), "中国".to_string()];
        let hints = vec![hint("k"), hint("lgyinay")];
        assert_eq!(layout_code_hint_line(&words, &hints), "k  lgyi");
    }

    #[test]
    fn layout_oov_is_blank_padded() {
        // 未登录词提示留空，但定宽占位（2 空格），不破坏对齐。
        let words = vec!["中".to_string()];
        let hints = vec![CodeHint {
            word: String::new(),
            code: String::new(),
            strokes: 0,
            is_oov: true,
        }];
        assert_eq!(layout_code_hint_line(&words, &hints), "  ");
    }
}
