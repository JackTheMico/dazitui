//! 输入法码表与方案反查模块 (Scheme Reverse Mapping & Chording Decomposition)
//!
//! 支持从纯文本码表 (TSV / 空格分隔) 与 Rime .dict.yaml 文件加载形码/并击方案，
//! 将汉字反查为击键序列或并击键位组合（例如麓鸣并击、虎码、五笔、小鹤音形等）。

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

/// 方案反查与码表映射管理器。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SchemeDict {
    /// 词/单字 -> 编码列表（可能有重码，保留首选编码）
    word_to_codes: HashMap<String, Vec<String>>,
}

impl std::str::FromStr for SchemeDict {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse(s))
    }
}

impl SchemeDict {
    /// 从字符串内容解析码表（支持纯文本与 Rime .dict.yaml 格式）。
    pub fn parse(content: &str) -> Self {
        let mut dict = Self::default();
        let mut in_yaml_header = false;
        let mut yaml_header_count = 0;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            // 处理 Rime .dict.yaml 的 frontmatter: `---` ... `...`
            if trimmed == "---" {
                in_yaml_header = true;
                yaml_header_count += 1;
                continue;
            }
            if in_yaml_header {
                if trimmed == "..." || (trimmed == "---" && yaml_header_count >= 1) {
                    in_yaml_header = false;
                }
                continue;
            }

            // 解析形如 `字\t编码` 或 `字\t编码\t权重` 或 `编码\t字`
            let parts: Vec<&str> = trimmed.split('\t').collect();
            if parts.len() >= 2 {
                let first = parts[0].trim();
                let second = parts[1].trim();

                let (word, code) = if is_likely_code(second) && !is_likely_code(first) {
                    (first, second)
                } else if is_likely_code(first) && !is_likely_code(second) {
                    (second, first)
                } else {
                    (first, second)
                };

                if !word.is_empty() && !code.is_empty() {
                    dict.add_entry(word, code);
                }
            } else {
                let space_parts: Vec<&str> = trimmed.split_whitespace().collect();
                if space_parts.len() >= 2 {
                    let first = space_parts[0];
                    let second = space_parts[1];
                    let (word, code) = if is_likely_code(second) && !is_likely_code(first) {
                        (first, second)
                    } else if is_likely_code(first) && !is_likely_code(second) {
                        (second, first)
                    } else {
                        (first, second)
                    };
                    if !word.is_empty() && !code.is_empty() {
                        dict.add_entry(word, code);
                    }
                }
            }
        }

        dict
    }

    /// 从文件加载码表。
    pub fn load_from_file(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut content = String::new();
        for line in reader.lines() {
            content.push_str(&line?);
            content.push('\n');
        }
        Ok(Self::parse(&content))
    }

    /// 查找系统预设或自定义配置的方案码表文件路径。
    pub fn resolve_scheme_path(
        scheme: &str,
        custom_mappings: &HashMap<String, String>,
    ) -> Option<PathBuf> {
        if scheme.is_empty() {
            return None;
        }

        // 1. 显式映射
        if let Some(custom_path_str) = custom_mappings.get(scheme) {
            let path = PathBuf::from(custom_path_str);
            if path.exists() {
                return Some(path);
            }
        }

        // 2. 默认配置目录 ~/.config/dazitui/schemes/
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                home.join(".config")
            });
        let schemes_dir = config_home.join("dazitui").join("schemes");

        let candidates = [
            schemes_dir.join(format!("{scheme}.txt")),
            schemes_dir.join(format!("{scheme}.dict.yaml")),
            schemes_dir.join(format!("{scheme}.schema.yaml")),
            schemes_dir.join(scheme),
        ];

        candidates.into_iter().find(|c| c.exists())
    }

    /// 添加一条词条编码。
    pub fn add_entry(&mut self, word: &str, code: &str) {
        let codes = self.word_to_codes.entry(word.to_string()).or_default();
        if !codes.contains(&code.to_string()) {
            codes.push(code.to_string());
        }
    }

    /// 获取字典总词条数。
    pub fn entry_count(&self) -> usize {
        self.word_to_codes.len()
    }

    /// 反查指定汉字或词组的首选击键序列（或并击组合）。
    pub fn get_primary_code(&self, word: &str) -> Option<&str> {
        self.word_to_codes.get(word).and_then(|c| c.first()).map(|s| s.as_str())
    }

    /// 分解编码为单次物理按键频次序列。
    ///
    /// 支持两种常见模式：
    /// 1. 传统形码/拼音顺序击键（如 "vbg" -> ["v", "b", "g"]）
    /// 2. 并击方案（如麓鸣并击 "w+e" 或 "sd" -> ["s", "d"]）
    pub fn decompose_code_to_keys(code: &str) -> Vec<String> {
        let mut keys = Vec::new();
        for c in code.chars() {
            if c == '+' || c == '/' || c == '-' || c == '_' || c.is_whitespace() {
                continue;
            }
            if c.is_ascii_alphanumeric() || c.is_ascii_punctuation() {
                keys.push(c.to_ascii_lowercase().to_string());
            } else {
                keys.push(c.to_string());
            }
        }
        keys
    }

    /// 将一段文本根据码表反查投射为按键总计 HashMap<Key, Count>。
    pub fn project_text_to_keys(&self, text: &str) -> HashMap<String, u32> {
        let mut key_counts = HashMap::new();
        for ch in text.chars() {
            let s = ch.to_string();
            if let Some(code) = self.get_primary_code(&s) {
                let keys = Self::decompose_code_to_keys(code);
                for k in keys {
                    *key_counts.entry(k).or_insert(0) += 1;
                }
            } else if ch.is_ascii_alphanumeric() || ch.is_ascii_punctuation() {
                let k = ch.to_ascii_lowercase().to_string();
                *key_counts.entry(k).or_insert(0) += 1;
            } else if ch == ' ' {
                *key_counts.entry("Space".to_string()).or_insert(0) += 1;
            }
        }
        key_counts
    }
}

fn is_likely_code(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '-' || c == '_' || c == ';')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_tsv_dict_parsing() {
        let tsv = "我\tq\n你\twq\n他\tt\n";
        let dict = SchemeDict::parse(tsv);
        assert_eq!(dict.get_primary_code("我"), Some("q"));
        assert_eq!(dict.get_primary_code("你"), Some("wq"));
        assert_eq!(dict.get_primary_code("他"), Some("t"));
    }

    #[test]
    fn test_rime_dict_yaml_parsing() {
        let yaml = "---\nname: huma\nversion: \"1.0\"\n...\n\n世界\tsj\t100\n人民\trm\n中华\tzh\n";
        let dict = SchemeDict::parse(yaml);
        assert_eq!(dict.get_primary_code("世界"), Some("sj"));
        assert_eq!(dict.get_primary_code("人民"), Some("rm"));
        assert_eq!(dict.get_primary_code("中华"), Some("zh"));
    }

    #[test]
    fn test_chording_and_sequence_decomposition() {
        // 麓鸣并击等并击组合：如 "w+e", "sd", "j+k"
        assert_eq!(SchemeDict::decompose_code_to_keys("w+e"), vec!["w", "e"]);
        assert_eq!(SchemeDict::decompose_code_to_keys("df"), vec!["d", "f"]);
        assert_eq!(SchemeDict::decompose_code_to_keys("a_s"), vec!["a", "s"]);

        let mut dict = SchemeDict::default();
        dict.add_entry("麓", "w+e");
        dict.add_entry("鸣", "j+k");

        let counts = dict.project_text_to_keys("麓鸣");
        assert_eq!(counts.get("w"), Some(&1));
        assert_eq!(counts.get("e"), Some(&1));
        assert_eq!(counts.get("j"), Some(&1));
        assert_eq!(counts.get("k"), Some(&1));
    }
}
