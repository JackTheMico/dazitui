# 调研：基于 DuckDB 的跟打数据统计与热力图分析架构

**调研日期**：2026-08-24  
**目标系统**：dazitui (Rust + Ratatui Linux 中文跟打 TUI)  
**涉及特性**：WPM 历史折线图、键盘热力图（标准斜列 / 4x12 与 5x12 直列）、错字与错词高频排行榜、DuckDB 嵌入式存储与性能分析。

---

## 1. 需求可行性总览 (Executive Summary)

针对 `dazitui` 引入打字数据持久化、WPM 历史趋势、键位热力图（标准斜列与直列切换）以及错字错词频次排行需求，本调研从 **DuckDB Rust 生态与嵌入式架构**、**时序折线与 LTTB 降采样**、**热力图渲染与 Linux 输入法（IME）按键拦截困境**、**中文分词错词归因** 及 **TUI 异步非阻塞架构** 进行了全面深度论证。

### 核心结论
1. **完全可行**：DuckDB (`duckdb` crate) 在嵌入式 OLAP 场景下拥有出色的分析能力（窗口函数、分位数、时间分桶、原生 Parquet 互导），与 Ratatui 结合可实现极佳的终端数据分析体验。
2. **主要技术难点与权衡**：
   - **编译与体积开销**：DuckDB 采用 C++ 编写，`bundled` 特性使冷编译增加 2~4 分钟，二进制增大约 25~35MB（对比 SQLite 约 1.5MB）。
   - **Linux IME 物理按键截断难题（核心难点）**：在 Linux X11/Wayland 下，Fcitx5/IBus 在输入法框架层拦截了拼音/形码字母击键，crossterm 仅能收到最终上屏的汉字 `KeyCode::Char('中')`。因此物理键盘热力图无法直接通过终端 raw 击键获取汉字输入键位。
   - **解决方案**：引入 **“方案反查码表（Reverse Code Mapping）+ 双层热力图（Raw 击键 / 方案编码投射）”**，根据用户当前选择的输入法（拼音/虎码/五笔等）反向分解汉字键码，还原键盘指法负荷。

---

## 2. DuckDB 嵌入式存储与 Rust 集成可行性分析

### 2.1 DuckDB 核心能力与 Rust API 设计

DuckDB 是专为进程内（In-Process）分析型查询（OLAP）设计的列式数据库引擎。官方维护的 `duckdb` Rust Crate 提供了类似 `rusqlite` 的符合 Rust 惯用法的安全 API。

#### 依赖配置 (`Cargo.toml`)
```toml
[dependencies]
duckdb = { version = "1.1", features = ["bundled", "chrono", "uuid"] }
```

#### 核心 API 模式与交互
```rust
use duckdb::{params, Connection, Result};

pub struct StatsDb {
    conn: Connection,
}

impl StatsDb {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        // 开启自动检查点与内存限制优化
        conn.execute_batch(
            "PRAGMA auto_checkpoint='10MB';
             PRAGMA threads=2;
             PRAGMA memory_limit='128MB';",
        )?;
        let db = Self { conn };
        db.init_tables()?;
        Ok(db)
    }

    fn init_tables(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id VARCHAR PRIMARY KEY,
                created_at TIMESTAMP NOT NULL,
                duration_secs DOUBLE NOT NULL,
                wpm DOUBLE NOT NULL,
                accuracy DOUBLE NOT NULL,
                correct_chars UINTEGER NOT NULL,
                wrong_chars UINTEGER NOT NULL,
                edits UINTEGER NOT NULL,
                typed_chars UINTEGER NOT NULL,
                text_title VARCHAR NOT NULL,
                text_type VARCHAR NOT NULL,
                input_scheme VARCHAR NOT NULL
            );

            CREATE TABLE IF NOT EXISTS error_records (
                id VARCHAR PRIMARY KEY,
                session_id VARCHAR NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                created_at TIMESTAMP NOT NULL,
                char_index UINTEGER NOT NULL,
                target_char VARCHAR NOT NULL,
                actual_char VARCHAR,
                target_word VARCHAR,
                error_type VARCHAR NOT NULL
            );

            CREATE TABLE IF NOT EXISTS keypress_stats (
                session_id VARCHAR NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                key_code VARCHAR NOT NULL,
                press_count UINTEGER NOT NULL,
                is_raw BOOLEAN NOT NULL,
                PRIMARY KEY (session_id, key_code, is_raw)
            );
            "#,
        )?;
        Ok(())
    }
}
```

### 2.2 并发与文件锁机制 (Concurrency & Storage Model)
- **单写多读（Single-Writer, Multiple-Reader）**：DuckDB 对数据库文件采用排他式文件锁（Exclusive File Lock）。同一个进程内支持多线程并发读取和 MVCC 隔离；但跨进程时，一旦某个进程以读写模式打开文件，其他进程无法以读写模式打开。
- **对 dazitui 的影响**：作为单机桌面 TUI 应用，同一时刻用户一般只运行一个 dazitui 实例。在 dazitui 内部，采用 **单数据库连接持有者（Actor/Worker 线程）** 管理写操作，即可规避文件锁竞争与写阻塞。

### 2.3 构建开销与二进制体积深度对比

| 方案 | 优势 | 劣势 | 冷编译增量 | 二进制体积增量 | 适用场景 |
|---|---|---|---|---|---|
| **DuckDB (`bundled`)** | 原生列式计算、复杂窗口函数、分位数、时间分桶、零成本导出 Parquet/Arrow | C++ 模板重度编译，体积偏大 | +2 ~ 4 分钟 | +25 ~ 35 MB | **高阶分析型 TUI、统计报表、大数据量跟打历史** |
| **SQLite (`rusqlite`)** | 极轻量、成熟极高、毫秒级编译 | 行式存储，缺乏时间分桶/分位数聚合等高阶分析函数，导出 Parquet 需手写 | +5 ~ 10 秒 | +1.5 ~ 2.0 MB | 简单 CRUD、仅做记录保存 |
| **Sled / KV Store** | 纯 Rust、零 C++ 依赖 | 无 SQL 引擎，所有聚合、统计、排行必须在内存中手写 Rust 迭代器 | +15 秒 | +2 ~ 3 MB | 键值缓存、无结构化查询 |
| **本地 JSONL / Parquet** | 简单直观，无数据库引擎依赖 | 追加写简单但查询慢（全表扫描），缺乏索引与 ACID 约束 | 0 | +0.5 MB | 纯归档导出 |

> **评估结论**：DuckDB 对于需要计算 **滑动平均 WPM、历史百分位数、时间分桶聚合、错字 Top-N 词频** 的场景，其单条 SQL 能够替代大量手工 Rust 分析算法，且原生支持导出标准 `.parquet` 文件供外部分析工具使用。

---

## 3. 功能一：WPM 历史趋势与降采样折线图 (WPM Trends & Charts)

### 3.1 统计聚合查询设计

#### 1. 滑动平均与历史趋势查询 (Rolling Average)
```sql
SELECT 
    created_at,
    wpm,
    accuracy,
    AVG(wpm) OVER (
        ORDER BY created_at 
        ROWS BETWEEN 9 PRECEDING AND CURRENT ROW
    ) AS rolling_wpm_10,
    MAX(wpm) OVER (
        ORDER BY created_at 
        ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
    ) AS cumulative_best_wpm
FROM sessions
ORDER BY created_at ASC;
```

#### 2. 按天/周汇总分析 (Time Bucket Aggregation)
```sql
SELECT 
    date_trunc('day', created_at) AS session_date,
    COUNT(*) AS total_sessions,
    ROUND(AVG(wpm), 2) AS avg_wpm,
    ROUND(MAX(wpm), 2) AS max_wpm,
    ROUND(quantile_cont(wpm, 0.5), 2) AS median_wpm,
    ROUND(quantile_cont(wpm, 0.9), 2) AS p90_wpm,
    SUM(typed_chars) AS total_chars,
    ROUND(AVG(accuracy), 4) AS avg_accuracy
FROM sessions
GROUP BY 1
ORDER BY 1 DESC
LIMIT 30;
```

### 3.2 大数据量可视化：LTTB 降采样算法 (Largest Triangle Three Buckets)

当历史练习场次达到数千场时，直接把数千个点喂给 Ratatui `Chart` 会导致终端渲染卡顿且盲文字符（Braille Marker）重叠混乱。终端屏幕宽度通常为 80~160 列，适合渲染的折线点数为  pprox 100 \sim 150$ 个点。

#### LTTB 原理与 Rust 实现
LTTB 算法在保留时序数据局部极值（波峰、波谷）和视觉整体趋势上远优于简单等间距抽样或朴素均值。

```rust
#[derive(Debug, Clone, Copy)]
pub struct DataPoint {
    pub x: f64,
    pub y: f64,
}

pub fn lttb_downsample(data: &[DataPoint], threshold: usize) -> Vec<DataPoint> {
    let n = data.len();
    if threshold >= n || threshold < 3 {
        return data.to_vec();
    }

    let mut sampled = Vec::with_capacity(threshold);
    sampled.push(data[0]); // 首点固定保留

    let bucket_size = (n - 2) as f64 / (threshold - 2) as f64;
    let mut a = 0; // 上一个选中的点索引

    for i in 0..(threshold - 2) {
        // 计算下一个桶 (Bucket C) 的几何中心 (Average Center)
        let c_start = (((i + 1) as f64 * bucket_size) as usize + 1).min(n - 1);
        let c_end = (((i + 2) as f64 * bucket_size) as usize + 1).min(n);
        
        let (mut avg_x, mut avg_y) = (0.0, 0.0);
        let c_count = (c_end - c_start).max(1) as f64;
        for pt in &data[c_start..c_end] {
            avg_x += pt.x;
            avg_y += pt.y;
        }
        avg_x /= c_count;
        avg_y /= c_count;

        // 在当前桶 (Bucket B) 中寻找与 Point A 及 Center C 构成三角形面积最大的点
        let b_start = ((i as f64 * bucket_size) as usize + 1).min(n - 1);
        let b_end = (((i + 1) as f64 * bucket_size) as usize + 1).min(n);

        let pt_a = data[a];
        let mut max_area = -1.0;
        let mut max_idx = b_start;

        for (idx, pt_b) in data[b_start..b_end].iter().enumerate() {
            let actual_idx = b_start + idx;
            let area = ((pt_a.x - avg_x) * (pt_b.y - pt_a.y) - (pt_a.x - pt_b.x) * (avg_y - pt_a.y)).abs() * 0.5;
            if area > max_area {
                max_area = area;
                max_idx = actual_idx;
            }
        }

        sampled.push(data[max_idx]);
        a = max_idx;
    }

    sampled.push(data[n - 1]); // 末点固定保留
    sampled
}
```

---

## 4. 功能二：键盘热力图与双布局系统 (Keypress Heatmap & Layouts)

### 4.1 核心挑战：Linux 下中文输入法 (IME) 的按键拦截困境

#### 现象剖析
在 Linux X11/Wayland 环境下（运行 Fcitx5 / IBus / Rime）：
1. 用户击键 `z`, `h`, `o`, `n`, `g`, `g`, `u`, `o`, `Space`；
2. 输入法框架在 XIM / Wayland text-input 协议层拦截了这些物理按键，用于拼音/形码组字；
3. 终端模拟器（Kitty, Alacritty, WezTerm 等）仅在选字确认后收到提交字符串 `"中国"`；
4. `crossterm::event::read()` 捕获到的是两个上屏事件：`KeyEvent { code: KeyCode::Char('中') }` 与 `KeyEvent { code: KeyCode::Char('国') }`。
5. **痛点**：标准 ANSI 键盘上没有“中”键或“国”键。如果直接用 crossterm 捕获的字符绘制 QWERTY 热力图，所有中文字符击键在键盘上都无法对应到 `A~Z` 物理键位。

#### 解决方案：反查码表分解 + 双模式热力图 (Dual-Track Heatmap)

dazitui 已经在 `Settings` 中维护了用户的 `input_scheme`（如 虎码 / 五笔86 / 五笔98 / 拼音 / 仓颉 / 郑码 等）。

1. **反查码表机制（Reverse Scheme Mapping）**：
   - 建立汉字到物理键位序列的映射字典（内置或载入码表）。
   - 例如：当上屏 `'中'`，若当前输入法为五笔，则映射分解为键码 `['k']`；若为虎码，映射为 `['l', 'i']`；若为全拼，映射为 `['z', 'h', 'o', 'n', 'g']`。
   - 回改按键（Backspace）与英文字符、标点符号则直接由 crossterm 捕获。
2. **TUI 热力图双模式切换（按 `m` 键切换）**：
   - **模式 A：方案编码投射热力图（Scheme Projected Heatmap）**：通过码表反查汉字击键，展示该输入法方案下的指法负荷与键位分布（中文字打字员核心关注点）。
   - **模式 B：原始捕获按键热力图（Raw Captured Heatmap）**：展示 crossterm 实际捕获到的按键（退格、空格、标点、英文跟打模式下的纯击键）。

---

### 4.2 布局坐标建模与 Ratatui 渲染架构

#### 1. 标准斜列布局 (ANSI Staggered 60% / 104)
标准键盘存在物理错位（Stagger）：
- 第 1 行 (数字行): `1 2 3 ... =` (偏移 0.0u)
- 第 2 行 (QWERTY): `Tab(1.5u) Q W E R T Y U I O P [ ] \` (偏移 1.5u)
- 第 3 行 (Home Row): `Caps(1.75u) A S D F G H J K L ; ' Enter(2.25u)` (偏移 1.75u)
- 第 4 行 (Bottom Row): `Shift(2.25u) Z X C V B N M , . / Shift(2.75u)` (偏移 2.25u)
- 第 5 行 (Space Row): `Ctrl(1.25u) Alt(1.25u) Space(6.25u) Alt(1.25u) Ctrl(1.25u)`

#### 2. 直列网格布局 (Ortholinear Planck 4x12 / Preonic 5x12)
直列键盘没有行错位，呈现严格的 4×12 或 5×12 矩阵网格，每列宽度严格对齐：
- Row 0: `1  2  3  4  5  6  7  8  9  0  -  =`
- Row 1: `Tab Q  W  E  R  T  Y  U  I  O  P  Bksp`
- Row 2: `Esc A  S  D  F  G  H  J  K  L  ;  '`
- Row 3: `Shf Z  X  C  V  B  N  M  ,  .  /  Ent`
- Row 4: `Ctrl Alt GUI Low Spc Spc Rse Left Down Up Right`

---

### 4.3 热力色彩映射与键帽绘制

#### 热力强度归一化公式
为了避免个别极高频按键（如空格或常用字母 `e`/`i`）将其他键压缩到全暗，采用**对数平滑归一化**：
1389806I(k) = rac{\ln(1 + 	ext{count}(k))}{\ln(1 + \max_{j} 	ext{count}(j))}1389806

#### 颜色渐变阶梯映射 (Color Ramp -> ThemePalette)
将强度  \in [0.0, 1.0]$ 映射为 5 个层级色彩：
-  = 0$：`palette.bg` / `palette.muted`（冷灰，无击键）
- bash < I \le 0.25$：深蓝冷色（低负荷）
- bash.25 < I \le 0.50$：`palette.accent`（青/天蓝，温和负荷）
- bash.50 < I \le 0.75$：`palette.warn`（黄/橙，较高频）
- bash.75 < I \le 1.00$：`palette.wrong`（高亮艳红，极高频负荷）

---

## 5. 功能三：错字与错词高频排行榜 (Error Characters & Words Ranking)

### 5.1 中文词组错误归因算法 (Word-Level Segmentation)

在中文打字中，输入记忆往往是以“词”为单位提取的（如输入“计算机”时误打成“计算器”）。

1. **内置词组赛文（Builtin Word Sets）**：
   - 赛文已具备结构化的 `word_boundaries: &[(usize, usize)]`，直接根据发生错误的字符索引定位其所在的精准词范围。
2. **通用自然文本（离线/在线/发文赛文）**：
   - 引入轻量级 `jieba-rs`（中文分词 Crate），在加载赛文时预先完成分词并建立字符偏移索引映射表：`CharIndex -> WordToken`。

### 5.2 DuckDB 排行分析查询

#### 错字排行榜查询
```sql
SELECT 
    target_char,
    COALESCE(actual_char, '⌫') AS typed_char,
    COUNT(*) AS err_count,
    ROUND(COUNT(*) * 100.0 / SUM(COUNT(*)) OVER (), 2) AS err_percent
FROM error_records
GROUP BY target_char, actual_char
ORDER BY err_count DESC
LIMIT 50;
```

#### 错词排行榜查询
```sql
SELECT 
    target_word,
    COUNT(*) AS err_count,
    ROUND(COUNT(*) * 100.0 / SUM(COUNT(*)) OVER (), 2) AS err_percent,
    COUNT(DISTINCT session_id) AS affected_sessions
FROM error_records
WHERE target_word IS NOT NULL AND length(target_word) > 1
GROUP BY target_word
ORDER BY err_count DESC
LIMIT 50;
```

---

## 6. dazitui 架构集成与工程实践

### 6.1 数据存储路径规范 (XDG Data Specification)

严格遵循 Linux XDG Base Directory 规范：
- **配置文件**（已实现）：`~/.config/dazitui/settings` (`XDG_CONFIG_HOME`)
- **统计数据库**（本次新增）：`~/.local/share/dazitui/stats.duckdb` (`XDG_DATA_HOME`)

### 6.2 异步非阻塞数据库写入架构 (Frame-Drop Prevention)

为保证 TUI 在高刷新率下不出现磁盘写阻塞或界面掉帧，采用 **MPSC Channel + 后台 DB Worker**：

```
[ TUI Main Thread ] 
      │ (Session Finished / Event Triggered)
      │ mpsc::Sender::send(DbTask::SaveSession(...))
      ▼
[ Background DB Worker Thread ] ──(Connection::open / Appender)──> [ stats.duckdb ]
```

### 6.3 统计子页面（StatsView）与 Tab 导航设计

在主程序状态机中新增 `AppState::StatsView(StatsSubpage)`：
- **快捷键入口**：主菜单功能栏新增 `[数据统计 (F4 / s)]`；
- **子页面 Tabs 切换**：
  - `Tab 1` / 快捷键 `1`：**历史趋势 (WPM Overview)** — 均速、极速、中位数、LTTB 采样折线图；
  - `Tab 2` / 快捷键 `2`：**键位热力 (Keyboard Heatmap)** — 按 `Tab` 切换斜列/直列布局，按 `m` 切换物理/反查键位；
  - `Tab 3` / 快捷键 `3`：**错字错词 (Error Ranking)** — 上下键浏览高频错字与错词排行榜。

---

## 7. 难点与技术风险总结 (Roadblocks & Mitigation)

| 难点 / 风险 | 表现 | 应对与解决方案 |
|---|---|---|
| **1. Linux IME 击键吞没** | 拼音/形码输入法不把物理击键给 crossterm，导致键盘热力图失真 | 内置/加载输入方案反查码表（虎码/五笔/拼音），将上屏汉字反解为物理键码进行热力投射；提供 Raw / Scheme 双视角切换 |
| **2. DuckDB 编译耗时与包体积** | `cargo build` 增加 2~4 分钟，二进制增大 ~30MB | 开发期可选用动态链接或通过 Cargo feature 开关控制；发布期启用 `bundled` 保证无任何系统运行时依赖分发 |
| **3. 大量历史数据折线图卡顿** | 几千场练习数据使 Braille 绘图错乱、渲染掉帧 | 引入高保真 LTTB（最大三角形三桶算法）降采样至 100~150 个视觉关键点 |
| **4. 中文错词边界模糊** | 自由赛文没有现成词边界，无法直接归因错“词” | 集成 `jieba-rs`，在载文时异步分词预建立字符索引；内置词组赛文直接沿用原生词边界 |

---

## 8. 落地实施路线图 (Actionable Roadmap)

- **阶段一：数据模型与底层存储 (`dazitui-core`)**
  1. 引入 `duckdb` 与 `jieba-rs` 依赖（可置于 `stats` feature 下）；
  2. 实现 `StatsDb` 数据库初始化、DDL 创建与参数化插入方法；
  3. 实现反查码表解析器（支持将汉字转换为当前输入法的字母编码流）。
- **阶段二：后台异步 Worker 与会话入库 (`dazitui`)**
  1. 创建 `DbWorker` 线程，在会话完成（`app.finish()`）时异步推送成绩与错字流水；
  2. 完善 XDG 数据目录自动创建与数据持久化逻辑。
- **阶段三：统计查询与 LTTB 算法组件**
  1. 实现 `lttb_downsample` 工具函数与单元测试；
  2. 封装 DuckDB 高频统计 SQL（滚动平均、分位数、错字 Top50、按键频次汇总）。
- **阶段四：Ratatui 可视化视图与导航实现**
  1. 新增 `StatsView` 状态与三级 Tab 导航；
  2. 绘制 WPM 历史趋势图、标准斜列 / 4x12 直列键盘热力图组件；
  3. 绘制错字/错词排行榜双列滚动表格。
