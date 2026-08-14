//! 界面外观设置：主题配色 + 区域大小 + 字体开关。
//!
//! 纯数据 + 纯函数 + 极简 key=value 文件读写，零 ratatui/网络依赖，可完全单测。

use std::io;
use std::path::{Path, PathBuf};

/// 内置主题预设。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePreset {
    /// 默认配色（复刻原有绿/红/黄/灰）。
    Default,
    /// Catppuccin Mocha。
    Catppuccin,
    /// Dracula。
    Dracula,
    /// Gruvbox（dark）。
    Gruvbox,
}

impl ThemePreset {
    /// 全部预设，按设置视图展示顺序排列。
    pub const ALL: [ThemePreset; 4] = [
        ThemePreset::Default,
        ThemePreset::Catppuccin,
        ThemePreset::Dracula,
        ThemePreset::Gruvbox,
    ];

    /// 预设显示名（用于设置视图）。
    pub fn name(&self) -> &'static str {
        match self {
            Self::Default => "默认",
            Self::Catppuccin => "Catppuccin",
            Self::Dracula => "Dracula",
            Self::Gruvbox => "Gruvbox",
        }
    }

    /// 序列化标识（用于 settings 文件）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Catppuccin => "catppuccin",
            Self::Dracula => "dracula",
            Self::Gruvbox => "gruvbox",
        }
    }

    /// 从字符串解析（忽略大小写与首尾空白）；未知值返回 `None`。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "default" => Some(Self::Default),
            "catppuccin" => Some(Self::Catppuccin),
            "dracula" => Some(Self::Dracula),
            "gruvbox" => Some(Self::Gruvbox),
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
            ThemePreset::Default => Self {
                text: Rgb(255, 255, 255),
                correct: Rgb(0, 255, 0),
                wrong: Rgb(255, 0, 0),
                accent: Rgb(0, 255, 0),
                warn: Rgb(255, 255, 0),
                muted: Rgb(128, 128, 128),
            },
            ThemePreset::Catppuccin => Self {
                text: Rgb(0xcd, 0xd6, 0xf4),
                correct: Rgb(0xa6, 0xe3, 0xa1),
                wrong: Rgb(0xf3, 0x8b, 0xa8),
                accent: Rgb(0x89, 0xb4, 0xfa),
                warn: Rgb(0xf9, 0xe2, 0xaf),
                muted: Rgb(0x6c, 0x70, 0x86),
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
        }
    }
}

/// 应用外观设置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// 主题预设。
    pub theme: ThemePreset,
    /// 对照区占比（%）。
    pub reference_ratio: u8,
    /// 粗体开关。
    pub bold: bool,
    /// 字体设置开关（尽力而为的 OSC 尝试）。
    pub font: bool,
}

impl Settings {
    /// 对照区占比的合法下限（%）。
    pub const RATIO_MIN: u8 = 30;
    /// 对照区占比的合法上限（%）。
    pub const RATIO_MAX: u8 = 80;

    /// 校验并修正占比到合法范围。
    pub fn clamp_ratio(ratio: u8) -> u8 {
        ratio.clamp(Self::RATIO_MIN, Self::RATIO_MAX)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePreset::Default,
            reference_ratio: 62,
            bold: false,
            font: false,
        }
    }
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
        let content = format!(
            "theme={}\nreference_ratio={}\nbold={}\nfont={}\n",
            settings.theme.as_str(),
            settings.reference_ratio,
            settings.bold,
            settings.font,
        );
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
                _ => {}
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
        let t = Theme::preset(ThemePreset::Catppuccin);
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
    fn preset_default_keeps_original_palette() {
        let t = Theme::preset(ThemePreset::Default);
        assert_eq!(t.correct, Rgb(0, 255, 0));
        assert_eq!(t.wrong, Rgb(255, 0, 0));
        assert_eq!(t.warn, Rgb(255, 255, 0));
        assert_eq!(t.muted, Rgb(128, 128, 128));
    }

    #[test]
    fn preset_parse_roundtrip_and_case_insensitive() {
        for p in ThemePreset::ALL {
            assert_eq!(ThemePreset::parse(p.as_str()), Some(p));
        }
        assert_eq!(
            ThemePreset::parse("  CATPPUCCIN "),
            Some(ThemePreset::Catppuccin)
        );
        assert_eq!(ThemePreset::parse("solarized"), None);
        assert_eq!(ThemePreset::parse(""), None);
    }

    #[test]
    fn preset_next_and_prev_wrap_around() {
        assert_eq!(ThemePreset::Default.next(), ThemePreset::Catppuccin);
        assert_eq!(ThemePreset::Gruvbox.next(), ThemePreset::Default);
        assert_eq!(ThemePreset::Default.prev(), ThemePreset::Gruvbox);
        assert_eq!(ThemePreset::Catppuccin.prev(), ThemePreset::Default);
    }

    #[test]
    fn settings_defaults() {
        let s = Settings::default();
        assert_eq!(s.theme, ThemePreset::Default);
        assert_eq!(s.reference_ratio, 62);
        assert!(!s.bold);
        assert!(!s.font);
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
    fn store_roundtrip() {
        let store = SettingsStore::new(temp_path("roundtrip"));
        let s = Settings {
            theme: ThemePreset::Dracula,
            reference_ratio: 70,
            bold: true,
            font: false,
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
}
