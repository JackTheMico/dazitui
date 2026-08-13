use std::path::Path;

mod session;

#[cfg(feature = "online")]
mod online;

pub use session::{CharStatus, Session, Stats, TypeResult};

#[cfg(feature = "online")]
pub use online::client::{ApiClient, ApiError, CompetitionText, LoginResult, RankResult};
#[cfg(feature = "online")]
pub use online::protocol::{ProtocolError, build_request, decrypt, encrypt, parse_json};
#[cfg(feature = "online")]
pub use online::share::{UploadStats, format_share_text, to_upload_stats};
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

/// 赛文来源：本地文件或 52dazi.cn 在线比赛。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextSource {
    /// 本地文件。
    File,
    /// 52dazi.cn 在线赛文。
    Online { competition_type: CompetitionType },
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
}
