//! 跟打会话：接收上屏文本，与原文逐字比对并维护回改记录。

/// 单个字符的比对状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharStatus {
    /// 与原文对齐，打对。
    Correct,
    /// 与原文对不齐，打错。
    Wrong,
}

/// 一次上屏文本后的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeResult {
    /// 本次上屏每个字符的对/错状态，与 committed 一一对应。
    pub statuses: Vec<CharStatus>,
    /// 本次上屏过程中发生的回改次数。
    pub edit_count: u32,
}

/// 跟打会话状态机。
///
/// 持有原文与当前已上屏的输入，通过 LCS 对齐逐字比对。
pub struct Session {
    original: Vec<char>,
    input: Vec<char>,
    edits: u32,
}

impl Session {
    /// 以赛文原文初始化跟打会话。
    pub fn new(original: &str) -> Self {
        Self {
            original: original.chars().collect(),
            input: Vec::new(),
            edits: 0,
        }
    }

    /// 上屏一段文本：追加到输入末尾，重新与原文比对，返回本次字符的对/错。
    pub fn type_text(&mut self, committed: &str) -> TypeResult {
        let chars: Vec<char> = committed.chars().collect();
        let start = self.input.len();
        self.input.extend(chars.iter().copied());
        let statuses = self.align();
        let statuses = statuses[start..].to_vec();
        TypeResult {
            statuses,
            edit_count: 0,
        }
    }

    /// 回改一次：删除最后一个已上屏字符，返回是否成功（输入非空才有效）。
    pub fn backspace(&mut self) -> bool {
        if self.input.pop().is_some() {
            self.edits += 1;
            true
        } else {
            false
        }
    }

    /// 当前跟打区的全部字符及其对/错状态（TUI 全量渲染用）。
    pub fn display(&self) -> Vec<(char, CharStatus)> {
        self.align()
            .into_iter()
            .zip(self.input.iter().copied())
            .map(|(s, c)| (c, s))
            .collect()
    }

    /// 已上屏的字符数。
    pub fn len(&self) -> usize {
        self.input.len()
    }

    /// 是否还没有任何上屏字符。
    pub fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    /// 是否已上屏完整篇原文。
    pub fn is_complete(&self) -> bool {
        self.input.len() >= self.original.len()
    }

    /// 累计回改次数。
    pub fn edit_count(&self) -> u32 {
        self.edits
    }

    /// 对 input 与 original 做 LCS 对齐，返回 input 每个字符的对/错。
    fn align(&self) -> Vec<CharStatus> {
        let m = self.input.len();
        let n = self.original.len();
        if m == 0 || n == 0 {
            return vec![CharStatus::Wrong; m];
        }

        // dp[i][j] = LCS(input[0..i], original[0..j]) 长度
        let mut dp = vec![vec![0usize; n + 1]; m + 1];
        for i in 1..=m {
            for j in 1..=n {
                dp[i][j] = if self.input[i - 1] == self.original[j - 1] {
                    dp[i - 1][j - 1] + 1
                } else {
                    dp[i - 1][j].max(dp[i][j - 1])
                };
            }
        }

        // 回溯：input 中参与 LCS 的字符为 Correct，其余为 Wrong。
        let mut statuses = vec![CharStatus::Wrong; m];
        let (mut i, mut j) = (m, n);
        while i > 0 && j > 0 {
            if self.input[i - 1] == self.original[j - 1] {
                statuses[i - 1] = CharStatus::Correct;
                i -= 1;
                j -= 1;
            } else if dp[i - 1][j] >= dp[i][j - 1] {
                i -= 1;
            } else {
                j -= 1;
            }
        }
        statuses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_correct_type_text_marks_every_char_correct() {
        let mut session = Session::new("你好世界");
        let r = session.type_text("你好世界");
        assert_eq!(
            r.statuses,
            vec![
                CharStatus::Correct,
                CharStatus::Correct,
                CharStatus::Correct,
                CharStatus::Correct
            ]
        );
        assert!(session.is_complete());
    }

    #[test]
    fn wrong_char_is_marked_wrong() {
        let mut session = Session::new("你好世界");
        // 打错一个字：把「世」打成「四」
        let r = session.type_text("你好四界");
        assert_eq!(
            r.statuses,
            vec![
                CharStatus::Correct,
                CharStatus::Correct,
                CharStatus::Wrong,
                CharStatus::Correct
            ]
        );
    }

    #[test]
    fn extra_typed_char_is_wrong_but_does_not_crash() {
        let mut session = Session::new("你好世界");
        // 多打一个字：你好呀世界
        let r = session.type_text("你好呀世界");
        // LCS 对齐后「呀」为 Wrong，其余 Correct
        assert_eq!(r.statuses.len(), 5);
        assert_eq!(r.statuses[2], CharStatus::Wrong);
        assert!(
            r.statuses
                .iter()
                .filter(|s| **s == CharStatus::Correct)
                .count()
                == 4
        );
    }

    #[test]
    fn missing_char_advances_without_crash() {
        let mut session = Session::new("你好世界");
        // 少打一个字：你好界
        let r = session.type_text("你好界");
        assert_eq!(r.statuses.len(), 3);
        assert!(r.statuses.iter().all(|s| *s == CharStatus::Correct));
    }

    #[test]
    fn backspace_counts_as_edit() {
        let mut session = Session::new("你好世界");
        session.type_text("你好");
        assert_eq!(session.edit_count(), 0);
        assert!(session.backspace());
        assert_eq!(session.edit_count(), 1);
        assert_eq!(session.len(), 1);
        // 空输入时回改无效
        assert!(session.backspace());
        assert!(!session.backspace());
    }

    #[test]
    fn display_reflects_current_statuses() {
        let mut session = Session::new("你好世界");
        session.type_text("你好四");
        let display = session.display();
        assert_eq!(display.len(), 3);
        assert_eq!(display[0], ('你', CharStatus::Correct));
        assert_eq!(display[2], ('四', CharStatus::Wrong));
    }
}
