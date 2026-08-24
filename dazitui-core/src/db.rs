//! 跟打历史数据持久化与统计存储 (SQLite / rusqlite)
//!
//! 遵循 XDG 数据目录规范 (~/.local/share/dazitui/stats.db)，
//! 采用 WAL (Write-Ahead Logging) 模式与后台异步 DbWorker 管道。

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::SystemTime;

use rand::Rng;
use rusqlite::{params, Connection, OptionalExtension};

/// 数据库操作错误。
#[derive(Debug)]
pub enum DbError {
    Sqlite(rusqlite::Error),
    Io(io::Error),
    Channel(String),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "SQLite error: {e}"),
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Channel(e) => write!(f, "Channel error: {e}"),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::Channel(_) => None,
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl From<io::Error> for DbError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// 单场跟打练习记录。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: String,
    pub created_at: String,
    pub duration_secs: f64,
    pub wpm: f64,
    pub accuracy: f64,
    pub correct_chars: u32,
    pub wrong_chars: u32,
    pub edits: u32,
    pub typed_chars: u32,
    pub text_title: String,
    pub input_scheme: String,
}

impl SessionRecord {
    /// 创建一个新的练习记录并自动生成唯一 ID 与当前时间戳。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        duration_secs: f64,
        wpm: f64,
        accuracy: f64,
        correct_chars: u32,
        wrong_chars: u32,
        edits: u32,
        typed_chars: u32,
        text_title: impl Into<String>,
        input_scheme: impl Into<String>,
    ) -> Self {
        Self {
            id: generate_unique_id(),
            created_at: current_iso_timestamp(),
            duration_secs,
            wpm,
            accuracy,
            correct_chars,
            wrong_chars,
            edits,
            typed_chars,
            text_title: text_title.into(),
            input_scheme: input_scheme.into(),
        }
    }
}

/// 错字/错词记录项。
#[derive(Debug, Clone, PartialEq)]
pub struct ErrorRecordItem {
    pub id: String,
    pub session_id: String,
    pub created_at: String,
    pub time_secs: f64,
    pub char_index: u32,
    pub target_char: Option<char>,
    pub actual_char: Option<char>,
    pub target_word: Option<String>,
    pub error_type: String,
}

impl ErrorRecordItem {
    pub fn new(
        session_id: impl Into<String>,
        time_secs: f64,
        char_index: u32,
        target_char: Option<char>,
        actual_char: Option<char>,
        target_word: Option<String>,
        error_type: impl Into<String>,
    ) -> Self {
        Self {
            id: generate_unique_id(),
            session_id: session_id.into(),
            created_at: current_iso_timestamp(),
            time_secs,
            char_index,
            target_char,
            actual_char,
            target_word,
            error_type: error_type.into(),
        }
    }
}

/// 按键频次记录项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeypressRecordItem {
    pub session_id: String,
    pub key_code: String,
    pub press_count: u32,
    pub is_raw: bool,
}

/// 全局历史概览统计指标。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GlobalStatsSummary {
    pub total_sessions: usize,
    pub total_duration_secs: f64,
    pub total_typed_chars: u64,
    pub total_correct_chars: u64,
    pub total_wrong_chars: u64,
    pub total_edits: u64,
    pub best_wpm: f64,
    pub avg_wpm: f64,
    pub recent_10_avg_wpm: f64,
    pub avg_accuracy: f64,
}

/// 高频错字统计。
#[derive(Debug, Clone, PartialEq)]
pub struct MistypedCharStat {
    pub target_char: char,
    pub error_count: u32,
    pub top_actual_char: Option<char>,
}

/// 高频错词统计。
#[derive(Debug, Clone, PartialEq)]
pub struct MistypedWordStat {
    pub target_word: String,
    pub error_count: u32,
    pub affected_sessions: u32,
}

/// 数据库管理对象。
pub struct StatsDb {
    conn: Connection,
    path: Option<PathBuf>,
}

impl StatsDb {
    /// 打开或创建指定路径的 SQLite 数据库。
    pub fn open(path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let mut db = Self {
            conn,
            path: Some(path.to_path_buf()),
        };
        db.init_pragmas_and_tables()?;
        Ok(db)
    }

    /// 打开内存数据库（单元测试与无盘环境使用）。
    pub fn open_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        let mut db = Self { conn, path: None };
        db.init_pragmas_and_tables()?;
        Ok(db)
    }

    /// 默认数据文件路径：~/.local/share/dazitui/stats.db。
    pub fn default_path() -> PathBuf {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                home.join(".local").join("share")
            });
        data_home.join("dazitui").join("stats.db")
    }

    /// 打开默认路径下的统计数据库。
    pub fn with_default_path() -> Result<Self, DbError> {
        Self::open(&Self::default_path())
    }

    /// 获取数据库路径（内存数据库返回 None）。
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// 初始化 WAL 模式与数据表结构。
    fn init_pragmas_and_tables(&mut self) -> Result<(), DbError> {
        self.conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;

             CREATE TABLE IF NOT EXISTS sessions (
                 id TEXT PRIMARY KEY,
                 created_at TEXT NOT NULL,
                 duration_secs REAL NOT NULL,
                 wpm REAL NOT NULL,
                 accuracy REAL NOT NULL,
                 correct_chars INTEGER NOT NULL,
                 wrong_chars INTEGER NOT NULL,
                 edits INTEGER NOT NULL,
                 typed_chars INTEGER NOT NULL,
                 text_title TEXT NOT NULL,
                 input_scheme TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS error_records (
                 id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 created_at TEXT NOT NULL,
                 time_secs REAL NOT NULL DEFAULT 0.0,
                 char_index INTEGER NOT NULL DEFAULT 0,
                 target_char TEXT,
                 actual_char TEXT,
                 target_word TEXT,
                 error_type TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS keypress_stats (
                 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 key_code TEXT NOT NULL,
                 press_count INTEGER NOT NULL,
                 is_raw INTEGER NOT NULL DEFAULT 1,
                 PRIMARY KEY (session_id, key_code, is_raw)
             );

             CREATE INDEX IF NOT EXISTS idx_sessions_created_at ON sessions(created_at);
             CREATE INDEX IF NOT EXISTS idx_error_records_session_id ON error_records(session_id);
             CREATE INDEX IF NOT EXISTS idx_error_records_target_char ON error_records(target_char);
             CREATE INDEX IF NOT EXISTS idx_error_records_target_word ON error_records(target_word);
             CREATE INDEX IF NOT EXISTS idx_keypress_stats_key_code ON keypress_stats(key_code);",
        )?;
        Ok(())
    }

    /// 完整插入一次练习会话（包含会话摘要、错字明细与按键频次），使用事务保证原子性。
    pub fn insert_session_full(
        &mut self,
        session: &SessionRecord,
        errors: &[ErrorRecordItem],
        keys: &[KeypressRecordItem],
    ) -> Result<(), DbError> {
        let tx = self.conn.transaction()?;

        // 1. 插入 sessions
        tx.execute(
            "INSERT INTO sessions (
                id, created_at, duration_secs, wpm, accuracy,
                correct_chars, wrong_chars, edits, typed_chars, text_title, input_scheme
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                session.id,
                session.created_at,
                session.duration_secs,
                session.wpm,
                session.accuracy,
                session.correct_chars,
                session.wrong_chars,
                session.edits,
                session.typed_chars,
                session.text_title,
                session.input_scheme,
            ],
        )?;

        // 2. 插入 error_records
        {
            let mut stmt = tx.prepare(
                "INSERT INTO error_records (
                    id, session_id, created_at, time_secs, char_index,
                    target_char, actual_char, target_word, error_type
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for err in errors {
                stmt.execute(params![
                    err.id,
                    err.session_id,
                    err.created_at,
                    err.time_secs,
                    err.char_index,
                    err.target_char.map(|c| c.to_string()),
                    err.actual_char.map(|c| c.to_string()),
                    err.target_word,
                    err.error_type,
                ])?;
            }
        }

        // 3. 插入 keypress_stats
        {
            let mut stmt = tx.prepare(
                "INSERT INTO keypress_stats (
                    session_id, key_code, press_count, is_raw
                ) VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(session_id, key_code, is_raw) DO UPDATE SET
                    press_count = excluded.press_count",
            )?;
            for k in keys {
                stmt.execute(params![
                    k.session_id,
                    k.key_code,
                    k.press_count,
                    if k.is_raw { 1 } else { 0 },
                ])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// 获取总练习场次。
    pub fn get_session_count(&self) -> Result<usize, DbError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// 查询所有练习会话（按创建时间升序排列）。
    pub fn get_all_sessions(&self) -> Result<Vec<SessionRecord>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, duration_secs, wpm, accuracy,
                    correct_chars, wrong_chars, edits, typed_chars, text_title, input_scheme
             FROM sessions
             ORDER BY created_at ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(SessionRecord {
                id: row.get(0)?,
                created_at: row.get(1)?,
                duration_secs: row.get(2)?,
                wpm: row.get(3)?,
                accuracy: row.get(4)?,
                correct_chars: row.get(5)?,
                wrong_chars: row.get(6)?,
                edits: row.get(7)?,
                typed_chars: row.get(8)?,
                text_title: row.get(9)?,
                input_scheme: row.get(10)?,
            })
        })?;

        let mut sessions = Vec::new();
        for s in rows {
            sessions.push(s?);
        }
        Ok(sessions)
    }

    /// 查询最近 limit 场练习会话（按创建时间降序排列）。
    pub fn get_recent_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, duration_secs, wpm, accuracy,
                    correct_chars, wrong_chars, edits, typed_chars, text_title, input_scheme
             FROM sessions
             ORDER BY created_at DESC
             LIMIT ?1",
        )?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(SessionRecord {
                id: row.get(0)?,
                created_at: row.get(1)?,
                duration_secs: row.get(2)?,
                wpm: row.get(3)?,
                accuracy: row.get(4)?,
                correct_chars: row.get(5)?,
                wrong_chars: row.get(6)?,
                edits: row.get(7)?,
                typed_chars: row.get(8)?,
                text_title: row.get(9)?,
                input_scheme: row.get(10)?,
            })
        })?;

        let mut sessions = Vec::new();
        for s in rows {
            sessions.push(s?);
        }
        Ok(sessions)
    }

    /// 查询带窗口滚动平均的 WPM 时序数据：Vec<(created_at, wpm, rolling_wpm)>。
    pub fn get_rolling_wpm_history(
        &self,
        window_size: usize,
    ) -> Result<Vec<(String, f64, f64)>, DbError> {
        let preceding = window_size.saturating_sub(1);
        let sql = format!(
            "SELECT created_at, wpm,
                    AVG(wpm) OVER (
                        ORDER BY created_at ASC
                        ROWS BETWEEN {preceding} PRECEDING AND CURRENT ROW
                    ) AS rolling_wpm
             FROM sessions
             ORDER BY created_at ASC"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;

        let mut points = Vec::new();
        for p in rows {
            points.push(p?);
        }
        Ok(points)
    }

    /// 获取全局概览统计。
    pub fn get_global_summary(&self) -> Result<GlobalStatsSummary, DbError> {
        let mut summary = GlobalStatsSummary::default();
        let total = self.get_session_count()?;
        if total == 0 {
            return Ok(summary);
        }

        summary.total_sessions = total;

        self.conn.query_row(
            "SELECT 
                COALESCE(SUM(duration_secs), 0.0),
                COALESCE(SUM(typed_chars), 0),
                COALESCE(SUM(correct_chars), 0),
                COALESCE(SUM(wrong_chars), 0),
                COALESCE(SUM(edits), 0),
                COALESCE(MAX(wpm), 0.0),
                COALESCE(AVG(wpm), 0.0),
                COALESCE(AVG(accuracy), 0.0)
             FROM sessions",
            [],
            |row| {
                summary.total_duration_secs = row.get(0)?;
                summary.total_typed_chars = row.get::<_, i64>(1)? as u64;
                summary.total_correct_chars = row.get::<_, i64>(2)? as u64;
                summary.total_wrong_chars = row.get::<_, i64>(3)? as u64;
                summary.total_edits = row.get::<_, i64>(4)? as u64;
                summary.best_wpm = row.get(5)?;
                summary.avg_wpm = row.get(6)?;
                summary.avg_accuracy = row.get(7)?;
                Ok(())
            },
        )?;

        // 计算最近 10 场的平均速度
        let recent_10_avg: Option<f64> = self
            .conn
            .query_row(
                "SELECT AVG(wpm) FROM (
                     SELECT wpm FROM sessions ORDER BY created_at DESC LIMIT 10
                 )",
                [],
                |row| row.get(0),
            )
            .optional()?;
        summary.recent_10_avg_wpm = recent_10_avg.unwrap_or(summary.avg_wpm);

        Ok(summary)
    }

    /// 聚合各键位的总按压次数（is_raw 指定是否只统计物理按键或方案反查，None 为全部）。
    pub fn get_key_press_totals(
        &self,
        is_raw: Option<bool>,
    ) -> Result<HashMap<String, u32>, DbError> {
        let (sql, params_vec): (&str, Vec<rusqlite::types::Value>) = match is_raw {
            Some(raw) => (
                "SELECT key_code, SUM(press_count) FROM keypress_stats WHERE is_raw = ?1 GROUP BY key_code",
                vec![rusqlite::types::Value::Integer(if raw { 1 } else { 0 })],
            ),
            None => (
                "SELECT key_code, SUM(press_count) FROM keypress_stats GROUP BY key_code",
                vec![],
            ),
        };

        let mut stmt = self.conn.prepare(sql)?;
        let mut map = HashMap::new();
        let rows = stmt.query_map(rusqlite::params_from_iter(params_vec.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u32))
        })?;

        for r in rows {
            let (k, v) = r?;
            map.insert(k, v);
        }
        Ok(map)
    }

    /// 查询高频错字 Top N。
    pub fn get_top_mistyped_chars(&self, limit: usize) -> Result<Vec<MistypedCharStat>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT target_char, COUNT(*) as err_cnt,
                    (SELECT actual_char FROM error_records e2 
                     WHERE e2.target_char = e1.target_char AND e2.actual_char IS NOT NULL 
                     GROUP BY actual_char ORDER BY COUNT(*) DESC LIMIT 1) as top_actual
             FROM error_records e1
             WHERE target_char IS NOT NULL AND length(target_char) > 0
             GROUP BY target_char
             ORDER BY err_cnt DESC
             LIMIT ?1",
        )?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            let target_str: String = row.get(0)?;
            let err_cnt: i64 = row.get(1)?;
            let actual_str: Option<String> = row.get(2)?;
            Ok(MistypedCharStat {
                target_char: target_str.chars().next().unwrap_or('?'),
                error_count: err_cnt as u32,
                top_actual_char: actual_str.and_then(|s| s.chars().next()),
            })
        })?;

        let mut list = Vec::new();
        for item in rows {
            list.push(item?);
        }
        Ok(list)
    }

    /// 查询高频错词 Top N。
    pub fn get_top_mistyped_words(&self, limit: usize) -> Result<Vec<MistypedWordStat>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT target_word, COUNT(*) as err_cnt, COUNT(DISTINCT session_id) as sess_cnt
             FROM error_records
             WHERE target_word IS NOT NULL AND length(target_word) >= 2
             GROUP BY target_word
             ORDER BY err_cnt DESC
             LIMIT ?1",
        )?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(MistypedWordStat {
                target_word: row.get(0)?,
                error_count: row.get::<_, i64>(1)? as u32,
                affected_sessions: row.get::<_, i64>(2)? as u32,
            })
        })?;

        let mut list = Vec::new();
        for item in rows {
            list.push(item?);
        }
        Ok(list)
    }
}

/// 后台数据库异步任务。
#[derive(Debug)]
pub enum DbTask {
    SaveSession {
        session: SessionRecord,
        errors: Vec<ErrorRecordItem>,
        keys: Vec<KeypressRecordItem>,
    },
}

/// 后台非阻塞持久化 Worker。
pub struct DbWorker {
    sender: Sender<DbTask>,
    handle: Option<JoinHandle<()>>,
}

impl DbWorker {
    /// 启动连接到指定文件路径的异步写入 Worker。
    pub fn start(db_path: PathBuf) -> Result<Self, DbError> {
        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("dazitui-db-worker".to_string())
            .spawn(move || {
                let mut db = match StatsDb::open(&db_path) {
                    Ok(db) => db,
                    Err(e) => {
                        eprintln!("[dazitui] Failed to open stats database: {e}");
                        return;
                    }
                };
                worker_loop(&mut db, rx);
            })?;

        Ok(Self {
            sender: tx,
            handle: Some(handle),
        })
    }

    /// 启动内存数据库 Worker（带共享 Mutex 访问用于测试）。
    pub fn start_in_memory() -> Result<(Self, Arc<Mutex<StatsDb>>), DbError> {
        let db = StatsDb::open_in_memory()?;
        let shared_db = Arc::new(Mutex::new(db));
        let db_clone = Arc::clone(&shared_db);
        let (tx, rx) = mpsc::channel();

        let handle = thread::Builder::new()
            .name("dazitui-db-worker-test".to_string())
            .spawn(move || {
                while let Ok(task) = rx.recv() {
                    match task {
                        DbTask::SaveSession {
                            session,
                            errors,
                            keys,
                        } => {
                            if let Ok(mut db) = db_clone.lock() {
                                let _ = db.insert_session_full(&session, &errors, &keys);
                            }
                        }
                    }
                }
            })?;

        let worker = Self {
            sender: tx,
            handle: Some(handle),
        };
        Ok((worker, shared_db))
    }

    /// 异步发送持久化任务（非阻塞）。
    pub fn send(&self, task: DbTask) -> Result<(), DbError> {
        self.sender
            .send(task)
            .map_err(|e| DbError::Channel(e.to_string()))
    }

    /// 优雅停机并等待后台任务刷盘完成。
    pub fn flush_and_stop(mut self) {
        drop(self.sender);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker_loop(db: &mut StatsDb, rx: Receiver<DbTask>) {
    while let Ok(task) = rx.recv() {
        match task {
            DbTask::SaveSession {
                session,
                errors,
                keys,
            } => {
                if let Err(e) = db.insert_session_full(&session, &errors, &keys) {
                    eprintln!("[dazitui] Error saving session to database: {e}");
                }
            }
        }
    }
}

/// 生成短唯一标识。
fn generate_unique_id() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let rand_val: u32 = rand::rng().random();
    format!("{:x}-{:08x}", now, rand_val)
}

/// 获取当前 UTC 时间戳字符串 (ISO 8601 格式)。
fn current_iso_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = now / 86400;
    let rem = now % 86400;
    let hours = rem / 3600;
    let minutes = (rem % 3600) / 60;
    let seconds = rem % 60;

    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02}")
}

fn days_to_ymd(mut days: u64) -> (u32, u32, u32) {
    let mut year = 1970;
    loop {
        let leap = is_leap_year(year);
        let days_in_year = if leap { 366 } else { 365 };
        if days >= days_in_year {
            days -= days_in_year;
            year += 1;
        } else {
            break;
        }
    }
    let leap = is_leap_year(year);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for &d in &month_days {
        if days >= d {
            days -= d;
            month += 1;
        } else {
            break;
        }
    }
    let day = (days + 1) as u32;
    (year, month, day)
}

#[allow(clippy::manual_is_multiple_of)]
fn is_leap_year(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_db_in_memory_crud() {
        let mut db = StatsDb::open_in_memory().expect("in-memory db create");
        assert_eq!(db.get_session_count().unwrap(), 0);

        let session = SessionRecord::new(
            60.0,
            85.5,
            0.98,
            120,
            2,
            1,
            122,
            "常用单字前五百",
            "虎码",
        );
        let errors = vec![
            ErrorRecordItem::new(
                &session.id,
                12.5,
                15,
                Some('世'),
                Some('四'),
                Some("世界".to_string()),
                "Mismatch",
            ),
            ErrorRecordItem::new(
                &session.id,
                25.0,
                30,
                Some('界'),
                None,
                Some("世界".to_string()),
                "Backspace",
            ),
        ];
        let keys = vec![
            KeypressRecordItem {
                session_id: session.id.clone(),
                key_code: "a".to_string(),
                press_count: 25,
                is_raw: true,
            },
            KeypressRecordItem {
                session_id: session.id.clone(),
                key_code: "b".to_string(),
                press_count: 10,
                is_raw: true,
            },
        ];

        db.insert_session_full(&session, &errors, &keys).unwrap();
        assert_eq!(db.get_session_count().unwrap(), 1);

        let sessions = db.get_all_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].text_title, "常用单字前五百");
        assert_eq!(sessions[0].wpm, 85.5);

        // 测试概览统计
        let summary = db.get_global_summary().unwrap();
        assert_eq!(summary.total_sessions, 1);
        assert_eq!(summary.best_wpm, 85.5);
        assert_eq!(summary.avg_wpm, 85.5);
        assert_eq!(summary.total_typed_chars, 122);

        // 测试键位汇总
        let key_totals = db.get_key_press_totals(Some(true)).unwrap();
        assert_eq!(key_totals.get("a"), Some(&25));
        assert_eq!(key_totals.get("b"), Some(&10));

        // 测试错字错词排行榜
        let top_chars = db.get_top_mistyped_chars(10).unwrap();
        assert_eq!(top_chars.len(), 2);
        assert_eq!(top_chars[0].target_char, '世');
        assert_eq!(top_chars[0].error_count, 1);

        let top_words = db.get_top_mistyped_words(10).unwrap();
        assert_eq!(top_words.len(), 1);
        assert_eq!(top_words[0].target_word, "世界");
        assert_eq!(top_words[0].error_count, 2);
    }

    #[test]
    fn test_rolling_wpm_calculation() {
        let mut db = StatsDb::open_in_memory().unwrap();
        for i in 1..=5 {
            let mut s = SessionRecord::new(
                60.0,
                (i * 10) as f64,
                0.99,
                100,
                0,
                0,
                100,
                "test",
                "全拼",
            );
            s.created_at = format!("2026-08-24 10:00:0{i}");
            db.insert_session_full(&s, &[], &[]).unwrap();
        }

        let history = db.get_rolling_wpm_history(3).unwrap();
        assert_eq!(history.len(), 5);
        // 第 1 场: 10, rolling=10
        assert_eq!(history[0].1, 10.0);
        assert_eq!(history[0].2, 10.0);
        // 第 2 场: 20, rolling=(10+20)/2=15
        assert_eq!(history[1].1, 20.0);
        assert_eq!(history[1].2, 15.0);
        // 第 3 场: 30, rolling=(10+20+30)/3=20
        assert_eq!(history[2].1, 30.0);
        assert_eq!(history[2].2, 20.0);
        // 第 4 场: 40, rolling=(20+30+40)/3=30
        assert_eq!(history[3].1, 40.0);
        assert_eq!(history[3].2, 30.0);
    }

    #[test]
    fn test_db_worker_async_pipeline() {
        let (worker, shared_db) = DbWorker::start_in_memory().unwrap();
        let session = SessionRecord::new(
            50.0,
            92.0,
            0.99,
            100,
            1,
            0,
            101,
            "异步测试",
            "虎码",
        );
        worker
            .send(DbTask::SaveSession {
                session,
                errors: vec![],
                keys: vec![],
            })
            .unwrap();

        worker.flush_and_stop();

        let db = shared_db.lock().unwrap();
        assert_eq!(db.get_session_count().unwrap(), 1);
        let s = db.get_recent_sessions(1).unwrap();
        assert_eq!(s[0].text_title, "异步测试");
        assert_eq!(s[0].wpm, 92.0);
    }
}
