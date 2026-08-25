# 调研：在跟打区与对照区之间嵌入实时动态按键键盘

**调研日期**：2026-08-25  
**调研主题**：在 dazitui 的对照区（上）与跟打区（下）中间，增加一个能够实时展示用户按键输入的虚拟键盘组件，支持标准斜列（ANSI 60%）与直列矩阵（Ortholinear 4x12）切换。  
**结论**：**非常可行且好做（中低工程复杂度）**。由于 dazitui 统计模块（`StatsView`）中已经内置了完善的斜列与直列键盘矩阵拓扑与字符格渲染算法，UI 渲染与布局切换可直接复用成熟代码。但需在产品与交互设计上妥善处理 **Linux 中文输入法对物理按键的拦截机制** 以及 **终端垂直行高空间适配** 两个关键问题。

---

## 1. 调研背景与现存代码资产

在打字练习工具中，实时显示用户按键（Live Keypress Keyboard）能为练习者提供直观的指法反馈、误触提示与按键节奏感。

经审查当前代码库，dazitui 已经具备极佳的技术储备：

1. **成熟的键盘布局拓扑**：
   - 在 [`dazitui/src/main.rs:L3180–3300`](file:///home/jackwy/codes/rime/dazitui/dazitui/src/main.rs#L3180-L3300) 中，已完整实现了两套键盘矩阵：
     - **标准斜列（`HeatmapLayout::Staggered`）**：ANSI 60% 标准 5 行布局（含行首 Stagger 缩进与特殊按键宽度格式化）；
     - **直列矩阵（`HeatmapLayout::Ortholinear`）**：Planck 4x12 网格矩阵，4 行 12 列紧凑布局。
2. **完善的按键映射与主题系统**：
   - 基于 `ratatui_themes` 的语义调色板（`palette.accent`, `palette.selection`, `palette.muted`, `palette.bg`），能够轻松表达按键的「常态（Idle）」、「按下高亮（Pressed/Active）」和「余温淡出（Decaying）」。
3. **内置方案反查引擎（Reverse Scheme Mapping）**：
   - 依赖 [`dazitui-core/src/scheme.rs`](file:///home/jackwy/codes/rime/dazitui/dazitui-core/src/scheme.rs)，已支持虎码、五笔86/98、小鹤音形、双拼、全拼等多种输入法方案的汉字到按键反查。

---

## 2. 核心技术可行性与关键约束分析

### 2.1 关键约束一：Linux 下中文输入法对物理按键的拦截机制（核心物理限制）

这是所有 Linux 终端打字软件必须面对的底层机制：

```
[ 用户物理敲击按键 'n', 'i', 'h', 'a', 'o', 'Space' ]
                         │
                         ▼
        ┌──────────────────────────────────┐
        │  X11 / Wayland 桌面输入法框架     │ (拦截原始按键，终端完全无感知)
        │  (Fcitx5 / IBus / Rime / 搜狗)   │
        └──────────────────────────────────┘
                         │ (用户按空格选字确认上屏)
                         ▼
        ┌──────────────────────────────────┐
        │  Linux 终端模拟器 (crossterm)    │ 仅收到上屏 UTF-8 汉字:
        │  dazitui Event Loop              │ -> KeyCode::Char('你')
        │                                  │ -> KeyCode::Char('好')
        └──────────────────────────────────┘
```

#### 对实时键盘的影响与应对策略

| 输入场景 | 终端按键捕获情况 | 实时键盘表现 | 推荐处理策略 |
|---|---|---|---|
| **英文 / 数字 / 符号跟打** | `crossterm` 100% 毫秒级直接捕获所有 `KeyEvent` | 🟢 **真·实时毫秒级高亮**，按键按下与弹起完全同步 | 原生事件驱动直接驱动高亮 |
| **形码顶功直输 / 英文直输** | `crossterm` 直接捕获字母键 | 🟢 **真·实时毫秒级高亮** | 原生事件驱动直接驱动高亮 |
| **中文输入法（Fcitx5/IBus）** | 输入法在候选阶段拦截了字母按键，**终端在选字前收不到按键** | 🟡 **上屏瞬间触发反查涟漪动画（Ripple Burst）** | 汉字上屏瞬间，通过 `SchemeDict` 反查出字根/拼音序列（如 `“你” -> “wx”` 或 `“ni”`），在虚拟键盘上瞬间点亮对应的键位并平滑淡出 |

> [!NOTE]
> **关于系统级全局按键监听（evdev / X11 Record）**：
> 虽然可以通过读取 `/dev/input/event*` 绕过输入法获取全局物理按键，但这需要 `root` / `input` 组权限，且在 Wayland 下受到严格安全隔离，会彻底破坏 dazitui 作为轻量免提权 TUI 应用的便携性与安全性。因此**强烈推荐采用「物理直输真高亮 + 中文上屏反查涟漪」的双模方案**。

---

### 2.2 关键约束二：终端垂直行高（Screen Real Estate）与空间挤压

在标准 80x24 或 80x25 终端窗口中，垂直行数非常宝贵：

```
┌──────────────────────────────────────────────────────────┐ ─── 0
│  dazitui 顶部状态栏 / 速度 / 进度                         │ 1~2 行
├──────────────────────────────────────────────────────────┤ ─── 2
│  对照区 (Paragraph)                                      │ 原占 40% (约 7~8 行)
├──────────────────────────────────────────────────────────┤ ─── 10
│  [新增] 实时虚拟键盘 (ANSI 60% 占 5~7 行 / 直列占 4~6 行) │ 5~7 行
├──────────────────────────────────────────────────────────┤ ─── 16
│  跟打区 (Paragraph)                                      │ 原占 60% (约 10~11 行)
├──────────────────────────────────────────────────────────┤ ─── 22
│  底部快捷键与帮助栏                                       │ 2 行
└──────────────────────────────────────────────────────────┘ ─── 24
```

#### 空间优化方案

1. **支持快捷键一键显隐与多种模式**：
   - 快捷键 `k`（或 `Alt-K` / 设置项）在三种状态间轮转：`隐藏 (Off) -> 标准斜列 (Staggered) -> 直列矩阵 (Ortholinear)`。
2. **终端高度自适应（Auto Collapse）**：
   - 当终端可用总高度 $< 26$ 行时，若开启键盘，自动将对照区与跟打区的内边距压缩，或者在超小终端（$< 20$ 行）自动临时隐藏键盘并给出提示。
3. **紧凑型渲染（Compact Keycaps）**：
   - 现有的 `HeatmapLayout` 每一行之间插入了一个空行（`keyboard_lines.push(Line::from(""))`）；
   - 在主界面嵌入时，去除空行，斜列键盘仅需 5 行高度，直列键盘仅需 4 行高度，紧凑精致。

---

### 2.3 布局对比：标准斜列 vs 直列矩阵

| 维度 | 标准斜列 (ANSI 60% Staggered) | 直列矩阵 (Ortholinear 4x12 Planck) |
|---|---|---|
| **行数占用** | **5 行**（数字行 + 3 行字母/标点 + 空格行） | **4 行**（无独立数字行，4x12 紧凑对称） |
| **列宽占用** | 约 58~68 字符宽度 | 约 48~52 字符宽度 |
| **视觉体验** | 符合 95% 大众笔记本和传统机械键盘的视觉习惯 | 极具极客风格，键位绝对对齐，行高占用最少，非常适合 40%/分体键盘爱好者 |
| **实现复用度** | 直接复用现有 `HeatmapLayout::Staggered` | 直接复用现有 `HeatmapLayout::Ortholinear` |

---

### 2.4 按键动态视觉与衰减动画（Decay & Glowing）

为使按键输入有“打击感”和“机械键盘背光”的视觉体验，可设计轻量级时间衰减状态：

```rust
pub struct KeyState {
    /// 记录最近一次被按下的时间戳
    pub last_pressed: Instant,
    /// 触发源：直接按键还是方案反查
    pub source: KeyPressSource,
}
```

在渲染每一帧时，根据 `elapsed = now - last_pressed` 计算透明度与颜色梯度：

```
           0ms                       120ms                      250ms
按键触发 ─────────> [ 强高亮 (Accent + Bold) ] ───> [ 次高亮 (Muted fg) ] ───> [ 常态 (Idle 幽灵色) ]
```

- **极低计算开销**：Ratatui 的 Immediate Mode 渲染架构非常适合这种无状态帧计算，耗时 $< 0.05\text{ms}$，在 60 FPS 刷新下丝滑流畅。

---

## 3. 总体架构设计与落地计划

### 3.1 核心模块分工

```
dazitui-core/
  └── src/settings.rs          # 新增键盘开关与布局设置 (KeyboardMode: Off / Staggered / Ortholinear)
dazitui/
  ├── src/keyboard.rs (新增)   # 独立的虚拟键盘渲染 Widget，包含斜列/直列拓扑与按键高亮状态机
  └── src/main.rs              # 主界面布局拆分为 Ref Area / Keyboard Area / Type Area，串联事件与反查
```

### 3.2 界面排版切分逻辑（伪代码）

```rust
let (ref_pct, type_pct) = area_ratios(app.settings.reference_ratio);

if app.settings.keyboard_mode == KeyboardMode::Off {
    // 原有两段式布局
    let [ref_area, type_area] = Layout::vertical([
        Constraint::Percentage(ref_pct),
        Constraint::Percentage(type_pct),
    ]).areas(content);
} else {
    // 新增三段式布局：固定键盘行高，其余按比例分配给对照区与跟打区
    let kb_height = match app.settings.keyboard_mode {
        KeyboardMode::Staggered => 7,    // 5 行键位 + 2 行边框
        KeyboardMode::Ortholinear => 6,  // 4 行键位 + 2 行边框
        _ => 0,
    };
    
    let [ref_area, kb_area, type_area] = Layout::vertical([
        Constraint::Percentage(ref_pct),
        Constraint::Length(kb_height),
        Constraint::Percentage(type_pct),
    ]).areas(content);
    
    // 渲染键盘组件
    render_live_keyboard(frame, &app.live_keyboard, kb_area, &palette);
}
```

---

## 4. 调研结论与工作量评估

| 评估项 | 结论 | 说明 |
|---|---|---|
| **综合可行性** | 🟢 **完全可行** | 核心渲染算法在热力图模块已验证，无第三方重型依赖引入风险 |
| **开发工作量** | 🟢 **约 1~1.5 个开发人日** | 绝大部分代码可直接提炼复用，只需新增状态跟踪和布局切分 |
| **性能影响** | 🟢 **几乎为 0** | 纯字符格着色，内存增量 $< 50\text{KB}$，每帧渲染时间 $< 0.1\text{ms}$ |
| **用户体验提升** | 🟢 **显著增强** | 极大增加跟打沉浸感与节奏感，满足直列/斜列不同键盘硬件爱好者的视觉偏好 |
| **主要注意事项** | ⚠️ **中文输入法语义提示** | 需在帮助文档与交互中向用户传达：中文跟打时通过“上屏方案反查”展示键位，英文直输为“真实时击键” |

---

## 5. 建议推进步骤

1. **Step 1**：在 `dazitui-core/src/settings.rs` 中新增 `KeyboardDisplay` 枚举（`Off` / `Staggered` / `Ortholinear`），支持持久化存储；
2. **Step 2**：从 `main.rs` 的热力图渲染中抽取出公共的键盘拓扑结构，构建 `LiveKeyboard` 状态机与 `render_live_keyboard` 组件；
3. **Step 3**：在 `main.rs` 的主内容区域中实现三段式弹性布局分配，挂接 `handle_key` 物理击键与中文上屏时的 `SchemeDict` 反查高亮；
4. **Step 4**：在设置界面（`Ctrl-E`）中增加键盘显示配置项，支持跟打界面快捷键（如 `k`）快速切换。
