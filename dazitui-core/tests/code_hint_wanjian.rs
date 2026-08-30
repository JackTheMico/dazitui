//! 端到端回归：真实并击方案下，词组提示码宽于词宽时必须完整显示（不得截断）。
//!
//! 用户报告：「腕间」的遍码提示显示为 `HjYI`，少了末位码元 `w`。
//! 根因：词格列宽取词的可视宽（2 字 = 4 列），而 yoyo-pure 下该词无整词条目，
//! 逐字拼接得 `HjY`(腕) + `Iw`(间) = `HjYIw`（5 列），第 5 列被 `format_hint_cell` 截断。

use dazitui_core::{SchemeDict, default_rime_data_dir, layout_code_hint_line};

/// 本机可用的并击方案（按优先级尝试；均不存在时跳过，避免在 CI 上误报）。
const CANDIDATES: [&str; 2] = ["yoyo-pure-km", "yoyo-pure"];

fn cells_text(cells: &[dazitui_core::HintCell]) -> String {
    cells
        .iter()
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn word_hint_wider_than_word_is_rendered_in_full() {
    let dir = default_rime_data_dir();
    let Some(path) = CANDIDATES
        .iter()
        .map(|id| dir.join(format!("{id}.schema.yaml")))
        .find(|p| p.exists())
    else {
        eprintln!("skip: no yoyo scheme in {}", dir.display());
        return;
    };

    let dict = SchemeDict::load_from_file(&path).expect("加载方案失败");
    // 前置条件：腕/间各自的编码（整词「腕间」未登录，走逐字回退）。
    assert_eq!(dict.get_primary_code("腕"), Some("HjY"));
    assert_eq!(dict.get_primary_code("间"), Some("Iw"));

    let words = vec!["腕间".to_string()];
    let hints = dict.build_code_hints(&words);
    assert_eq!(hints[0].code, "HjYIw");

    let cells = layout_code_hint_line(&words, &hints, &[]);
    assert_eq!(
        cells_text(&cells),
        "HjYIw",
        "提示行应完整显示编码，不得因词宽不足而截断"
    );
}
