//! 界面外观设置：主题配色 + 区域大小 + 字体开关。
//!
//! 纯数据 + 纯函数 + 极简 key=value 文件读写，零 ratatui/网络依赖，可完全单测。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// 内置主题预设。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePreset {
    /// Catppuccin Mocha（默认）。
    #[default]
    CatppuccinMocha,
    /// Tokyo Night。
    TokyoNight,
    /// Nord。
    Nord,
    /// Dracula。
    Dracula,
    /// Gruvbox（dark）。
    Gruvbox,
    /// Rosé Pine。
    RosePine,
    /// Kanagawa。
    Kanagawa,
    /// One Dark。
    OneDark,
}

impl ThemePreset {
    /// 全部预设，按设置视图展示顺序排列。
    pub const ALL: [ThemePreset; 8] = [
        ThemePreset::CatppuccinMocha,
        ThemePreset::TokyoNight,
        ThemePreset::Nord,
        ThemePreset::Dracula,
        ThemePreset::Gruvbox,
        ThemePreset::RosePine,
        ThemePreset::Kanagawa,
        ThemePreset::OneDark,
    ];

    /// 预设显示名（用于设置视图）。
    pub fn name(&self) -> &'static str {
        match self {
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::TokyoNight => "Tokyo Night",
            Self::Nord => "Nord",
            Self::Dracula => "Dracula",
            Self::Gruvbox => "Gruvbox Dark",
            Self::RosePine => "Rosé Pine",
            Self::Kanagawa => "Kanagawa",
            Self::OneDark => "One Dark",
        }
    }

    /// 序列化标识（用于 settings 文件）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CatppuccinMocha => "catppuccin-mocha",
            Self::TokyoNight => "tokyo-night",
            Self::Nord => "nord",
            Self::Dracula => "dracula",
            Self::Gruvbox => "gruvbox",
            Self::RosePine => "rose-pine",
            Self::Kanagawa => "kanagawa",
            Self::OneDark => "one-dark",
        }
    }

    /// 从字符串解析（忽略大小写与首尾空白）；未知值返回 `None`。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "default" | "catppuccin" | "catppuccin-mocha" => Some(Self::CatppuccinMocha),
            "tokyo-night" | "tokyonight" => Some(Self::TokyoNight),
            "nord" => Some(Self::Nord),
            "dracula" => Some(Self::Dracula),
            "gruvbox" | "gruvbox-dark" => Some(Self::Gruvbox),
            "rose-pine" | "rosepine" => Some(Self::RosePine),
            "kanagawa" => Some(Self::Kanagawa),
            "one-dark" | "onedark" => Some(Self::OneDark),
            _ => None,
        }
    }

    /// 在全部预设中的下标。
    fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    /// 下一个预设（循环）。
    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// 上一个预设（循环）。
    pub fn prev(self) -> Self {
        let len = Self::ALL.len();
        Self::ALL[(self.index() + len - 1) % len]
    }
}

/// RGB 颜色三元组（0–255）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// 语义色主题：6 个语义槽位，对应界面上不同角色的颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// 默认文本 / 边框基色。
    pub text: Rgb,
    /// 已打对（原绿）。
    pub correct: Rgb,
    /// 已打错（原红）。
    pub wrong: Rgb,
    /// 成功 / 已登录 / 强调（原绿强调）。
    pub accent: Rgb,
    /// 警告 / 上传中 / 待登录（原黄）。
    pub warn: Rgb,
    /// 次要文本（原灰）。
    pub muted: Rgb,
}

impl Theme {
    /// 返回某预设的内置调色板。
    pub fn preset(p: ThemePreset) -> Self {
        match p {
            ThemePreset::CatppuccinMocha => Self {
                text: Rgb(0xcd, 0xd6, 0xf4),
                correct: Rgb(0xa6, 0xe3, 0xa1),
                wrong: Rgb(0xf3, 0x8b, 0xa8),
                accent: Rgb(0x89, 0xb4, 0xfa),
                warn: Rgb(0xf9, 0xe2, 0xaf),
                muted: Rgb(0x6c, 0x70, 0x86),
            },
            ThemePreset::TokyoNight => Self {
                text: Rgb(192, 202, 245),
                correct: Rgb(158, 206, 106),
                wrong: Rgb(247, 118, 142),
                accent: Rgb(122, 162, 247),
                warn: Rgb(224, 175, 104),
                muted: Rgb(86, 95, 137),
            },
            ThemePreset::Nord => Self {
                text: Rgb(236, 239, 244),
                correct: Rgb(163, 190, 140),
                wrong: Rgb(191, 97, 106),
                accent: Rgb(136, 192, 208),
                warn: Rgb(235, 203, 139),
                muted: Rgb(76, 86, 106),
            },
            ThemePreset::Dracula => Self {
                text: Rgb(0xf8, 0xf8, 0xf2),
                correct: Rgb(0x50, 0xfa, 0x7b),
                wrong: Rgb(0xff, 0x55, 0x55),
                accent: Rgb(0xbd, 0x93, 0xf9),
                warn: Rgb(0xf1, 0xfa, 0x8c),
                muted: Rgb(0x62, 0x72, 0xa4),
            },
            ThemePreset::Gruvbox => Self {
                text: Rgb(0xeb, 0xdb, 0xb2),
                correct: Rgb(0xb8, 0xbb, 0x26),
                wrong: Rgb(0xfb, 0x49, 0x34),
                accent: Rgb(0x83, 0xa5, 0x98),
                warn: Rgb(0xfa, 0xbd, 0x2f),
                muted: Rgb(0x92, 0x83, 0x74),
            },
            ThemePreset::RosePine => Self {
                text: Rgb(224, 222, 244),
                correct: Rgb(156, 207, 216),
                wrong: Rgb(235, 111, 146),
                accent: Rgb(235, 188, 186),
                warn: Rgb(246, 193, 119),
                muted: Rgb(110, 106, 134),
            },
            ThemePreset::Kanagawa => Self {
                text: Rgb(220, 215, 186),
                correct: Rgb(118, 148, 106),
                wrong: Rgb(195, 64, 67),
                accent: Rgb(126, 156, 216),
                warn: Rgb(255, 160, 102),
                muted: Rgb(114, 113, 105),
            },
            ThemePreset::OneDark => Self {
                text: Rgb(171, 178, 191),
                correct: Rgb(152, 195, 121),
                wrong: Rgb(224, 108, 117),
                accent: Rgb(97, 175, 239),
                warn: Rgb(229, 192, 123),
                muted: Rgb(92, 99, 112),
            },
        }
    }
}

/// 实时键盘显示模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyboardMode {
    /// 关闭（默认）。
    #[default]
    Off,
    /// 标准斜列（ANSI 60%）。
    Staggered,
    /// 直列矩阵（Planck 4x12）。
    Ortholinear,
}

impl KeyboardMode {
    /// 全部模式，按设置视图展示顺序排列。
    pub const ALL: [KeyboardMode; 3] = [
        KeyboardMode::Off,
        KeyboardMode::Staggered,
        KeyboardMode::Ortholinear,
    ];

    /// 模式显示名（用于设置视图）。
    pub fn name(&self) -> &'static str {
        match self {
            Self::Off => "关闭",
            Self::Staggered => "标准斜列 (ANSI 60%)",
            Self::Ortholinear => "直列矩阵 (Planck 4x12)",
        }
    }

    /// 序列化标识（用于 settings 文件）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Staggered => "staggered",
            Self::Ortholinear => "ortholinear",
        }
    }

    /// 从字符串解析（忽略大小写与首尾空白）；未知值返回 `None`。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "false" | "0" | "disabled" => Some(Self::Off),
            "staggered" | "standard" | "ansi" => Some(Self::Staggered),
            "ortholinear" | "ortho" | "matrix" | "planck" => Some(Self::Ortholinear),
            _ => None,
        }
    }

    /// 在全部预设中的下标。
    fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    /// 下一个模式（循环）。
    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// 上一个模式（循环）。
    pub fn prev(self) -> Self {
        let len = Self::ALL.len();
        Self::ALL[(self.index() + len - 1) % len]
    }

    /// 是否开启显示。
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// 应用外观设置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// 主题预设。
    pub theme: ThemePreset,
    /// 对照区占比（%）。
    pub reference_ratio: u8,
    /// 粗体开关。
    pub bold: bool,
    /// 字体设置开关（尽力而为的 OSC 尝试）。
    pub font: bool,
    /// 实时键盘显示模式。
    pub keyboard_mode: KeyboardMode,
    /// 输入法名称（上传与分享携带）；空串表示不配置（显示「无」）。
    /// 最多 20 字符（遵守 52dazi 协议限制）。
    pub input_method: String,
    /// 自定义方案码表映射：输入法方案名 -> 码表文件绝对/相对路径。
    pub scheme_dict_paths: HashMap<String, String>,
}

impl Settings {
    /// 对照区占比的合法下限（%）。
    pub const RATIO_MIN: u8 = 30;
    /// 对照区占比的合法上限（%）。
    pub const RATIO_MAX: u8 = 80;
    /// 输入法名称的最大长度（字符数，遵守 52dazi 协议限制）。
    pub const INPUT_METHOD_MAX_CHARS: usize = 20;

    /// 校验并修正占比到合法范围。
    pub fn clamp_ratio(ratio: u8) -> u8 {
        ratio.clamp(Self::RATIO_MIN, Self::RATIO_MAX)
    }

    /// 截断输入法名称到最多 20 字符；空白字符串视为空串。
    pub fn clamp_input_method(s: &str) -> String {
        let trimmed = s.trim();
        trimmed.chars().take(Self::INPUT_METHOD_MAX_CHARS).collect()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePreset::CatppuccinMocha,
            reference_ratio: 62,
            bold: false,
            font: false,
            keyboard_mode: KeyboardMode::Off,
            input_method: String::new(),
            scheme_dict_paths: HashMap::new(),
        }
    }
}

/// 字体开关对应的字号（pt）。
pub const FONT_SIZE_PT: u16 = 16;

/// 生成 kitty 兼容的 OSC 50 字号设置序列（尽力而为，仅 kitty 等少数终端支持）。
///
/// 返回形如 `\x1b]50;font_size=<size>\x07` 的字节序列；不支持的终端会静默忽略。
pub fn osc_font_size_sequence(size: u16) -> String {
    format!("\x1b]50;font_size={size}\x07")
}

/// 设置文件读写（极简 key=value 格式，无 serde）。
///
/// 文件缺失或损坏时回退到 `Settings::default()`；单个字段损坏仅该字段回退默认。
#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    /// 默认存储路径：`~/.config/dazitui/settings`。
    pub fn with_default_path() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::new(home.join(".config").join("dazitui").join("settings"))
    }

    /// 指定路径的存储。
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 存储路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 保存设置（自动创建父目录）。
    pub fn save(&self, settings: &Settings) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut content = format!(
            "theme={}\nreference_ratio={}\nbold={}\nfont={}\nkeyboard_mode={}\ninput_method={}\n",
            settings.theme.as_str(),
            settings.reference_ratio,
            settings.bold,
            settings.font,
            settings.keyboard_mode.as_str(),
            settings.input_method,
        );
        for (scheme, path) in &settings.scheme_dict_paths {
            content.push_str(&format!("scheme_dict.{}={}\n", scheme, path));
        }
        std::fs::write(&self.path, content)
    }

    /// 读取设置；文件缺失或损坏回退默认值。
    pub fn load(&self) -> Settings {
        let mut settings = Settings::default();
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return settings;
        };
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "theme" => {
                    if let Some(preset) = ThemePreset::parse(value) {
                        settings.theme = preset;
                    }
                }
                "reference_ratio" => {
                    if let Ok(ratio) = value.parse::<u8>() {
                        settings.reference_ratio = Settings::clamp_ratio(ratio);
                    }
                }
                "bold" => settings.bold = value == "true",
                "font" => settings.font = value == "true",
                "keyboard_mode" => {
                    if let Some(mode) = KeyboardMode::parse(value) {
                        settings.keyboard_mode = mode;
                    }
                }
                "input_method" => {
                    settings.input_method = Settings::clamp_input_method(value);
                }
                _ => {
                    if let Some(scheme) = key
                        .strip_prefix("scheme_dict.")
                        .filter(|s| !s.is_empty() && !value.is_empty())
                    {
                        settings
                            .scheme_dict_paths
                            .insert(scheme.to_string(), value.to_string());
                    }
                }
            }
        }
        settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(suffix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("dazitui-settings-{stamp}-{suffix}"))
    }

    #[test]
    fn preset_catppuccin_rgb_exact() {
        let t = Theme::preset(ThemePreset::CatppuccinMocha);
        assert_eq!(t.text, Rgb(0xcd, 0xd6, 0xf4));
        assert_eq!(t.correct, Rgb(0xa6, 0xe3, 0xa1));
        assert_eq!(t.wrong, Rgb(0xf3, 0x8b, 0xa8));
        assert_eq!(t.accent, Rgb(0x89, 0xb4, 0xfa));
        assert_eq!(t.warn, Rgb(0xf9, 0xe2, 0xaf));
        assert_eq!(t.muted, Rgb(0x6c, 0x70, 0x86));
    }

    #[test]
    fn preset_dracula_rgb_exact() {
        let t = Theme::preset(ThemePreset::Dracula);
        assert_eq!(t.text, Rgb(0xf8, 0xf8, 0xf2));
        assert_eq!(t.correct, Rgb(0x50, 0xfa, 0x7b));
        assert_eq!(t.wrong, Rgb(0xff, 0x55, 0x55));
        assert_eq!(t.accent, Rgb(0xbd, 0x93, 0xf9));
        assert_eq!(t.warn, Rgb(0xf1, 0xfa, 0x8c));
        assert_eq!(t.muted, Rgb(0x62, 0x72, 0xa4));
    }

    #[test]
    fn preset_gruvbox_rgb_exact() {
        let t = Theme::preset(ThemePreset::Gruvbox);
        assert_eq!(t.text, Rgb(0xeb, 0xdb, 0xb2));
        assert_eq!(t.correct, Rgb(0xb8, 0xbb, 0x26));
        assert_eq!(t.wrong, Rgb(0xfb, 0x49, 0x34));
        assert_eq!(t.accent, Rgb(0x83, 0xa5, 0x98));
        assert_eq!(t.warn, Rgb(0xfa, 0xbd, 0x2f));
        assert_eq!(t.muted, Rgb(0x92, 0x83, 0x74));
    }

    #[test]
    fn preset_tokyo_night_rgb_exact() {
        let t = Theme::preset(ThemePreset::TokyoNight);
        assert_eq!(t.text, Rgb(192, 202, 245));
        assert_eq!(t.correct, Rgb(158, 206, 106));
        assert_eq!(t.wrong, Rgb(247, 118, 142));
        assert_eq!(t.accent, Rgb(122, 162, 247));
        assert_eq!(t.warn, Rgb(224, 175, 104));
        assert_eq!(t.muted, Rgb(86, 95, 137));
    }

    #[test]
    fn preset_nord_rgb_exact() {
        let t = Theme::preset(ThemePreset::Nord);
        assert_eq!(t.text, Rgb(236, 239, 244));
        assert_eq!(t.correct, Rgb(163, 190, 140));
        assert_eq!(t.wrong, Rgb(191, 97, 106));
        assert_eq!(t.accent, Rgb(136, 192, 208));
        assert_eq!(t.warn, Rgb(235, 203, 139));
        assert_eq!(t.muted, Rgb(76, 86, 106));
    }

    #[test]
    fn preset_parse_roundtrip_and_case_insensitive() {
        for p in ThemePreset::ALL {
            assert_eq!(ThemePreset::parse(p.as_str()), Some(p));
        }
        assert_eq!(
            ThemePreset::parse("  CATPPUCCIN "),
            Some(ThemePreset::CatppuccinMocha)
        );
        assert_eq!(
            ThemePreset::parse("default"),
            Some(ThemePreset::CatppuccinMocha)
        );
        assert_eq!(
            ThemePreset::parse("TOKYO_NIGHT"),
            Some(ThemePreset::TokyoNight)
        );
        assert_eq!(ThemePreset::parse("solarized"), None);
        assert_eq!(ThemePreset::parse(""), None);
    }

    #[test]
    fn preset_next_and_prev_wrap_around() {
        assert_eq!(ThemePreset::CatppuccinMocha.next(), ThemePreset::TokyoNight);
        assert_eq!(ThemePreset::OneDark.next(), ThemePreset::CatppuccinMocha);
        assert_eq!(ThemePreset::CatppuccinMocha.prev(), ThemePreset::OneDark);
        assert_eq!(ThemePreset::TokyoNight.prev(), ThemePreset::CatppuccinMocha);
    }

    #[test]
    fn settings_defaults() {
        let s = Settings::default();
        assert_eq!(s.theme, ThemePreset::CatppuccinMocha);
        assert_eq!(s.reference_ratio, 62);
        assert!(!s.bold);
        assert!(!s.font);
        assert_eq!(s.input_method, "");
    }

    #[test]
    fn clamp_ratio_bounds() {
        assert_eq!(Settings::clamp_ratio(30), 30);
        assert_eq!(Settings::clamp_ratio(80), 80);
        assert_eq!(Settings::clamp_ratio(0), 30);
        assert_eq!(Settings::clamp_ratio(100), 80);
        assert_eq!(Settings::clamp_ratio(62), 62);
    }

    #[test]
    fn clamp_input_method_truncates_to_20_chars() {
        // 21 个汉字 → 截断到 20
        let long = "虎码虎码虎码虎码虎码虎码虎码虎码虎码虎码虎";
        let result = Settings::clamp_input_method(long);
        assert_eq!(result.chars().count(), 20);
        // 20 个字符原样保留
        let exact = "虎码虎码虎码虎码虎码虎码虎码虎码虎码虎码";
        assert_eq!(Settings::clamp_input_method(exact), exact);
    }

    #[test]
    fn clamp_input_method_trims_whitespace_only_to_empty() {
        assert_eq!(Settings::clamp_input_method("   "), "");
        assert_eq!(Settings::clamp_input_method(""), "");
    }

    #[test]
    fn store_roundtrip() {
        let store = SettingsStore::new(temp_path("roundtrip"));
        let mut scheme_dict_paths = HashMap::new();
        scheme_dict_paths.insert("麓鸣并击".to_string(), "/path/to/luming.txt".to_string());
        let s = Settings {
            theme: ThemePreset::Dracula,
            reference_ratio: 70,
            bold: true,
            font: false,
            keyboard_mode: KeyboardMode::Staggered,
            input_method: "虎码".to_string(),
            scheme_dict_paths,
        };
        store.save(&s).unwrap();
        assert_eq!(store.load(), s);
        let _ = std::fs::remove_file(store.path());
    }

    #[test]
    fn store_missing_file_returns_defaults() {
        let store = SettingsStore::new(temp_path("missing"));
        assert_eq!(store.load(), Settings::default());
    }

    #[test]
    fn store_corrupted_lines_fall_back_per_field() {
        let store = SettingsStore::new(temp_path("corrupt"));
        std::fs::write(
            store.path(),
            "theme=gruvbox\nreference_ratio=not-a-number\nbold=maybe\nfont=true\nunknown=1\n",
        )
        .unwrap();
        let s = store.load();
        // 合法字段生效，损坏字段回退默认。
        assert_eq!(s.theme, ThemePreset::Gruvbox);
        assert_eq!(s.reference_ratio, 62);
        assert!(!s.bold);
        assert!(s.font);
        assert_eq!(s.input_method, ""); // 缺省时为空串
        let _ = std::fs::remove_file(store.path());
    }

    #[test]
    fn store_out_of_range_ratio_is_clamped() {
        let store = SettingsStore::new(temp_path("clamp"));
        std::fs::write(store.path(), "reference_ratio=100\n").unwrap();
        assert_eq!(store.load().reference_ratio, 80);
        std::fs::write(store.path(), "reference_ratio=10\n").unwrap();
        assert_eq!(store.load().reference_ratio, 30);
        let _ = std::fs::remove_file(store.path());
    }

    #[test]
    fn store_save_creates_parent_directories() {
        let dir = temp_path("nested");
        let store = SettingsStore::new(dir.join("sub").join("settings"));
        store.save(&Settings::default()).unwrap();
        assert_eq!(store.load(), Settings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_input_method_missing_key_defaults_to_empty() {
        // 旧 settings 文件不含 input_method 行，读取后应为空串
        let store = SettingsStore::new(temp_path("old_format"));
        std::fs::write(store.path(), "theme=default\nbold=false\nfont=false\n").unwrap();
        assert_eq!(store.load().input_method, "");
        let _ = std::fs::remove_file(store.path());
    }

    #[test]
    fn store_input_method_too_long_is_truncated() {
        let store = SettingsStore::new(temp_path("too_long"));
        // 21 字符的值存入文件
        let long = "虎码虎码虎码虎码虎码虎码虎码虎码虎码虎码虎";
        std::fs::write(store.path(), format!("input_method={long}\n")).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.input_method.chars().count(), 20);
        let _ = std::fs::remove_file(store.path());
    }

    #[test]
    fn store_input_method_roundtrip_chinese() {
        let store = SettingsStore::new(temp_path("im_roundtrip"));
        let s = Settings {
            input_method: "空明码并击".to_string(),
            ..Default::default()
        };
        store.save(&s).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.input_method, "空明码并击");
        let _ = std::fs::remove_file(store.path());
    }

    #[test]
    fn keyboard_mode_enum_properties() {
        assert_eq!(KeyboardMode::default(), KeyboardMode::Off);
        assert!(!KeyboardMode::Off.is_enabled());
        assert!(KeyboardMode::Staggered.is_enabled());
        assert!(KeyboardMode::Ortholinear.is_enabled());

        assert_eq!(KeyboardMode::Off.name(), "关闭");
        assert_eq!(KeyboardMode::Staggered.name(), "标准斜列 (ANSI 60%)");
        assert_eq!(KeyboardMode::Ortholinear.name(), "直列矩阵 (Planck 4x12)");

        assert_eq!(KeyboardMode::Off.as_str(), "off");
        assert_eq!(KeyboardMode::Staggered.as_str(), "staggered");
        assert_eq!(KeyboardMode::Ortholinear.as_str(), "ortholinear");

        assert_eq!(KeyboardMode::Off.next(), KeyboardMode::Staggered);
        assert_eq!(KeyboardMode::Staggered.next(), KeyboardMode::Ortholinear);
        assert_eq!(KeyboardMode::Ortholinear.next(), KeyboardMode::Off);

        assert_eq!(KeyboardMode::Off.prev(), KeyboardMode::Ortholinear);
        assert_eq!(KeyboardMode::Ortholinear.prev(), KeyboardMode::Staggered);
        assert_eq!(KeyboardMode::Staggered.prev(), KeyboardMode::Off);

        assert_eq!(KeyboardMode::parse("off"), Some(KeyboardMode::Off));
        assert_eq!(KeyboardMode::parse("none"), Some(KeyboardMode::Off));
        assert_eq!(KeyboardMode::parse("false"), Some(KeyboardMode::Off));
        assert_eq!(KeyboardMode::parse("staggered"), Some(KeyboardMode::Staggered));
        assert_eq!(KeyboardMode::parse("standard"), Some(KeyboardMode::Staggered));
        assert_eq!(KeyboardMode::parse("ansi"), Some(KeyboardMode::Staggered));
        assert_eq!(KeyboardMode::parse("ortholinear"), Some(KeyboardMode::Ortholinear));
        assert_eq!(KeyboardMode::parse("planck"), Some(KeyboardMode::Ortholinear));
        assert_eq!(KeyboardMode::parse("invalid"), None);
    }

    #[test]
    fn store_keyboard_mode_roundtrip() {
        let store = SettingsStore::new(temp_path("kb_roundtrip"));
        let s = Settings {
            keyboard_mode: KeyboardMode::Staggered,
            ..Default::default()
        };
        store.save(&s).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.keyboard_mode, KeyboardMode::Staggered);

        let s2 = Settings {
            keyboard_mode: KeyboardMode::Ortholinear,
            ..Default::default()
        };
        store.save(&s2).unwrap();
        let loaded2 = store.load();
        assert_eq!(loaded2.keyboard_mode, KeyboardMode::Ortholinear);
        let _ = std::fs::remove_file(store.path());
    }

    #[test]
    fn store_keyboard_mode_missing_defaults_to_off() {
        let store = SettingsStore::new(temp_path("kb_missing"));
        std::fs::write(store.path(), "theme=default\nreference_ratio=60\n").unwrap();
        assert_eq!(store.load().keyboard_mode, KeyboardMode::Off);
        let _ = std::fs::remove_file(store.path());
    }

    #[test]
    fn osc_font_size_sequence_emits_kitty_osc50() {
        assert_eq!(osc_font_size_sequence(16), "\x1b]50;font_size=16\x07");
        assert_eq!(osc_font_size_sequence(20), "\x1b]50;font_size=20\x07");
        assert_eq!(FONT_SIZE_PT, 16);
    }
}
