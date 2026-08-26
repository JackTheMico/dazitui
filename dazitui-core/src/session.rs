//! 跟打会话：接收上屏文本，与原文逐字比对并维护回改记录。

use std::collections::HashMap;
use std::time::Duration;

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

/// 错字类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorType {
    /// 与原文不一致的错字（typed: 输入的字符, expected: 期望的原文对应位置字符）。
    Mismatch { typed: char, expected: Option<char> },
    /// 回改（删除的字符）。
    Backspace { deleted: char },
}

/// 打错点信息（发生时间 + 当时即时 WPM + 错误类型）。
#[derive(Debug, Clone, PartialEq)]
pub struct ErrorPoint {
    /// 发生时间（相对会话开始的秒数）。
    pub time_secs: f64,
    /// 发生时刻的即时/平滑 WPM。
    pub wpm: f64,
    /// 错误类型。
    pub error_type: ErrorType,
}

/// 内部记录的打字事件。
#[derive(Debug, Clone, PartialEq)]
struct TypingEvent {
    elapsed: Duration,
    strokes: u32,
    is_correct: bool,
    error: Option<ErrorType>,
}

/// 跟打进行中的实时指标读数（用于 TUI 对照区右下角实时渲染）。
#[derive(Debug, Clone, PartialEq)]
pub struct RealtimeMetrics {
    /// 累计平均 WPM（正确字数 / 已用时分钟）。
    pub cumulative_wpm: f64,
    /// 即时平滑 WPM（近 2.0 秒滑动窗口）。
    pub rolling_wpm: f64,
    /// 累计平均击速 KPS（总击数 / 已用时秒）。
    pub cumulative_kps: f64,
    /// 即时平滑击速 KPS（近 2.0 秒滑动窗口）。
    pub rolling_kps: f64,
    /// 平均码长（总击数 / 已上屏字数）。
    pub key_length: f64,
    /// 累计总击数（并击算 1 击，含回改）。
    pub total_strokes: u32,
}

/// 跟打统计结果（完成或提前结束时计算）。
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    /// WPM：每分钟正确字数（正确字数 / 用时分钟）。
    pub wpm: f64,
    /// KPS：每秒击键数（总击数 / 用时秒）。
    pub kps: f64,
    /// 码长：总击数 / 已上屏字数。
    pub key_length: f64,
    /// 总击数（含回改，并击算一击）。
    pub total_strokes: u32,
    /// 最终比对一致的字符数（正确字数）。
    pub correct_chars: usize,
    /// 最终比对不一致的字符数（不含回改）。
    pub wrong_chars: usize,
    /// 回改次数。
    pub edits: u32,
    /// 错字 = 最终不一致字符数 + 回改次数。
    pub wrong_total: u32,
    /// 已上屏字符数。
    pub typed_chars: usize,
    /// 打词字符数（通过多字词组提交输入的字符总数，用于 52dazi 打词率统计）。
    pub phrase_chars: usize,
    /// 按键频率（按键 → 次数，按次数降序）。
    pub key_frequency: Vec<(String, u32)>,
    /// 回改明细：被删除的字符（按删除顺序）。
    pub edit_details: Vec<char>,
    /// 速度折线采样点：[(时间秒, 即时/平滑WPM)]。
    pub speed_samples: Vec<(f64, f64)>,
    /// 击速折线采样点：[(时间秒, 即时/平滑KPS)]。
    pub kps_samples: Vec<(f64, f64)>,
    /// 打错点集合（按时间排序）。
    pub error_points: Vec<ErrorPoint>,
}

/// 内置赛文组大小：每组 10 个单位（单字赛文=10 字，词组赛文=10 词）。
/// 组内可自由打/退；组边界处设门槛——当前组全对才放行进下一组。
pub const GROUP_SIZE: usize = 10;

/// 判断字符是否为 ASCII 或常用中文全角标点符号。
fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(
            c,
            '，' | '。'
                | '！'
                | '？'
                | '；'
                | '：'
                | '“'
                | '”'
                | '‘'
                | '’'
                | '（'
                | '）'
                | '【'
                | '】'
                | '《'
                | '》'
                | '、'
                | '…'
                | '—'
                | '～'
                | '·'
                | '「'
                | '」'
                | '『'
                | '』'
                | '〔'
                | '〕'
                | '〈'
                | '〉'
                | '﹏'
                | '＿'
        )
}

/// 跟打会话状态机。
///
/// 持有原文与当前已上屏的输入，通过 LCS 对齐逐字比对。
/// `completed_groups` 跟踪已全对完成的组数，
/// 用于内置赛文的组边界门槛：组内可自由打/退，但退格不可跨越已完成组边界。
/// `group_gated` 为 true 时启用组边界门槛（内置赛文），为 false 时无门槛（离线/在线赛文）。
///
/// `group_bounds` 为预计算的每组字符范围 `[(start, end), ...]`（词组赛文用）。
/// 非空时 `current_group_bounds` 按词组边界确定组的字符范围
/// （每组 GROUP_SIZE 个词）；为空时回退到单字赛文逻辑（每组 GROUP_SIZE 字）。
pub struct Session {
    original: Vec<char>,
    input: Vec<char>,
    edits: u32,
    total_strokes: u32,
    phrase_chars: usize,
    key_counts: HashMap<String, u32>,
    edit_details: Vec<char>,
    completed_groups: usize,
    group_gated: bool,
    group_bounds: Vec<(usize, usize)>,
    events: Vec<TypingEvent>,
}

impl Session {
    /// 以赛文原文初始化跟打会话（无组门槛）。
    pub fn new(original: &str) -> Self {
        Self::new_gated(original, false)
    }

    /// 以赛文原文初始化跟打会话，指定是否启用组边界门槛（内置赛文）。
    pub fn new_gated(original: &str, group_gated: bool) -> Self {
        Self::new_gated_with_words(original, group_gated, &[])
    }

    /// 以赛文原文初始化跟打会话，指定组边界门槛及词组边界。
    ///
    /// `word_boundaries` 为词组赛文每个词的 `(char_start, char_end)` 范围。
    /// 非空时按词组确定组边界（每组 `GROUP_SIZE` 个词）；为空时回退到单字逻辑。
    pub fn new_gated_with_words(
        original: &str,
        group_gated: bool,
        word_boundaries: &[(usize, usize)],
    ) -> Self {
        let group_bounds = if group_gated && !word_boundaries.is_empty() {
            // 每 GROUP_SIZE 个词合并为一组的字符范围
            word_boundaries
                .chunks(GROUP_SIZE)
                .map(|chunk| {
                    let start = chunk.first().map(|b| b.0).unwrap_or(0);
                    let end = chunk.last().map(|b| b.1).unwrap_or(0);
                    (start, end)
                })
                .collect()
        } else {
            Vec::new()
        };
        Self {
            original: original.chars().collect(),
            input: Vec::new(),
            edits: 0,
            total_strokes: 0,
            phrase_chars: 0,
            key_counts: HashMap::new(),
            edit_details: Vec::new(),
            completed_groups: 0,
            group_gated,
            group_bounds,
            events: Vec::new(),
        }
    }

    /// 获取当前总击数（并击算一击，含回改）。
    pub fn total_strokes(&self) -> u32 {
        self.total_strokes
    }

    /// 上屏一段文本：追加到输入末尾，重新与原文比对，返回本次字符的对/错。
    pub fn type_text(&mut self, committed: &str) -> TypeResult {
        self.type_text_with_strokes_at(committed, committed.chars().count() as u32, Duration::ZERO)
    }

    /// 上屏一段文本并携带相对时间戳。
    ///
    /// 组边界门槛（内置赛文）：当前组（`GROUP_SIZE` 字）全对才放行。
    /// 多字符输入跨组边界时只接受到当前组末尾，超出部分丢弃。
    pub fn type_text_at(&mut self, committed: &str, elapsed: Duration) -> TypeResult {
        self.type_text_with_strokes_at(committed, committed.chars().count() as u32, elapsed)
    }

    /// 上屏一段文本，指定本次物理击数（支持并击算一击）并携带相对时间戳。
    pub fn type_text_with_strokes_at(
        &mut self,
        committed: &str,
        strokes: u32,
        elapsed: Duration,
    ) -> TypeResult {
        let chars: Vec<char> = committed.chars().collect();
        let start = self.input.len();
        let accept_len = if self.group_gated {
            let (_, group_end) = self.current_group_bounds();
            group_end.saturating_sub(self.input.len()).min(chars.len())
        } else {
            chars.len()
        };

        if accept_len > 0 {
            let accepted_slice = &chars[..accept_len];
            let mut word_len = accepted_slice.len();
            if word_len > 1 {
                if let Some(&last_char) = accepted_slice.last() {
                    if is_punctuation(last_char) {
                        word_len -= 1;
                    }
                }
                if word_len > 1 {
                    self.phrase_chars += word_len;
                }
            }
        }

        self.input.extend(chars[..accept_len].iter().copied());

        let all_statuses = self.align();
        let statuses = all_statuses[start..].to_vec();

        let effective_strokes = if accept_len == 0 { 0 } else { strokes.max(1) };
        self.total_strokes += effective_strokes;

        for (offset, &c) in chars[..accept_len].iter().enumerate() {
            let pos = start + offset;
            let is_correct = statuses.get(offset) == Some(&CharStatus::Correct);
            let expected = self.original.get(pos).copied();
            let error = if is_correct {
                None
            } else {
                Some(ErrorType::Mismatch { typed: c, expected })
            };
            let ev_strokes = if offset == 0 { effective_strokes } else { 0 };
            self.events.push(TypingEvent {
                elapsed,
                strokes: ev_strokes,
                is_correct,
                error,
            });
        }

        // 检查当前组是否全对（仅组门槛模式）
        if self.group_gated {
            let (group_start, group_end) = self.current_group_bounds();
            if self.input.len() >= group_end && group_end > group_start {
                let all_correct =
                    (group_start..group_end).all(|i| self.input.get(i) == Some(&self.original[i]));
                if all_correct {
                    self.completed_groups += 1;
                }
            }
        }
        TypeResult {
            statuses,
            edit_count: 0,
        }
    }

    /// 回改一次：删除最后一个已上屏字符，返回是否成功。
    pub fn backspace(&mut self) -> bool {
        self.backspace_at(Duration::ZERO)
    }

    /// 回改一次并携带相对时间戳。
    ///
    /// 组边界门槛（内置赛文）：已完成组的起始位置不可回改（锁住已完成组）。
    pub fn backspace_at(&mut self, elapsed: Duration) -> bool {
        if self.group_gated {
            let (group_start, _) = self.current_group_bounds();
            if self.input.len() <= group_start {
                return false;
            }
        }
        if let Some(c) = self.input.pop() {
            self.edits += 1;
            self.total_strokes += 1;
            self.edit_details.push(c);
            self.events.push(TypingEvent {
                elapsed,
                strokes: 1,
                is_correct: false,
                error: Some(ErrorType::Backspace { deleted: c }),
            });
            true
        } else {
            false
        }
    }

    /// 记录一次按键（用于按键频率统计）。
    ///
    /// `key` 为按键的字符串表示，如 "a"、"Backspace"、"Enter"。
    pub fn record_key(&mut self, key: &str) {
        *self.key_counts.entry(key.to_string()).or_insert(0) += 1;
    }

    /// 筛选在滑动窗口 `(t_start, t]` 内的打字事件及有效时间跨度（带前 0.5s 平滑防抖）。
    fn events_in_window(&self, t: f64, window: f64) -> (impl Iterator<Item = &TypingEvent>, f64) {
        let t_start = (t - window).max(0.0);
        let dt = (t - t_start).max(0.5);
        let iter = self.events.iter().filter(move |e| {
            let s = e.elapsed.as_secs_f64();
            if t_start == 0.0 {
                s >= 0.0 && s <= t
            } else {
                s > t_start && s <= t
            }
        });
        (iter, dt)
    }

    /// 计算给定时间点 `t` 处的即时/平滑 WPM（基于 2 秒滑动窗口）。
    fn calc_rolling_wpm(&self, t: f64) -> f64 {
        if t <= 0.0 {
            return 0.0;
        }
        let (events, dt) = self.events_in_window(t, 2.0);
        let correct_count = events.filter(|e| e.is_correct).count();
        (correct_count as f64 / dt) * 60.0
    }

    /// 计算给定时间点 `t` 处的即时/平滑 KPS（基于 2 秒滑动窗口）。
    fn calc_rolling_kps(&self, t: f64) -> f64 {
        if t <= 0.0 {
            return 0.0;
        }
        let (events, dt) = self.events_in_window(t, 2.0);
        let stroke_count: u32 = events.map(|e| e.strokes).sum();
        stroke_count as f64 / dt
    }

    /// 计算时间序列速度采样点。
    fn compute_speed_samples(&self, total_secs: f64) -> Vec<(f64, f64)> {
        if total_secs <= 0.0 {
            return vec![(0.0, 0.0)];
        }
        let mut samples = Vec::new();
        samples.push((0.0, 0.0));

        let mut t = 1.0;
        while t < total_secs {
            let wpm = self.calc_rolling_wpm(t);
            samples.push((t, wpm));
            t += 1.0;
        }
        if total_secs > 0.0 {
            let last_t = samples.last().map(|s| s.0).unwrap_or(0.0);
            if (total_secs - last_t).abs() > 0.01 {
                let wpm = self.calc_rolling_wpm(total_secs);
                samples.push((total_secs, wpm));
            }
        }
        samples
    }

    /// 计算时间序列击速采样点。
    fn compute_kps_samples(&self, total_secs: f64) -> Vec<(f64, f64)> {
        if total_secs <= 0.0 {
            return vec![(0.0, 0.0)];
        }
        let mut samples = Vec::new();
        samples.push((0.0, 0.0));

        let mut t = 1.0;
        while t < total_secs {
            let kps = self.calc_rolling_kps(t);
            samples.push((t, kps));
            t += 1.0;
        }
        if total_secs > 0.0 {
            let last_t = samples.last().map(|s| s.0).unwrap_or(0.0);
            if (total_secs - last_t).abs() > 0.01 {
                let kps = self.calc_rolling_kps(total_secs);
                samples.push((total_secs, kps));
            }
        }
        samples
    }

    /// 计算打错点信息列表。
    fn compute_error_points(&self) -> Vec<ErrorPoint> {
        self.events
            .iter()
            .filter_map(|e| {
                e.error.as_ref().map(|err| {
                    let t = e.elapsed.as_secs_f64();
                    let wpm = self.calc_rolling_wpm(t);
                    ErrorPoint {
                        time_secs: t,
                        wpm,
                        error_type: err.clone(),
                    }
                })
            })
            .collect()
    }

    /// 获取当前时刻的实时复合指标（用于 TUI 对照区右下角实时渲染）。
    pub fn realtime_metrics(&self, elapsed: Duration) -> RealtimeMetrics {
        let secs = elapsed.as_secs_f64();
        let statuses = self.align();
        let correct = statuses
            .iter()
            .filter(|s| **s == CharStatus::Correct)
            .count();
        let effective_secs = secs.max(0.5);
        let cumulative_wpm = if secs <= 0.0 {
            0.0
        } else {
            (correct as f64 / effective_secs) * 60.0
        };
        let cumulative_kps = if secs <= 0.0 {
            0.0
        } else {
            self.total_strokes as f64 / effective_secs
        };
        let rolling_wpm = self.calc_rolling_wpm(secs);
        let rolling_kps = self.calc_rolling_kps(secs);
        let key_length = if self.input.is_empty() {
            0.0
        } else {
            self.total_strokes as f64 / self.input.len() as f64
        };
        RealtimeMetrics {
            cumulative_wpm,
            rolling_wpm,
            cumulative_kps,
            rolling_kps,
            key_length,
            total_strokes: self.total_strokes,
        }
    }

    /// 计算跟打统计（完成或提前结束时调用，不消耗会话）。
    pub fn finish(&self, elapsed: Duration) -> Stats {
        let statuses = self.align();
        let correct = statuses
            .iter()
            .filter(|s| **s == CharStatus::Correct)
            .count();
        let wrong = statuses.len() - correct;
        let total_secs = elapsed.as_secs_f64();
        let wpm = if total_secs <= 0.0 {
            0.0
        } else {
            correct as f64 / total_secs * 60.0
        };
        let kps = if total_secs <= 0.0 {
            0.0
        } else {
            self.total_strokes as f64 / total_secs
        };
        let key_length = if self.input.is_empty() {
            0.0
        } else {
            self.total_strokes as f64 / self.input.len() as f64
        };
        let mut key_frequency: Vec<(String, u32)> = self
            .key_counts
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        key_frequency.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let speed_samples = self.compute_speed_samples(total_secs);
        let kps_samples = self.compute_kps_samples(total_secs);
        let error_points = self.compute_error_points();
        Stats {
            wpm,
            kps,
            key_length,
            total_strokes: self.total_strokes,
            correct_chars: correct,
            wrong_chars: wrong,
            edits: self.edits,
            wrong_total: (wrong as u32) + self.edits,
            typed_chars: self.input.len(),
            phrase_chars: self.phrase_chars,
            key_frequency,
            edit_details: self.edit_details.clone(),
            speed_samples,
            kps_samples,
            error_points,
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

    /// 原文侧每个字符的跟打状态（TUI 对照区着色用）。
    ///
    /// 逐位比较：input 的第 k 个字符对应 original 的第 k 个位置；
    /// 相同 → `Some(Correct)`，不同 → `Some(Wrong)`，未被 input 覆盖 → `None`。
    pub fn original_status(&self) -> Vec<(char, Option<CharStatus>)> {
        self.original
            .iter()
            .enumerate()
            .map(|(i, &c)| {
                let status = self.input.get(i).map(|&ic| {
                    if ic == c {
                        CharStatus::Correct
                    } else {
                        CharStatus::Wrong
                    }
                });
                (c, status)
            })
            .collect()
    }

    /// 已上屏的字符数。
    pub fn len(&self) -> usize {
        self.input.len()
    }

    /// 赛文总字符数。
    pub fn original_len(&self) -> usize {
        self.original.len()
    }

    /// 是否还没有任何上屏字符。
    pub fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    /// 是否已上屏完整篇原文。
    ///
    /// 组边界门槛（内置赛文）：所有组全部全对才算完成。
    /// 词组赛文：`completed_groups >= group_bounds.len()`。
    /// 单字赛文：`completed_groups * GROUP_SIZE >= original.len()`。
    /// 非门槛模式：上屏长度 ≥ 原文长度即完成。
    pub fn is_complete(&self) -> bool {
        if self.group_gated {
            if !self.group_bounds.is_empty() {
                self.completed_groups >= self.group_bounds.len()
            } else {
                self.completed_groups * GROUP_SIZE >= self.original.len()
            }
        } else {
            self.input.len() >= self.original.len()
        }
    }

    /// 累计回改次数。
    pub fn edit_count(&self) -> u32 {
        self.edits
    }

    /// 已全对完成的组数。TUI 据此计算当前页起始。
    pub fn completed_groups(&self) -> usize {
        self.completed_groups
    }

    /// 当前组的字符范围 `[start, end)`（`end` 尾组截断到原文长度）。
    ///
    /// 词组赛文（`group_bounds` 非空）：按词组边界确定，每组 `GROUP_SIZE` 个词。
    /// 单字赛文（`group_bounds` 为空）：按字符索引，每组 `GROUP_SIZE` 字。
    fn current_group_bounds(&self) -> (usize, usize) {
        if let Some(&(s, e)) = self.group_bounds.get(self.completed_groups) {
            let end = e.min(self.original.len());
            (s, end)
        } else {
            let start = self.completed_groups * GROUP_SIZE;
            let end = (start + GROUP_SIZE).min(self.original.len());
            (start, end)
        }
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
        // 多打一个字：你好呀世界 — 非门槛模式不截断，全部接受
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

    #[test]
    fn finish_all_correct_gives_correct_wpm() {
        let mut session = Session::new("你好世界");
        session.type_text("你好世界");
        // 10 个正确字 / 60 秒 = 10 WPM
        let stats = session.finish(Duration::from_secs(60));
        assert_eq!(stats.correct_chars, 4);
        assert_eq!(stats.wrong_chars, 0);
        assert_eq!(stats.wrong_total, 0);
        assert_eq!(stats.typed_chars, 4);
        assert!(
            (stats.wpm - 4.0).abs() < 1e-9,
            "wpm 应为 4.0，得到 {}",
            stats.wpm
        );
    }

    #[test]
    fn finish_wrong_plus_edits_is_wrong_total() {
        let mut session = Session::new("你好世界");
        session.type_text("你好四"); // 「四」错
        session.backspace(); // 回改一次
        session.type_text("世");
        session.type_text("界");
        let stats = session.finish(Duration::from_secs(60));
        // 最终不一致 0（都改对了），回改 1 → 错字 1
        assert_eq!(stats.wrong_chars, 0);
        assert_eq!(stats.edits, 1);
        assert_eq!(stats.wrong_total, 1);
        assert_eq!(stats.edit_details, vec!['四']);
    }

    #[test]
    fn finish_wpm_zero_when_no_elapsed_time() {
        let mut session = Session::new("你好世界");
        session.type_text("你好");
        let stats = session.finish(Duration::ZERO);
        assert_eq!(stats.wpm, 0.0);
    }

    #[test]
    fn original_status_none_when_nothing_typed() {
        let session = Session::new("你好世界");
        let statuses = session.original_status();
        assert_eq!(statuses.len(), 4);
        assert!(statuses.iter().all(|(_, s)| s.is_none()));
    }

    #[test]
    fn original_status_correct_for_matching_prefix() {
        let mut session = Session::new("你好世界");
        session.type_text("你好");
        let statuses = session.original_status();
        assert_eq!(statuses[0], ('你', Some(CharStatus::Correct)));
        assert_eq!(statuses[1], ('好', Some(CharStatus::Correct)));
        assert_eq!(statuses[2], ('世', None));
        assert_eq!(statuses[3], ('界', None));
    }

    #[test]
    fn original_status_wrong_at_mismatched_position() {
        let mut session = Session::new("你好世界");
        session.type_text("你好四");
        let statuses = session.original_status();
        assert_eq!(statuses[0], ('你', Some(CharStatus::Correct)));
        assert_eq!(statuses[1], ('好', Some(CharStatus::Correct)));
        assert_eq!(statuses[2], ('世', Some(CharStatus::Wrong)));
        assert_eq!(statuses[3], ('界', None));
    }

    #[test]
    fn original_status_ignores_extra_input_chars() {
        let mut session = Session::new("你好世界");
        session.type_text("你好世界四"); // 多打一个
        let statuses = session.original_status();
        assert!(
            statuses
                .iter()
                .all(|(_, s)| *s == Some(CharStatus::Correct))
        );
        assert_eq!(statuses.len(), 4);
    }

    #[test]
    fn original_status_after_backspace_reverts() {
        let mut session = Session::new("你好世界");
        session.type_text("你好四");
        session.backspace();
        let statuses = session.original_status();
        assert_eq!(statuses[2], ('世', None)); // 回改后该位置回到未打到
        assert_eq!(statuses[0], ('你', Some(CharStatus::Correct)));
    }

    #[test]
    fn key_frequency_reconstructed_sorted() {
        let mut session = Session::new("你好世界");
        session.record_key("n");
        session.record_key("i");
        session.record_key("n");
        session.record_key("Backspace");
        session.record_key("n");
        let stats = session.finish(Duration::from_secs(60));
        let freq: Vec<(String, u32)> = stats.key_frequency;
        assert_eq!(freq[0], ("n".to_string(), 3));
        assert_eq!(freq[1], ("Backspace".to_string(), 1));
        assert_eq!(freq[2], ("i".to_string(), 1));
        // 按次数降序：n(3) > Backspace(1) = i(1)，同次数按键名升序
        assert_eq!(freq.len(), 3);
    }

    #[test]
    fn completed_groups_starts_at_zero() {
        let session = Session::new_gated("一二三四五六七八九十甲乙丙", true);
        assert_eq!(session.completed_groups(), 0);
    }

    #[test]
    fn type_text_advances_group_when_all_correct() {
        // 13 字赛文：第一组 10 字全对 → completed_groups 推进到 1
        let mut session = Session::new_gated("一二三四五六七八九十甲乙丙", true);
        session.type_text("一二三四五六七八九十");
        assert_eq!(session.completed_groups(), 1, "第一组 10 字全对应推进到 1");
        assert!(!session.is_complete(), "尾组未打完不应判定完成");
    }

    #[test]
    fn type_text_does_not_advance_when_wrong() {
        let mut session = Session::new_gated("一二三四五六七八九十甲乙丙", true);
        // 第 10 字打错 → 组未全对 → completed_groups 不推进
        session.type_text("一二三四五六七八九X");
        assert_eq!(session.completed_groups(), 0, "组内有错字不应推进");
        assert!(!session.is_complete());
    }

    #[test]
    fn type_text_advances_after_correcting_wrong_in_group() {
        // 组内打错 → 回改 → 改对 → 推进
        let mut session = Session::new_gated("一二三四五六七八九十甲乙丙", true);
        session.type_text("一二三四五六七八九X");
        assert_eq!(session.completed_groups(), 0);
        session.backspace(); // 删掉 'X'
        session.type_text("十"); // 改对
        assert_eq!(session.completed_groups(), 1, "改对后应推进");
    }

    #[test]
    fn backspace_locked_at_group_boundary() {
        let mut session = Session::new_gated("一二三四五六七八九十甲乙丙", true);
        session.type_text("一二三四五六七八九十");
        assert_eq!(session.completed_groups(), 1);
        // 已完成组的起始位置不可退格
        assert!(!session.backspace(), "组边界处退格应返回 false");
        assert_eq!(session.len(), 10, "退格失败不应改变 input 长度");
    }

    #[test]
    fn backspace_works_within_group() {
        let mut session = Session::new_gated("一二三四五六七八九十甲乙丙", true);
        session.type_text("一二三");
        assert!(session.backspace(), "组内退格应成功");
        assert_eq!(session.len(), 2);
    }

    #[test]
    fn type_text_truncates_at_group_boundary() {
        // 赛文 13 字，第一组 10 字。一次上屏 12 字 → 只接受 10 字
        let mut session = Session::new_gated("一二三四五六七八九十甲乙丙", true);
        let r = session.type_text("一二三四五六七八九十甲乙");
        assert_eq!(session.len(), 10, "跨组边界应截断到 10 字");
        assert_eq!(r.statuses.len(), 10, "返回的状态应只含接受的 10 字");
        assert_eq!(session.completed_groups(), 1, "第一组全对应推进");
    }

    #[test]
    fn is_complete_requires_all_groups_correct() {
        // 13 字赛文：第一组 10 字 + 尾组 3 字，尾组全对才算完成
        let mut session = Session::new_gated("一二三四五六七八九十甲乙丙", true);
        session.type_text("一二三四五六七八九十");
        assert!(!session.is_complete(), "尾组未打不应完成");
        session.type_text("甲乙丙");
        assert!(session.is_complete(), "尾组全对应判定完成");
        assert_eq!(session.completed_groups(), 2);
    }

    #[test]
    fn is_complete_false_when_group_has_wrong() {
        let mut session = Session::new_gated("一二三四五六七八九十甲乙丙", true);
        session.type_text("一二三四五六七八九十");
        session.type_text("甲乙X"); // 尾组有错字
        assert!(!session.is_complete(), "尾组有错字不应完成");
        assert_eq!(session.completed_groups(), 1);
    }

    // ---- Issue #39 时序速度采样与打错事件记录测试 ----

    #[test]
    fn time_series_speed_samples_reflects_burst_and_pause() {
        let mut session = Session::new("一二三四五六七八九十");
        // 第 1 秒打了 2 个字（正确）
        session.type_text_at("一二", Duration::from_secs(1));
        // 第 2 秒打了 2 个字（正确）
        session.type_text_at("三四", Duration::from_secs(2));
        // 第 3-4 秒暂停（无输入）
        // 第 5 秒打了 2 个字（正确）
        session.type_text_at("五六", Duration::from_secs(5));

        let stats = session.finish(Duration::from_secs(5));
        assert!(!stats.speed_samples.is_empty());
        assert_eq!(stats.speed_samples[0], (0.0, 0.0));

        // 第 1 秒样本
        let s1 = stats
            .speed_samples
            .iter()
            .find(|(t, _)| (*t - 1.0).abs() < 1e-3)
            .unwrap();
        // 2 字 / 1s * 60 = 120 WPM
        assert!((s1.1 - 120.0).abs() < 1.0);

        // 第 4 秒样本（暂停阶段：在 [2.0, 4.0] 窗口内只有 2.0s 时的输入，窗口内无新字增量）
        let s4 = stats
            .speed_samples
            .iter()
            .find(|(t, _)| (*t - 4.0).abs() < 1e-3)
            .unwrap();
        assert_eq!(s4.1, 0.0);
    }

    #[test]
    fn error_points_records_mismatches_and_backspaces() {
        let mut session = Session::new("你好世界");
        // 1.0s: 打对「你」
        session.type_text_at("你", Duration::from_secs_f64(1.0));
        // 2.0s: 打错「四」（期望「好」）
        session.type_text_at("四", Duration::from_secs_f64(2.0));
        // 2.5s: 回改删掉「四」
        session.backspace_at(Duration::from_secs_f64(2.5));
        // 3.0s: 改对「好」
        session.type_text_at("好", Duration::from_secs_f64(3.0));

        let stats = session.finish(Duration::from_secs_f64(3.0));
        assert_eq!(stats.error_points.len(), 2);

        // 错误点 1：Mismatch
        let ep1 = &stats.error_points[0];
        assert!((ep1.time_secs - 2.0).abs() < 1e-3);
        assert_eq!(
            ep1.error_type,
            ErrorType::Mismatch {
                typed: '四',
                expected: Some('好'),
            }
        );

        // 错误点 2：Backspace
        let ep2 = &stats.error_points[1];
        assert!((ep2.time_secs - 2.5).abs() < 1e-3);
        assert_eq!(ep2.error_type, ErrorType::Backspace { deleted: '四' });
    }

    #[test]
    fn finish_empty_session_has_default_speed_samples_and_error_points() {
        let session = Session::new("你好");
        let stats = session.finish(Duration::ZERO);
        assert_eq!(stats.speed_samples, vec![(0.0, 0.0)]);
        assert_eq!(stats.kps_samples, vec![(0.0, 0.0)]);
        assert_eq!(stats.kps, 0.0);
        assert_eq!(stats.key_length, 0.0);
        assert_eq!(stats.total_strokes, 0);
        assert!(stats.error_points.is_empty());
    }

    #[test]
    fn strokes_and_chording_kps_calculation() {
        let mut session = Session::new("到是王");
        // 「到」：并击 1 击，用时 1.0s
        session.type_text_with_strokes_at("到", 1, Duration::from_secs_f64(1.0));
        assert_eq!(session.total_strokes(), 1);

        // 「是」：三码 3 击，用时 2.0s
        session.type_text_with_strokes_at("是", 3, Duration::from_secs_f64(2.0));
        assert_eq!(session.total_strokes(), 4);

        // 回改 1 击（删「是」），用时 2.5s
        session.backspace_at(Duration::from_secs_f64(2.5));
        assert_eq!(session.total_strokes(), 5);

        // 重新打「是」3 击，用时 3.0s
        session.type_text_with_strokes_at("是", 3, Duration::from_secs_f64(3.0));
        assert_eq!(session.total_strokes(), 8);

        // 打「王」4 击，用时 4.0s
        session.type_text_with_strokes_at("王", 4, Duration::from_secs_f64(4.0));
        assert_eq!(session.total_strokes(), 12);

        let stats = session.finish(Duration::from_secs_f64(4.0));
        assert_eq!(stats.total_strokes, 12);
        // KPS = 12 击 / 4s = 3.0
        assert_eq!(stats.kps, 3.0);
        // 码长 = 12 击 / 3 字 = 4.0
        assert_eq!(stats.key_length, 4.0);
        assert_eq!(stats.correct_chars, 3);
        assert_eq!(stats.edits, 1);
        assert!(!stats.kps_samples.is_empty());
    }

    #[test]
    fn realtime_metrics_cumulative_and_rolling() {
        let mut session = Session::new("一二三四五六");
        // 0.0s 初始状态
        let m0 = session.realtime_metrics(Duration::ZERO);
        assert_eq!(m0.cumulative_wpm, 0.0);
        assert_eq!(m0.cumulative_kps, 0.0);
        assert_eq!(m0.rolling_wpm, 0.0);
        assert_eq!(m0.rolling_kps, 0.0);

        // 1.0s: 打 2 字（2 击）
        session.type_text_with_strokes_at("一二", 2, Duration::from_secs_f64(1.0));
        let m1 = session.realtime_metrics(Duration::from_secs_f64(1.0));
        assert_eq!(m1.cumulative_wpm, 120.0); // 2 字 / 1s * 60 = 120
        assert_eq!(m1.cumulative_kps, 2.0);   // 2 击 / 1s = 2.0
        assert_eq!(m1.key_length, 1.0);       // 2 击 / 2 字 = 1.0

        // 2.0s: 打 2 字（2 击）
        session.type_text_with_strokes_at("三四", 2, Duration::from_secs_f64(2.0));
        let m2 = session.realtime_metrics(Duration::from_secs_f64(2.0));
        assert_eq!(m2.cumulative_wpm, 120.0); // 4 字 / 2s * 60 = 120
        assert_eq!(m2.cumulative_kps, 2.0);   // 4 击 / 2s = 2.0
    }

    #[test]
    fn session_tracks_phrase_chars_on_word_commits() {
        let mut session = Session::new("我们一起打字推练习。");
        // 词组「我们」（2字）
        session.type_text("我们");
        // 单字「一」
        session.type_text("一");
        // 单字「起」
        session.type_text("起");
        // 词组「打字推」（3字）
        session.type_text("打字推");
        // 词组带标点「练习。」（2字中文 + 1标点，去除末尾标点后为 2 字词）
        session.type_text("练习。");

        let stats = session.finish(Duration::from_secs(5));
        // 打词字符总数 = 2(我们) + 0(一) + 0(起) + 3(打字推) + 2(练习) = 7
        assert_eq!(stats.phrase_chars, 7);
    }
}


