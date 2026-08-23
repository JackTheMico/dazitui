# 意图
Linux 上的中文打字练习工具（TUI），基于 Rust + Ratatui。用户对照给定的赛文跟打，实时标红打错的字，打完后得到 WPM 等统计；支持加载本地文件和 52dazi.cn 在线赛文、上传成绩。

# 文件

## Cargo.toml
Workspace 根配置文件，管理 dazitui 与 dazitui-core 子 crate

## CONTEXT.md
领域术语表与业务概念定义

## dazitui-core/
中文打字练习核心领域库，负责赛文载入与处理、跟打会话状态机、用户配置以及 52dazi 在线服务交互。

## dazitui/
基于 Ratatui 构建的 Linux TUI 终端打字跟打应用。
