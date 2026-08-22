use std::path::Path;

mod session;
mod settings;

#[cfg(feature = "online")]
mod online;

pub use session::{CharStatus, GROUP_SIZE, Session, Stats, TypeResult};
pub use settings::{
    FONT_SIZE_PT, Rgb, Settings, SettingsStore, Theme, ThemePreset, osc_font_size_sequence,
};

#[cfg(feature = "online")]
pub use online::auth::{env_credentials, is_auth_failure, should_auto_relogin};
#[cfg(feature = "online")]
pub use online::client::{ApiClient, ApiError, CompetitionText, LoginResult, RankResult};
#[cfg(feature = "online")]
pub use online::protocol::{ProtocolError, build_request, decrypt, encrypt, parse_json};
#[cfg(feature = "online")]
pub use online::share::{
    UploadStats, build_upload_payload, format_share_text, format_time, osc52_clipboard,
    to_upload_stats,
};
#[cfg(feature = "online")]
pub use online::token::TokenStore;

/// 赛文：练习/比赛用的文字内容，来自本地文件或 52dazi.cn。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    /// 赛文的标题（本地文件载入时为文件名）。
    pub title: String,
    /// 赛文内容。
    pub content: String,
    /// 赛文来源。
    pub source: TextSource,
    /// 乱序词组赛文的重排后词边界（`(char_start, char_end)`）。
    /// 顺序版与非词组赛文为 `None`，渲染时回退到 `BuiltinSet::word_boundaries()`。
    pub word_boundaries: Option<Vec<(usize, usize)>>,
    /// 是否为乱序版（`load_builtin_text_shuffled` 产出 true）。
    /// `restart()` 据此判断是否重新打乱。
    pub shuffled: bool,
}

/// 赛文来源：本地文件、内置赛文或 52dazi.cn 在线比赛。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextSource {
    /// 本地文件。
    #[default]
    File,
    /// 内置赛文（随二进制分发的练习材料，如常用单字）。
    Builtin { set: BuiltinSet },
    /// 52dazi.cn 在线赛文。
    Online { competition_type: CompetitionType },
}

impl TextSource {
    /// 是否以词组为单位分页显示（每页 10 个词）。
    pub fn is_word_paged(&self) -> bool {
        matches!(
            self,
            TextSource::Builtin {
                set: BuiltinSet::CommonWordsQian
                    | BuiltinSet::CommonWordsZhong
                    | BuiltinSet::CommonWordsHou
            }
        )
    }

    /// 是否为内置赛文（启用组边界门槛）。
    pub fn is_builtin(&self) -> bool {
        matches!(self, TextSource::Builtin { .. })
    }
}

/// 内置赛文集合（每套一个枚举变体）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinSet {
    /// 常用单字前五百。
    CommonCharsQian,
    /// 常用单字中五百。
    CommonCharsZhong,
    /// 常用单字后五百。
    CommonCharsHou,
    /// 常用词组前五百。
    CommonWordsQian,
    /// 常用词组中五百。
    CommonWordsZhong,
    /// 常用词组后五百。
    CommonWordsHou,
}

impl BuiltinSet {
    /// 赛文的中文名。
    pub fn name(&self) -> &'static str {
        match self {
            Self::CommonCharsQian => "常用单字前五百",
            Self::CommonCharsZhong => "常用单字中五百",
            Self::CommonCharsHou => "常用单字后五百",
            Self::CommonWordsQian => "常用词组前五百",
            Self::CommonWordsZhong => "常用词组中五百",
            Self::CommonWordsHou => "常用词组后五百",
        }
    }

    /// 是否为词组赛文（以词组为单位分页显示，显示时词间加空格、不显示逗号）。
    pub fn is_words(&self) -> bool {
        matches!(
            self,
            Self::CommonWordsQian | Self::CommonWordsZhong | Self::CommonWordsHou
        )
    }

    /// 赛文内容（已去换行，为纯字符串）。
    pub fn content(&self) -> &'static str {
        match self {
            Self::CommonCharsQian => include_str!("../data/common-chars-qian.txt"),
            Self::CommonCharsZhong => include_str!("../data/common-chars-zhong.txt"),
            Self::CommonCharsHou => include_str!("../data/common-chars-hou.txt"),
            Self::CommonWordsQian => include_str!("../data/common-words-qian.txt"),
            Self::CommonWordsZhong => include_str!("../data/common-words-zhong.txt"),
            Self::CommonWordsHou => include_str!("../data/common-words-hou.txt"),
        }
    }

    /// 将词组赛文的原始内容（逗号分隔）切分为词组的字符范围列表。
    /// 返回每个词组在**去逗号后**内容中的 `(char_start, char_end)` 索引。
    /// 非词组赛文返回空 Vec。
    pub fn word_boundaries(&self) -> Vec<(usize, usize)> {
        if !self.is_words() {
            return Vec::new();
        }
        // 逐字符扫描逗号分隔的原始内容，跳过逗号，
        // 记录每个词组在去逗号后的 char 索引范围。
        let content = self.content();
        let mut boundaries = Vec::new();
        let mut decommad_start: Option<usize> = None;
        let mut decommad_idx: usize = 0;
        for ch in content.chars() {
            if ch == '，' || ch == ',' {
                if let Some(s) = decommad_start.take() {
                    boundaries.push((s, decommad_idx));
                }
            } else if ch != '\n' && ch != '\r' {
                if decommad_start.is_none() {
                    decommad_start = Some(decommad_idx);
                }
                decommad_idx += 1;
            }
        }
        if let Some(s) = decommad_start.take() {
            boundaries.push((s, decommad_idx));
        }
        boundaries
    }

    /// 词组赛文去逗号后的纯字符内容（词组直接拼接，无分隔符）。
    /// 非词组赛文返回 content() 本身。
    pub fn content_no_commas(&self) -> String {
        if !self.is_words() {
            return self.content().to_string();
        }
        self.content()
            .chars()
            .filter(|c| *c != '，' && *c != ',' && *c != '\n' && *c != '\r')
            .collect()
    }
}

/// 所有内置赛文，按功能栏展示顺序。
pub const BUILTIN_SETS: [BuiltinSet; 6] = [
    BuiltinSet::CommonCharsQian,
    BuiltinSet::CommonCharsZhong,
    BuiltinSet::CommonCharsHou,
    BuiltinSet::CommonWordsQian,
    BuiltinSet::CommonWordsZhong,
    BuiltinSet::CommonWordsHou,
];

/// 载入内置赛文：内容为纯字符串（已去除换行）。
/// 词组赛文去掉逗号（用户无需打逗号），分页渲染时按 word_boundaries 切词、词间加空格显示。
pub fn load_builtin_text(set: BuiltinSet) -> Text {
    let content = if set.is_words() {
        set.content_no_commas()
    } else {
        set.content().replace(['\n', '\r'], "")
    };
    Text {
        title: set.name().to_string(),
        content,
        source: TextSource::Builtin { set },
        word_boundaries: None,
        shuffled: false,
    }
}

/// 载入内置赛文的乱序版：每次调用随机打乱排列，产出新 Text。
///
/// - 单字赛文：打散字符顺序
/// - 词组赛文：打乱词组顺序（每个词组内部字符顺序不变），重排 content 并重算 word_boundaries
///
/// `title` 带「（乱序）」后缀；`source` 仍为 `TextSource::Builtin { set }`；
/// `shuffled=true`；词组赛文 `word_boundaries=Some(...)`。
pub fn load_builtin_text_shuffled(set: BuiltinSet) -> Text {
    use rand::seq::SliceRandom;
    let mut rng = rand::rng();
    if set.is_words() {
        let no_commas = set.content_no_commas();
        let chars: Vec<char> = no_commas.chars().collect();
        let mut boundaries = set.word_boundaries();
        boundaries.shuffle(&mut rng);
        // 按打乱后的词序重排字符，并重算连续边界
        let mut new_content = String::with_capacity(chars.len());
        let mut new_boundaries = Vec::with_capacity(boundaries.len());
        let mut char_count = 0;
        for &(ws, we) in &boundaries {
            let start = char_count;
            for &c in &chars[ws..we] {
                new_content.push(c);
            }
            char_count += we - ws;
            new_boundaries.push((start, char_count));
        }
        Text {
            title: shuffled_title(set),
            content: new_content,
            source: TextSource::Builtin { set },
            word_boundaries: Some(new_boundaries),
            shuffled: true,
        }
    } else {
        let mut chars: Vec<char> = set.content().replace(['\n', '\r'], "").chars().collect();
        chars.shuffle(&mut rng);
        let content: String = chars.into_iter().collect();
        Text {
            title: shuffled_title(set),
            content,
            source: TextSource::Builtin { set },
            word_boundaries: None,
            shuffled: true,
        }
    }
}

/// 乱序版赛文标题：`set.name()` +「（乱序）」后缀。
fn shuffled_title(set: BuiltinSet) -> String {
    format!("{}（乱序）", set.name())
}

/// 52dazi.cn 比赛类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompetitionType {
    /// 极速杯。
    Jisu,
    /// 锦标赛。
    Jinbiao,
    /// 键神杯。
    Jianshen,
}

impl CompetitionType {
    /// 比赛类型的中文名。
    pub fn name(&self) -> &'static str {
        match self {
            Self::Jisu => "极速杯",
            Self::Jinbiao => "锦标赛",
            Self::Jianshen => "键神杯",
        }
    }

    /// 52dazi.cn API 使用的比赛类型编号（极速杯=0、锦标赛=2、键神杯=4）。
    pub fn code(&self) -> u8 {
        match self {
            Self::Jisu => 0,
            Self::Jinbiao => 2,
            Self::Jianshen => 4,
        }
    }
}

impl Text {
    /// 是否在线赛文（来自 52dazi.cn）。在线赛文跟打时禁用重打。
    pub fn is_online(&self) -> bool {
        matches!(self.source, TextSource::Online { .. })
    }
}

/// 载文失败的分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// 文件不存在。
    NotFound,
    /// 文件存在但内容为空。
    Empty,
    /// 读取失败（权限等）。
    ReadFailed,
}

/// 载文选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoadOptions {
    /// 去除所有空白字符（空格/换行/制表符）。
    pub strip_whitespace: bool,
    /// 去除中英文标点符号。
    pub strip_punctuation: bool,
}

/// 从本地文件载入赛文（默认选项：保留原文）。
pub fn load_text_from_file(path: &Path) -> Result<Text, LoadError> {
    load_text_from_file_with_options(path, &LoadOptions::default())
}

/// 从本地文件载入赛文，按选项处理内容。
pub fn load_text_from_file_with_options(
    path: &Path,
    options: &LoadOptions,
) -> Result<Text, LoadError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            LoadError::NotFound
        } else {
            LoadError::ReadFailed
        }
    })?;
    let content = process_content(content, options)?;
    let title = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(Text {
        title,
        content,
        source: TextSource::File,
        word_boundaries: None,
        shuffled: false,
    })
}

/// 按选项处理原文：去空格、去标点、去首尾空白；处理为空则报 Empty。
fn process_content(raw: String, options: &LoadOptions) -> Result<String, LoadError> {
    if raw.is_empty() {
        return Err(LoadError::Empty);
    }
    let mut content = raw;
    if options.strip_whitespace {
        content = content.chars().filter(|c| !c.is_whitespace()).collect();
    }
    if options.strip_punctuation {
        content = content.chars().filter(|c| !is_punctuation(*c)).collect();
    }
    // 去掉首尾空白（尤其文件末尾换行）：跟打时用户不会打换行，否则永远无法「完成」
    let content = content.trim().to_string();
    if content.is_empty() {
        return Err(LoadError::Empty);
    }
    Ok(content)
}

/// 判断字符是否为标点：ASCII 标点 + 常见中英文标点。
fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(
            c,
            '，' | '。'
                | '！'
                | '？'
                | '；'
                | '：'
                | '、'
                | '“'
                | '”'
                | '‘'
                | '’'
                | '（'
                | '）'
                | '《'
                | '》'
                | '〈'
                | '〉'
                | '「'
                | '」'
                | '『'
                | '』'
                | '—'
                | '…'
                | '·'
                | '～'
                | '￥'
                | '×'
                | '÷'
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(suffix: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("dazitui-test-{stamp}-{suffix}"))
    }

    #[test]
    fn loads_existing_file_into_text() {
        let path = temp_path("exists.txt");
        fs::write(&path, "你好，世界。\n这是第二行。").unwrap();

        let text = load_text_from_file(&path).expect("载入应成功");
        assert!(
            text.title.ends_with("exists.txt"),
            "title 应为文件名，得到: {}",
            text.title
        );
        assert_eq!(text.content, "你好，世界。\n这是第二行。");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn trailing_newline_is_trimmed() {
        let path = temp_path("trailing.txt");
        fs::write(&path, "你好世界。\n").unwrap();

        let text = load_text_from_file(&path).expect("载入应成功");
        assert_eq!(text.content, "你好世界。");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn whitespace_only_file_is_empty_error() {
        let path = temp_path("blank.txt");
        fs::write(&path, "  \n\t\n").unwrap();

        let err = load_text_from_file(&path).expect_err("纯空白文件应报错");
        assert_eq!(err, LoadError::Empty);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_not_found_error() {
        let path = temp_path("missing.txt");
        let _ = fs::remove_file(&path); // 确保不存在

        let err = load_text_from_file(&path).expect_err("缺失文件应报错");
        assert_eq!(err, LoadError::NotFound);
    }

    #[test]
    fn empty_file_is_empty_error() {
        let path = temp_path("empty.txt");
        fs::write(&path, "").unwrap();

        let err = load_text_from_file(&path).expect_err("空文件应报错");
        assert_eq!(err, LoadError::Empty);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn load_options_default_preserves_content() {
        let opts = LoadOptions::default();
        assert!(!opts.strip_whitespace);
        assert!(!opts.strip_punctuation);

        let path = temp_path("default.txt");
        fs::write(&path, "你好， 世界。\n第二行").unwrap();
        let text = load_text_from_file_with_options(&path, &opts).unwrap();
        assert_eq!(text.content, "你好， 世界。\n第二行");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn strip_whitespace_removes_all_whitespace() {
        let opts = LoadOptions {
            strip_whitespace: true,
            ..LoadOptions::default()
        };
        let path = temp_path("ws.txt");
        fs::write(&path, "你好 世界\n第二 行").unwrap();
        let text = load_text_from_file_with_options(&path, &opts).unwrap();
        assert_eq!(text.content, "你好世界第二行");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn strip_punctuation_removes_cn_and_ascii_punct() {
        let opts = LoadOptions {
            strip_punctuation: true,
            ..LoadOptions::default()
        };
        let path = temp_path("punct.txt");
        fs::write(&path, "你好，世界！Hello, world! \"ok\"（好）").unwrap();
        let text = load_text_from_file_with_options(&path, &opts).unwrap();
        assert_eq!(text.content, "你好世界Hello world ok好");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn strip_both_combines() {
        let opts = LoadOptions {
            strip_whitespace: true,
            strip_punctuation: true,
        };
        let path = temp_path("both.txt");
        fs::write(&path, "你好， 世界！\n第二行。").unwrap();
        let text = load_text_from_file_with_options(&path, &opts).unwrap();
        assert_eq!(text.content, "你好世界第二行");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn strip_to_empty_is_empty_error() {
        let opts = LoadOptions {
            strip_punctuation: true,
            ..LoadOptions::default()
        };
        let path = temp_path("punctonly.txt");
        fs::write(&path, "，。！？；：、").unwrap();
        let err =
            load_text_from_file_with_options(&path, &opts).expect_err("全符号文件去符号后应报空");
        assert_eq!(err, LoadError::Empty);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn text_is_online_distinguishes_source() {
        let file_text = Text {
            title: "f".into(),
            content: "c".into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        };
        let online_text = Text {
            title: "o".into(),
            content: "c".into(),
            source: TextSource::Online {
                competition_type: CompetitionType::Jisu,
            },
            word_boundaries: None,
            shuffled: false,
        };
        assert!(!file_text.is_online());
        assert!(online_text.is_online());
    }

    #[test]
    fn word_sets_strip_commas_on_load() {
        // 词组赛文加载后内容无逗号、无换行：用户无需打逗号。
        for &set in &[
            BuiltinSet::CommonWordsQian,
            BuiltinSet::CommonWordsZhong,
            BuiltinSet::CommonWordsHou,
        ] {
            let text = load_builtin_text(set);
            assert!(
                !text.content.contains('，') && !text.content.contains(','),
                "{} 加载后含逗号",
                set.name()
            );
            assert!(
                !text.content.contains('\n') && !text.content.contains('\r'),
                "{} 加载后含换行",
                set.name()
            );
        }
    }

    #[test]
    fn word_boundaries_match_word_count() {
        // 词组赛文 word_boundaries 返回 500 个词，每个词的字符范围在 content_no_commas 内有效。
        for &set in &[
            BuiltinSet::CommonWordsQian,
            BuiltinSet::CommonWordsZhong,
            BuiltinSet::CommonWordsHou,
        ] {
            let boundaries = set.word_boundaries();
            assert_eq!(boundaries.len(), 500, "{} 应有 500 个词", set.name());
            let no_commas = set.content_no_commas();
            let char_count = no_commas.chars().count();
            // 每个词的 (start, end) 范围合法且不重叠
            let mut prev_end = 0;
            for &(start, end) in &boundaries {
                assert!(start < end, "{} 词范围空: ({}, {})", set.name(), start, end);
                assert!(
                    start >= prev_end,
                    "{} 词范围重叠: prev_end={}, start={}",
                    set.name(),
                    prev_end,
                    start
                );
                assert!(
                    end <= char_count,
                    "{} 词范围越界: end={}, char_count={}",
                    set.name(),
                    end,
                    char_count
                );
                prev_end = end;
            }
            // 最后一个词的结尾 = content_no_commas 的字符数
            assert_eq!(
                boundaries.last().unwrap().1, char_count,
                "{} 最后一个词的结尾应等于去逗号后的字符数",
                set.name()
            );
        }
    }

    #[test]
    fn is_words_flags_only_word_sets() {
        for &set in &BUILTIN_SETS {
            let expected = matches!(
                set,
                BuiltinSet::CommonWordsQian
                    | BuiltinSet::CommonWordsZhong
                    | BuiltinSet::CommonWordsHou
            );
            assert_eq!(set.is_words(), expected, "{} is_words 不正确", set.name());
        }
    }

    #[test]
    fn shuffled_text_has_correct_metadata() {
        for &set in &BUILTIN_SETS {
            let text = load_builtin_text_shuffled(set);
            assert!(text.shuffled, "{} 乱序版 shuffled 应为 true", set.name());
            assert!(
                text.title.ends_with("（乱序）"),
                "{} 乱序版标题应带（乱序）后缀, got {}",
                set.name(),
                text.title
            );
            assert_eq!(
                text.source,
                TextSource::Builtin { set },
                "{} 乱序版 source 应保持 Builtin",
                set.name()
            );
            if set.is_words() {
                assert!(
                    text.word_boundaries.is_some(),
                    "{} 乱序词组版应有 word_boundaries",
                    set.name()
                );
            }
        }
    }

    #[test]
    fn shuffled_char_set_preserves_multiset() {
        // 单字赛文乱序后字符多重集不变（排序后相同）。
        for &set in &[
            BuiltinSet::CommonCharsQian,
            BuiltinSet::CommonCharsZhong,
            BuiltinSet::CommonCharsHou,
        ] {
            let original: Vec<char> = set.content().replace(['\n', '\r'], "").chars().collect();
            let shuffled: Vec<char> = load_builtin_text_shuffled(set).content.chars().collect();
            assert_eq!(
                original.len(),
                shuffled.len(),
                "{} 乱序后字符数变化",
                set.name()
            );
            let mut orig_sorted = original.clone();
            orig_sorted.sort_unstable();
            let mut shuf_sorted = shuffled.clone();
            shuf_sorted.sort_unstable();
            assert_eq!(
                orig_sorted, shuf_sorted,
                "{} 乱序后字符多重集不一致",
                set.name()
            );
        }
    }

    #[test]
    fn shuffled_word_set_preserves_word_multiset() {
        // 词组赛文乱序后词组多重集不变（每个词的字符序列不变）。
        for &set in &[
            BuiltinSet::CommonWordsQian,
            BuiltinSet::CommonWordsZhong,
            BuiltinSet::CommonWordsHou,
        ] {
            let original_words: Vec<String> = set
                .word_boundaries()
                .iter()
                .map(|&(s, e)| {
                    set.content_no_commas()
                        .chars()
                        .skip(s)
                        .take(e - s)
                        .collect()
                })
                .collect();
            let text = load_builtin_text_shuffled(set);
            let boundaries = text.word_boundaries.unwrap();
            let shuffled_words: Vec<String> = boundaries
                .iter()
                .map(|&(s, e)| text.content.chars().skip(s).take(e - s).collect())
                .collect();
            assert_eq!(
                original_words.len(),
                shuffled_words.len(),
                "{} 乱序后词数变化",
                set.name()
            );
            let mut orig_sorted = original_words.clone();
            orig_sorted.sort_unstable();
            let mut shuf_sorted = shuffled_words.clone();
            shuf_sorted.sort_unstable();
            assert_eq!(
                orig_sorted, shuf_sorted,
                "{} 乱序后词组多重集不一致",
                set.name()
            );
        }
    }

    #[test]
    fn shuffled_word_boundaries_are_contiguous_and_valid() {
        // 词组赛文乱序后 word_boundaries 应覆盖整个 content 且不重叠。
        for &set in &[
            BuiltinSet::CommonWordsQian,
            BuiltinSet::CommonWordsZhong,
            BuiltinSet::CommonWordsHou,
        ] {
            let text = load_builtin_text_shuffled(set);
            let boundaries = text.word_boundaries.unwrap();
            let total_chars = text.content.chars().count();
            let mut prev_end = 0;
            for &(start, end) in &boundaries {
                assert!(
                    start == prev_end,
                    "{} 乱序边界不连续: prev_end={}, start={}",
                    set.name(),
                    prev_end,
                    start
                );
                assert!(start < end, "{} 乱序词范围空", set.name());
                assert!(
                    end <= total_chars,
                    "{} 乱序词范围越界: end={}, total={}",
                    set.name(),
                    end,
                    total_chars
                );
                prev_end = end;
            }
            assert_eq!(
                prev_end, total_chars,
                "{} 乱序边界未覆盖全部 content",
                set.name()
            );
        }
    }

    #[test]
    fn shuffled_differs_from_ordered_on_average() {
        // 乱序后与顺序版不同的概率应很高（对 500 字/词来说几乎为 1）。
        for &set in &BUILTIN_SETS {
            let ordered = load_builtin_text(set);
            let shuffled = load_builtin_text_shuffled(set);
            assert_ne!(
                ordered.content, shuffled.content,
                "{} 乱序后内容与顺序版完全相同（极低概率，可能乱序未生效）",
                set.name()
            );
        }
    }
}
