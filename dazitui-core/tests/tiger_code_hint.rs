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

#[test]
fn test_tiger_hint_prefers_certain_and_marks_chongma_rank() {
    let dir = default_rime_data_dir();
    let path = dir.join("tiger.schema.yaml");
    if !path.exists() {
        eprintln!("skip: tiger.schema.yaml not found in {}", dir.display());
        return;
    }
    let dict = SchemeDict::load_from_file(&path).expect("加载方案失败");

    // 1. 4 码唯一 → 自动上屏，4 击，无上屏键标记。
    let jieguo = dict.build_code_hints(&["结果".to_string()]).remove(0);
    assert_eq!(jieguo.code, "igqe", "唯一 4 码应自动上屏、不补空格标记");
    assert_eq!(jieguo.strokes, 4);
    assert_eq!(jieguo.rank, 1);

    // 2. 4 码有重码但目标词为首选 → 补一次空格上屏，5 击（ADR 0013 D5）。
    //    `jgjr` 组：个人(36641) / 佳人(1192) / 伟人(630)。
    let geren = dict.build_code_hints(&["个人".to_string()]).remove(0);
    assert_eq!(geren.code, "jgjr\u{2423}", "重码首选应补空格键标记");
    assert_eq!(geren.strokes, 5);
    assert_eq!(geren.rank, 1);

    // 3. 同码非首选 → 提示串尾部直接写出要按的选重键（ADR 0014 D2）：
    //    万象虎字母表含 `;` 不含 `'`（`'` 是其拼音反查前缀），故第 2 位为 `;`、第 3 位为数字 `3`。
    let jiaren = dict.build_code_hints(&["佳人".to_string()]).remove(0);
    assert_eq!(jiaren.code, "jgjr;", "第 2 位应提示按分号选重");
    assert_eq!(jiaren.strokes, 5);
    assert_eq!(jiaren.rank, 2, "佳人应为第二位");

    let weiren = dict.build_code_hints(&["伟人".to_string()]).remove(0);
    assert_eq!(weiren.rank, 3, "伟人应为第三位");
    assert_eq!(weiren.strokes, 5);
    assert_eq!(weiren.code, "jgjr3", "第 3 位无 `'` 选重键时退回数字");
}

#[test]
fn test_tiger_hint_oov_falls_back_to_per_char_commit() {
    let dir = default_rime_data_dir();
    let path = dir.join("tiger.schema.yaml");
    if !path.exists() {
        eprintln!("skip: tiger.schema.yaml not found in {}", dir.display());
        return;
    }
    let dict = SchemeDict::load_from_file(&path).expect("加载方案失败");

    // 「腕间」未收录：逐字独立上屏，各字码之间以 ␣ 显式分隔（ADR 0013 D6）。
    let hint = dict.build_code_hints(&["腕间".to_string()]).remove(0);
    assert!(!hint.is_oov, "各字均登录时应给出逐字提示，而非留空");
    let marks = hint.code.matches('\u{2423}').count();
    assert_eq!(
        marks, 2,
        "两字词逐字独立上屏应有 2 个上屏键标记：{:?}",
        hint.code
    );

    // 击数等于各字独立上屏之和（腕 vwl 3码+空格=4，间 ao 2码+空格=3）。
    let wan = dict.build_code_hints(&["腕".to_string()]).remove(0);
    let jian = dict.build_code_hints(&["间".to_string()]).remove(0);
    assert!(!wan.code.is_empty() && !jian.code.is_empty());
    assert_eq!(
        hint.strokes,
        wan.strokes + jian.strokes,
        "未收录词击数应为逐字击数之和：{:?}",
        hint.code
    );
    assert!(
        hint.code.starts_with(&wan.code),
        "逐字提示应以首字码开头：{:?} vs {:?}",
        hint.code,
        wan.code
    );
}

#[test]
fn test_tiger_hint_keystroke_stats_share_one_measure() {
    let dir = default_rime_data_dir();
    let path = dir.join("tiger.schema.yaml");
    if !path.exists() {
        return;
    }
    let dict = SchemeDict::load_from_file(&path).expect("加载方案失败");

    // 词提与击键统计必须同源：同一个词的提示击数 = 统计击数（ADR 0013 D5 口径单一）。
    for w in ["结果", "个人", "佳人", "很多人", "做"] {
        let hint = dict.build_code_hints(&[w.to_string()]).remove(0);
        let (strokes, _) = dict.resolve_strokes_and_keys(w);
        assert_eq!(
            hint.strokes, strokes,
            "「{w}」词提击数与统计击数应一致（提示码 {:?}）",
            hint.code
        );
    }
}

/// 虎整句：码表显式收录的「简词」必须优先于逐字连续码（ADR 0014）。
///
/// `tiger_sentence.codes.txt` 共 9896 条，其中多字词仅 102 个（1 码 29 / 2 码 63 /
/// 3 码 9 / 4 码 1），全部为简词；逐一断言不会被逐字回退挤掉。
#[test]
fn test_tiger_sentence_jian_ci_wins_over_continuous_code() {
    let dir = default_rime_data_dir();
    let path = dir.join("tiger_sentence.schema.yaml");
    if !path.exists() {
        eprintln!("skip: tiger_sentence.schema.yaml not found in {}", dir.display());
        return;
    }
    let dict = SchemeDict::load_from_file(&path).expect("加载虎整句失败");
    assert!(dict.is_sentence_mode(), "虎整句应判定为变长整句方案");

    // 一码简词：整段只有一码时合法 → 2 击（码 + 选重/上屏键）。
    // 位次按码表录入顺序（单字一简在前、简词在后），权重缺失时不按词条字符串排序——
    // 否则「的」会被「工作」压到第 2 位（见 test_tiger_sentence_single_char_yijian_is_first）。
    for (word, code) in [
        ("所以", "m;"),
        ("怎么", "w;"),
        ("那个", "a;"),
        ("工作", "u;"),
        ("东西", "r'"),
    ] {
        let h = dict.build_code_hints(&[word.to_string()]).remove(0);
        assert_eq!(h.code, code, "「{word}」应提示简词码");
        assert_eq!(h.strokes, 2, "「{word}」一码简词应为 2 击");
    }
    // 第 3 位简词：`'` 在虎整句字母表内，故选重键为 `'`（不是数字 3）。
    let zhidao = dict.build_code_hints(&["知道".to_string()]).remove(0);
    assert_eq!(zhidao.code, "o'");
    assert_eq!(zhidao.rank, 3);

    // 二码简词：3 击。
    for (word, code) in [("慢慢", "hh;"), ("一般", "fi;")] {
        let h = dict.build_code_hints(&[word.to_string()]).remove(0);
        assert_eq!(h.code, code, "「{word}」应提示简词码");
        assert_eq!(h.strokes, 3, "「{word}」二码简词应为 3 击");
    }

    // 逐个遍历码表里全部多字词条，确认无一退化成逐字连续码。
    let raw = std::fs::read_to_string(dir.join("tiger_sentence.codes.txt")).unwrap_or_default();
    let mut checked = 0;
    for line in raw.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let mut it = t.split('\t');
        let (w, c) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        if w.chars().count() < 2 || c.is_empty() {
            continue;
        }
        let h = dict.build_code_hints(&[w.to_string()]).remove(0);
        assert!(
            h.code.starts_with(c),
            "「{w}」未使用简词 {c}，实得 {:?}",
            h.code
        );
        assert!(
            h.strokes <= c.chars().count() as u32 + 1,
            "「{w}」简词击数不应超过 码长+1，实得 {}",
            h.strokes
        );
        checked += 1;
    }
    assert!(checked >= 100, "应覆盖码表中全部多字简词，实得 {checked}");
}

/// 虎整句：未收录词走**连续码**，且每字段长 >= 2（ADR 0014）。
///
/// 一码段只在「整段输入只有一码」时合法，故单字母码不能并进多字连续码：
/// 「个人」拼 `jgj` 会被解码成「佝」，必须用「人」的二码 `jr` → `jgjr`。
#[test]
fn test_tiger_sentence_continuous_code_skips_one_letter_codes() {
    let dir = default_rime_data_dir();
    let path = dir.join("tiger_sentence.schema.yaml");
    if !path.exists() {
        return;
    }
    let dict = SchemeDict::load_from_file(&path).expect("加载虎整句失败");

    // 期望值与官方 lua 解码器 `tiger_sentence.decode_full` 实测一致。
    for (word, code, strokes) in [
        ("结果", "igdqem\u{2423}", 7),
        ("个人", "jgjr\u{2423}", 5),
        ("大量", "mdofd\u{2423}", 6),
        ("情绪", "haiqo\u{2423}", 6),
        ("精力", "pasl\u{2423}", 5),
        ("很多人", "willjr\u{2423}", 7),
    ] {
        let h = dict.build_code_hints(&[word.to_string()]).remove(0);
        assert_eq!(h.code, code, "「{word}」连续码不符");
        assert_eq!(h.strokes, strokes, "「{word}」击数不符");
        assert!(!h.is_oov, "「{word}」各字均已登录，不应留空");
        assert_eq!(h.rank, 1, "连续码由语言模型分词，按首选计");
    }

    // 单字词整段只有一码，不受「一码段」限制。
    let wo = dict.build_code_hints(&["我".to_string()]).remove(0);
    assert_eq!(wo.code, "t\u{2423}");
    assert_eq!(wo.strokes, 2);
}

/// 词提击数、击键统计、按键序列长度三者必须同源（ADR 0014 D1）。
#[test]
fn test_tiger_sentence_hint_and_stats_share_one_measure() {
    let dir = default_rime_data_dir();
    let path = dir.join("tiger_sentence.schema.yaml");
    if !path.exists() {
        return;
    }
    let dict = SchemeDict::load_from_file(&path).expect("加载虎整句失败");

    for w in ["慢慢", "所以", "自己", "结果", "个人", "习惯性", "我", "做"] {
        let h = dict.build_code_hints(&[w.to_string()]).remove(0);
        let (strokes, keys) = dict.resolve_strokes_and_keys(w);
        assert_eq!(
            h.strokes, strokes,
            "「{w}」词提击数与统计击数应一致（提示码 {:?}）",
            h.code
        );
        assert_eq!(
            keys.len() as u32,
            strokes,
            "「{w}」按键序列长度应等于击数：{:?}",
            keys
        );
    }
}

#[test]
fn test_tiger_sentence_keystroke_and_strokes_statistics() {
    let dir = default_rime_data_dir();
    let schema_path = dir.join("tiger_sentence.schema.yaml");
    let codes_path = dir.join("tiger_sentence.codes.txt");

    if !schema_path.exists() && !codes_path.exists() {
        return;
    }

    if schema_path.exists() {
        let dict_schema = SchemeDict::load_from_file(&schema_path).expect("加载虎整句 schema 失败");
        assert!(dict_schema.entry_count() > 9000, "虎整句 schema 应成功关联并加载 tiger_sentence.codes.txt 词条");
        assert!(dict_schema.is_four_auto_commit(), "虎整句 schema 应自动判定为 four_auto_commit");

        let (s_wo, keys_wo) = dict_schema.resolve_strokes_and_keys("我");
        assert_eq!(s_wo, 2, "我(1码)独立出字需敲空格，应为 2 击");
        assert_eq!(keys_wo, vec!["t", "Space"]);

        let (s_zuo, keys_zuo) = dict_schema.resolve_strokes_and_keys("做");
        assert_eq!(s_zuo, 3, "做(2码)独立出字需敲空格，应为 3 击");
        assert_eq!(keys_zuo, vec!["j", "c", "Space"]);

        // 验证 100 WPM (600ms/字) 下能正常通过 4.0 击键放行门槛
        let text1 = "的一是了不在有个人这";
        let mut session = dazitui_core::Session::new_gated_with_words_and_size(text1, true, &[], 10);
        session.set_targets(4.0, 0);
        let mut elapsed = std::time::Duration::from_millis(500);
        for c in text1.chars() {
            let (s, _) = dict_schema.resolve_strokes_and_keys(&c.to_string());
            session.type_text_with_strokes_at(&c.to_string(), s, elapsed);
            elapsed += std::time::Duration::from_millis(600);
        }
        assert_eq!(session.completed_groups(), 1, "100 WPM 下打虎整句单字应达标放行进入下一组");
    }

    if codes_path.exists() {
        let dict_codes = SchemeDict::load_from_file(&codes_path).expect("加载 tiger_sentence.codes.txt 失败");
        assert!(dict_codes.entry_count() > 9000, "tiger_sentence.codes.txt 词条应大于 9000");
        assert!(dict_codes.is_four_auto_commit(), "tiger_sentence.codes.txt 应自动识别为 four_auto_commit");

        let (s_wo, keys_wo) = dict_codes.resolve_strokes_and_keys("我");
        assert_eq!(s_wo, 2, "我(1码)独立出字需敲空格，应为 2 击");
        assert_eq!(keys_wo, vec!["t", "Space"]);

        let text11 = "左块索酒值态按陈河巴";
        let mut session = dazitui_core::Session::new_gated_with_words_and_size(text11, true, &[], 10);
        session.set_targets(4.0, 0);
        let mut elapsed = std::time::Duration::from_millis(500);
        for c in text11.chars() {
            let (s, _) = dict_codes.resolve_strokes_and_keys(&c.to_string());
            session.type_text_with_strokes_at(&c.to_string(), s, elapsed);
            elapsed += std::time::Duration::from_millis(600);
        }
        assert_eq!(session.completed_groups(), 1, "100 WPM 下常用单字中五百第11组应达标放行");
    }
}

/// 万象虎：一码简词（`tigress_simp_ci`）必须压过整词全码。
///
/// 「不是」在 `tigress_ci` 里有全码 `cbot`（4 击自动上屏），在 `tigress_simp_ci` 里有一码简词
/// `c`（第 2 位，`;` 选重 → 2 击）。按「击数第一、位次仅作同击数决胜」，简词胜出。
#[test]
fn test_wanxiang_one_key_jian_ci_beats_full_code() {
    let dir = default_rime_data_dir();
    let path = dir.join("tiger.schema.yaml");
    if !path.exists() {
        eprintln!("skip: tiger.schema.yaml not found in {}", dir.display());
        return;
    }
    let dict = SchemeDict::load_from_file(&path).expect("加载万象虎失败");

    for (word, code) in [
        ("不是", "c;"),
        ("那个", "a;"),
        ("如果", "b;"),
        ("哪个", "d;"),
        ("突然", "e;"),
        ("开始", "f;"),
    ] {
        let h = dict.build_code_hints(&[word.to_string()]).remove(0);
        assert_eq!(h.code, code, "「{word}」应提示一码简词而非全码");
        assert_eq!(h.strokes, 2, "「{word}」一码简词 + 选重键应为 2 击");
        assert_eq!(h.rank, 2, "「{word}」一码简词应排在第 2 位（不的一简在前）");
    }

    // 二码简词同样成立：3 击 < 全码 4 击。
    let manman = dict.build_code_hints(&["慢慢".to_string()]).remove(0);
    assert_eq!(manman.code, "hh;");
    assert_eq!(manman.strokes, 3);

    // 无简词的词仍走全码，且首选全码不被更短的非首选码挤掉。
    let jieguo = dict.build_code_hints(&["结果".to_string()]).remove(0);
    assert_eq!(jieguo.code, "igqe");
    assert_eq!(jieguo.strokes, 4);
    assert_eq!(jieguo.rank, 1);
}

/// 虎整句：权重缺失时位次按**码表录入顺序**定，不得按词条字符串排序。
///
/// `tiger_sentence.codes.txt` 里「的」在第 6 行、「工作」在第 2545 行，二者同为一码 `u`。
/// 曾因等值权重回退到按词条码点排序（工 U+5DE5 < 的 U+7684），把「的」判成第 2 位，
/// 提示成 `u;`；实际应为首选 `u␣`。
#[test]
fn test_tiger_sentence_single_char_yijian_is_first() {
    let dir = default_rime_data_dir();
    let path = dir.join("tiger_sentence.schema.yaml");
    if !path.exists() {
        eprintln!(
            "skip: tiger_sentence.schema.yaml not found in {}",
            dir.display()
        );
        return;
    }
    let dict = SchemeDict::load_from_file(&path).expect("加载虎整句失败");
    assert!(dict.is_sentence_mode(), "虎整句应判定为变长整句方案");

    let de = dict.build_code_hints(&["的".to_string()]).remove(0);
    assert_eq!(
        de.code, "u\u{2423}",
        "「的」是一简 u 的首选，应提示 u + 空格"
    );
    assert_eq!(de.rank, 1);
    assert_eq!(de.strokes, 2);

    // 同码的简词「工作」排在单字之后 → 第 2 位，选重键 `;`。
    let gongzuo = dict.build_code_hints(&["工作".to_string()]).remove(0);
    assert_eq!(gongzuo.code, "u;");
    assert_eq!(gongzuo.rank, 2);
}
