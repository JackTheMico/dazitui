# 基于 Rime Schema 的并击指法逆向解析与实时键盘映射架构

为解决在并击方案（如麓鸣纯形 `yoyo-pure` / 空明拳 / 六脉神剑）下，汉字反查码元（如 `.`、`W`、`Q` 等）在实时虚拟键盘中被误识别为单一物理键的问题，确立了以下 Rime Schema 解析、指法逆向映射与实时键盘可视化架构决策。

## 1. 方案关联与配置发现机制 (Unified Discovery & Association)

- **自闭环自动绑定**：用户在设置中指定方案名称或自定义路径时，`dazitui` 优先在方案目录（`~/.config/dazitui/schemes/`、`~/.local/share/fcitx5/rime/` 或自定义路径）检索：
  - 若指定或找到 `.schema.yaml`：自动读取其 `translator/dictionary` 属性绑定对应的 `.dict.yaml` 词典；
  - 若同名目录下同时存在 `<name>.schema.yaml` 与 `<name>.dict.yaml`，自动关联加载；
  - 若仅存在 `.dict.yaml` 或纯文本码表，自动平滑降级为传统单键直出模式。

## 2. 轻量级 Rime YAML Include/Patch 解析器 (Lightweight AST Resolver)

针对 Rime 方案普遍采用的 `__include: file:/section`、`__patch` 与 `__append` 宏展开机制：
- **专用预处理器**：在 `dazitui-core` 中内置聚焦于 `chord_composer.algebra` 提取的轻量级递归 Resolver；
- **同目录引用展开**：支持解析当前文件内部片段引用与同目录下伴生 YAML（如 `yoyo.yaml`）的跨文件片段引用，将宏调用递归合并求值为最终扁平的 `algebra` 规则列表；
- **零运行时环境依赖**：无需用户提前编译部署（无强依赖 `rime_deployer` 或外部 C 库），源码态 YAML 即插即用。

## 3. 指法代数逆向与左右手镜像映射 (Algebra Inversion & Hand-Aware Mirroring)

Rime `chord_composer.algebra` 为正向映射（物理键 $\to$ 逻辑码元），逆向引擎在初始化时完成结构化分类构建：
- **左右手镜像表 (Mirror Map)**：提取单键到单键的镜像规则（如 `y` $\leftrightarrow$ `t`, `u` $\leftrightarrow$ `r`, `.` $\leftrightarrow$ `x` 等）；
- **码元并击逆向表 (Symbol to Chord Map)**：提取多键到单码元的代数规则（如 `xv` $\to$ `.`, `vw` $\to$ `W`, `esf` $\to$ `Q`）；
- **手区感知反查展开**：
  - 带 `_` 前缀（左手区）：码元直接通过并击表还原为左手按键；
  - 带 `+` 前缀（右手区）：码元还原后通过镜像表映射为真实的右手物理按键（如 `+e` $\to$ `i`，`+.` $\to$ `,` + `.`）；
  - 全码/词语无前缀：逐码元展开为对应的物理按键集合。

## 4. 实时键盘多键并击与衰减动画 (Simultaneous Burst & Decay)

- **瞬间全量激活**：反查出的所有物理按键集合在当前时刻 $t$ 一并送入 `LiveKeyboard::active_keys`；
- **统一余温衰减**：所有参与该字/词输入的物理按键在同一帧呈现强调色高亮（Accent 反白），并在 250ms 内伴随余温平滑淡出；
- **极简无锁性能**：纯立即模式（Immediate Mode）计算，零队列调度开销，在 200+ WPM 高速击键下保持 60 FPS 稳定渲染。
