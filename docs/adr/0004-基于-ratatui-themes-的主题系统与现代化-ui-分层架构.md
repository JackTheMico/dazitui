# 基于 ratatui-themes 的主题系统与现代化 UI 分层架构

界面视觉与色彩系统需要重构以解决边框刺眼、标题与计数模糊、缺乏层次焦点的问题。在引入 `ratatui-themes` 和升级 UI 样式时，确立了分层架构与视觉规范。

## 架构分层决策

- **`dazitui-core` 保持零 UI 依赖**：仅负责存储主题标识字符串（如 `"tokyo-night"`, `"catppuccin-mocha"`）与设置文件持久化，不依赖 `ratatui` 或 `ratatui-themes`。
- **`dazitui`（TUI 层）负责主题映射与渲染**：引入 `ratatui-themes`，将核心配置解析为 `ratatui_themes::ThemePalette`，并转换为打字领域语义色（正文、打对、打错、光标、活动焦点、统计进度等）。

## 视觉与排版规范

1. **圆角边框（Rounded Borders）**：全界面采用 `BorderType::Rounded`。
2. **动态焦点联动（Focus State）**：
   - 处于活动输入/导航状态的面板采用 `palette.accent` 加粗高亮；
   - 非活动/背景面板采用 `palette.muted` 柔和暗边框。
3. **复合结构化标题（Multi-Span Titles）**：
   - 模块名采用 `palette.accent` 加粗；
   - 进度计数（`0/500 字符`）采用高对比度副色分离展示；
   - 暂停与警告状态以 `palette.warning` 徽标突出。
4. **胶囊快捷键栏（Keycap Badges）**：底部提示条采用键帽底色块与说明文字分段排版。
