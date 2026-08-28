//! 编码提示（遍码提示）渲染布局：将每词最优编码提示排版到正文行之上，逐词对齐。

use unicode_width::UnicodeWidthChar;

use crate::scheme::CodeHint;

/// 简码/并击码的手区归属，用于提示区配色（左手粉、右手黄、双手并击青）。
///
/// 由编码的前导手区修饰符推断：`_` 左手、`+` 右手、`-` 其它；无前缀（双手并击或普通码）为
/// `TwoHand`；`None` 仅用于已打/缺失提示（留空占位，不显色）。
/// 该枚举不依赖 ratatui，颜色映射由渲染层（`dazitui`）据此施加。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintHand {
    Left,
    Right,
    Other,
    /// 无前缀的双手并击/普通全码（原 muted 灰，现单独配色）。
    TwoHand,
    /// 已全对上屏或缺失提示的留空占位（不显色）。
    None,
}

/// 提示区一个词单元的渲染单元：已按词宽截断/居中/留空的可视文本，与其手区归属（用于配色）。
pub struct HintCell {
    pub text: String,
    pub hand: HintHand,
}

/// 由编码推断其手区归属（用于提示区左右手/双手并击配色）。
pub fn hand_of_code(code: &str) -> HintHand {
    if code.starts_with('_') {
        HintHand::Left
    } else if code.starts_with('+') {
        HintHand::Right
    } else if code.starts_with('-') {
        HintHand::Other
    } else {
        HintHand::TwoHand
    }
}

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

/// 去掉编码的手区修饰符前缀（`_` 左手 / `+` 右手 / `-` 其它），仅保留实际按键。
///
/// 简码（单手派生形式）与并击规范形式均可能带此前缀，提示区只展示用户真正要按的键。
fn strip_hand_prefix(code: &str) -> &str {
    code.strip_prefix(['_', '+', '-']).unwrap_or(code)
}

/// 内置赛文对照区双行词格：生成本页「提示行」的逐词渲染单元。
///
/// `words` 为本页正文词（与 `hints` 同序），每个提示按对应词的可视列宽截断/居中，
/// 词间以单空格分隔，使提示行与正文行（同样以单空格分词）逐词对齐。提示码会先去掉
/// 手区修饰符前缀（如单手简码 `_b` → `b`），仅显示实际按键；其手区归属（`HintHand`）
/// 一并返回，交由渲染层据左右手上色（左手粉、右手黄）。
///
/// `typed_mask[i]` 为真表示该词已全部正确上屏，其提示留空（仍按词宽占位，不影响对齐）。
pub fn layout_code_hint_line(
    words: &[String],
    hints: &[CodeHint],
    typed_mask: &[bool],
) -> Vec<HintCell> {
    words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let target = display_width(w);
            let typed = typed_mask.get(i).copied().unwrap_or(false);
            if typed {
                return HintCell {
                    text: " ".repeat(target),
                    hand: HintHand::None,
                };
            }
            match hints.get(i) {
                Some(h) => {
                    let hand = hand_of_code(&h.code);
                    let code = strip_hand_prefix(&h.code);
                    HintCell {
                        text: format_hint_cell(code, target),
                        hand,
                    }
                }
                None => HintCell {
                    text: " ".repeat(target),
                    hand: HintHand::None,
                },
            }
        })
        .collect()
}

/// 贪心按词宽打包：返回若干「行」，每行包含若干词的索引，
/// 使该行（词宽之和 + 词间单空格分隔）不超过 `max_width`。
///
/// 若单个词宽已超过 `max_width`，则独占一行（由上层按原宽溢出渲染），
/// 保证永不分词错位、也不会产生空行或无限循环。
pub fn pack_words_by_width(word_widths: &[usize], max_width: usize) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_w = 0usize;
    for (i, &w) in word_widths.iter().enumerate() {
        let sep = if cur_w == 0 { 0 } else { 1 };
        if cur_w > 0 && cur_w + sep + w > max_width {
            rows.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        if cur_w > 0 {
            cur_w += 1; // 词间分隔空格
        }
        cur.push(i);
        cur_w += w;
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    rows
}

/// 双行词格（长文）：将提示行与正文行按词边界锁步折行，返回逐行 `(提示单元, 正文行)`。
///
/// 每词提示按对应词宽截断/居中，词间单空格分隔；正文行为对应词原文（同样单空格分隔）。
/// 两者列结构完全一致，故每行提示与其下方正文逐词对齐、永不错位。提示单元携带手区归属，
/// 交由渲染层上色（左手粉、右手黄）。`typed_mask[i]` 为真表示该词已全部正确上屏，
/// 其提示单元留空但仍占位。
///
/// 与 `layout_code_hint_line`（内置分页单页单行）不同，本函数面向非内置长文：
/// 以 `WordIndex` 词边界为最小换行单元打包，使提示行与正文行折行点锁步。
pub fn layout_code_hint_grid(
    words: &[String],
    hints: &[CodeHint],
    typed_mask: &[bool],
    max_width: usize,
) -> Vec<(Vec<HintCell>, String)> {
    let widths: Vec<usize> = words.iter().map(|w| display_width(w)).collect();
    let rows = pack_words_by_width(&widths, max_width);
    rows.into_iter()
        .map(|row| {
            let row_words: Vec<String> = row.iter().map(|&i| words[i].clone()).collect();
            let row_hints: Vec<CodeHint> = row
                .iter()
                .map(|&i| {
                    hints
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| CodeHint {
                            word: String::new(),
                            code: String::new(),
                            strokes: 0,
                            is_oov: true,
                        })
                })
                .collect();
            let row_typed: Vec<bool> = row
                .iter()
                .map(|&i| typed_mask.get(i).copied().unwrap_or(false))
                .collect();
            let hint_cells = layout_code_hint_line(&row_words, &row_hints, &row_typed);
            let body_line = row_words.join(" ");
            (hint_cells, body_line)
        })
        .collect()
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

    /// 将提示单元拼回单行文本（词间单空格），便于与既有对齐预期比较。
    fn cells_text(cells: &[HintCell]) -> String {
        cells
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn layout_single_char_centered() {
        // 单字「中」可视宽 2，提示 "k" 居中 → 定宽 2、右侧补 1 空格。
        let words = vec!["中".to_string()];
        let hints = vec![hint("k")];
        assert_eq!(cells_text(&layout_code_hint_line(&words, &hints, &[])), "k ");
    }

    #[test]
    fn layout_word_truncated_and_joined() {
        // 「中」(2) + 「中国」(4)；「中国」提示 lgyinay(7)>4 截断为 lgyi；
        // 词间单空格分隔，提示行与正文行逐词对齐。
        let words = vec!["中".to_string(), "中国".to_string()];
        let hints = vec![hint("k"), hint("lgyinay")];
        assert_eq!(
            cells_text(&layout_code_hint_line(&words, &hints, &[])),
            "k  lgyi"
        );
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
        assert_eq!(cells_text(&layout_code_hint_line(&words, &hints, &[])), "  ");
    }

    #[test]
    fn layout_typed_word_is_blank_but_aligned() {
        // T04：已全对上屏的词，其上方提示留空（按词宽占位，手区为 None）。
        // 「中」(2) 已打 → 2 空格；词间单空格分隔符；「中国」(4) 未打 → 显示 lgyi。
        // 故提示行 = "  " + " " + "lgyi" = "   lgyi"（3 前导空格）。
        let words = vec!["中".to_string(), "中国".to_string()];
        let hints = vec![hint("k"), hint("lgyinay")];
        let typed_mask = vec![true, false];
        let cells = layout_code_hint_line(&words, &hints, &typed_mask);
        assert_eq!(cells_text(&cells), "   lgyi");
        assert_eq!(cells[0].hand, HintHand::None);
    }

    #[test]
    fn layout_overwide_chord_truncated_to_word_width() {
        // T05：超宽并击码（如 wCsA，4 字符）在单字（CJK 宽 2）提示区内按词宽截断为 2 列。
        let words = vec!["中".to_string()];
        let hints = vec![hint("wCsA")];
        assert_eq!(
            cells_text(&layout_code_hint_line(&words, &hints, &[])),
            "wC"
        );
    }

    #[test]
    fn layout_jianma_strips_hand_prefix_for_display() {
        // 简码（单手派生形式）带手区修饰符 _/+/-；提示区只显示实际按键，去掉前缀，
        // 并据手区归属（左手 Left / 右手 Right / 无 None）携出供渲染层上色。
        // 「中」(宽2) 简码 _b → 去掉前缀 "b"，居中宽2（奇宽补右侧空格）→ "b "，手区 Left。
        let words = vec!["中".to_string()];
        let hints = vec![hint("_b")];
        let cells = layout_code_hint_line(&words, &hints, &[]);
        assert_eq!(cells_text(&cells), "b ");
        assert_eq!(cells[0].hand, HintHand::Left);
        // 右手简码 +e 同样去掉前缀 → "e "，手区 Right。
        let hints2 = vec![hint("+e")];
        let cells2 = layout_code_hint_line(&words, &hints2, &[]);
        assert_eq!(cells_text(&cells2), "e ");
        assert_eq!(cells2[0].hand, HintHand::Right);
        // 无前缀并击码 wCs 不受影响（超宽截断为 2 列），手区 TwoHand（单独配色）。
        let hints3 = vec![hint("wCs")];
        let cells3 = layout_code_hint_line(&words, &hints3, &[]);
        assert_eq!(cells_text(&cells3), "wC");
        assert_eq!(cells3[0].hand, HintHand::TwoHand);
    }

    #[test]
    fn layout_chord_centered_in_wide_word_width() {
        // T05：3 码并击 wCs 在双字（CJK 宽 4）提示区内居中为 "wCs "（右补 1 空格）。
        let words = vec!["世界".to_string()];
        let hints = vec![hint("wCs")];
        assert_eq!(
            cells_text(&layout_code_hint_line(&words, &hints, &[])),
            "wCs "
        );
    }

    // ---- T06 长文折行对齐 ----

    /// 断言提示行与正文行逐词锁步对齐：整行列宽一致，且提示行在各词位置处恰为对应词宽的单元。
    ///
    /// 提示单元可能内含居中/留空空格，故不能简单按空格切分；改为由正文行（仅词间有单空格）
    /// 推断各词宽，再校验提示行在同样分隔下逐单元列宽一致、总宽相等。
    fn assert_lockstep_aligned(rows: &[(Vec<HintCell>, String)]) {
        for (cells, b) in rows {
            let h = cells_text(cells);
            assert_eq!(
                display_width(&h),
                display_width(b),
                "row width mismatch hint={:?} body={:?}",
                h,
                b
            );
            let body_words: Vec<&str> = b.split(' ').filter(|w| !w.is_empty()).collect();
            let mut pos = 0usize;
            for w in body_words {
                let ww = display_width(w);
                // 提示行在 [pos, pos+ww) 必须是宽为 ww 的单元（居中/留空/截断后均恰为 ww）。
                assert!(
                    display_width(&h) >= pos + ww,
                    "hint too short vs body at pos {}: hint={:?} body={:?}",
                    pos,
                    h,
                    b
                );
                pos += ww + 1; // 跳过词间单空格分隔
            }
        }
    }

    #[test]
    fn pack_words_by_width_groups_within_max() {
        // 词宽 [4,4,4]，max=10：前两词同处一行（4+1+4=9 ≤ 10），第三词另起一行。
        let widths = vec![4usize, 4, 4];
        let rows = pack_words_by_width(&widths, 10);
        assert_eq!(rows, vec![vec![0, 1], vec![2]]);
    }

    #[test]
    fn pack_words_by_width_oversized_word_own_row() {
        // 单文宽 14 远超 max=5：独占一行（避免空行/无限循环），由上层按原宽溢出渲染。
        let widths = vec![14usize, 2, 2];
        let rows = pack_words_by_width(&widths, 5);
        assert_eq!(rows, vec![vec![0], vec![1, 2]]);
    }

    #[test]
    fn layout_code_hint_grid_narrow_keeps_alignment() {
        // 窄宽度 + 长文：6 个双字词（各宽 4），max=9 → 每行 2 词、共 3 行；
        // 每行提示与正文折行点锁步、逐词对齐。
        let words = vec![
            "中国".to_string(),
            "发展".to_string(),
            "人民".to_string(),
            "社会".to_string(),
            "主义".to_string(),
            "制度".to_string(),
        ];
        let hints = vec![
            hint("zk"),
            hint("vzoi"),
            hint("wfaa"),
            hint("pwwi"),
            hint("uyit"),
            hint("sira"),
        ];
        let rows = layout_code_hint_grid(&words, &hints, &[], 9);
        assert_eq!(rows.len(), 3, "expected 3 wrapped rows, got {}", rows.len());
        assert_lockstep_aligned(&rows);
        // 首行应包含前两词（"中国"+"发展"），提示码按词宽居中/原样。
        assert_eq!(rows[0].1, "中国 发展");
        assert_eq!(cells_text(&rows[0].0), " zk  vzoi"); // "zk"居中宽4→" zk "；"vzoi"恰为宽4原样
    }

    #[test]
    fn layout_code_hint_grid_typed_word_blank_but_aligned() {
        // 已全对上屏的词提示留空（仍按词宽占位），折行后仍与其下方字词对齐。
        let words = vec!["中".to_string(), "中国".to_string()];
        let hints = vec![hint("k"), hint("lgyinay")];
        let typed = vec![true, false];
        let rows = layout_code_hint_grid(&words, &hints, &typed, 10);
        assert_eq!(rows.len(), 1);
        assert_lockstep_aligned(&rows);
        assert_eq!(rows[0].1, "中 中国");
        assert_eq!(cells_text(&rows[0].0), "   lgyi"); // 「中」(2) 留空→2空格 + 词间1空格 = 3 前导空格；「中国」(4) 显示 lgyi
    }

    #[test]
    fn layout_code_hint_grid_oversized_word_still_aligned() {
        // 超长单字（宽 14）超过 max：独占一行，提示码截断到词宽、仍与正文对齐。
        let words = vec!["中华人民共和国".to_string()];
        let hints = vec![hint("abc")];
        let rows = layout_code_hint_grid(&words, &hints, &[], 5);
        assert_eq!(rows.len(), 1);
        assert_lockstep_aligned(&rows);
        assert_eq!(rows[0].1, "中华人民共和国");
        assert_eq!(display_width(&cells_text(&rows[0].0)), 14);
    }

    #[test]
    fn layout_code_hint_grid_carries_hand_for_color() {
        // 简码单元携出手区归属，供渲染层左右手上色（左粉右黄）。
        let words = vec!["是".to_string(), "有".to_string(), "中".to_string()];
        let hints = vec![hint("_w"), hint("+e"), hint("wCs")];
        let rows = layout_code_hint_grid(&words, &hints, &[], 20);
        let cells = &rows[0].0;
        assert_eq!(cells[0].hand, HintHand::Left); // 是 → _w 左手
        assert_eq!(cells[1].hand, HintHand::Right); // 有 → +e 右手
        assert_eq!(cells[2].hand, HintHand::TwoHand); // 中 → wCs 双手并击无前缀 → TwoHand 单独配色
    }
}
