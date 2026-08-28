# 遍码提示（Code-Hint）功能可行性研究

> 范围：在 `dazitui`（打字推/跟打器）中新增「遍码提示开关」，载文时利用用户 Rime 方案文件反查，为对照区每个字词上方渲染「按键最少 / 最优」的输入码提示。
> 性质：**纯可行性研究，未编写任何实现代码**。所有论断均带 `path:line` 引用。

---

## 1. 摘要结论

**可以完成，置信度：高（约 90%）。难度评级：中等（medium）。**

- 「方案反查引擎」已经完整存在：`SchemeDict` 已能解析 TSV / `.dict.yaml` / `.schema.yaml`（含并击 `chord_composer.algebra`），并暴露 `get_primary_code`、`resolve_strokes_and_keys`、`calculate_code_strokes`、`decompose_code`（`dazitui-core/src/scheme.rs:14`、`scheme.rs:305`、`scheme.rs:329`、`scheme.rs:314`、`scheme.rs:380`）。
- 方案已按设置加载并长期持有在 `App.scheme_dict: Option<SchemeDict>`（`dazitui/src/main.rs:668`），反查/热力图/实时键盘已复用同一实例，无需重新解析（`dazitui/src/main.rs:855` `reload_scheme_dict`）。
- 对照区按「词/字」逐段渲染，且已有分词边界（`WordIndex` + `word_boundaries`），可为每个词/字附着提示（`dazitui-core/src/segmenter.rs:30`、`dazitui/src/main.rs:6133` `build_word_spans`、`dazitui/src/main.rs:6211` `original_line`）。
- 设置开关模式成熟：`bold`（布尔）与 `keyboard_mode`（可循环枚举）的「加字段 + save/load 行 + 设置视图行 + 焦点常量 + 切换处理」是现成模板（`dazitui-core/src/settings.rs:341`、`settings.rs:528`、`dazitui/src/main.rs:234-240`、`main.rs:928`、`main.rs:934`、`main.rs:5157-5168`、`main.rs:1924-1985`）。

**主要风险**（详见第 6 节，均非阻塞）：
1. 对照区是单 `Paragraph` + `Wrap`（`dazitui/src/main.rs:3155-3163`），在非内置（长文自动折行）场景下，提示行与正文行的折行独立，CJK 双宽字符对齐会被破坏；内置词组赛文每页单行渲染，对齐天然成立。
2. 并击码（如 `wCs`、`.Wd`）宽于所提示的 1 字（2 列宽），需做宽度裁剪/居中/截断。
3. 部分方案只有 `.schema.yaml` 规则而无 `.dict.yaml` 词条时反查不完整（`scheme.rs:130` `load_from_file` 在找不到伴随词典时回退到空 `dict`）。

---

## 2. 现有可复用基础设施

### 2.1 方案反查引擎 `SchemeDict`（`dazitui-core/src/scheme.rs`）
- 结构体与字段：`SchemeDict { name, word_to_codes: HashMap<String, Vec<String>>, chord_algebra }`（`scheme.rs:14-21`）。注意 `word_to_codes` 的值为 `Vec<String>`——**一个词条可能对应多个编码**（重码），如 `虎码` 等方案常有多编码。
- 解析入口：`parse`（`scheme.rs:52`，支持 TSV/空格/`.dict.yaml` frontmatter）、`load_from_file`（`scheme.rs:130`，自动识别 `.schema.yaml`/`.dict.yaml`/`.txt` 并关联同目录词典与指法）。
- 路径解析：`resolve_scheme_path(scheme, custom_mappings)`（`scheme.rs:219`）覆盖自定义映射、绝对/相对路径、`~/.config/dazitui/schemes/` 与各大 Rime 配置目录。
- 反查 API：
  - `get_primary_code(word) -> Option<&str>`（`scheme.rs:305`）——取该词条 `Vec` 的 `first()`。
  - `calculate_code_strokes(code) -> u32`（`scheme.rs:314`）——过滤手区修饰符 `_ + - ' /` 与空白，按逻辑码元计数（并击算 1 击）。
  - `resolve_strokes_and_keys(text)`（`scheme.rs:329`）——先整词命中，否则贪心最长前缀匹配分段反查，返回总击数 + 物理按键序列。
  - `decompose_code(code)`（`scheme.rs:380`）——若有 `chord_algebra` 则走并击逆向展开，否则单字符展开。**并击方案已原生支持。**
- 并击引擎：`ChordAlgebra { symbol_to_keys, mirror_right_to_left, mirror_left_to_right }`（`scheme.rs:432`），`from_rules`（`scheme.rs:469`）解析 `chord_composer.algebra`，`decompose_code`（`scheme.rs:530`）处理 `_` 左手 / `+` 右手 / 双手交替。

### 2.2 设置与持久化（`dazitui-core/src/settings.rs`）
- `Settings` 结构体（`settings.rs:335-357`）已含 `bold: bool`（`settings.rs:341`）、`keyboard_mode: KeyboardMode`（`settings.rs:343`）、`scheme: String`（`settings.rs:345`）、`input_method: String`（`settings.rs:348`）、`scheme_dict_paths: HashMap<String,String>`（`settings.rs:350`）。
- `SettingsStore::save`（`settings.rs:457-482`）与 `SettingsStore::load`（`settings.rs:485-569`）为极简 key=value 读写，带单字段回退。布尔示例：`save` 中 `bold={}`（`settings.rs:465`），`load` 中 `"bold" => settings.bold = value == "true"`（`settings.rs:528`）；枚举示例：`keyboard_mode` 的 `as_str`/`parse`（`settings.rs:228-244`）与 save/load（`settings.rs:466`、`settings.rs:530`）。`input-method-setting.md` 第 4.1 节已论证「加字段 + save/load 行」是成熟轻量改动。

### 2.3 架构决策（ADR）
- `docs/adr/0005-基于-rusqlite-与方案反查的跟打统计与热力图架构.md` 第 2 节「键位分析与方案反查机制」已确立：基于用户配置方案做反查投射，并在第 3 节用 `WordIndex`（`segmenter.rs`）做错词归因（`set.is_words()` 原生边界 + Jieba 通用分词）。
- `docs/adr/0006-基于-rime-schema-的并击指法逆向解析与实时键盘映射架构.md` 第 1-3 节已确立：`.schema.yaml` 自动关联词典、轻量 `chord_composer.algebra` 解析、`_`/`+`/双手交替的逆向展开。**本功能可直接复用同一 `SchemeDict`，无需新增解析器。**

### 2.4 分词边界（`dazitui-core/src/segmenter.rs`、`dazitui-core/src/lib.rs`）
- `WordIndex { char_to_token: Vec<Option<WordToken>>, char_lookup }`（`segmenter.rs:30`），`WordToken { word, start_char_idx, end_char_idx }`（`segmenter.rs:19`）。
- `WordIndex::build(text, is_builtin_words)`（`segmenter.rs:42`）：内置词组赛文按空格原生切词；否则 Jieba 分词（`segmenter.rs:86`），且只对长度 ≥2 的词建索引（`segmenter.rs:91`）。
- `Text::build_word_index()`（`lib.rs:79`）是对外入口；`Text::session_word_boundaries()`（`lib.rs:64`）返回 `Vec<(usize,usize)>` 词边界，内置/乱序/非词组统一。
- `Session::new_gated_with_words`（`session.rs:184`）已接收 `word_boundaries`，并存于 `Session.group_bounds`（`session.rs:167`）。

---

## 3. 实现路径（逐项映射到文件/函数）

| 步骤 | 文件 | 函数/位置 | 说明 |
|---|---|---|---|
| ① 加设置字段 | `dazitui-core/src/settings.rs` | `Settings`（`settings.rs:335`）、`Default`（`settings.rs:402`）、`save`（`settings.rs:461`）、`load`（`settings.rs:485`） | 新增 `code_hint: bool`（默认 `false`），save 写 `code_hint={}`，load 解析 `code_hint=true`。套用 `bold` 模式（`settings.rs:465`、`settings.rs:528`）。 |
| ② 设置视图行 | `dazitui/src/main.rs` | `render_settings`（`main.rs:5140`）、`settings_row`（`:5216`）、`on_off`（`:5227`） | 仿 `FOCUS_BOLD` 行（`:5157-5162`），新增 `FOCUS_CODE_HINT` 常量（接在 `FOCUS_GROUP_SIZE=6` 之后，`main.rs:234-240`）。 |
| ③ 焦点与切换 | `dazitui/src/main.rs` | `const FOCUS_*`（`main.rs:234`）、`move_focus` 分支（`main.rs:1928`）、`toggle_code_hint` | 仿 `toggle_bold`（`main.rs:928`）：`self.settings.code_hint = !self.settings.code_hint; settings_store.save(...)`。 |
| ④ 取方案实例 | `dazitui/src/main.rs` | `App.scheme_dict`（`main.rs:668`）、`reload_scheme_dict`（`main.rs:855`） | 渲染时直接用 `app.scheme_dict.as_ref()`，无需重新解析（见第 4 节）。 |
| ⑤ 计算提示 | 新增 `dazitui-core/src/hint.rs`（或 `scheme.rs` 内加 `fn code_hint_for(...)`） | 见第 5 节算法 | 输入 `&Text` + `&SchemeDict`，输出 `Vec<Option<String>>`（按字符下标）或 `Vec<(word, Option<String>)>`。 |
| ⑥ 渲染提示 | `dazitui/src/main.rs` | `build_word_spans`（`main.rs:6133`）、`original_line`（`main.rs:6211`）、`Paragraph` 渲染（`main.rs:3154-3165`） | 在正文行之上插入一行「提示行」，逐词/逐字对齐（见第 5、6 节）。 |

---

## 4. 设置开关接入（精确模式）

### 4.1 布尔字段模板（套用 `bold`）
`dazitui-core/src/settings.rs`：
- 结构体加字段（`settings.rs:341` 附近）：
  ```rust
  /// 遍码提示开关：载文时在对照区字词上方渲染最优输入码。
  pub code_hint: bool,
  ```
- `Default`（`:402`）加 `code_hint: false,`。
- `save`（`:461` 的 `format!`）加 `code_hint={}\n`，并在 `write` 处拼接 `settings.code_hint`。
- `load`（`:528` 附近）加分支 `"code_hint" => settings.code_hint = value == "true"`。

### 4.2 设置视图行模板（套用 `FOCUS_BOLD`）
- 新增焦点常量：`const FOCUS_CODE_HINT: usize = 7;`（接在 `main.rs:240` `FOCUS_GROUP_SIZE = 6` 之后；注意 `move_focus` 上限需覆盖到 7）。
- `render_settings`（`:5140`）在「分组大小」行之后追加：
  ```rust
  lines.push(settings_row(
      "遍码提示",
      on_off(app.settings.code_hint),
      focus == FOCUS_CODE_HINT,
      &palette,
  ));
  ```
  `on_off` 已存在（`main.rs:5227`）。

### 4.3 切换处理模板（套用 `toggle_bold`）
- `main.rs:1928` 的 `match app.settings_focus` 增加 `FOCUS_CODE_HINT => app.toggle_code_hint(),`。
- 新增方法（仿 `main.rs:928`）：
  ```rust
  fn toggle_code_hint(&mut self) {
      self.settings.code_hint = !self.settings.code_hint;
      let _ = self.settings_store.save(&self.settings);
  }
  ```
- 若提示需要随方案重新加载时失效/刷新，可在 `reload_scheme_dict`（`main.rs:855`）末尾一并使缓存失效（见 ⑥ 渲染缓存说明）。

---

## 5. 最优编码算法（基于现有 API）

### 5.1 目标
对对照区每个「词/字」给出击数最少的提示编码。

### 5.2 候选编码来源
- 整词级：`SchemeDict::get_primary_code(word)`（`scheme.rs:305`）返回 `Vec` 的 `first()`。
- 字符级：对词内每个单字 `get_primary_code(single_char)`，累加 `calculate_code_strokes`（`scheme.rs:314`）。
- 兜底：`resolve_strokes_and_keys(word)`（`scheme.rs:329`）会自动贪心最长匹配并给出总击数。

### 5.3 推荐算法（逐词/逐字）
对每个分段单元 `seg`（由 `word_boundaries` 或单字决定）：
1. 直接取整词编码 `w = get_primary_code(seg)`；若命中，记 `word_strokes = calculate_code_strokes(w)`。
2. 逐字符求和：`char_sum = Σ calculate_code_strokes(get_primary_code(ch))`（缺失字符按 1 击计，参照 `scheme.rs:363` ASCII / `scheme.rs:368` 其它字符逻辑）。
3. **取较小者**：若 `word_strokes` 存在且 `<= char_sum`，提示 = `w`；否则提示 = 逐字编码串接（如 `你`+`好` → `vbvr`），或仅提示逐字列表。
4. 若整词与逐字均缺失（`get_primary_code` 返回 `None`），提示为 `None`（对照区该位不渲染提示）。

> 该逻辑与 `resolve_strokes_and_keys`（`scheme.rs:329-375`）的贪心最长匹配本质一致，但本功能**只需一个显示用的优选编码串**，故在第 ③ 步做「整词 vs 逐字」的最小击数比较即可，不必展开物理按键。

### 5.4 多编码歧义（重要）
`word_to_codes` 为 `Vec<String>`（`scheme.rs:18`），`get_primary_code` 永远取 `first()`（`scheme.rs:306`）。这意味着：
- 若词条有多编码（如 `abc` 与 `ab`），当前只展示第一个，**不保证是「最优/最少键」**。
- 改进方向（可选，非必需）：在 `hint.rs` 中对同一词条的 `Vec<String>` 取 `calculate_code_strokes` 最小者；或新增 `SchemeDict::get_best_code(word)` 返回击数最小编码。这属于小增强，不在可行性阻塞之列。

### 5.5 并击方案支持
若 `scheme_dict.chord_algebra().is_some()`，提示显示的是**逻辑码元**（如 `wCs`、`.Wd`、`_z`），与既有 `decompose_code`（`scheme.rs:380`）行为一致；用户若要看物理键，可再调用 `decompose_code` 展开。直接在上方显示逻辑码元即可，无需额外工作。

### 5.6 输出结构建议
- 单字赛文/非内置：按字符下标 `Vec<Option<String>>`，与 `original_status()`（`session.rs:566`，返回 `Vec<(char, Option<CharStatus>)>`）一一对应，渲染时并行遍历。
- 词组赛文：按 `word_boundaries` 取 `get_word_at`（`segmenter.rs:117`）得到词文本后查 `get_primary_code`，再回写到该词的起止字符区间。

---

## 6. 提示渲染方案

### 6.1 现状
对照区主体为：
```rust
Paragraph::new(original_line(&app.session, &app.text, theme, bold))
    .block(ref_block).wrap(Wrap { trim: false })
    .scroll((ref_scroll_y, 0))   // main.rs:3154-3165
```
- 词组赛文：`build_word_spans`（`main.rs:6133`）产出**单行** `Vec<Span>`（词间用空格 `Span` 分隔，`main.rs:6151`），每页 `group_size` 个词，**不折行** → 在此场景下，于正文 Line 之上插入一条「提示 Line」即可 1:1 对齐。
- 单字赛文：`original_line` 经 `group_spans`（`main.rs:6371`）按 `group_size` 字符切分为多行；同样可逐行在其上插入提示行。
- 非内置长文：`original_line` 输出全文（`:6262-6275`），由 `Wrap` 自动折行。

### 6.2 可行渲染法
**推荐方案 A（最契合现有结构，内置赛文完美成立）：**
在 `build_word_spans` / `original_line` 中，当 `code_hint` 开启且 `scheme_dict` 存在时，额外产出一条「提示行」`Line`，其 `Span` 与正文行逐词/逐字对应：每个提示 `Span` 的内容为该词的最优编码，并用空格 padding 到该词在正文中的显示宽度（CJK=2 列、ASCII=1 列），以保证提示居中于对应字词之上。把提示行与正文行作为一个 `Vec<Line>` 传给 `Paragraph`（替换当前单行 `TextLines`）。
- 词组赛文每页单行 → 提示行也单行，折行风险为零。
- 单字赛文按 `group_spans` 分行 → 提示行同步分行。

**方案 B（非内置长文更稳妥）：** 把「提示 + 正文」作为一个**不可分割的视觉单元**逐「视觉行」拼装，先按 `ref_inner_width`（`main.rs:3142`）计算折行断点，再成对输出（提示视觉行，正文视觉行），避免 `Wrap` 独立折行导致错位。复杂度高于 A。

### 6.3 CJK 双宽与对齐注意
- CJK 字符显示宽度为 2（ratatui 默认双宽），而提示编码多为 ASCII（宽度 1）。例如单字「到」占 2 列，上方提示 `_.`（2 列）恰好；但并击码 `wCs`（3 列）宽于单字「是」（2 列）。必须：对提示做 `truncate` 到词宽、或 `center` 对齐并 `pad`，超出则截断并可能以颜色/省略号提示。建议提示行仅显示逻辑码元且**截断到 4 列以内**，超长折叠。
- 词间分隔空格 `Span::raw(" ")`（`main.rs:6151`）也要在提示行对应位置保留同宽空格，否则错位。

---

## 7. 风险与边界

1. **折行对齐（中风险，仅非内置长文）**：`Paragraph` + `Wrap` 下提示行与正文行各自折行会错位。缓解：内置赛文天然单行不受影响；非内置采用方案 B 或限定「仅内置词组/单字赛文启用提示」首版。
2. **并击码超宽（低风险）**：`wCs`/`+H'` 等宽于单字，需截断/居中（见 6.3）。
3. **反查不完整（中风险，功能正确性）**：若方案只有 `.schema.yaml` 规则无 `.dict.yaml` 词条，`load_from_file`（`scheme.rs:130`）回退到空 `dict`（`scheme.rs:166`），`get_primary_code` 全 `None` → 提示全空。应在 UI 给出「方案未配置/词条为空」提示（可复用 ADR 0005/热力图的状态文案风格 `main.rs:4565-4575` 的 scheme 未加载提示）。**结论：功能依赖方案已正确配置（设置中 `scheme`/`scheme_dict_paths` 指向含词典的方案）。**
4. **已打/已提交的段落是否显示提示（产品决策）**：`original_status()`（`session.rs:566`）已给出每字 `Correct/Wrong/None` 状态。建议：已打对（或已打过）的字词**隐藏提示**（提示仅用于未打到的前瞻部分），避免剧透答案；实现上是按 `status == None` 才渲染提示。
5. **性能（低风险）**：载文时一次性为整篇计算提示（`scheme.rs:329` 贪心匹配为 O(n·len²) 但词条短，典型赛文数百字，毫秒级）。应在载文/`reload_scheme_dict` 时**预计算并缓存**到 `App`（如 `code_hints: Option<Vec<Option<String>>>`），而非每帧重算（`render_settings` 每帧调用）。注意 `original_line` 每帧调用（`main.rs:3155`），缓存可避免热路径重复反查。
6. **与现有对照区布局冲突（低风险）**：提示占用额外 1 行高度，会压缩对照区实际正文可视行数；`reference_ratio` 已控占比（`settings.rs:339`、`main.rs:2855` `area_ratios`），提示行只是内部多一行，可接受。粗体（`bold`）、主题色均通过 `Span` style 继承，与提示 `Span` 不冲突。
7. **scheme 配置前置依赖**：提示需要 `Settings.scheme` 指向有效方案（`main.rs:856`）。若为空，复用 `reload_scheme_dict` 结果 `app.scheme_dict == None`，此时提示默认关闭或空。

---

## 8. 工作量评估

**难度：中等（medium）。** 基础设施齐备，主要工作量在「渲染对齐」与「缓存/边界处理」。

| 模块 | 改动 | 量级 |
|---|---|---|
| `dazitui-core/src/settings.rs` | 加 `code_hint: bool` + save/load（套用 `bold` 模板） | 极小（~15 行） |
| `dazitui/src/main.rs`（设置） | 加 `FOCUS_CODE_HINT` 常量、`render_settings` 行、`toggle_code_hint`、焦点 match 分支 | 小（~20 行） |
| `dazitui-core/src/hint.rs`（新增）或 `scheme.rs` 加 `code_hint_for` | 逐词/逐字优选编码算法（第 5 节） | 小（~60 行，复用现有 API） |
| `dazitui/src/main.rs`（渲染） | `build_word_spans`/`original_line` 注入提示行 + CJK 对齐 padding/截断；`App` 加缓存字段 + 载文/换方案时预计算 | 中（~100-150 行，含对齐与测试） |
| 单元测试 | `hint.rs` 算法单测 + `original_line` 含提示行的渲染单测（仿 `main.rs:6894` 等现有断言风格） | 小 |

** blocked 检查：** 无硬阻塞。所有依赖（反查引擎、方案加载、分词边界、设置模式、渲染入口）均已存在且被现有功能验证过（`scheme.rs` 测试 `:998`、ADR 0005/0006、现有热力图 `main.rs:4507` 与实时键盘 `:5576` 复用同一 `SchemeDict`）。

**建议首版范围（降低风险）：** 仅在**内置词组赛文与单字赛文**（每页单行/按组分行，无自动折行）启用提示；非内置长文首版可关闭或走方案 B。提示默认对「未打到（`status == None`）」的字词显示，已打对/错后隐藏。
