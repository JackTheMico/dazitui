# AGENTS.md

Linux 上的中文打字练习工具（TUI），Rust + Ratatui。开发前先读 `CONTEXT.md`（术语表）和相关 ADR。

## 开发命令

```bash
cargo build          # 构建
cargo test           # 运行测试
cargo run -- <file>  # 运行：dazitui 文件名（载文跟打）
```

## Agent skills

### Issue tracker

Issues 和 PRD 存于 GitHub Issues，用 `gh` CLI 操作。See `docs/agents/issue-tracker.md`.

### Triage labels

默认五标签：`needs-triage` / `needs-info` / `ready-for-agent` / `ready-for-human` / `wontfix`。See `docs/agents/triage-labels.md`.

### Domain docs

单上下文：根目录 `CONTEXT.md` + `docs/adr/`。See `docs/agents/domain.md`.
