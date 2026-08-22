use std::path::Path;

mod session;
mod settings;

#[cfg(feature = "online")]
mod online;

pub use session::{CharStatus, Session, Stats, TypeResult};
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
}

/// 赛文来源：本地文件、内置赛文或 52dazi.cn 在线比赛。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextSource {
    /// 本地文件。
    File,
    /// 内置赛文（随二进制分发的练习材料，如常用单字）。
    Builtin { set: BuiltinSet },
    /// 52dazi.cn 在线赛文。
    Online { competition_type: CompetitionType },
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
pub fn load_builtin_text(set: BuiltinSet) -> Text {
    Text {
        title: set.name().to_string(),
        content: set.content().replace(['\n', '\r'], ""),
        source: TextSource::Builtin { set },
    }
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
        };
        let online_text = Text {
            title: "o".into(),
            content: "c".into(),
            source: TextSource::Online {
                competition_type: CompetitionType::Jisu,
            },
        };
        assert!(!file_text.is_online());
        assert!(online_text.is_online());
    }
}
