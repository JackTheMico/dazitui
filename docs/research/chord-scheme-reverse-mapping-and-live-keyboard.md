# 调研：并击方案（Rime chord_composer）码表反查与实时键盘按键映射分析及解决方案

**调研日期**：2026-08-25  
**调研主题**：解决在并击方案（如麓鸣纯形 `yoyo-pure` / 空明拳 / 六脉神剑）下，汉字反查编码中的并击码元（如 `.`）在 dazitui 实时键盘中被错误识别为单一物理键 `.`，而无法同时点亮物理并击键位（如 `x` 和 `v`）的问题。  
**关联代码库**：
- `dazitui`: [`dazitui-core/src/scheme.rs`](file:///home/jackwy/codes/rime/dazitui/dazitui-core/src/scheme.rs), [`dazitui/src/main.rs`](file:///home/jackwy/codes/rime/dazitui/dazitui/src/main.rs)
- `yoyo`: [`yoyo-pure.schema.yaml`](file:///home/jackwy/codes/rime/yoyo/rime/yoyo-pure.schema.yaml), [`yoyo.yaml`](file:///home/jackwy/codes/rime/yoyo/rime/yoyo.yaml), [`yoyo-pure.dict.yaml`](file:///home/jackwy/codes/rime/yoyo/rime/yoyo-pure.dict.yaml)

---

## 1. 问题复现与现象定义

用户在 `dazitui` 中启用实时虚拟键盘（`LiveKeyboard`）并配置方案为 `yoyo-pure`（麓鸣·纯形·六脉 / 空明）：
1. 用户在物理键盘上使用左手同时按下 `x` 和 `v`（并击出码元 `.`）；
2. Rime 输入法通过 `chord_composer` 将并击的物理按键 `xv` 转化为码元 `.`，并成功上屏汉字（例如一简字「到」`_.` 或三码全码「到」`.Wd`）；
3. 汉字上屏后，`dazitui` 的方案反查引擎 `SchemeDict` 将「到」反查为编码 `_.`（或 `.Wd`）；
4. `dazitui` 调用 `SchemeDict::decompose_code_to_keys` 将编码分解为按键序列，结果得到了 `["."]`（或 `[".", "w", "d"]`）；
5. 实时虚拟键盘高亮了物理键盘右下方的标点键 **`[.]`**；
6. **用户预期**：用户实际敲击的是左手的 **`x`** 与 **`v`**（或右手镜像键），实时键盘应当**同时点亮 `x` 和 `v`**，而非点亮无关的标点符号键 `.`。

---

## 2. 第一方事实与根因溯源（Primary Sources）

### 2.1 Rime 输入法引擎的分层架构（物理按键 vs 逻辑码元）

在 Rime 输入法体系（参见 [`yoyo-pure.schema.yaml:L36-49`](file:///home/jackwy/codes/rime/yoyo/rime/yoyo-pure.schema.yaml#L36-L49) 与 [`yoyo/docs/design/pure-shape-unified-architecture.md:L80-100`](file:///home/jackwy/codes/rime/yoyo/docs/design/pure-shape-unified-architecture.md#L80-L100)）中，击键到上屏经历了严格的分层：

```
[ 用户物理击键: 左手 'x' + 'v' 并击 ]
                 │
                 ▼
┌──────────────────────────────────────────────────────────┐
│ 1. chord_composer (并击物理层)                            │
│    - 依据 alphabet: "12345qwertasdfgzxcvb 67890..."      │
│    - 依据 algebra: xform|xv|.| / xform|vx|.|             │
│    - 将物理按键重写为逻辑码元 (Pure Symbol) '.'            │
└──────────────────────────────────────────────────────────┘
                 │ 产出纯码元 '.'
                 ▼
┌──────────────────────────────────────────────────────────┐
│ 2. pure_popping.lua (上下文感知 FSM 状态机)               │
│    - 接收码元 '.'，构建输入缓冲区 Preedit Buffer           │
└──────────────────────────────────────────────────────────┘
                 │
                 ▼
┌──────────────────────────────────────────────────────────┐
│ 3. table_translator (词典查表层)                          │
│    - 查询 yoyo-pure.dict.yaml: "到\t_.\t14948468"        │
│    - 命中词条 "到" 并 Commit 上屏                         │
└──────────────────────────────────────────────────────────┘
                 │ 提交 UTF-8 汉字 '到'
                 ▼
┌──────────────────────────────────────────────────────────┐
│ 4. Linux 终端模拟器 (crossterm)                          │
│    - dazitui 仅能收到最终上屏的 KeyEvent::Char('到')      │
└──────────────────────────────────────────────────────────┘
```

**关键事实 1**：词典文件（`yoyo-pure.dict.yaml`）中记录的全部是**逻辑码元（Logical Code Symbols）**，例如：
- `到\t_.\t14948468`
- `是\twCs\t0`
- `为\tO<O\t0`
- `等\tcVz\t0`
- `就\tsE:\t0`

其中 `.`、`W`、`C`、`O`、`<`、`V`、`E`、`:` 等字符均是**并击码元**，并非标准的单键物理按键。

### 2.2 yoyo 方案中的指法映射规则（Chord Algebra）

在 [`yoyo.yaml:L547-683`](file:///home/jackwy/codes/rime/yoyo/rime/yoyo.yaml#L547-L683)（以空明拳指法为例，六脉神剑同理）中，定义了完整的码元代数变换规则：

1. **第零行码元（字母+符号双键代替数字行）**：
   - `qa` / `aq` $\to$ `0`
   - `wc` / `cw` $\to$ `9`
   - `ex` / `xe` $\to$ `8`
   - `rz` / `zr` $\to$ `7`
   - `rt` / `tr` $\to$ `6`
2. **符号码元（8 个双键并击）**：
   - `af` / `fa` $\to$ `;`
   - `as` / `sa` $\to$ `:`
   - `cx` / `xc` $\to$ `,`
   - **`vx` / `xv` $\to$ `.`** （← 溯源点）
   - `vz` / `zv` $\to$ `/`
   - `at` / `ta` $\to$ `<`
   - `de` / `ed` $\to$ `>`
   - `xz` / `zx` $\to$ `?`
3. **大写双键/三键码元（26 个）**：
   - `av` $\to$ `A`, `qw` $\to$ `B`, `cf` $\to$ `C`, `dv` $\to$ `D`, `ev` $\to$ `E`, `gs` $\to$ `F`, `dg` $\to$ `G`, `ar` $\to$ `H`, `eg` $\to$ `I`, `fx` $\to$ `J`, `dr` $\to$ `K`, `rs` $\to$ `L`, `dw` $\to$ `M`, `es` $\to$ `N`, `aw` $\to$ `O`, `fq` $\to$ `P`, `et` $\to$ `R`, `sv` $\to$ `S`, `qt` $\to$ `T`, `gw` $\to$ `U`, `sz` $\to$ `V`, `vw` $\to$ `W`, `dx` $\to$ `X`, `tw` $\to$ `Y`, `fz` $\to$ `Z`
   - `esf` / `efs` $\to$ `Q`（三键并击）
4. **小写双键码元（11 个）**：
   - `fw` $\to$ `h`, `ew` $\to$ `i`, `df` $\to$ `j`, `ds` $\to$ `k`, `fs` $\to$ `l`, `cv` $\to$ `m`, `cs` $\to$ `n`, `rw` $\to$ `o`, `qr` $\to$ `p`, `er` $\to$ `u`, `ef` $\to$ `y`
5. **单键码元（15 个，单键直出）**：
   - `a`, `b`, `c`, `d`, `e`, `f`, `g`, `q`, `r`, `s`, `t`, `v`, `w`, `x`, `z`
6. **左右手镜像规则**：
   - 右手区按键 `y u i o p h j k l ; n m , . /` 镜像对应左手区 `t r e w q g f d s a b v c x z`。

### 2.3 dazitui 现行反查与分解逻辑的缺陷分析

审查 [`dazitui-core/src/scheme.rs:L164-177`](file:///home/jackwy/codes/rime/dazitui/dazitui-core/src/scheme.rs#L164-L177)：

```rust
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
```

以及 [`dazitui/src/main.rs:L1951-1958`](file:///home/jackwy/codes/rime/dazitui/dazitui/src/main.rs#L1951-L1958)：

```rust
} else if let Some(dict) = scheme_dict {
    if let Some(code) = dict.get_primary_code(&s) {
        let keys = SchemeDict::decompose_code_to_keys(code);
        for k in &keys {
            live_kb.press_key(k, now);
        }
    }
}
```

**缺陷链路**：
1. `dazitui` 在反查「到」时，从字典得到 `_.`（或 `.Wd`）；
2. `decompose_code_to_keys` 过滤掉前缀 `_` 后，看到字符 `.`；
3. 因为 `'.'.is_ascii_punctuation()` 为 `true`，直接将其放入 `keys` 列表：`vec!["."]`；
4. `live_kb.press_key(".", now)` 触发，实时键盘根据键名 `"."` 寻找 ANSI 布局第 4 行的 `.` 键位；
5. **导致虚拟键盘上点亮了 `.`，而不是物理击键 `x` 和 `v`**。
6. 同样地，对于 `W`（物理键 `v+w`），`to_ascii_lowercase()` 变成了 `"w"`，只点亮了 `w` 键，遗失了并击的 `v` 键；对于 `0`~`9`、`:`、`<`、`>`、`?` 等并击码元，均无法还原真实物理指法。

---

## 3. 解决方案设计 (Architectural Solutions)

要让实时键盘正确显示真实的物理击键（如 `x` 和 `v`），必须在反查编码与键盘渲染之间建立**指法码元展开层（Chord Fingering Expansion Layer）**。

### 方案 A：在 `SchemeDict` 中引入指法代数展开器（推荐 · 优雅且性能极高）

在 `SchemeDict` 中增加可选的 `FingeringMap`（指法展开映射表）。

#### 3.1.1 数据结构设计

```rust
/// 并击方案指法类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FingeringKind {
    #[default]
    None,               // 传统单键形码/拼音（五笔、虎码、小鹤音形等）
    YoyoKongming,       // 麓鸣·空明拳指法 (yoyo-km)
    YoyoLiumai,         // 麓鸣·六脉神剑指法 (yoyo-pure)
    YoyoZhemei,         // 麓鸣·折梅指法 (192)
    YoyoHanmei,         // 麓鸣·寒梅指法 (192)
}

/// 码元展开映射表
#[derive(Debug, Clone, Default)]
pub struct FingeringMap {
    /// 逻辑码元字符 -> 展开后的物理按键切片（如 '.' -> &["x", "v"], 'W' -> &["v", "w"], 'Q' -> &["e", "s", "f"]）
    symbol_to_keys: HashMap<char, Vec<&'static str>>,
}
```

#### 3.1.2 码元到物理键展开逻辑

当方案配置为 `YoyoKongming`（空明拳）或 `YoyoLiumai`（六脉神剑）时，定义静态映射表：

```rust
impl FingeringMap {
    pub fn for_kind(kind: FingeringKind) -> Option<Self> {
        match kind {
            FingeringKind::None => None,
            FingeringKind::YoyoKongming => {
                let mut map = HashMap::new();
                // 符号双键码元
                map.insert('.', vec!["x", "v"]);
                map.insert('/', vec!["v", "z"]);
                map.insert(',', vec!["c", "x"]);
                map.insert(';', vec!["a", "f"]);
                map.insert(':', vec!["a", "s"]);
                map.insert('<', vec!["a", "t"]);
                map.insert('>', vec!["d", "e"]);
                map.insert('?', vec!["x", "z"]);
                
                // 第零行双键码元
                map.insert('0', vec!["q", "a"]);
                map.insert('9', vec!["w", "c"]);
                map.insert('8', vec!["e", "x"]);
                map.insert('7', vec!["r", "z"]);
                map.insert('6', vec!["r", "t"]);

                // 大写双键码元
                map.insert('A', vec!["a", "v"]);
                map.insert('B', vec!["q", "w"]);
                map.insert('C', vec!["c", "f"]);
                map.insert('D', vec!["d", "v"]);
                map.insert('E', vec!["e", "v"]);
                map.insert('F', vec!["g", "s"]);
                map.insert('G', vec!["d", "g"]);
                map.insert('H', vec!["a", "r"]);
                map.insert('I', vec!["e", "g"]);
                map.insert('J', vec!["f", "x"]);
                map.insert('K', vec!["d", "r"]);
                map.insert('L', vec!["r", "s"]);
                map.insert('M', vec!["d", "w"]);
                map.insert('N', vec!["e", "s"]);
                map.insert('O', vec!["a", "w"]);
                map.insert('P', vec!["f", "q"]);
                map.insert('Q', vec!["e", "s", "f"]); // 三键并击
                map.insert('R', vec!["e", "t"]);
                map.insert('S', vec!["s", "v"]);
                map.insert('T', vec!["q", "t"]);
                map.insert('U', vec!["g", "w"]);
                map.insert('V', vec!["s", "z"]);
                map.insert('W', vec!["v", "w"]);
                map.insert('X', vec!["d", "x"]);
                map.insert('Y', vec!["t", "w"]);
                map.insert('Z', vec!["f", "z"]);

                // 小写双键码元
                map.insert('h', vec!["f", "w"]);
                map.insert('i', vec!["e", "w"]);
                map.insert('j', vec!["d", "f"]);
                map.insert('k', vec!["d", "s"]);
                map.insert('l', vec!["f", "s"]);
                map.insert('m', vec!["c", "v"]);
                map.insert('n', vec!["c", "s"]);
                map.insert('o', vec!["r", "w"]);
                map.insert('p', vec!["q", "r"]);
                map.insert('u', vec!["e", "r"]);
                map.insert('y', vec!["e", "f"]);

                Some(Self { symbol_to_keys: map })
            }
            FingeringKind::YoyoLiumai => {
                // 六脉神剑映射表 (与 yoyo.yaml:L415-543 一致)
                // 如 xz -> '.', xv -> 'm', vc -> ',', vz -> '/' 等
                let mut map = HashMap::new();
                map.insert('.', vec!["x", "z"]);
                map.insert('/', vec!["v", "z"]);
                map.insert(',', vec!["v", "c"]);
                map.insert(';', vec!["f", "a"]);
                // ...
                Some(Self { symbol_to_keys: map })
            }
            _ => None,
        }
    }

    /// 展开单个码元为物理按键集合
    pub fn expand_symbol(&self, c: char) -> Vec<String> {
        if let Some(keys) = self.symbol_to_keys.get(&c) {
            keys.iter().map(|k| k.to_string()).collect()
        } else if c.is_ascii_alphanumeric() {
            vec![c.to_ascii_lowercase().to_string()]
        } else {
            vec![c.to_string()]
        }
    }
}
```

#### 3.1.3 改造 `SchemeDict::decompose_code_to_keys`

```rust
impl SchemeDict {
    pub fn decompose_code_to_keys_with_fingering(
        code: &str,
        fingering: Option<&FingeringMap>,
    ) -> Vec<String> {
        let mut keys = Vec::new();
        for c in code.chars() {
            if c == '+' || c == '-' || c == '_' || c.is_whitespace() {
                continue;
            }
            if let Some(f) = fingering {
                keys.extend(f.expand_symbol(c));
            } else {
                if c == '/' {
                    continue;
                }
                if c.is_ascii_alphanumeric() || c.is_ascii_punctuation() {
                    keys.push(c.to_ascii_lowercase().to_string());
                } else {
                    keys.push(c.to_string());
                }
            }
        }
        keys
    }
}
```

#### 3.1.4 效果演示

- 用户打单字「到」（反查编码 `_.`）：
  - `decompose_code_to_keys_with_fingering("_.", Some(&kongming))`
  - 遇到 `_` $\to$ 跳过；
  - 遇到 `.` $\to$ 查表得到 `["x", "v"]`；
  - 返回 `vec!["x", "v"]`；
  - `LiveKeyboard` 接收到 `["x", "v"]`，**在虚拟键盘上同时点亮 `X` 和 `V` 两个按键**！
- 用户打三码字「是」（反查编码 `wCs`）：
  - `w` $\to$ `["w"]`
  - `C` $\to$ `["c", "f"]`
  - `s` $\to$ `["s"]`
  - 返回 `vec!["w", "c", "f", "s"]`，物理指法完全真实还原！

---

### 方案 B：自动解析 Rime `.schema.yaml` 中的 `chord_composer.algebra`

若希望彻底通用化，无需在 Rust 代码中硬编码任何特定方案映射，可实现一个轻量的 Rime Schema `chord_composer.algebra` 逆向解析器：

1. **读取 Schema**：解析 `yoyo-pure.schema.yaml` 中的 `chord_composer.algebra`；
2. **提取 `xform` 规则**：
   - 提取形如 `- xform|xv|.|`、`- xform|vw|W|`、`- xform|esf|Q|` 的规则；
   - 逆向构建 `HashMap<char, Vec<&str>>`：`map.insert('.', vec!["x", "v"])`；
3. **伴生加载**：在 `SchemeDict::resolve_scheme_path` 时，若发现同目录下存在 `*.schema.yaml`，自动提取指法并注入 `SchemeDict`。

---

## 4. 方案对比与决策建议

| 维度 | 方案 A：内置指法映射 + 自动识别 | 方案 B：运行时全量解析 Rime Schema Algebra |
|---|---|---|
| **工程复杂度** | 🟢 **低（约 80~120 行纯 Rust 代码）** | 🟡 **中（需实现 YAML include/patch 与正则解析器）** |
| **执行性能** | 🟢 **零运行时开销，启动瞬间就绪** | 🟢 **启动时解析一次，之后零开销** |
| **可维护性** | 🟢 **覆盖主流方案（麓鸣并击全系列），单测极易编写** | 🟢 **对任意未知 Rime 并击方案自适应** |
| **推荐优先级** | 🌟 **第一阶段优先实施（立竿见影）** | 🚀 **第二阶段作为通用能力增强** |

---

## 5. 落地执行路线与代码清单 (Checklist)

1. [ ] **更新 `dazitui-core/src/scheme.rs`**：
   - 新增 `FingeringKind` 枚举与 `FingeringMap`；
   - 修复 `is_likely_code`（加入 `.`, `:`, `<`, `>`, `?` 判定）；
   - 在 `SchemeDict` 中持有 `fingering: Option<FingeringMap>`，并在 `decompose_code_to_keys` 中支持码元展开；
   - 编写单元测试验证 `_.` 展开为 `["x", "v"]`，`wCs` 展开为 `["w", "c", "f", "s"]`。
2. [ ] **更新 `dazitui/src/main.rs`**：
   - 在 `handle_key` 中调用反查展开时传入 scheme 绑定的指法规则；
   - 确保 `live_kb.press_keys(keys, now)` 能够批量瞬间激活所有并击键位。
3. [ ] **验证跟打手感**：
   - 运行 `cargo run -- <赛文>` 载文测试，使用 `yoyo-pure` 跟打，观察敲击 `到` 时实时键盘是否完美同步点亮 `x` 与 `v`。
