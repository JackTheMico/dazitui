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

#[test]
fn test_tiger_keystroke_and_strokes_statistics() {
    let dir = default_rime_data_dir();
    let path = dir.join("tiger.schema.yaml");
    if !path.exists() {
        return;
    }

    let dict = SchemeDict::load_from_file(&path).expect("加载方案失败");
    assert!(dict.is_four_auto_commit(), "万象虎应自动判定为 four_auto_commit 定长形码方案");
    assert!(!dict.is_pure_chord(), "万象虎不是纯并击方案");

    // 1. 1码高频简码字（如 "我" -> t）：需按空格上屏，计 2 击，按键序列为 ["t", "Space"]
    let (s_wo, keys_wo) = dict.resolve_strokes_and_keys("我");
    assert_eq!(s_wo, 2, "1码简码独立出字需敲空格，应为 2 击");
    assert_eq!(keys_wo, vec!["t", "Space"]);

    // 2. 2码简码字（如 "做" -> jc）：需按空格上屏，计 3 击，按键序列为 ["j", "c", "Space"]
    let (s_zuo, keys_zuo) = dict.resolve_strokes_and_keys("做");
    assert_eq!(s_zuo, 3, "2码简码独立出字需敲空格，应为 3 击");
    assert_eq!(keys_zuo, vec!["j", "c", "Space"]);

    // 3. 3码简码字（如 "完" -> wmp）：需按空格上屏，计 4 击，按键序列为 ["w", "m", "p", "Space"]
    let (s_wan, keys_wan) = dict.resolve_strokes_and_keys("完");
    assert_eq!(s_wan, 4, "3码简码独立出字需敲空格，应为 4 击");
    assert_eq!(keys_wan, vec!["w", "m", "p", "Space"]);

    // 4. 4码定长自动上屏词（如 "结果" -> igqe）：自动上屏无需空格，计 4 击，按键序列为 4 字母
    let (s_jieguo, keys_jieguo) = dict.resolve_strokes_and_keys("结果");
    assert_eq!(s_jieguo, 4, "4码定长词自动上屏，应为 4 击");
    assert_eq!(keys_jieguo, vec!["i", "g", "q", "e"]);

    // 5. 复合句段贪婪匹配："做" + "结果"
    let (s_combo, keys_combo) = dict.resolve_strokes_and_keys("做结果");
    assert_eq!(s_combo, 3 + 4, "做(3击) + 结果(4击) 应为 7 击");
    assert_eq!(keys_combo, vec!["j", "c", "Space", "i", "g", "q", "e"]);

    // 6. 句中简词以 '/' 结尾（如 "一个人" -> fjjr/）：保留 '/'，计 5 击
    let (s_yigr, keys_yigr) = dict.resolve_strokes_and_keys("一个人");
    if dict.get_primary_code("一个人") == Some("fjjr/") {
        assert_eq!(s_yigr, 5, "/引导简词应计 5 击");
        assert_eq!(keys_yigr, vec!["f", "j", "j", "r", "/"]);
    }
}
