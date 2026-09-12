use dazitui_core::{default_rime_data_dir, SchemeDict};

#[test]
fn test_tiger_user_reported_text() {
    let dir = default_rime_data_dir();
    let path = dir.join("tiger.schema.yaml");
    if !path.exists() {
        eprintln!("skip: tiger.schema.yaml not found in {}", dir.display());
        return;
    }

    let dict = SchemeDict::load_from_file(&path).expect("加载方案失败");
    let test_words = vec![
        "结果".to_string(),
        "反复".to_string(),
        "自我".to_string(),
        "拉扯".to_string(),
        "否定".to_string(),
        "消耗".to_string(),
        "大量".to_string(),
        "情绪".to_string(),
        "精力".to_string(),
        "很多人".to_string(),
        "在职".to_string(),
        "场中".to_string(),
        "习惯性".to_string(),
        "内耗".to_string(),
        "做".to_string(),
        "完".to_string(),
    ];
    let hints = dict.build_code_hints(&test_words);
    for hint in &hints {
        println!("word: {:<6} code: {}", hint.word, hint.code);
        // 不得包含内部宏与数字乱码 (如 ',' '-' '0')
        assert!(
            !hint.code.contains(',') && !hint.code.contains('-'),
            "词提包含格式乱码字符: 词 {}, 编码 {}",
            hint.word, hint.code
        );
        let has_digit = hint.code.chars().any(|c| c.is_ascii_digit());
        assert!(
            !has_digit,
            "万象虎词提不应包含数字乱码: 词 {}, 编码 {}",
            hint.word, hint.code
        );
    }
}
