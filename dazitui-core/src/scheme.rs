//! 输入法码表与方案反查模块 (Scheme Reverse Mapping & Chording Decomposition)
//!
//! 支持从纯文本码表 (TSV / 空格分隔)、Rime .dict.yaml 文件与 Rime .schema.yaml 指法方案加载形码/并击方案，
//! 支持自动解析 chord_composer.algebra 规则（展开 __include 宏、__patch 补丁、左右手镜像与并击码元映射），
//! 将汉字编码精准逆向还原为物理按键列表（例如麓鸣并击、空明码并击、虎码、五笔、小鹤音形等）。

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

/// 方案反查与码表映射管理器。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SchemeDict {
    /// 方案展示名称（如从 .schema.yaml 中的 schema.name 提取）
    name: Option<String>,
    /// 词/单字 -> 编码列表（可能有重码，保留首选编码）
    word_to_codes: HashMap<String, Vec<String>>,
    /// 并击代数指法规则逆向引擎（若方案提供了 .schema.yaml 中的 chord_composer.algebra）
    chord_algebra: Option<ChordAlgebra>,
}

impl std::str::FromStr for SchemeDict {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse(s))
    }
}

impl SchemeDict {
    /// 获取方案显示名称。
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// 设置方案显示名称。
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = Some(name.into());
    }

    /// 从指定 .schema.yaml 文件中提取方案名称 (schema.name)。
    pub fn extract_schema_name(path: &Path) -> Option<String> {
        let content = std::fs::read_to_string(path).ok()?;
        let doc = parse_rime_yaml(&content);
        doc.get("schema/name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

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

    /// 获取关联的并击代数指法规则引擎。
    pub fn chord_algebra(&self) -> Option<&ChordAlgebra> {
        self.chord_algebra.as_ref()
    }

    /// 设置并击代数指法规则引擎。
    pub fn set_chord_algebra(&mut self, algebra: ChordAlgebra) {
        self.chord_algebra = Some(algebra);
    }

    /// 从文件加载码表与指法方案。
    ///
    /// 智能识别文件类型与同名伴随文件：
    /// - 若输入为 `.schema.yaml`：解析其 `chord_composer.algebra`，提取 `schema.name`，并查找同目录下关联词典加载词条。
    /// - 若输入为 `.dict.yaml` 或 `.txt`：加载词条，并尝试加载同目录下同名 `.schema.yaml` 提取指法映射与方案名。
    pub fn load_from_file(path: &Path) -> io::Result<Self> {
        let file_name = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
        let parent_dir = path.parent().unwrap_or(Path::new("."));

        if file_name.ends_with(".schema.yaml") {
            // 1. 从 schema 解析指法规则与方案名
            let mut resolver = RimeSchemaResolver::new();
            let rules = resolver.resolve_chord_algebra(path);
            let algebra = if !rules.is_empty() {
                Some(ChordAlgebra::from_rules(&rules))
            } else {
                None
            };
            let schema_name = Self::extract_schema_name(path);

            // 2. 查找伴随词典（优先检查 translator/dictionary，其次使用文件名词干）
            let schema_doc = resolver.load_doc(path).ok().cloned();
            let dict_name = schema_doc.as_ref().and_then(|doc| {
                doc.get("translator/dictionary")
                    .or_else(|| doc.get("__patch/translator/dictionary"))
                    .and_then(|v| v.as_str())
            });

            let schema_stem = file_name.strip_suffix(".schema.yaml").unwrap_or(file_name);
            let mut candidate_dicts = Vec::new();
            if let Some(custom_dict) = dict_name {
                candidate_dicts.push(parent_dir.join(format!("{custom_dict}.dict.yaml")));
                candidate_dicts.push(parent_dir.join(format!("{custom_dict}.txt")));
            }
            candidate_dicts.push(parent_dir.join(format!("{schema_stem}.dict.yaml")));
            candidate_dicts.push(parent_dir.join(format!("{schema_stem}.txt")));

            let mut dict = if let Some(dict_path) = candidate_dicts.into_iter().find(|p| p.exists()) {
                let content = std::fs::read_to_string(dict_path)?;
                Self::parse(&content)
            } else {
                Self::default()
            };

            if let Some(alg) = algebra {
                dict.set_chord_algebra(alg);
            }
            if let Some(name) = schema_name {
                dict.set_name(name);
            }
            return Ok(dict);
        }

        // 加载词典文本
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut content = String::new();
        for line in reader.lines() {
            content.push_str(&line?);
            content.push('\n');
        }
        let mut dict = Self::parse(&content);

        // 尝试自动绑定同目录同名 schema.yaml
        let stem = if file_name.ends_with(".dict.yaml") {
            file_name.strip_suffix(".dict.yaml").unwrap_or(file_name)
        } else if let Some(pos) = file_name.rfind('.') {
            &file_name[..pos]
        } else {
            file_name
        };

        let schema_candidate = parent_dir.join(format!("{stem}.schema.yaml"));
        if schema_candidate.exists() {
            let mut resolver = RimeSchemaResolver::new();
            let rules = resolver.resolve_chord_algebra(&schema_candidate);
            if !rules.is_empty() {
                dict.set_chord_algebra(ChordAlgebra::from_rules(&rules));
            }
            if let Some(name) = Self::extract_schema_name(&schema_candidate) {
                dict.set_name(name);
            }
        }

        Ok(dict)
    }

    /// 查找系统预设或自定义配置的方案码表文件路径。
    ///
    /// 搜索优先级：
    /// 1. 显式自定义映射表 `custom_mappings`
    /// 2. 绝对路径或相对路径直接存在性检测
    /// 3. 打字推默认方案目录 `~/.config/dazitui/schemes/`
    /// 4. 常见 Rime 用户配置目录 (Fcitx5 / Fcitx / IBus / Squirrel / Rime)
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

        // 2. 绝对或相对文件路径直接判断
        let direct_path = PathBuf::from(scheme);
        if direct_path.exists() {
            return Some(direct_path);
        }

        // 3. 构建搜索目录列表
        let mut search_dirs = Vec::new();

        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                home.join(".config")
            });
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                home.join(".local").join("share")
            });
        let home_dir = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));

        // dazitui 自带目录
        search_dirs.push(config_home.join("dazitui").join("schemes"));
        // fcitx5 rime
        search_dirs.push(data_home.join("fcitx5").join("rime"));
        // fcitx rime
        search_dirs.push(config_home.join("fcitx").join("rime"));
        // ibus rime
        search_dirs.push(config_home.join("ibus").join("rime"));
        // macOS Squirrel
        search_dirs.push(home_dir.join("Library").join("Rime"));
        // 传统 .rime
        search_dirs.push(home_dir.join(".rime"));

        for dir in search_dirs {
            let candidates = [
                dir.join(format!("{scheme}.schema.yaml")),
                dir.join(format!("{scheme}.dict.yaml")),
                dir.join(format!("{scheme}.txt")),
                dir.join(scheme),
            ];

            if let Some(found) = candidates.into_iter().find(|c| c.exists()) {
                return Some(found);
            }
        }

        None
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

    /// 分解编码为物理按键序列。
    /// 若方案附带 `chord_composer.algebra` 指法规则，则由指法代数引擎完成逆向映射；
    /// 否则按单字符过滤展开。
    pub fn decompose_code(&self, code: &str) -> Vec<String> {
        if let Some(ref algebra) = self.chord_algebra {
            algebra.decompose_code(code)
        } else {
            Self::decompose_code_to_keys(code)
        }
    }

    /// 向前兼容的静态单字符分解方法。
    pub fn decompose_code_to_keys(code: &str) -> Vec<String> {
        let mut keys = Vec::new();
        for c in code.chars() {
            if c == '+' || c == '/' || c == '-' || c == '_' || c == '\'' || c.is_whitespace() {
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
                let keys = self.decompose_code(code);
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

/// 并击代数指法规则逆向引擎。
///
/// 负责将 Rime 中的 `chord_composer.algebra` 规则（如 `xform|xv|.|`, `xform|y|t|` 等）
/// 逆向分类为：
/// 1. 左右手按键镜像映射表（Left <-> Right Mirror Map）
/// 2. 逻辑码元到物理并击按键映射表（Symbol -> Physical Keys Map）
#[derive(Debug, Clone, PartialEq)]
pub struct ChordAlgebra {
    /// 码元 -> 物理按键列表（如 '.' -> ["x", "v"], 'W' -> ["v", "w"], 'Q' -> ["e", "s", "f"]）
    symbol_to_keys: HashMap<char, Vec<String>>,
    /// 右手单键 -> 左手镜像单键（如 'y' -> 't', 'u' -> 'r', 'j' -> 'f' 等）
    mirror_right_to_left: HashMap<char, char>,
    /// 左手单键 -> 右手镜像单键（如 't' -> 'y', 'r' -> 'u', 'f' -> 'j' 等）
    mirror_left_to_right: HashMap<char, char>,
}

impl Default for ChordAlgebra {
    fn default() -> Self {
        let mut algebra = Self {
            symbol_to_keys: HashMap::new(),
            mirror_right_to_left: HashMap::new(),
            mirror_left_to_right: HashMap::new(),
        };
        algebra.init_default_mirrors();
        algebra
    }
}

impl ChordAlgebra {
    /// 初始化标准 QWERTY 左右手对称镜像映射默认基线。
    fn init_default_mirrors(&mut self) {
        let pairs = [
            ('6', '5'), ('7', '4'), ('8', '3'), ('9', '2'), ('0', '1'),
            ('y', 't'), ('u', 'r'), ('i', 'e'), ('o', 'w'), ('p', 'q'),
            ('h', 'g'), ('j', 'f'), ('k', 'd'), ('l', 's'), (';', 'a'),
            ('n', 'b'), ('m', 'v'), (',', 'c'), ('.', 'x'), ('/', 'z'),
        ];
        for (r, l) in pairs {
            self.mirror_right_to_left.insert(r, l);
            self.mirror_left_to_right.insert(l, r);
        }
    }

    /// 从 Rime `chord_composer.algebra` 规则列表构造逆向代数引擎。
    pub fn from_rules(rules: &[String]) -> Self {
        let mut algebra = Self::default();

        for rule in rules {
            if let Some((pattern, replacement)) = parse_xform_rule(rule) {
                let pat_chars: Vec<char> = pattern.chars().collect();
                let rep_chars: Vec<char> = replacement.chars().collect();

                // 1. 镜像单键规则：例如 `xform|y|t|` (右手 y 映射到左手 t)
                if pat_chars.len() == 1 && rep_chars.len() == 1 {
                    let r = pat_chars[0];
                    let l = rep_chars[0];
                    if is_right_hand_key(r) && is_left_hand_key(l) {
                        algebra.mirror_right_to_left.insert(r, l);
                        algebra.mirror_left_to_right.insert(l, r);
                    }
                }
                // 2. 码元并击规则：例如 `xform|xv|.|` 或 `xform|esf|Q|`
                else if rep_chars.len() == 1 && pat_chars.len() >= 2 {
                    let symbol = rep_chars[0];
                    let mut keys: Vec<String> = pat_chars.iter().map(|c| c.to_ascii_lowercase().to_string()).collect();
                    keys.sort();
                    algebra.symbol_to_keys.entry(symbol).or_insert(keys);
                }
            }
        }

        algebra
    }

    /// 将逻辑编码根据指法规则展开为实际物理按键列表。
    pub fn decompose_code(&self, code: &str) -> Vec<String> {
        if code.is_empty() {
            return Vec::new();
        }

        // 1. 左手单手前缀 `_`：如 `_.` 或 `_v`
        if let Some(rest) = code.strip_prefix('_') {
            let mut keys = Vec::new();
            for c in rest.chars() {
                if c == '-' || c == '\'' || c.is_whitespace() {
                    continue;
                }
                if let Some(chord_keys) = self.symbol_to_keys.get(&c) {
                    keys.extend(chord_keys.clone());
                } else if c.is_ascii_alphanumeric() || c.is_ascii_punctuation() {
                    keys.push(c.to_ascii_lowercase().to_string());
                }
            }
            return keys;
        }

        // 2. 右手单手前缀 `+`：如 `+e` (单键右手镜像为 i) 或 `+.` (并击 xv 镜像为 .m)
        if let Some(rest) = code.strip_prefix('+') {
            let mut keys = Vec::new();
            for c in rest.chars() {
                if c == '-' || c == '\'' || c.is_whitespace() {
                    continue;
                }
                if let Some(chord_keys) = self.symbol_to_keys.get(&c) {
                    for k in chord_keys {
                        let ch = k.chars().next().unwrap_or(' ');
                        let mirrored = self.mirror_left_to_right.get(&ch).copied().unwrap_or(ch);
                        keys.push(mirrored.to_string());
                    }
                } else if c.is_ascii_alphanumeric() || c.is_ascii_punctuation() {
                    let base_c = c.to_ascii_lowercase();
                    let mirrored = self.mirror_left_to_right.get(&base_c).copied().unwrap_or(base_c);
                    keys.push(mirrored.to_string());
                }
            }
            return keys;
        }

        // 3. 无单手前缀（双手并击 / 序列击键，如 ".Wd", "wCs", "x;de"）
        let mut keys = Vec::new();
        for c in code.chars() {
            if c == '+' || c == '_' || c == '-' || c == '\'' || c.is_whitespace() {
                continue;
            }
            if let Some(chord_keys) = self.symbol_to_keys.get(&c) {
                keys.extend(chord_keys.clone());
            } else if c.is_ascii_alphanumeric() || c.is_ascii_punctuation() {
                keys.push(c.to_ascii_lowercase().to_string());
            }
        }
        keys
    }

    /// 获取码元对应的物理按键映射。
    pub fn get_symbol_keys(&self, symbol: char) -> Option<&[String]> {
        self.symbol_to_keys.get(&symbol).map(|v| v.as_slice())
    }
}

fn is_left_hand_key(c: char) -> bool {
    let lower = c.to_ascii_lowercase();
    "12345qwertasdfgzxcvb".contains(lower)
}

fn is_right_hand_key(c: char) -> bool {
    let lower = c.to_ascii_lowercase();
    "67890yuiophjkl;nm,./".contains(lower) || c == ';' || c == ',' || c == '.' || c == '/'
}

/// 解析 `xform` 规则：`xform|pattern|replacement|` 或 `xform/pattern/replacement/`。
fn parse_xform_rule(rule: &str) -> Option<(String, String)> {
    let trimmed = rule.trim();
    if !trimmed.starts_with("xform") {
        return None;
    }
    let after = trimmed.strip_prefix("xform")?.trim();
    if after.is_empty() {
        return None;
    }
    let delimiter = after.chars().next()?;
    let parts: Vec<&str> = after.split(delimiter).collect();
    if parts.len() >= 3 {
        let pattern = parts[1].replace('\\', "");
        let replacement = parts[2].replace('\\', "");
        Some((pattern, replacement))
    } else {
        None
    }
}

/// Rime YAML 轻量节点表示。
#[derive(Debug, Clone, PartialEq)]
pub enum YamlValue {
    String(String),
    List(Vec<YamlValue>),
    Mapping(Vec<(String, YamlValue)>),
}

impl YamlValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            YamlValue::String(s) => Some(s),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn as_list(&self) -> Option<&[YamlValue]> {
        match self {
            YamlValue::List(l) => Some(l),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn as_mapping(&self) -> Option<&[(String, YamlValue)]> {
        match self {
            YamlValue::Mapping(m) => Some(m),
            _ => None,
        }
    }

    pub fn get(&self, path: &str) -> Option<&YamlValue> {
        let parts: Vec<&str> = path.split(['/', '.']).filter(|s| !s.is_empty()).collect();
        let mut current = self;
        for part in parts {
            match current {
                YamlValue::Mapping(m) => {
                    let mut found = None;
                    for (k, v) in m {
                        if k == part {
                            found = Some(v);
                            break;
                        }
                    }
                    current = found?;
                }
                _ => return None,
            }
        }
        Some(current)
    }
}

/// 轻量级 Rime YAML 解析器。
pub fn parse_rime_yaml(content: &str) -> YamlValue {
    let mut lines = Vec::new();
    for raw_line in content.lines() {
        let stripped = strip_yaml_comment(raw_line);
        if stripped.trim().is_empty() {
            continue;
        }
        let indent = stripped.chars().take_while(|c| *c == ' ').count();
        lines.push((indent, stripped.trim()));
    }

    let mut parser = YamlParser { lines, pos: 0 };
    if parser.lines.is_empty() {
        return YamlValue::Mapping(Vec::new());
    }
    if parser.lines[0].1.starts_with("- ") || parser.lines[0].1 == "-" {
        YamlValue::List(parser.parse_list(0))
    } else {
        YamlValue::Mapping(parser.parse_mapping(0))
    }
}

struct YamlParser<'a> {
    lines: Vec<(usize, &'a str)>,
    pos: usize,
}

impl<'a> YamlParser<'a> {
    fn parse_mapping(&mut self, min_indent: usize) -> Vec<(String, YamlValue)> {
        let mut entries = Vec::new();
        while self.pos < self.lines.len() {
            let (indent, line) = self.lines[self.pos];
            if indent < min_indent {
                break;
            }
            if line.starts_with("- ") || line == "-" {
                break;
            }

            if let Some(colon) = line.find(':') {
                let key = line[..colon].trim().trim_matches(['"', '\'']).to_string();
                let val_part = line[colon + 1..].trim();
                self.pos += 1;

                if val_part.is_empty() {
                    let next_indent = self.peek_indent();
                    if next_indent > indent {
                        let is_list = self.lines[self.pos].1.starts_with("- ") || self.lines[self.pos].1 == "-";
                        let val = if is_list {
                            YamlValue::List(self.parse_list(next_indent))
                        } else {
                            YamlValue::Mapping(self.parse_mapping(next_indent))
                        };
                        entries.push((key, val));
                    } else {
                        entries.push((key, YamlValue::String(String::new())));
                    }
                } else if val_part == "|" || val_part == ">" {
                    // 多行文本块
                    let next_indent = self.peek_indent();
                    let mut block = String::new();
                    while self.pos < self.lines.len() && self.lines[self.pos].0 >= next_indent {
                        if !block.is_empty() {
                            block.push('\n');
                        }
                        block.push_str(self.lines[self.pos].1);
                        self.pos += 1;
                    }
                    entries.push((key, YamlValue::String(block)));
                } else {
                    entries.push((key, YamlValue::String(clean_scalar(val_part))));
                }
            } else {
                self.pos += 1;
            }
        }
        entries
    }

    fn parse_list(&mut self, min_indent: usize) -> Vec<YamlValue> {
        let mut items = Vec::new();
        while self.pos < self.lines.len() {
            let (indent, line) = self.lines[self.pos];
            if indent < min_indent {
                break;
            }
            if !line.starts_with("- ") && line != "-" {
                break;
            }

            let item_str = line.strip_prefix('-').unwrap().trim();
            self.pos += 1;

            if item_str.is_empty() {
                let next_indent = self.peek_indent();
                if next_indent > indent {
                    let is_list = self.lines[self.pos].1.starts_with("- ") || self.lines[self.pos].1 == "-";
                    let val = if is_list {
                        YamlValue::List(self.parse_list(next_indent))
                    } else {
                        YamlValue::Mapping(self.parse_mapping(next_indent))
                    };
                    items.push(val);
                }
            } else if item_str.contains(':')
                && !item_str.starts_with("xform")
                && !item_str.starts_with("derive")
                && !item_str.starts_with("erase")
            {
                let colon = item_str.find(':').unwrap();
                let k = item_str[..colon].trim().to_string();
                let v = clean_scalar(item_str[colon + 1..].trim());
                let mut map = vec![(k, YamlValue::String(v))];

                let item_sub_indent = indent + 1;
                while self.pos < self.lines.len() {
                    let (sub_indent, sub_line) = self.lines[self.pos];
                    if sub_indent < item_sub_indent || sub_line.starts_with("- ") || sub_line == "-" {
                        break;
                    }
                    if let Some(sub_colon) = sub_line.find(':') {
                        let sub_k = sub_line[..sub_colon].trim().to_string();
                        let sub_v = clean_scalar(sub_line[sub_colon + 1..].trim());
                        map.push((sub_k, YamlValue::String(sub_v)));
                    }
                    self.pos += 1;
                }
                items.push(YamlValue::Mapping(map));
            } else {
                items.push(YamlValue::String(clean_scalar(item_str)));
            }
        }
        items
    }

    fn peek_indent(&self) -> usize {
        if self.pos < self.lines.len() {
            self.lines[self.pos].0
        } else {
            0
        }
    }
}

fn strip_yaml_comment(line: &str) -> &str {
    let mut in_quote = false;
    let mut quote_char = ' ';
    for (idx, c) in line.char_indices() {
        if (c == '"' || c == '\'') && quote_char == ' ' {
            in_quote = true;
            quote_char = c;
        } else if in_quote && c == quote_char {
            in_quote = false;
            quote_char = ' ';
        } else if !in_quote && c == '#' && (idx == 0 || line[..idx].ends_with(|ws: char| ws.is_whitespace())) {
            return &line[..idx];
        }
    }
    line
}

fn clean_scalar(s: &str) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

/// Rime Schema 宏展开与指法代数提取器。
#[derive(Debug, Default)]
pub struct RimeSchemaResolver {
    docs: HashMap<PathBuf, YamlValue>,
}

impl RimeSchemaResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load_doc(&mut self, path: &Path) -> io::Result<&YamlValue> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !self.docs.contains_key(&canonical) {
            let content = std::fs::read_to_string(path)?;
            let parsed = parse_rime_yaml(&content);
            self.docs.insert(canonical.clone(), parsed);
        }
        Ok(self.docs.get(&canonical).unwrap())
    }

    /// 解析指定 schema 文件中定义的 chord_composer.algebra 完整展开规则列表。
    pub fn resolve_chord_algebra(&mut self, schema_path: &Path) -> Vec<String> {
        let base_dir = schema_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        if self.load_doc(schema_path).is_err() {
            return Vec::new();
        }

        let canonical = schema_path.canonicalize().unwrap_or_else(|_| schema_path.to_path_buf());
        let doc = self.docs.get(&canonical).cloned().unwrap_or_else(|| YamlValue::Mapping(Vec::new()));

        let mut rules = Vec::new();
        let mut visited = Vec::new();

        // 查找 chord_composer/algebra 或 __patch 下的 chord_composer/algebra
        if let Some(algebra_node) = doc.get("chord_composer/algebra").or_else(|| doc.get("__patch/chord_composer/algebra")) {
            self.resolve_node_rules(&doc, algebra_node, &base_dir, &mut rules, &mut visited);
        } else if let Some(patch_node) = doc.get("__patch") {
            if let Some(cc) = patch_node.get("chord_composer") {
                if let Some(alg) = cc.get("algebra") {
                    self.resolve_node_rules(&doc, alg, &base_dir, &mut rules, &mut visited);
                }
            }
        }

        rules
    }

    fn resolve_node_rules(
        &mut self,
        current_doc: &YamlValue,
        node: &YamlValue,
        base_dir: &Path,
        rules: &mut Vec<String>,
        visited: &mut Vec<String>,
    ) {
        match node {
            YamlValue::String(s) => {
                let trimmed = s.trim();
                if trimmed.starts_with("xform") || trimmed.starts_with("derive") || trimmed.starts_with("erase") {
                    rules.push(trimmed.to_string());
                } else if !trimmed.is_empty() {
                    self.resolve_target_reference(current_doc, trimmed, base_dir, rules, visited);
                }
            }
            YamlValue::List(list) => {
                for item in list {
                    self.resolve_node_rules(current_doc, item, base_dir, rules, visited);
                }
            }
            YamlValue::Mapping(map) => {
                for (k, v) in map {
                    if k == "__patch" || k == "__append" {
                        self.resolve_node_rules(current_doc, v, base_dir, rules, visited);
                    } else if k == "__include" {
                        if let Some(target) = v.as_str() {
                            self.resolve_target_reference(current_doc, target, base_dir, rules, visited);
                        } else {
                            self.resolve_node_rules(current_doc, v, base_dir, rules, visited);
                        }
                    } else if k.starts_with("xform") {
                        rules.push(k.to_string());
                    }
                }
            }
        }
    }

    fn resolve_target_reference(
        &mut self,
        current_doc: &YamlValue,
        target: &str,
        base_dir: &Path,
        rules: &mut Vec<String>,
        visited: &mut Vec<String>,
    ) {
        let target = target.trim();
        if target.is_empty() || visited.iter().any(|v| v == target) {
            return;
        }
        visited.push(target.to_string());

        if target.contains(":/") {
            // 跨文件引用：例如 "yoyo:/六脉神剑"
            let parts: Vec<&str> = target.split(":/").collect();
            let file_prefix = parts[0].trim();
            let section_path = parts.get(1).map(|s| s.trim()).unwrap_or("");

            let candidate_files = [
                base_dir.join(format!("{file_prefix}.yaml")),
                base_dir.join(format!("{file_prefix}.schema.yaml")),
            ];
            if let Some(target_file) = candidate_files.into_iter().find(|p| p.exists()) {
                if self.load_doc(&target_file).is_ok() {
                    let canonical = target_file.canonicalize().unwrap_or(target_file);
                    if let Some(ext_doc) = self.docs.get(&canonical).cloned() {
                        if section_path.is_empty() {
                            self.resolve_node_rules(&ext_doc, &ext_doc, base_dir, rules, visited);
                        } else if let Some(sec_node) = ext_doc.get(section_path) {
                            self.resolve_node_rules(&ext_doc, sec_node, base_dir, rules, visited);
                        }
                    }
                }
            }
        } else {
            // 本地文件节引用：例如 "纯形统一心法" 或 "心法"
            if let Some(sec_node) = current_doc.get(target) {
                self.resolve_node_rules(current_doc, sec_node, base_dir, rules, visited);
            }
        }
    }
}

fn is_likely_code(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '+' | '/' | '-' | '_' | ';' | ':' | '<' | '>' | '?' | '.' | ',' | '\'' | '='
                )
        })
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
    fn test_rime_dict_yaml_parsing_with_punctuation_codes() {
        let yaml = "---\nname: yoyo-pure\nversion: \"1.0\"\n...\n\n到\t_.\t14948468\n是\twCs\t1000\n为\tO<O\t500\n就\tsE:\t200\n";
        let dict = SchemeDict::parse(yaml);
        assert_eq!(dict.get_primary_code("到"), Some("_."));
        assert_eq!(dict.get_primary_code("是"), Some("wCs"));
        assert_eq!(dict.get_primary_code("为"), Some("O<O"));
        assert_eq!(dict.get_primary_code("就"), Some("sE:"));
    }

    #[test]
    fn test_yaml_parser_basic_mapping_and_list() {
        let yaml = r#"
schema:
  name: "测试方案"
  schema_id: test
algebra:
  - xform|y|t|
  - xform|xv|.|
  - xform|esf|Q|
"#;
        let parsed = parse_rime_yaml(yaml);
        assert_eq!(parsed.get("schema/name").and_then(|v| v.as_str()), Some("测试方案"));
        assert_eq!(parsed.get("schema/schema_id").and_then(|v| v.as_str()), Some("test"));
        let alg = parsed.get("algebra").and_then(|v| v.as_list()).unwrap();
        assert_eq!(alg.len(), 3);
        assert_eq!(alg[0].as_str(), Some("xform|y|t|"));
        assert_eq!(alg[1].as_str(), Some("xform|xv|.|"));
        assert_eq!(alg[2].as_str(), Some("xform|esf|Q|"));
    }

    #[test]
    fn test_chord_algebra_parsing_and_decomposition() {
        let rules = vec![
            "xform|6|5|".to_string(),
            "xform|y|t|".to_string(),
            "xform|u|r|".to_string(),
            "xform|i|e|".to_string(),
            "xform|o|w|".to_string(),
            "xform|p|q|".to_string(),
            "xform|h|g|".to_string(),
            "xform|j|f|".to_string(),
            "xform|k|d|".to_string(),
            "xform|l|s|".to_string(),
            "xform|;|a|".to_string(),
            "xform|n|b|".to_string(),
            "xform|m|v|".to_string(),
            "xform|,|c|".to_string(),
            "xform|\\.|x|".to_string(),
            "xform|/|z|".to_string(),
            // 码元映射
            "xform|xv|\\.|".to_string(),
            "xform|vw|W|".to_string(),
            "xform|cf|C|".to_string(),
            "xform|esf|Q|".to_string(),
        ];

        let algebra = ChordAlgebra::from_rules(&rules);

        // 1. 左手单手前缀 `_`
        assert_eq!(algebra.decompose_code("_."), vec!["v", "x"]);
        assert_eq!(algebra.decompose_code("_v"), vec!["v"]);

        // 2. 右手单手前缀 `+` (单键映射)
        assert_eq!(algebra.decompose_code("+e"), vec!["i"]);
        assert_eq!(algebra.decompose_code("+r"), vec!["u"]);

        // 3. 右手单手前缀 `+` (并击码元镜像映射)
        // xv 镜像到右手 -> v 对应 m, x 对应 . -> ["m", "."]
        assert_eq!(algebra.decompose_code("+."), vec!["m", "."]);

        // 4. 双手并击 / 混打 (无前缀)
        assert_eq!(algebra.decompose_code("wCs"), vec!["w", "c", "f", "s"]);
        assert_eq!(algebra.decompose_code(".Wd"), vec!["v", "x", "v", "w", "d"]);
        assert_eq!(algebra.decompose_code("Q"), vec!["e", "f", "s"]);
    }

    #[test]
    fn test_scheme_dict_with_chord_algebra_integration() {
        let mut dict = SchemeDict::default();
        dict.add_entry("到", "_.");
        dict.add_entry("是", "wCs");

        let rules = vec![
            "xform|xv|\\.|".to_string(),
            "xform|cf|C|".to_string(),
        ];
        dict.set_chord_algebra(ChordAlgebra::from_rules(&rules));

        assert_eq!(dict.decompose_code("_."), vec!["v", "x"]);
        assert_eq!(dict.decompose_code("wCs"), vec!["w", "c", "f", "s"]);

        let counts = dict.project_text_to_keys("到是");
        assert_eq!(counts.get("x"), Some(&1));
        assert_eq!(counts.get("v"), Some(&1));
        assert_eq!(counts.get("w"), Some(&1));
        assert_eq!(counts.get("c"), Some(&1));
        assert_eq!(counts.get("f"), Some(&1));
        assert_eq!(counts.get("s"), Some(&1));
    }

    #[test]
    fn test_yoyo_pure_schema_live_integration() {
        let schema_path = Path::new("/home/jackwy/codes/rime/yoyo/rime/yoyo-pure.schema.yaml");
        if schema_path.exists() {
            let mut resolver = RimeSchemaResolver::new();
            let rules = resolver.resolve_chord_algebra(schema_path);
            assert!(!rules.is_empty(), "Rules should not be empty");

            let dict = SchemeDict::load_from_file(schema_path).expect("加载 yoyo-pure 方案");
            assert!(dict.chord_algebra().is_some());
            let algebra = dict.chord_algebra().unwrap();

            // yoyo-pure 使用「六脉神剑」指法：. 为 xz 并击，C 为 cx 并击
            assert_eq!(algebra.decompose_code("_."), vec!["x", "z"]);
            assert_eq!(algebra.decompose_code("wCs"), vec!["w", "c", "x", "s"]);
            assert!(dict.entry_count() > 1000);
            assert_eq!(dict.name(), Some("麓鸣·纯形·六脉"));
            assert_eq!(dict.get_primary_code("到"), Some("_."));
        }

        let km_schema_path = Path::new("/home/jackwy/codes/rime/yoyo/rime/yoyo-pure-km.schema.yaml");
        if km_schema_path.exists() {
            let dict = SchemeDict::load_from_file(km_schema_path).expect("加载 yoyo-pure-km 方案");
            assert!(dict.chord_algebra().is_some());
            let algebra = dict.chord_algebra().unwrap();

            // yoyo-pure-km 使用「空明拳」指法：. 为 xv 并击，C 为 cf 并击
            assert_eq!(algebra.decompose_code("_."), vec!["v", "x"]);
            assert_eq!(algebra.decompose_code("wCs"), vec!["w", "c", "f", "s"]);
        }
    }

    #[test]
    fn test_schema_name_extraction_and_resolution() {
        let temp_dir = std::env::temp_dir().join("dazitui_test_scheme");
        let _ = std::fs::create_dir_all(&temp_dir);
        let schema_file = temp_dir.join("demo.schema.yaml");
        let yaml_content = "schema:\n  name: \"演示方案·六脉\"\n  schema_id: demo\n\nchord_composer:\n  algebra:\n    - xform|xv|.|";
        std::fs::write(&schema_file, yaml_content).unwrap();

        assert_eq!(SchemeDict::extract_schema_name(&schema_file), Some("演示方案·六脉".to_string()));

        // 直接路径解析
        let mut custom = HashMap::new();
        let resolved = SchemeDict::resolve_scheme_path(schema_file.to_str().unwrap(), &custom);
        assert_eq!(resolved, Some(schema_file.clone()));

        // 自定义别名映射解析
        custom.insert("my_demo".to_string(), schema_file.to_str().unwrap().to_string());
        let resolved_alias = SchemeDict::resolve_scheme_path("my_demo", &custom);
        assert_eq!(resolved_alias, Some(schema_file));
    }
}
