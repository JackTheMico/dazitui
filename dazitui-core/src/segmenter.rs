//! 词汇分词与字符到词索引模块 (Word Segmentation & Character-to-Word Index)
//!
//! 在载文时对赛文建立每个字符位置到词汇词条的映射 (CharIndex -> WordToken)。
//! 支持内置词组赛文原生词边界与通用/在线文章 Jieba 分词索引。

use std::sync::LazyLock;
use jieba_rs::Jieba;

static JIEBA: LazyLock<Jieba> = LazyLock::new(Jieba::new);

/// 赛文中单词/词组的切分单元。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordToken {
    /// 词文本内容 (例如 "发展", "打字推")
    pub word: String,
    /// 字符起始下标 (包含)
    pub start_char_idx: usize,
    /// 字符结束下标 (不包含)
    pub end_char_idx: usize,
}

/// 赛文字符到词的倒排索引。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WordIndex {
    /// 每个字符下标对应的词单元 (长度等于文本字符总数)
    char_to_token: Vec<Option<WordToken>>,
}

impl WordIndex {
    /// 从文本内容构建分词索引。
    ///
    /// - 若 `is_builtin_words` 为真或文本中含有空格分隔的词，优先根据空格/换行拆分原生词；
    /// - 否则使用 Jieba 分词构建词汇索引。
    pub fn build(text: &str, is_builtin_words: bool) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let total_chars = chars.len();
        let mut char_to_token: Vec<Option<WordToken>> = vec![None; total_chars];

        if is_builtin_words {
            let mut word_start = None;
            for (idx, &ch) in chars.iter().enumerate() {
                if ch.is_whitespace() {
                    if let Some(start) = word_start {
                        let word: String = chars[start..idx].iter().collect();
                        let token = WordToken {
                            word,
                            start_char_idx: start,
                            end_char_idx: idx,
                        };
                        for slot in &mut char_to_token[start..idx] {
                            *slot = Some(token.clone());
                        }
                        word_start = None;
                    }
                } else if word_start.is_none() {
                    word_start = Some(idx);
                }
            }
            if let Some(start) = word_start {
                let word: String = chars[start..total_chars].iter().collect();
                let token = WordToken {
                    word,
                    start_char_idx: start,
                    end_char_idx: total_chars,
                };
                for slot in &mut char_to_token[start..total_chars] {
                    *slot = Some(token.clone());
                }
            }
        } else {
            for tag in JIEBA.tokenize(text, jieba_rs::TokenizeMode::Default, true) {
                let start_char = tag.start;
                let end_char = tag.end;

                let word = tag.word.trim();
                if !word.is_empty() && word.chars().count() > 1 {
                    let token = WordToken {
                        word: word.to_string(),
                        start_char_idx: start_char,
                        end_char_idx: end_char,
                    };
                    let end_bound = end_char.min(total_chars);
                    if start_char < end_bound {
                        for slot in &mut char_to_token[start_char..end_bound] {
                            *slot = Some(token.clone());
                        }
                    }
                }
            }
        }

        Self { char_to_token }
    }

    /// 根据发生错误时的字符下标，获取所归因的错词（若无归属词则返回 None）。
    pub fn get_word_at(&self, char_idx: usize) -> Option<&str> {
        self.char_to_token
            .get(char_idx)
            .and_then(|opt| opt.as_ref())
            .map(|t| t.word.as_str())
    }

    /// 尝试根据包含的字符查找所属词汇（作为回退启发式）。
    pub fn find_word_containing_char(&self, ch: char) -> Option<&str> {
        for token in self.char_to_token.iter().flatten() {
            if token.word.contains(ch) {
                return Some(&token.word);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_words_indexing() {
        let text = "打字 练习 极速 推进";
        let index = WordIndex::build(text, true);

        // "打字" -> chars 0..2
        assert_eq!(index.get_word_at(0), Some("打字"));
        assert_eq!(index.get_word_at(1), Some("打字"));
        assert_eq!(index.get_word_at(2), None); // space

        // "练习" -> chars 3..5
        assert_eq!(index.get_word_at(3), Some("练习"));
        assert_eq!(index.get_word_at(4), Some("练习"));
    }

    #[test]
    fn test_jieba_chinese_segmentation_indexing() {
        let text = "中国共产党领导人民不断创造历史伟业。";
        let index = WordIndex::build(text, false);

        // 验证分词归因
        let word_at_0 = index.get_word_at(0);
        assert!(word_at_0.is_some());
        let w = word_at_0.unwrap();
        assert!(w.contains("中国") || w.contains("共产党"));
        let word_at_14 = index.get_word_at(14); // "历史"
        assert_eq!(word_at_14, Some("历史"));
        let word_at_15 = index.get_word_at(15); // "伟业"
        assert_eq!(word_at_15, Some("伟业"));
    }
}
