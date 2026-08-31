use std::path::Path;
use std::time::Duration;

mod code_hint;
mod db;
mod lttb;
mod scheme;
mod segmenter;
mod session;
mod settings;

#[cfg(feature = "online")]
mod online;

pub use code_hint::{
    HintCell, HintHand, hint_cell_widths, layout_code_hint_grid, layout_code_hint_line,
    pack_words_by_width,
};
pub use db::{
    DbError, DbTask, DbWorker, ErrorRecordItem, GlobalStatsSummary, KeypressRecordItem,
    MistypedCharStat, MistypedWordStat, SessionRecord, StatsDb,
};
pub use lttb::lttb_downsample;
pub use scheme::{
    ChordAlgebra, CodeHint, RimeSchemaResolver, SchemeDict, SchemeInfo, YamlValue,
    default_rime_data_dir, discover_schemes, parse_rime_yaml, resolve_scheme_path_via_discovery,
};
pub use segmenter::{WordIndex, WordToken, prewarm_segmenter};
pub use session::{CharStatus, ErrorPoint, ErrorType, GROUP_SIZE, Session, Stats, TypeResult};
pub use settings::{
    BuiltinProgress, FONT_SIZE_PT, HeatmapLayout, KeyboardMode, RankColumnConfig, RankColumnId,
    Rgb, Settings, SettingsStore, Theme, ThemePreset, normalize_scheme_to_id,
    osc_font_size_sequence,
};

#[cfg(feature = "online")]
pub use online::auth::{env_credentials, is_auth_failure, should_auto_relogin};
#[cfg(feature = "online")]
pub use online::client::{
    ApiClient, ApiError, CompetitionRank, CompetitionRankRow, CompetitionText, LoginResult,
    RankResult, UploadOutcome, today_ymd,
};
#[cfg(feature = "online")]
pub use online::protocol::{ProtocolError, build_request, decrypt, encrypt, parse_json};
#[cfg(feature = "online")]
pub use online::share::{UploadStats, build_upload_payload, osc52_clipboard, to_upload_stats};
#[cfg(feature = "online")]
pub use online::token::{AuthSession, TokenStore};

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

impl Text {
    /// 返回用于 Session 组边界计算的词组边界。
    ///
    /// 乱序词组赛文使用 `Text.word_boundaries`；顺序词组赛文回退到 `BuiltinSet::word_boundaries()`。
    /// 非词组赛文返回空 Vec（Session 回退到单字逻辑：每组 GROUP_SIZE 字）。
    pub fn session_word_boundaries(&self) -> Vec<(usize, usize)> {
        if let Some(b) = &self.word_boundaries
            && !b.is_empty()
        {
            return b.clone();
        }
        if let TextSource::Builtin { set } = self.source
            && set.is_words()
        {
            return set.word_boundaries();
        }
        Vec::new()
    }

    /// 构建当前赛文的分词倒排索引（支持内置词组赛文原生词边界与通用/在线赛文 Jieba 分词）。
    pub fn build_word_index(&self) -> WordIndex {
        let is_builtin_words = match self.source {
            TextSource::Builtin { set } => set.is_words(),
            _ => false,
        };
        WordIndex::build(&self.content, is_builtin_words)
    }
}

/// 赛文来源：本地文件、自由输入、剪贴板、内置赛文或 52dazi.cn 在线比赛。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextSource {
    /// 本地文件。
    #[default]
    File,
    /// 自由输入赛文。
    Custom,
    /// 剪贴板赛文。
    Clipboard,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// yoyo 方案词典抽取的全部唯一单字（约 6640 字，即社区常说的「6636 单字无重」）。
    YoyoChars,
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
            Self::YoyoChars => "yoyo 单字",
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
            Self::YoyoChars => include_str!("../data/yoyo-chars.txt"),
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
pub const BUILTIN_SETS: [BuiltinSet; 7] = [
    BuiltinSet::CommonCharsQian,
    BuiltinSet::CommonCharsZhong,
    BuiltinSet::CommonCharsHou,
    BuiltinSet::CommonWordsQian,
    BuiltinSet::CommonWordsZhong,
    BuiltinSet::CommonWordsHou,
    BuiltinSet::YoyoChars,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

    /// UI 中三比赛的固定展示顺序（极速杯 → 锦标赛 → 键神杯）。
    pub const ALL: [CompetitionType; 3] = [Self::Jisu, Self::Jinbiao, Self::Jianshen];

    /// 在 `ALL` 顺序中前进到下一比赛（到末尾回环到开头）。
    pub fn next(&self) -> Self {
        let i = Self::ALL.iter().position(|c| c == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// 在 `ALL` 顺序中后退到上一比赛（到开头回环到末尾）。
    pub fn prev(&self) -> Self {
        let i = Self::ALL.iter().position(|c| c == self).unwrap_or(0);
        Self::ALL[(i + Self::ALL.len() - 1) % Self::ALL.len()]
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
    let title = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    load_text_from_string(&title, content, TextSource::File, options)
}

/// 从字符串内容载入赛文，按选项处理内容。
pub fn load_text_from_string(
    title: &str,
    raw: String,
    source: TextSource,
    options: &LoadOptions,
) -> Result<Text, LoadError> {
    let content = process_content(raw, options)?;
    Ok(Text {
        title: title.to_string(),
        content,
        source,
        word_boundaries: None,
        shuffled: false,
    })
}

/// 读取系统剪贴板的纯文本内容。
pub fn read_clipboard_text() -> Result<String, LoadError> {
    let mut clipboard = arboard::Clipboard::new().map_err(|_| LoadError::ReadFailed)?;
    let text = clipboard.get_text().map_err(|_| LoadError::ReadFailed)?;
    if text.is_empty() {
        return Err(LoadError::Empty);
    }
    Ok(text)
}

/// 从系统剪贴板载入赛文，按选项处理内容。
pub fn load_text_from_clipboard(options: &LoadOptions) -> Result<Text, LoadError> {
    let raw = read_clipboard_text()?;
    load_text_from_string("剪贴板赛文", raw, TextSource::Clipboard, options)
}

/// 将赛文保存到本地文件。
pub fn save_text_to_file(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// 去除字符串中所有空白字符（半角空格、制表符、换行、全角空格 U+3000 等），并去掉首尾空白。
///
/// 被 `process_content` 与 `normalize_online_content` 共用，避免去空白逻辑重复。
fn strip_whitespace_chars(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .trim()
        .to_string()
}

/// 按选项处理原文：去空格、去标点、去首尾空白；处理为空则报 Empty。
fn process_content(raw: String, options: &LoadOptions) -> Result<String, LoadError> {
    if raw.is_empty() {
        return Err(LoadError::Empty);
    }
    let mut content = raw;
    if options.strip_whitespace {
        content = strip_whitespace_chars(&content);
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

/// 在线赛文内容归一化：去除所有空白字符（半角空格、制表符、换行、全角空格 U+3000 等）。
///
/// 52dazi 部分赛文内容会带词间空格（如「经典 造型」「智能 手表 的 科技 感」），
/// 跟打时用户不需要、也不应输入空格；空白还会破坏遍码提示的分词（jieba 会按空格断开）。
/// 故载文时统一去除，得到连续中文正文。标题为元数据，不在此处理。
///
/// 仅做去空白，不做空内容校验（空校验由调用方在载文路径决定如何处理）。
pub fn normalize_online_content(content: &str) -> String {
    strip_whitespace_chars(content)
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

/// 用时格式化为 `MM:SS.sss`（与前端 `formatTime` 一致，秒保留 3 位小数）。
pub fn format_time(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    let minutes = (secs / 60.0).floor() as u64;
    let seconds = secs - (minutes as f64) * 60.0;
    format!("{minutes:02}:{seconds:06.3}")
}

/// 输入法分享文本后缀：空输入法返回空串，否则返回 ` · <输入法>`（在线分享与统计复制共用）。
fn input_method_suffix(input_method: &str) -> String {
    if input_method.is_empty() {
        String::new()
    } else {
        format!(" · {input_method}")
    }
}

/// 52dazi 官方键准（击键准确率）百分比：
/// `(总击数 - 退格数 - 回改次数 * 码长) / 总击数 * 100`，与 `online/share.rs` 上传字段口径一致。
/// 退格数优先取按键频率中的 `Backspace`，回退到回改次数。
pub fn key_accuracy_pct(stats: &Stats) -> f64 {
    let total_keys: u32 = stats.key_frequency.iter().map(|(_, n)| n).sum();
    let backspace: u32 = stats
        .key_frequency
        .iter()
        .find(|(k, _)| k == "Backspace")
        .map(|(_, n)| *n)
        .unwrap_or(stats.edits);
    let strokes = if stats.total_strokes > 0 {
        stats.total_strokes
    } else {
        total_keys
    };
    if stats.typed_chars == 0 || strokes == 0 {
        return 0.0;
    }
    let wasted_keys = backspace as f64 + (stats.edits as f64) * stats.key_length;
    let valid_keys = (strokes as f64 - wasted_keys).max(0.0);
    ((valid_keys / strokes as f64) * 100.0).clamp(0.0, 100.0)
}

/// 52dazi 官方打词率百分比：`打词字符数 / 赛文总字数 * 100`。
pub fn word_ratio_pct(text: &Text, stats: &Stats) -> f64 {
    let total_chars = text.content.chars().count();
    if total_chars == 0 {
        return 0.0;
    }
    (stats.phrase_chars as f64 / total_chars as f64 * 100.0).clamp(0.0, 100.0)
}

/// 把跟打统计结果格式化为单行分享文本（复制到剪贴板）。
///
/// 离线赛文、自由发文、剪贴板发文、内置赛文与在线赛文（比赛）统一使用本函数，
/// 保证各来源的复制结果口径一致：每个指标前加 emoji，并补充 回改 / 键数 / 键准 / 打词率；
/// `rank` 为 `Some(n)` 时（在线比赛上传成功）在来源名后追加 ` 第n名`；
/// 末尾保留输入法名并固定追加设备 `🖥️dazitui`。
pub fn format_stats_share_text(
    text: &Text,
    stats: &Stats,
    elapsed: Duration,
    input_method: &str,
    rank: Option<u32>,
) -> String {
    let source = match text.source {
        TextSource::File => "离线赛文",
        TextSource::Custom => "自由发文",
        TextSource::Clipboard => "剪贴板",
        TextSource::Builtin { set } => set.name(),
        TextSource::Online { competition_type } => competition_type.name(),
    };
    let rank_part = rank.map(|r| format!(" 第{r}名")).unwrap_or_default();
    let total_chars = text.content.chars().count();
    let strokes = if stats.total_strokes > 0 {
        stats.total_strokes
    } else {
        stats.key_frequency.iter().map(|(_, n)| n).sum()
    };
    let accuracy = key_accuracy_pct(stats);
    let word_ratio = word_ratio_pct(text, stats);
    let device_suffix = format!("{}{}", input_method_suffix(input_method), " 🖥️dazitui");
    format!(
        "{source}{rank_part}《{}》 · 🚀WPM {:.1} · ⌨️击键 {:.1} · 📏码长 {:.1} · ✅正确字数 {}/{} · ❌错字 {} · ↩️回改 {} · 🔢键数 {} · 🎯键准 {:.2}% · 💬打词率 {:.2}% · ⏱️用时 {}{}",
        text.title,
        stats.wpm,
        stats.kps,
        stats.key_length,
        stats.correct_chars,
        total_chars,
        stats.wrong_total,
        stats.edits,
        strokes,
        accuracy,
        word_ratio,
        format_time(elapsed),
        device_suffix
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
    fn normalize_online_content_strips_spaces_between_words() {
        // 52dazi 词间空格（如「经典 造型」）应被去除，得到连续正文。
        let raw = "智能 手表 的 科技 感 ， 复古 腕表 以 简约 设计 、 经典 造型";
        assert_eq!(
            normalize_online_content(raw),
            "智能手表的科技感，复古腕表以简约设计、经典造型"
        );
    }

    #[test]
    fn normalize_online_content_strips_newlines_tabs_and_fullwidth_space() {
        // 换行、制表符、全角空格（U+3000）等所有 Unicode 空白都应去除。
        let raw = "春眠不觉晓\t处处闻啼鸟\n夜来风雨声　花落知多少";
        assert_eq!(
            normalize_online_content(raw),
            "春眠不觉晓处处闻啼鸟夜来风雨声花落知多少"
        );
    }

    #[test]
    fn normalize_online_content_keeps_punctuation() {
        // 标点（逗号、顿号、句号）属于正文需输入，不应被当作空白去除。
        let raw = "你好 ， 世界 、 加油 。";
        assert_eq!(normalize_online_content(raw), "你好，世界、加油。");
    }

    #[test]
    fn normalize_online_content_trims_leading_trailing_space() {
        let raw = "  你好世界  ";
        assert_eq!(normalize_online_content(raw), "你好世界");
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
                boundaries.last().unwrap().1,
                char_count,
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
                orig_sorted,
                shuf_sorted,
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
                orig_sorted,
                shuf_sorted,
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
                prev_end,
                total_chars,
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
                ordered.content,
                shuffled.content,
                "{} 乱序后内容与顺序版完全相同（极低概率，可能乱序未生效）",
                set.name()
            );
        }
    }

    #[test]
    fn load_text_from_string_with_custom_and_clipboard_sources() {
        let opts = LoadOptions::default();
        let custom_text = load_text_from_string(
            "自由发文",
            "这是自定义赛文。\n换行测试。".to_string(),
            TextSource::Custom,
            &opts,
        )
        .expect("自定义文本载入应成功");
        assert_eq!(custom_text.title, "自由发文");
        assert_eq!(custom_text.content, "这是自定义赛文。\n换行测试。");
        assert_eq!(custom_text.source, TextSource::Custom);
        assert!(!custom_text.is_online());
        assert!(!custom_text.source.is_builtin());

        let clipboard_text = load_text_from_string(
            "剪贴板赛文",
            "剪贴板内容".to_string(),
            TextSource::Clipboard,
            &opts,
        )
        .expect("剪贴板文本载入应成功");
        assert_eq!(clipboard_text.title, "剪贴板赛文");
        assert_eq!(clipboard_text.content, "剪贴板内容");
        assert_eq!(clipboard_text.source, TextSource::Clipboard);
        assert!(!clipboard_text.is_online());
        assert!(!clipboard_text.source.is_builtin());
    }

    #[test]
    fn load_text_from_string_options_filter() {
        let opts = LoadOptions {
            strip_whitespace: true,
            strip_punctuation: true,
        };
        let text = load_text_from_string(
            "测试",
            " 你好， 世界！ \n 标点。 ".to_string(),
            TextSource::Custom,
            &opts,
        )
        .expect("清洗应成功");
        assert_eq!(text.content, "你好世界标点");

        let empty_err =
            load_text_from_string("空测试", "   \n\t  ".to_string(), TextSource::Custom, &opts)
                .expect_err("空白文本应返回 Empty 错误");
        assert_eq!(empty_err, LoadError::Empty);
    }

    #[test]
    fn save_text_to_file_creates_file_and_parent_dirs() {
        let path = temp_path("custom_sub/nested/test.txt");
        let content = "自定义内容\n第二行";
        save_text_to_file(&path, content).expect("保存应成功");

        let read_back = fs::read_to_string(&path).expect("读取应成功");
        assert_eq!(read_back, content);

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir(parent);
            if let Some(grand) = parent.parent() {
                let _ = fs::remove_dir(grand);
            }
        }
    }

    fn sample_stats() -> Stats {
        Stats {
            wpm: 85.2,
            kps: 3.5,
            key_length: 2.8,
            total_strokes: 140,
            correct_chars: 3,
            wrong_chars: 1,
            edits: 0,
            wrong_total: 1,
            typed_chars: 4,
            phrase_chars: 0,
            key_frequency: vec![],
            edit_details: vec![],
            speed_samples: vec![],
            kps_samples: vec![],
            error_points: vec![],
        }
    }

    #[test]
    fn format_time_minutes_seconds_millis() {
        assert_eq!(format_time(Duration::from_secs_f64(85.23)), "01:25.230");
        assert_eq!(format_time(Duration::from_secs(5)), "00:05.000");
        assert_eq!(format_time(Duration::ZERO), "00:00.000");
    }

    #[test]
    fn stats_share_text_file_source_full_line() {
        let text = Text {
            title: "背影节选".into(),
            content: "你好世界".into(),
            source: TextSource::File,
            word_boundaries: None,
            shuffled: false,
        };
        let stats = sample_stats();
        let s =
            format_stats_share_text(&text, &stats, Duration::from_secs_f64(85.23), "虎码", None);
        assert_eq!(
            s,
            "离线赛文《背影节选》 · 🚀WPM 85.2 · ⌨️击键 3.5 · 📏码长 2.8 · ✅正确字数 3/4 · ❌错字 1 · ↩️回改 0 · 🔢键数 140 · 🎯键准 100.00% · 💬打词率 0.00% · ⏱️用时 01:25.230 · 虎码 🖥️dazitui"
        );
    }

    #[test]
    fn stats_share_text_custom_source_omits_empty_input_method() {
        let text = Text {
            title: "日常练习".into(),
            content: "你好世界".into(),
            source: TextSource::Custom,
            word_boundaries: None,
            shuffled: false,
        };
        let stats = sample_stats();
        let s = format_stats_share_text(&text, &stats, Duration::from_secs(60), "", None);
        assert_eq!(
            s,
            "自由发文《日常练习》 · 🚀WPM 85.2 · ⌨️击键 3.5 · 📏码长 2.8 · ✅正确字数 3/4 · ❌错字 1 · ↩️回改 0 · 🔢键数 140 · 🎯键准 100.00% · 💬打词率 0.00% · ⏱️用时 01:00.000 🖥️dazitui"
        );
    }

    #[test]
    fn stats_share_text_online_with_rank_matches_offline_format() {
        // 在线比赛上传成功：排名追加在来源名后，且指标口径与离线完全一致
        // （含 🎯键准 / ↩️回改），不能像旧版那样只剩 WPM/击键/码长。
        let text = Text {
            title: "锦标赛第3279期".into(),
            content: "你好世界".into(),
            source: TextSource::Online {
                competition_type: CompetitionType::Jinbiao,
            },
            word_boundaries: None,
            shuffled: false,
        };
        let stats = sample_stats();
        let s = format_stats_share_text(&text, &stats, Duration::from_secs(60), "虎码", Some(5));
        assert_eq!(
            s,
            "锦标赛 第5名《锦标赛第3279期》 · 🚀WPM 85.2 · ⌨️击键 3.5 · 📏码长 2.8 · ✅正确字数 3/4 · ❌错字 1 · ↩️回改 0 · 🔢键数 140 · 🎯键准 100.00% · 💬打词率 0.00% · ⏱️用时 01:00.000 · 虎码 🖥️dazitui"
        );
        // 与离线格式一致的关键指标必须存在
        assert!(s.contains("🎯键准"), "在线复制结果必须含键准");
        assert!(s.contains("↩️回改"), "在线复制结果必须含回改");
    }
}
