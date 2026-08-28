# 遍码提示（编码提示）功能 PRD

> 本文档由设计会话合成，对应架构决策 **ADR 0008**。术语以 `CONTEXT.md` 词汇表为准：编码提示（渲染在字词上方的待敲编码）区别于方案反查（物理键位逆向，供热力图）。

## Problem Statement

跟打用户在学习新输入方案（尤其是并击纯形等高效方案）时，常常不知道某个字/词"最优该怎么敲"。当前对照区只显示原文，用户必须靠记忆或反复试错才能找到最少键的输入码。用户希望：在载文跟打时，直接在对照区把每个字词"按键最少、最优"的输入编码显示在它上方，边打边学，且不影响成绩与上传。

## Solution

在设置中新增布尔开关 **遍码提示**（默认关闭）。开启后，载文时复用既有的方案反查基础设施（SchemeDict / 方案反查），分析赛文文本，为每个词组单位（或单字）算出"最少击数"的输入码，并在对照区以"双行词格"渲染——上行是编码提示、下行是对应字词，提示永远对齐其下方字词。已正确打到的字词，其提示自动隐藏；回改会使其重现。覆盖全部赛文类型（内置/离线/自由发文/剪贴板发文/在线赛文），并击方案首版即支持。

## User Stories

1. As a 跟打用户, I want a "遍码提示" toggle in settings, so that I can persistently turn code hints on or off across sessions.
2. As a 跟打用户, I want code hints shown above each word/char in 内置赛文, so that I learn the optimal code while drilling single-char/word sets.
3. As a 跟打用户, I want code hints in 离线赛文 (local files), so that I can study codes for my own practice texts.
4. As a 跟打用户, I want code hints in 自由发文, so that I can learn codes while typing self-authored text.
5. As a 跟打用户, I want code hints in 剪贴板发文, so that pasted text is also annotated.
6. As a 跟打用户, I want code hints in 在线赛文, so that competition texts are still annotated without affecting upload.
7. As a 跟打用户, I want each hint to show the least-keystroke code, so that I learn the most efficient way to type it.
8. As a 跟打用户, I want a word-level code preferred when it is fewer keystrokes than typing the chars individually, so that I learn word-level input.
9. As a 跟打用户 using a 并击 scheme, I want to see the logical chord code (e.g. `wCs`), so that I know the exact chord to press.
10. As a 跟打用户, I want the hint to auto-hide once I have correctly typed that word/char, so that completed parts are not spoiled.
11. As a 跟打用户, I want the hint to reappear if I 回改 that unit, so that the prompt returns when I redo it.
12. As a 跟打用户, I want multi-code dictionary entries to show the fewest-keystroke variant, so that the displayed code is truly optimal.
13. As a 跟打用户, I want out-of-vocabulary chars to show no hint (blank) rather than an error, so that partial coverage does not break the view.
14. As a 跟打用户 using `yoyo-pure-km`, I want hints that match that chord 纯形 scheme (3-code single / 4-code word, two-hand form), so that the annotation is correct for my setup.
15. As a 跟打用户 with no scheme/dict configured, I want a "方案未配置/无词典" placeholder instead of blank or crash, so that the state is clear.
16. As a 跟打用户 on long articles, I want hints to stay aligned above their words even when text wraps, so that the annotation remains usable.
17. As a 跟打用户, I want super-wide chord codes (e.g. `wCs`) truncated/centered within the hint column, so that layout does not break.
18. As a 跟打用户, I want no performance regression — hints precomputed on 载文 / scheme reload, not recalculated every frame, so that 60 FPS is preserved.
19. As a 跟打用户, I want CJK double-width handled so alignment never drifts, so that hints line up under every character.
20. As a 跟打用户, I want code hints to respect the current 主题预设 for contrast/readability, so that hints are legible.
21. As a 跟打用户, I want 词组赛文 hints aligned per word (ignoring the stripped comma), so that the annotation matches the displayed word.
22. As a developer, I want the hint builder to reuse the existing SchemeDict / 方案反查 engine and the already-loaded `App.scheme_dict`, so that we do not re-parse the scheme.
23. As a 跟打用户, I want hints to never interfere with 成绩上传, so that annotated online runs still submit correctly.

## Implementation Decisions

- **设置开关**：新增布尔设置 `code_hint`（默认 `false`），随设置持久化（save/load 增加对应行），设置视图新增一行（独立焦点），套用既有布尔开关模式（参照现有 `bold` 开关）。
- **复用反查引擎**：编码提示的计算复用既有的方案反查基础设施（核心层的 SchemeDict：解析 TSV / `.dict.yaml` / `.schema.yaml` 含并击 `chord_composer.algebra`，提供 `get_primary_code` / `calculate_code_strokes` / `ChordAlgebra::decompose_code`）。方案通过既有的方案加载通道载入，复用已常驻的 `App.scheme_dict`，不重新解析。
- **最优编码算法（始终取最少击数）**：
  - 取整词码：若词组单位命中主码且其击数 ≤ 词内各字逐字编码击数之和，则展示整词码；否则逐字回退拼接；
  - 多编码词条取 `calculate_code_strokes` 最小者；
  - 并击方案经 `ChordAlgebra` 得逻辑码元序列，按"并击=1 击"度量比较整词码元数与逐字码元数取较少者，并展示击数最少的双手形式（如 `wCs` 优于单手 `wC+s`）；
  - 未登录词/字 → 无整词码则逐字，字亦 OOV 则该字留空；
  - **预计算与缓存**：载文与方案重载时一次性计算并缓存 hint 映射，绝不在每帧重算。
- **对照区排版架构（刻意偏离 Paragraph+Wrap）**：因覆盖长文/在线赛文需自动折行，而 `Paragraph`+`Wrap` 逐字符折行会导致提示行与正文行错位，故采用**手动双行词格**——以词为最小换行单元逐词打包，每个词行由"提示行 + 正文行"两行构成，二者折行点锁步；每词预留固定"提示列宽"（默认 4 列），CJK 双宽与超宽编码在打包阶段统一截断/居中。同一布局服务内置分页赛文与长文。
- **可见性规则**：提示仅在对应字词尚未被正确提交时显示（经 `session.original_status()` 判定），回改使其重现；对全部赛文类型一致生效。
- **参考/测试方案**：开发者与测试以 `yoyo-pure-km`（麓鸣·纯形·空明，位于 `~/.config/dazitui/schemes`：字典 `yoyo-pure.dict.yaml`、方案 `yoyo-pure-km.schema.yaml`）为基准方案。该方案为并击纯形：单字 3 码、词语 4 码，`chord_composer.algebra` 含左右手 `#` 分隔与单手 `_`/`+` 前缀。
  - **解析风险标注**：该 schema 的 `指法` 使用 `__include: yoyo:/空明拳` 外部引用；需验证核心层方案解析/ChordAlgebra 能解析此外部 include，否则并击分解（用于实时键盘高亮/击数度量，ADR 0006）可能不完整。显示的**逻辑码元**来自字典，故提示本身仍渲染；但击数比较与实时键盘映射依赖该 include 的解析。

## Testing Decisions

- **优先沿用现有测试接缝**：核心层为编码提示构建器新增单元测试；UI 层为双行词格增加渲染（对齐/折行）断言。理想情况是单一接缝，避免过度分散。
- **好测试的标准**：只测外部行为——给定"分词文本 + 已载入 SchemeDict"，断言 hint 映射正确（最少击数选择、OOV 留空、多码取最小、并击双手优先、重载缓存失效）；给定"终端宽度 + 文本"，断言打包后提示在折行后仍对齐其下方字词。不测内部布局像素运算。
- **待测模块**：核心层编码提示构建器；对照区双行词格打包。
- **先验（prior art）**：参照仓库内 SchemeDict（`scheme.rs`）既有测试、Segmenter（`segmenter.rs`）分词测试，以及任何既有的 ratatui 渲染断言辅助。
- **测试夹具**：以 `yoyo-pure-km` 方案文件（`yoyo-pure.dict.yaml` + `yoyo-pure-km.schema.yaml`）作为方案夹具；以内置赛文 + 一篇长文作为渲染对齐夹具（覆盖折行与超宽并击码）。

## Out of Scope

- 调整或扩展既有的方案反查热力图（独立功能）。
- 在未配置方案时自动探测"最佳方案"。
- 按上下文消歧多音字/多义编码（如拼音多音），v1 仅取最少击数者。
- v1 提示列宽用户可调（固定默认 4）。
- 修改实时键盘并击高亮逻辑（ADR 0006 已覆盖，本功能仅复用）。
- 非中文文本的编码提示与 i18n。

## Further Notes

- 本文档实现架构决策 **ADR 0008（编码提示/遍码提示功能架构）**；设计会话中确定的四项决策（全范围、已打隐藏、始终最少击数、v1 并击）已编码于此。
- 术语澄清：编码提示（用户待敲编码）区别于方案反查（物理键位），见 `CONTEXT.md`。
- 参考方案 `yoyo-pure-km` 为并击纯形方案，v1 并击支持由其直接驱动验证。
