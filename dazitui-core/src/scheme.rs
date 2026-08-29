//! 输入法码表与方案反查模块 (Scheme Reverse Mapping & Chording Decomposition)
//!
//! 支持从纯文本码表 (TSV / 空格分隔)、Rime .dict.yaml 文件与 Rime .schema.yaml 指法方案加载形码/并击方案，
//! 支持自动解析 chord_composer.algebra 规则（展开 __include 宏、__patch 补丁、左右手镜像与并击码元映射），
//! 将汉字编码精准逆向还原为物理按键列表（例如麓鸣并击、空明码并击、虎码、五笔、小鹤音形等）。

use std::collections::{HashMap, HashSet};
use std::io;
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
    /// 需要追加 `'` 提交符的编码集合（剥离手区前缀后的逻辑码）。
    /// 规则：某字词同时含「短码 c」与「长码 d」且 d 以 c 为严格前缀（如 文化 `vw`⊂`vwah`），
    /// 则短码 c 需键入 `'` 才能提交该候选，提示区据此补 `'`。
    prefix_commit_codes: HashSet<String>,
}

/// 单个词组单位的编码提示结果（供渲染层逐词对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeHint {
    /// 对应的词组单位原文（回显用，便于渲染层逐词对齐）。
    pub word: String,
    /// 最优输入编码（逻辑码元：拼音/形码字母/并击逻辑码元）；未登录留空串。
    pub code: String,
    /// 该编码的击数（并击记 1 击，即每个逻辑码元算 1）；未登录为 0。
    pub strokes: u32,
    /// 是否未登录（码表未收录，提示留空）。
    pub is_oov: bool,
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

        dict.rebuild_prefix_commit_codes();
        dict
    }

    /// 提取 Rime 词典 frontmatter（`---` … `---`/`...` 之间）文本，用于解析 `import_tables` 等元信息。
    fn extract_dict_frontmatter(content: &str) -> String {
        let mut buf = String::new();
        let mut in_header = false;
        let mut opened = false;
        for line in content.lines() {
            let t = line.trim();
            if t == "---" {
                if !opened {
                    opened = true;
                    in_header = true;
                    continue;
                }
                break;
            }
            if in_header {
                if t == "..." {
                    break;
                }
                buf.push_str(line);
                buf.push('\n');
            }
        }
        buf
    }

    /// 解析 Rime 词典的 `import_tables` 列表，返回被导入词典的逻辑名（如 `yoyo_kf`）。
    fn extract_import_tables(content: &str) -> Vec<String> {
        let fm = Self::extract_dict_frontmatter(content);
        if fm.trim().is_empty() {
            return Vec::new();
        }
        let doc = parse_rime_yaml(&fm);
        let mut out = Vec::new();
        if let Some(YamlValue::List(items)) = doc.get("import_tables") {
            for it in items {
                if let Some(s) = it.as_str() {
                    out.push(s.to_string());
                }
            }
        }
        out
    }

    /// 递归加载词典及其 `import_tables` 引用的兄弟词典，合并词条。
    ///
    /// `visited` 用规范路径去重，避免循环导入导致无限递归。导入文件缺失时静默跳过
    /// （与 Rime 宽松语义一致），不影响主词典已收录的词条。
    fn load_dict_with_imports(
        path: &Path,
        visited: &mut HashSet<PathBuf>,
    ) -> io::Result<Self> {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if visited.contains(&canon) {
            return Ok(Self::default());
        }
        visited.insert(canon);

        let content = std::fs::read_to_string(path)?;
        let mut dict = Self::parse(&content);
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        for name in Self::extract_import_tables(&content) {
            let import_path = parent.join(format!("{name}.dict.yaml"));
            if import_path.exists() {
                if let Ok(child) = Self::load_dict_with_imports(&import_path, visited) {
                    dict.merge(&child);
                }
            }
        }
        Ok(dict)
    }

    /// 将 `other` 的词条合并进自身：相同词的编码列表追加尚未存在的编码（去重）。
    fn merge(&mut self, other: &Self) {
        for (word, codes) in &other.word_to_codes {
            let entry = self.word_to_codes.entry(word.clone()).or_default();
            for c in codes {
                if !entry.contains(c) {
                    entry.push(c.clone());
                }
            }
        }
        self.rebuild_prefix_commit_codes();
    }

    /// 依据当前 `word_to_codes` 重建「需追加 `'` 提交符」的短码集合：
    /// 仅针对「无手区前缀」的纯双拼码；若同一字词同时含短码 c 与更长码 d（均为无前缀码）、
    /// 且 d 以 c 为严格前缀（如 文化 `vw`⊂`vwah`），则 c 需键入 `'` 提交候选（次选）。
    /// 单手简码（`_`/`+` 前缀）与双手并击全码分属不同输入方式，不补 `'`。
    fn rebuild_prefix_commit_codes(&mut self) {
        let mut set = HashSet::new();
        for codes in self.word_to_codes.values() {
            let unprefixed: Vec<&str> = codes
                .iter()
                .filter(|c| !c.starts_with('_') && !c.starts_with('+') && !c.starts_with('-'))
                .map(|c| c.as_str())
                .collect();
            for &ci in &unprefixed {
                for &cj in &unprefixed {
                    if ci.len() < cj.len() && cj.starts_with(ci) {
                        set.insert(ci.to_string());
                    }
                }
            }
        }
        self.prefix_commit_codes = set;
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
                let mut visited = HashSet::new();
                Self::load_dict_with_imports(&dict_path, &mut visited)?
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

        // 加载词典文本（含 import_tables 导入的兄弟词典合并）
        let mut visited = HashSet::new();
        let mut dict = Self::load_dict_with_imports(path, &mut visited)?;

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

    /// 计算指定编码的实际物理击数（Stroke Count）。
    ///
    /// 核心规则（并击算一击）：
    /// - 过滤手区修饰符（'_', '+', '-', '\'', '/'）与空白符；
    /// - 每个独立逻辑码元（无论单键还是并击码元，如 '.' 对应 xv，'W' 对应 vw，'Q' 对应 esf）严格计为 1 击。
    pub fn calculate_code_strokes(code: &str) -> u32 {
        if code.is_empty() {
            return 0;
        }
        // 空格并击简词（% 前缀）一击上屏：无论码元数一律记 1 击。
        if code.starts_with('%') {
            return 1;
        }
        code
            .chars()
            .filter(|&c| c != '_' && c != '+' && c != '-' && c != '\'' && c != '/' && !c.is_whitespace())
            .count() as u32
    }

    /// 解析指定文本对应的物理击数与展开的按键列表。
    ///
    /// - 若在方案码表中直接命中词条（单字或多字词组，如 "怎么"、"为什么"）：通过编码码元计算真实击数（并击算 1 击），并展开物理按键；
    /// - 若整段未直接命中（复合词句）：采用最大正向匹配 (Greedy Longest Matching) 分段反查码表，累加物理击数与按键序列；
    /// - 未匹配字符（ASCII、标点或码表未收录字）：按字符数计算击数（每个字符 1 击）。
    pub fn resolve_strokes_and_keys(&self, text: &str) -> (u32, Vec<String>) {
        if text.is_empty() {
            return (0, Vec::new());
        }
        if let Some(code) = self.get_primary_code(text) {
            let strokes = Self::calculate_code_strokes(code).max(1);
            let keys = self.decompose_code(code);
            return (strokes, keys);
        }

        let chars: Vec<char> = text.chars().collect();
        let mut total_strokes = 0;
        let mut all_keys = Vec::new();
        let mut start = 0;

        while start < chars.len() {
            let mut matched = false;
            // 尝试最长前缀匹配（从当前剩余最大长度到 1）
            for len in (1..=(chars.len() - start)).rev() {
                let sub: String = chars[start..start + len].iter().collect();
                if let Some(code) = self.get_primary_code(&sub) {
                    let strokes = Self::calculate_code_strokes(code).max(1);
                    let keys = self.decompose_code(code);
                    total_strokes += strokes;
                    all_keys.extend(keys);
                    start += len;
                    matched = true;
                    break;
                }
            }

            if !matched {
                let ch = chars[start];
                total_strokes += 1;
                if ch.is_ascii_graphic() {
                    all_keys.push(ch.to_ascii_lowercase().to_string());
                } else if ch == ' ' {
                    all_keys.push("Space".to_string());
                } else {
                    all_keys.push(ch.to_string());
                }
                start += 1;
            }
        }

        (total_strokes.max(1), all_keys)
    }

    /// 为已分词的文本逐词组单位计算「最少击数」最优输入编码提示。
    ///
    /// 规则（与 ADR 0008 决策一致）：
    /// - 整词已登录：取击数最小编码；若其击数 ≤ 逐字击数之和则取整词码，否则逐字拼接；
    ///   逐字分解采用各字「无手区前缀的双手形式」，避免单手简码拼进词组产生不可键入的混合码；
    /// - 整词未登录但各字均登录：逐字拼接各字最优编码；
    /// - 任意字未登录（含整词未登录且含未登录字）：提示留空并标记 `is_oov`。
    ///
    /// 结果为可缓存结构，渲染层只需在载文/`reload_scheme_dict` 时调用一次，不在每帧重算。
    pub fn build_code_hints(&self, words: &[String]) -> Vec<CodeHint> {
        words
            .iter()
            .map(|w| self.build_hint_for_word(w))
            .collect()
    }

    /// 计算单个词组单位的最优编码提示。
    fn build_hint_for_word(&self, word: &str) -> CodeHint {
        let word_best = self.best_code(word);
        // 逐字分解使用各字「词组语境」的最优编码（无手区前缀的双手形式），
        // 避免把单字独立输入用的单手简码拼进词组产生不可直接键入的混合码。
        let char_parts: Vec<Option<(String, u32)>> = word
            .chars()
            .map(|c| self.best_composition_code(&c.to_string()))
            .collect();
        let any_oov = char_parts.iter().any(|p| p.is_none());

        if let Some((wc, ws)) = word_best {
            // 整词已登录：优先整词，除非逐字明显更省且都能查到
            if any_oov {
                return CodeHint {
                    word: word.to_string(),
                    code: self.apply_commit_terminator(wc),
                    strokes: ws,
                    is_oov: false,
                };
            }
            let char_sum: u32 = char_parts
                .iter()
                .map(|p| p.as_ref().map(|(_, s)| *s).unwrap_or(0))
                .sum();
            if ws <= char_sum {
                return CodeHint {
                    word: word.to_string(),
                    code: self.apply_commit_terminator(wc),
                    strokes: ws,
                    is_oov: false,
                };
            }
            let code: String = char_parts
                .iter()
                .map(|p| p.as_ref().map(|(c, _)| c.as_str()).unwrap_or(""))
                .collect();
            return CodeHint {
                word: word.to_string(),
                code: self.apply_commit_terminator(code),
                strokes: char_sum,
                is_oov: false,
            };
        }

        // 整词未登录：各字均登录则逐字拼接，否则留空
        if any_oov {
            return CodeHint {
                word: word.to_string(),
                code: String::new(),
                strokes: 0,
                is_oov: true,
            };
        }
        let code: String = char_parts
            .iter()
            .map(|p| p.as_ref().map(|(c, _)| c.as_str()).unwrap_or(""))
            .collect();
        let strokes: u32 = char_parts
            .iter()
            .map(|p| p.as_ref().map(|(_, s)| *s).unwrap_or(0))
            .sum();
        CodeHint {
            word: word.to_string(),
            code: self.apply_commit_terminator(code),
            strokes,
            is_oov: false,
        }
    }

    /// 单手前缀形式（`_` 左手 / `+` 右手 / `-` 其它）：并击方案下由单手逐键输入，
    /// 字典中永远以带前缀的派生形式出现；无前缀者即双手并击的规范形式。
    fn has_hand_prefix(code: &str) -> bool {
        code.starts_with('_') || code.starts_with('+') || code.starts_with('-')
    }

    /// 若编码（剥离手区前缀后）属于「需追加 `'` 提交符」集合，则在码尾追加 `'`。
    /// 用于 yoyo 双拼中短码为长码严格前缀的字词（如 文化 `vw`⊂`vwah`），
    /// 提示用户键入 `'` 提交该候选（次选）。
    fn apply_commit_terminator(&self, code: String) -> String {
        // 空格并击简词（% 前缀）一击上屏，无需 ' 提交符，也不应被追加。
        if code.starts_with('%') {
            return code;
        }
        let stripped = code.strip_prefix(['_', '+', '-']).unwrap_or(&code);
        if self.prefix_commit_codes.contains(stripped) {
            format!("{code}'")
        } else {
            code
        }
    }

    /// 取某字在「多字词组逐字拼接」语境下的最优编码：优先无手区前缀的形式
    /// （双手并击/双拼全码），因为词组输入时各字以双手形式并击；单手简码（`_/+` 前缀）
    /// 仅适用于单字独立输入，若拼进词组会产生不可直接键入的混合码（如 方 的 `+<` 剥前缀后丢 `f`）。
    /// 若某字仅提供带前缀的简码（无无前缀形式），则退回其最优简码。未登录返回 `None`。
    fn best_composition_code(&self, ch: &str) -> Option<(String, u32)> {
        let codes = self.word_to_codes.get(ch)?;
        let unprefixed: Vec<&String> = codes.iter().filter(|c| !Self::has_hand_prefix(c)).collect();
        let pool: &[&String] = if unprefixed.is_empty() {
            &codes.iter().collect::<Vec<_>>()
        } else {
            &unprefixed
        };
        let best = pool
            .iter()
            .min_by_key(|c| (Self::calculate_code_strokes(c), c.len()))?;
        Some(((*best).clone(), Self::calculate_code_strokes(best)))
    }

    /// 取某词条「优先简码（击数最少）、其次并击/全码」的编码及其击数；未登录返回 `None`。
    ///
    /// 并击方案（`chord_algebra` 已载入）下，同一字词的字典常同时含无前缀的双手并击规范形式
    /// 与带 `_`/`+` 前缀的单手派生简码。择优键：
    ///   (击数, 是否单手前缀, 码长) —— 优先击数最少的简码；击数相同时保留无前缀的双手并击形式
    ///   （避免展示带手区修饰符的派生串）。非并击方案退化为「最少击数」择优（与旧行为一致）。
    /// 返回的 `strokes` 仍用 `calculate_code_strokes`（与练习统计一致）。
    fn best_code(&self, word: &str) -> Option<(String, u32)> {
        let codes = self.word_to_codes.get(word)?;
        let is_chord = self.chord_algebra.is_some();
        let best = codes.iter().min_by_key(|c| {
            let pref = if is_chord {
                if c.starts_with('%') {
                    0 // 空格并击简词：最优先（一击上屏）
                } else if Self::has_hand_prefix(c) {
                    2 // 单手简码：最不优先（避免展示带手区修饰符的派生串）
                } else {
                    1 // 双手并击/全码
                }
            } else {
                0
            };
            (
                Self::calculate_code_strokes(c),
                pref,
                c.len(),
            )
        })?;
        Some((best.clone(), Self::calculate_code_strokes(best)))
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

    /// 将单个码元字符根据手区展开为物理按键列表。
    /// - 若 `is_right_hand` 为 true：先展开码元基础键，再将每个基础键通过镜像表映射为右手物理按键；
    /// - 若 `is_right_hand` 为 false：展开码元基础键（左手物理按键）。
    pub fn expand_symbol_with_hand(&self, c: char, is_right_hand: bool) -> Vec<String> {
        let mut keys = Vec::new();
        if let Some(chord_keys) = self.symbol_to_keys.get(&c) {
            if is_right_hand {
                for k in chord_keys {
                    let ch = k.chars().next().unwrap_or(' ');
                    let mirrored = self.mirror_left_to_right.get(&ch).copied().unwrap_or(ch);
                    keys.push(mirrored.to_string());
                }
                keys.sort();
            } else {
                keys.extend(chord_keys.clone());
            }
        } else if c.is_ascii_alphanumeric() || c.is_ascii_punctuation() {
            let base_c = c.to_ascii_lowercase();
            if is_right_hand {
                let mirrored = self.mirror_left_to_right.get(&base_c).copied().unwrap_or(base_c);
                keys.push(mirrored.to_string());
            } else {
                keys.push(base_c.to_string());
            }
        } else {
            keys.push(c.to_string());
        }
        keys
    }

    /// 将逻辑编码根据指法规则展开为实际物理按键列表。
    pub fn decompose_code(&self, code: &str) -> Vec<String> {
        if code.is_empty() {
            return Vec::new();
        }

        // 1. 左手单手前缀 `_`：如 `_.` 或 `_v`（整段全为左手）
        if let Some(rest) = code.strip_prefix('_') {
            let mut keys = Vec::new();
            for c in rest.chars() {
                if c == '-' || c == '\'' || c.is_whitespace() {
                    continue;
                }
                keys.extend(self.expand_symbol_with_hand(c, false));
            }
            return keys;
        }

        // 2. 右手单手前缀 `+`：如 `+e` 或 `+H` 或 `+.`（整段全为右手镜像）
        if let Some(rest) = code.strip_prefix('+') {
            let mut keys = Vec::new();
            for c in rest.chars() {
                if c == '-' || c == '\'' || c.is_whitespace() {
                    continue;
                }
                keys.extend(self.expand_symbol_with_hand(c, true));
            }
            return keys;
        }

        // 3. 无单手前缀（双手并击 / 双手交替序列击键，如 "aI" (们), "az" (他们), "xkhr" (可以), "sl" (了), ".Wd", "wCs"）
        // 在并击方案中，双手并击由偶数位左手码元与奇数位右手码元交替构成：
        //   - 2 码 (c1 c2): c1 为左手，c2 为右手镜像
        //   - 4 码 (c1 c2 c3 c4): c1/c3 为左手，c2/c4 为右手镜像
        //   - 3 码 (c1 c2 c3): c1 为左手，c2 为右手镜像，c3 为第3码
        let raw_chars: Vec<char> = code
            .chars()
            .filter(|&c| c != '+' && c != '_' && c != '-' && c != '\'' && !c.is_whitespace())
            .collect();

        let mut keys = Vec::new();
        for (idx, &c) in raw_chars.iter().enumerate() {
            // 双手并击交替：第 2 码 (idx=1) 与 第 4 码 (idx=3) 为右手镜像
            let is_right_hand = (idx % 2 == 1) && (raw_chars.len() >= 2);
            keys.extend(self.expand_symbol_with_hand(c, is_right_hand));
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
        let trimmed_path = path.trim_matches(['/', '.']);
        if trimmed_path.is_empty() {
            return Some(self);
        }

        match self {
            YamlValue::Mapping(map) => {
                // 1. 直接匹配全路径（处理含斜杠的展平 key，例如 "translator/dictionary"）
                for (k, v) in map {
                    if k == trimmed_path {
                        return Some(v);
                    }
                }

                // 2. 尝试逐级分段匹配（例如 "__patch" -> "translator/dictionary" 或 "schema" -> "name"）
                for (k, v) in map {
                    if trimmed_path.starts_with(k) {
                        let rest = &trimmed_path[k.len()..];
                        if rest.starts_with('/') || rest.starts_with('.') {
                            let rest_trimmed = rest.trim_start_matches(['/', '.']);
                            if let Some(found) = v.get(rest_trimmed) {
                                return Some(found);
                            }
                        }
                    }
                }
                None
            }
            _ => None,
        }
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
        } else if let Some(alg) = doc.get("__patch").and_then(|p| p.get("chord_composer")).and_then(|cc| cc.get("algebra")) {
            self.resolve_node_rules(&doc, alg, &base_dir, &mut rules, &mut visited);
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
            if let Some(target_file) = candidate_files.into_iter().find(|p| p.exists())
                && self.load_doc(&target_file).is_ok()
            {
                let canonical = target_file.canonicalize().unwrap_or(target_file);
                if let Some(ext_doc) = self.docs.get(&canonical).cloned() {
                    if section_path.is_empty() {
                        self.resolve_node_rules(&ext_doc, &ext_doc, base_dir, rules, visited);
                    } else if let Some(sec_node) = ext_doc.get(section_path) {
                        self.resolve_node_rules(&ext_doc, sec_node, base_dir, rules, visited);
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
                        '+' | '/' | '-' | '_' | ';' | ':' | '<' | '>' | '?' | '.' | ',' | '\'' | '=' | '%'
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
    fn test_import_tables_merges_sibling_dict() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("dazitui_import_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let main_path = dir.join("main.dict.yaml");
        let sub_path = dir.join("sub.dict.yaml");

        // 主词典引用 sub，且自身不含「文化遗产」词条
        let main = "---\nname: main\nimport_tables:\n  - sub\n...\n\n文\tvw\t100\n化\tah\t100\n";
        // 被导入词典含整词「文化遗产」与单字「遗/产」
        let sub = "遗\tBGp\t100\n产\tCy\t100\n文化遗产\tvaBC\t100\n";
        fs::write(&main_path, main).unwrap();
        fs::write(&sub_path, sub).unwrap();

        let mut visited = std::collections::HashSet::new();
        let dict = SchemeDict::load_dict_with_imports(&main_path, &mut visited).unwrap();

        // 主词典词条保留
        assert_eq!(dict.get_primary_code("文"), Some("vw"));
        // 导入词典词条合并进来
        assert_eq!(dict.get_primary_code("遗"), Some("BGp"));
        assert_eq!(dict.get_primary_code("产"), Some("Cy"));
        // 整词「文化遗产」来自被导入词典，不再退化为逐字拼接
        let hints = dict.build_code_hints(&["文化遗产".to_string()]);
        assert_eq!(hints[0].code, "vaBC");
        assert!(!hints[0].is_oov);

        let _ = fs::remove_dir_all(&dir);
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
            "xform|eg|I|".to_string(),
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
        // xv 镜像到右手 -> v 对应 m, x 对应 . -> [".", "m"]
        assert_eq!(algebra.decompose_code("+."), vec![".", "m"]);

        // 4. 双手并击 / 混打 (无前缀，双手并击交替左/右镜像)
        // 们 (aI) -> 左手 a + 右手 I (eg 镜像为 h i) -> ["a", "h", "i"]
        assert_eq!(algebra.decompose_code("aI"), vec!["a", "h", "i"]);
        // 是 (wCs) -> 左手 w + 右手 C (cf 镜像为 , j) + 第3码 s -> ["w", ",", "j", "s"]
        assert_eq!(algebra.decompose_code("wCs"), vec!["w", ",", "j", "s"]);
        // 到 (.Wd) -> 左手 . (v x) + 右手 W (vw 镜像为 m o) + 第3码 d -> ["v", "x", "m", "o", "d"]
        assert_eq!(algebra.decompose_code(".Wd"), vec!["v", "x", "m", "o", "d"]);
        assert_eq!(algebra.decompose_code("Q"), vec!["e", "f", "s"]);
    }

    #[test]
    fn test_scheme_dict_with_chord_algebra_integration() {
        let mut dict = SchemeDict::default();
        dict.add_entry("到", "_.");
        dict.add_entry("是", "wCs");
        dict.add_entry("们", "aI");

        let rules = vec![
            "xform|xv|\\.|".to_string(),
            "xform|cf|C|".to_string(),
            "xform|eg|I|".to_string(),
            "xform|j|f|".to_string(),
            "xform|,|c|".to_string(),
            "xform|h|g|".to_string(),
            "xform|i|e|".to_string(),
        ];
        dict.set_chord_algebra(ChordAlgebra::from_rules(&rules));

        assert_eq!(dict.decompose_code("_."), vec!["v", "x"]);
        assert_eq!(dict.decompose_code("aI"), vec!["a", "h", "i"]);
        assert_eq!(dict.decompose_code("wCs"), vec!["w", ",", "j", "s"]);

        let counts = dict.project_text_to_keys("到是");
        assert_eq!(counts.get("x"), Some(&1));
        assert_eq!(counts.get("v"), Some(&1));
        assert_eq!(counts.get("w"), Some(&1));
        assert_eq!(counts.get(","), Some(&1));
        assert_eq!(counts.get("j"), Some(&1));
        assert_eq!(counts.get("s"), Some(&1));
    }

    #[test]
    fn test_yoyo_pure_schema_live_integration() {
        // 候选路径：开发仓库与用户配置目录（~/.config/dazitui/schemes）均可能存放方案。
        let candidates = [
            "/home/jackwy/codes/rime/yoyo/rime/yoyo-pure.schema.yaml",
            "/home/jackwy/codes/rime/yoyo/yoyo-pure.schema.yaml",
            "/home/jackwy/.config/dazitui/schemes/yoyo-pure.schema.yaml",
        ];
        if let Some(path) = candidates.iter().find(|p| Path::new(p).exists()) {
            let schema_path = Path::new(path);
            let mut resolver = RimeSchemaResolver::new();
            let rules = resolver.resolve_chord_algebra(schema_path);
            assert!(!rules.is_empty(), "Rules should not be empty");

            let dict = SchemeDict::load_from_file(schema_path).expect("加载 yoyo-pure 方案");
            assert!(dict.chord_algebra().is_some());
            let algebra = dict.chord_algebra().unwrap();

            // yoyo-pure 使用「六脉神剑」指法：. 为 xz 并击，C 为 cx 并击，I 为 eq 镜像为 ip
            assert_eq!(algebra.decompose_code("_."), vec!["x", "z"]);
            assert_eq!(algebra.decompose_code("aI"), vec!["a", "i", "p"]);
            assert_eq!(algebra.decompose_code("wCs"), vec!["w", ",", ".", "s"]);
            assert!(dict.entry_count() > 1000);
            assert_eq!(dict.get_primary_code("到"), Some("_."));
            assert_eq!(dict.get_primary_code("们"), Some("aI"));
        }

        let km_candidates = [
            "/home/jackwy/codes/rime/yoyo/rime/yoyo-pure-km.schema.yaml",
            "/home/jackwy/codes/rime/yoyo/yoyo-pure-km.schema.yaml",
            "/home/jackwy/.config/dazitui/schemes/yoyo-pure-km.schema.yaml",
        ];
        if let Some(path) = km_candidates.iter().find(|p| Path::new(p).exists()) {
            let km_schema_path = Path::new(path);
            let dict = SchemeDict::load_from_file(km_schema_path).expect("加载 yoyo-pure-km 方案");
            assert!(dict.chord_algebra().is_some());
            let algebra = dict.chord_algebra().unwrap();

            // yoyo-pure-km 使用「空明拳」指法（来自 __include: yoyo:/空明拳 外部引用）：
            // . 为 xv 并击、I 为 eg 镜像为 hi、C 为 cf 并击、wCs 分解为 w,逗号,j,s。
            assert_eq!(algebra.decompose_code("_."), vec!["v", "x"]);
            assert_eq!(algebra.decompose_code("aI"), vec!["a", "h", "i"]);
            assert_eq!(algebra.decompose_code("wCs"), vec!["w", ",", "j", "s"]);
        }
    }

    #[test]
    fn test_resolve_chord_algebra_resolves_external_include() {
        // T08：__include: <prefix>:/<section> 跨文件引用应被解析，外部文件中的规则进入结果。
        // 构造最小复现：main.schema.yaml 的 algebra 通过 __include: inc:/target 引用 inc.schema.yaml。
        let dir = std::env::temp_dir().join(format!("dazitui_t08_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let main_path = dir.join("main.schema.yaml");
        let inc_path = dir.join("inc.schema.yaml");
        std::fs::write(
            &main_path,
            "schema:\n  name: t\nchord_composer:\n  algebra:\n    __patch:\n      - 指法\n指法:\n  __include: inc:/target\n",
        )
        .unwrap();
        std::fs::write(
            &inc_path,
            "target:\n  __append:\n    - xform|a|b|\n    - xform|c|d|\n",
        )
        .unwrap();

        let mut resolver = RimeSchemaResolver::new();
        let rules = resolver.resolve_chord_algebra(&main_path);

        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            rules.iter().any(|r| r == "xform|a|b|"),
            "应解析外部 include 的规则 xform|a|b|，得到: {:?}",
            rules
        );
        assert!(
            rules.iter().any(|r| r == "xform|c|d|"),
            "应解析外部 include 的规则 xform|c|d|，得到: {:?}",
            rules
        );
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

    #[test]
    fn test_patch_flattened_keys_and_companion_dict_lookup() {
        let yaml = "__patch:\n  translator/dictionary: my-dict\n";
        let doc = parse_rime_yaml(yaml);
        assert_eq!(
            doc.get("__patch/translator/dictionary").and_then(|v| v.as_str()),
            Some("my-dict")
        );
    }

    #[test]
    fn test_calculate_code_strokes_and_resolve() {
        assert_eq!(SchemeDict::calculate_code_strokes("_."), 1);
        assert_eq!(SchemeDict::calculate_code_strokes(".Wd"), 3);
        assert_eq!(SchemeDict::calculate_code_strokes("wCs"), 3);
        assert_eq!(SchemeDict::calculate_code_strokes("ggll"), 4);
        assert_eq!(SchemeDict::calculate_code_strokes("+e"), 1);
        assert_eq!(SchemeDict::calculate_code_strokes("+H"), 1);
        assert_eq!(SchemeDict::calculate_code_strokes("+H'"), 1);
        assert_eq!(SchemeDict::calculate_code_strokes(""), 0);

        let mut dict = SchemeDict::default();
        dict.add_entry("到", "_.");
        dict.add_entry("是", "wCs");
        dict.add_entry("们", "aI");
        dict.add_entry("怎么", "+H");
        dict.add_entry("我们", "+w");
        dict.add_entry("在", "_z");

        let rules = vec![
            "xform|y|t|".to_string(),
            "xform|u|r|".to_string(),
            "xform|i|e|".to_string(),
            "xform|o|w|".to_string(),
            "xform|p|q|".to_string(),
            "xform|h|g|".to_string(),
            "xform|j|f|".to_string(),
            "xform|,|c|".to_string(),
            "xform|;|a|".to_string(),
            "xform|ar|H|".to_string(),
            "xform|xv|\\.|".to_string(),
            "xform|cf|C|".to_string(),
            "xform|eg|I|".to_string(),
        ];
        dict.set_chord_algebra(ChordAlgebra::from_rules(&rules));

        // 1. 单字与词组反查
        let (strokes_zenme, keys_zenme) = dict.resolve_strokes_and_keys("怎么");
        assert_eq!(strokes_zenme, 1);
        assert_eq!(keys_zenme, vec![";", "u"]);

        let (strokes_men, keys_men) = dict.resolve_strokes_and_keys("们");
        assert_eq!(strokes_men, 2);
        assert_eq!(keys_men, vec!["a", "h", "i"]);

        let (strokes_dao, keys_dao) = dict.resolve_strokes_and_keys("到");
        assert_eq!(strokes_dao, 1);
        assert_eq!(keys_dao, vec!["v", "x"]);

        let (strokes_shi, keys_shi) = dict.resolve_strokes_and_keys("是");
        assert_eq!(strokes_shi, 3);
        assert_eq!(keys_shi, vec!["w", ",", "j", "s"]);

        // 2. 复合词句最大正向匹配（"我们" + "在"）
        let (strokes_combo, keys_combo) = dict.resolve_strokes_and_keys("我们在");
        assert_eq!(strokes_combo, 2);
        assert_eq!(keys_combo, vec!["o", "z"]);

        let (strokes_en, keys_en) = dict.resolve_strokes_and_keys("hello");
        assert_eq!(strokes_en, 5);
        assert_eq!(keys_en, vec!["h", "e", "l", "l", "o"]);
    }

    #[test]
    fn test_build_code_hints() {
        // T02：给定分词文本 + SchemeDict，产出每词「最少击数」最优编码提示。
        let tsv = "中国\tlgy\n中\tk\n国\tlgyi\n人民\twvww\n人\tw\n民\tnay\n中国人\tzhongguoren\n好\tvb\n好\tgood\n";
        let dict = SchemeDict::parse(tsv);

        let words: Vec<String> = ["中", "中国", "国民", "中国人", "人民", "好", "囧", "好x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hints = dict.build_code_hints(&words);
        let get = |w: &str| hints.iter().find(|h| h.word == w).expect("应有该词提示");

        // 单字直接命中
        assert_eq!(get("中").code, "k");
        // 整词优先：中国 lgy(3) <= 中(1)+国(4)=5 → 取整词
        assert_eq!(get("中国").code, "lgy");
        assert_eq!(get("中国").strokes, 3);
        // 整词未登录、各字均登录 → 逐字拼接
        assert_eq!(get("国民").code, "lgyinay");
        // 整词击数更省时逐字：中国人 zhongguoren(12) > 逐字 6 → 拼接 klgyiw
        assert_eq!(get("中国人").code, "klgyiw");
        // 整词与逐字击数相等 → 取整词
        assert_eq!(get("人民").code, "wvww");
        // 多编码取最小击数：好 vb(2) < good(4)
        assert_eq!(get("好").code, "vb");
        // OOV：整词未登录且含未登录字 → 留空并标记 is_oov
        assert_eq!(get("囧").code, "");
        assert!(get("囧").is_oov);
        // 混合：已知字 + 未登录字 → 留空并标记 is_oov
        assert_eq!(get("好x").code, "");
        assert!(get("好x").is_oov);
    }

    #[test]
    fn build_code_hints_appends_commit_apostrophe_for_prefix_code() {
        // yoyo 双拼：某字词同时含「短码 vw」与「长码 vwah」且长码以短码为严格前缀
        // （文化 vw⊂vwah），则短码需键入 ' 提交该候选（次选），提示应显示 "vw'"。
        // 短码本身已是最优（击数最少），不影响择优；仅补 ' 提交符。
        let dict_str = "文化\tvw\t2924455\n文化\tvwah\t0\n遗产\tBGCy\t231136\n中\tk\t100\n";
        let dict = SchemeDict::parse(dict_str);

        let hints = dict.build_code_hints(&["文化".to_string(), "遗产".to_string(), "中".to_string()]);
        let get = |w: &str| hints.iter().find(|h| h.word == w).expect("应有该词提示");

        // 文化：短码 vw 是长码 vwah 的前缀 → 补 ' 提交符
        assert_eq!(get("文化").code, "vw'");
        // 遗产：BGCy 无更长前缀码 → 不补 '
        assert_eq!(get("遗产").code, "BGCy");
        // 中：单码 k 无前缀关系 → 不补 '
        assert_eq!(get("中").code, "k");
    }

    #[test]
    fn build_code_hints_prefers_jianma_shortest_over_full_chord() {
        // yoyo-pure 风格字典同时含「单手派生简码」(_/+ 前缀，逐键，击数最少) 与
        // 「双手并击形式」(无前缀、左右手混排，击数较多)。编码提示应优先显示简码
        // （击数最少的那个），无简码（如词语 4 码）才回退到并击/全码。
        let dict_str = "---\nname: yoyo-pure\n...\n\
的\t_d\t92123018\n的\td.O\t0\n\
是\t_w\t60632202\n是\twCs\t0\n\
有\t+e\t33869044\n有\teHy\t0\n\
就\t+s\t26406172\n就\tsE:\t0\n\
可以\txkhr\t0\n";
        // 并击方案：应用层 load_from_file 会载入 chord_algebra；单元夹具以 default 代数模拟。
        let mut dict = SchemeDict::parse(dict_str);
        dict.set_chord_algebra(ChordAlgebra::default());

        let words: Vec<String> = ["是", "有", "就", "可以"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hints = dict.build_code_hints(&words);
        let get = |w: &str| hints.iter().find(|h| h.word == w).expect("应有该词提示");

        // 单字：简码（单手 1 击）优先于双手并击全码（3 击）
        assert_eq!(get("是").code, "_w", "应优先单手简码 _w 而非双手 wCs");
        // 有：简码 +e（1 击）优先于并击 eHy（3 击）
        assert_eq!(get("有").code, "+e", "应优先单手简码 +e 而非并击 eHy");
        // 就：简码 +s（1 击）优先于并击 sE:（3 击）
        assert_eq!(get("就").code, "+s", "应优先单手简码 +s 而非并击 sE:");
        // 词语 4 码（无更短简码）：直接显示
        assert_eq!(get("可以").code, "xkhr");
    }

    #[test]
    fn build_code_hints_phrase_uses_double_hand_form_not_single_hand_jianma() {
        // yoyo-pure 中 方 同时含单手简码 `+<`(1 击) 与双手形式 `<f`(2 击)；
        // 言 为双手码 `uy`。整词「方言」在词典中登录为 `<fuy`(4 击)。
        // 逐字分解若误用单手简码 `+<` 会得到 `+<uy`，剥前缀后变 `<uy` 且丢 `f`。
        // 正确行为：逐字分解应采用双手形式 `<f`，拼得 `<fuy`，与整词码一致。
        let dict_str = "---\nname: yoyo-pure\n...\n\
方言\t<fuy\t121039\n方\t+<\t2500808\n方\t<f\t0\n言\tuy\t481513\n";
        let mut dict = SchemeDict::parse(dict_str);
        dict.set_chord_algebra(ChordAlgebra::default());

        let hints = dict.build_code_hints(&["方言".to_string(), "方".to_string(), "言".to_string()]);
        let get = |w: &str| hints.iter().find(|h| h.word == w).expect("应有该词提示");

        // 整词已登录：直接显示词典整词码 <fuy（而非退化混合码 <uy）
        assert_eq!(get("方言").code, "<fuy", "方言应显示整词码 <fuy");
        // 单字仍优先其单手简码（词组语境之外）
        assert_eq!(get("方").code, "+<", "单字方应优先单手简码 +<");
        assert_eq!(get("言").code, "uy");
    }

    #[test]
    fn build_code_hints_jianma_is_shortest_code_tiebreak_keeps_two_hand() {
        // 当简码与并击形式击数相同（都为 1 击）时，保留无前缀的双手并击形式作为提示，
        // 避免展示带手区修饰符的派生串。
        let dict_str = "---\nname: yoyo-pure\n...\n\
山\t_a\t100\n山\ta\t0\n";
        let mut dict = SchemeDict::parse(dict_str);
        dict.set_chord_algebra(ChordAlgebra::default());
        let hints = dict.build_code_hints(&["山".to_string()]);
        assert_eq!(hints[0].code, "a", "击数相同应保留无前缀双手形式 a");
    }

    #[test]
    fn build_code_hints_prefers_space_chord_brief() {
        // yoyo-pure-km 空格并击简词（% 前缀）一击上屏：应优先于更长并击/全码，
        // 且击数记为 1，显示带 % 前缀的规范码（渲染层去皮并加空格标记）。
        let dict_str = "---\nname: yoyo-pure\n...\n\
这种\t%_v\t273350\n这种\tzkwi\t0\n";
        let mut dict = SchemeDict::parse(dict_str);
        dict.set_chord_algebra(ChordAlgebra::default());
        let hints = dict.build_code_hints(&["这种".to_string()]);
        assert_eq!(hints[0].code, "%_v", "应优先空格并击简词 %_v");
        assert_eq!(hints[0].strokes, 1, "空格并击简词击数应为 1");
    }

    #[test]
    fn build_code_hints_space_chord_brief_wins_over_single_hand_jianma() {
        // 同一字词同时存在单手简码（_/+ 前缀，无空格）与空格并击简词（% 前缀）时，
        // 词提优先展示空格并击简词（% 为最优先 brief）。
        let dict_str = "---\nname: yoyo-pure\n...\n\
可以\t_v\t100\n可以\t%_v\t0\n";
        let mut dict = SchemeDict::parse(dict_str);
        dict.set_chord_algebra(ChordAlgebra::default());
        let hints = dict.build_code_hints(&["可以".to_string()]);
        assert_eq!(hints[0].code, "%_v", "应优先空格并击简词 %_v 而非单手简码 _v");
    }

    #[test]
    fn calculate_code_strokes_space_chord_is_one() {
        // 空格并击简词（% 前缀）一律记 1 击，不按码元数累加。
        assert_eq!(SchemeDict::calculate_code_strokes("%_v"), 1);
        assert_eq!(SchemeDict::calculate_code_strokes("%+X"), 1);
        assert_eq!(SchemeDict::calculate_code_strokes("%XY"), 1);
    }
}

