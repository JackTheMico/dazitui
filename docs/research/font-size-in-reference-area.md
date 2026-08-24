# 调研：dazitui 对照区字体能否放大

**调研日期**：2026-08-23  
**结论**：**不能**单独放大对照区字体。下文详述原因与现有实现。

---

## 背景：ratatui 的渲染模型

ratatui（当前版本 0.29.x / 0.30.x）基于 `crossterm` 后端，以"字符格（cell）"为最小绘制单位。  
终端模拟器负责决定每个字符格的像素宽高，即字体大小。  
**ratatui 本身没有任何控制字体大小的 API**——它只能决定：
- 某格渲染什么字符
- 应用什么 ANSI 样式（粗体、颜色、下划线等）

换句话说，"字号"不属于 ratatui 的职责范围。

---

## 现有实现：OSC 50 字体转义序列

### 代码位置

- [`settings.rs:L176–183`](file:///home/jackwy/codes/rime/dazitui/dazitui-core/src/settings.rs#L176-L183)：`osc_font_size_sequence(size)` 生成 OSC 50 转义序列：
  ```
  \x1b]50;font_size=<size>\x07
  ```
- [`main.rs:L822–827`](file:///home/jackwy/codes/rime/dazitui/dazitui/src/main.rs#L822-L827)：`emit_font_osc()` 通过 `crossterm::execute!` 写入 stdout。
- [`main.rs:L537–538`](file:///home/jackwy/codes/rime/dazitui/dazitui/src/main.rs#L537-L538)：TUI 启动时若 `settings.font == true`，调用 `emit_font_osc()`。
- [`main.rs:L680–681`](file:///home/jackwy/codes/rime/dazitui/dazitui/src/main.rs#L680-L681)：设置界面切换字体开关时，也会调用 `emit_font_osc()`。

### 固定字号

`emit_font_osc()` 硬编码使用 `FONT_SIZE_PT = 16u16`，没有用户可配置的字号大小。

### 关键限制

| 限制 | 说明 |
|------|------|
| **终端支持范围窄** | OSC 50（kitty 格式）只有 kitty、WezTerm 等少数终端支持；xterm、GNOME Terminal、iTerm2 等静默忽略 |
| **全局作用域** | OSC 50 改变的是**整个终端窗口**的字号，不能精准控制某一面板或区域 |
| **不可逆** | 发送后字号会保持到用户手动重置，dazitui 退出后不会恢复原字号（潜在副作用）|

---

## 对照区的渲染方式

[`main.rs:L964–982`](file:///home/jackwy/codes/rime/dazitui/dazitui/src/main.rs#L964-L982) 展示完整渲染流程：

```rust
// 按设置占比分配上下区域
let (ref_pct, type_pct) = area_ratios(app.settings.reference_ratio);
let [ref_area, type_area] = Layout::vertical([
    Constraint::Percentage(ref_pct),
    Constraint::Percentage(type_pct),
]).areas(content);

// 对照区：纯 Paragraph widget
frame.render_widget(
    Paragraph::new(original_line(&app.session, &app.text, app.theme(), app.settings.bold))
        .block(themed_block(app.theme()).title(format!(" 对照区 — {} ", app.text.title)))
        .wrap(Wrap { trim: false }),
    ref_area,
);
```

`original_line()` 返回的每个 `Span` 只携带颜色与粗体修饰符（`add_modifier(bold_modifier(bold))`），**没有字号维度**。  
`Paragraph` widget 亦然——它没有字体大小选项。

---

## 三条路径分析

### 路径 1：OSC 50 全局字号（已有实现）

**可行性**：不能用于「只放大对照区」。即使 OSC 50 生效，它放大的是整个终端，对照区与跟打区都会同等放大，且只有 kitty 等少数终端支持。

**适用场景**：为整体 TUI 字号偏小的 kitty 用户提供一键增大全局字号。

### 路径 2：模拟大字（ASCII art / block drawing 字符）

用多行字符块模拟更大的文字（如 `tui-big-text` crate，基于 Unicode Block 字符拼组汉字图形）。

**可行性**：**对中文不实用**。
- 英文字母可以用固定 ASCII 艺术字拼组（如 `figlet`），但中文有数千个字形，穷举拼组不现实。
- `tui-big-text` 本身仅支持 ASCII/英文。
- 即使强行拼组中文，需要消耗大量终端行数（每个字符占 3–5 行），对照区会显示极少字符，严重影响跟打体验。

**结论**：**此路不通**。

### 路径 3：增大对照区占比（已有功能）

通过 `settings.reference_ratio`（30%–80%）调大对照区所占的垂直行数，让文本自然折行显示更多内容。

**可行性**：**可行，但不是字体变大**，只是给对照区分配更多行空间。对于需要更大"视觉呈现区域"的用户，这是现阶段最实用的替代方案。

---

## 结论

| 方案 | 能否单独放大对照区字体 | 备注 |
|------|----------------------|------|
| OSC 50 转义序列 | ❌ 不能 | 全局字号，且仅 kitty 等少数终端有效 |
| ASCII art / block 字符 | ❌ 不能 | 中文不可行 |
| 调大 reference_ratio | ❌ 字号不变，仅增大区域占比 | 可改善可读性，但不是"字体放大" |

**最终结论：在纯 TUI（ratatui + crossterm）框架内，无法实现只对对照区字体进行放大。**

ratatui 的渲染模型以字符格为最小单位，字号由宿主终端决定；OSC 50 仅能全局作用，且终端支持有限。  
如需"视觉上更大"的对照区，目前唯一可行且无副作用的手段是增大 `reference_ratio` 的值。

---

## 可能的未来方向（Out of Scope for now）

- **图形协议渲染**（Kitty 图形协议 / Sixel）：在终端内渲染任意位图，可以绘制任意字号的字形图片插入对照区。实现极复杂，依赖终端图形协议支持，且需要字体文件 + 字形渲染库，远超当前架构复杂度。
- **改用 GUI 框架**（如 `egui`/`tauri`）：从根本上解决字体大小问题，但会完全脱离 TUI 定位。
