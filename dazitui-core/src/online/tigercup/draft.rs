//! 虎码杯生稿赛当日赛文草稿暂存保护（防异常退出、崩溃导致每日一次参赛机会作废）。

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 虎码杯当日拉取且未完赛的赛文草稿。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TigerDraft {
    /// 比赛日期（"YYYY-MM-DD"）。
    pub date: String,
    /// 赛文标题（如 "2026-09-15 每日赛文"）。
    pub title: String,
    /// 赛文正文。
    pub content: String,
    /// 获取时的时间戳（秒）。
    #[serde(default)]
    pub fetched_at_unix: u64,
}

/// 虎码杯草稿文件读写。
#[derive(Debug, Clone)]
pub struct TigerDraftStore {
    path: PathBuf,
}

impl TigerDraftStore {
    /// 默认存储路径：~/.local/share/dazitui/tigercup_draft.json。
    pub fn with_default_path() -> Self {
        Self::new(crate::paths::default_tigercup_draft_path())
    }

    /// 指定路径的存储。
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 存储路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 保存草稿。
    pub fn save(&self, draft: &TigerDraft) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string(draft)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(&self.path, json.as_bytes())
    }

    /// 读取当日未完成的草稿。若草稿跨天或不存在，返回 `None`。
    pub fn load_today(&self, today: &str) -> Option<TigerDraft> {
        if !self.path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(&self.path).ok()?;
        let draft: TigerDraft = serde_json::from_str(content.trim()).ok()?;
        if draft.date == today && !draft.content.is_empty() {
            Some(draft)
        } else {
            None
        }
    }

    /// 清除草稿（完赛提交后调用）。
    pub fn clear(&self) -> io::Result<()> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiger_draft_store_save_and_load_today() {
        let dir = std::env::temp_dir().join("dazitui_test_tigerdraft");
        let path = dir.join("draft.json");
        let _ = std::fs::remove_file(&path);

        let store = TigerDraftStore::new(path.clone());
        assert_eq!(store.load_today("2026-09-15"), None);

        let draft = TigerDraft {
            date: "2026-09-15".into(),
            title: "2026-09-15 每日赛文".into(),
            content: "生稿测试正文".into(),
            fetched_at_unix: 1773561600,
        };
        store.save(&draft).unwrap();

        // 当天能加载
        let loaded = store.load_today("2026-09-15").unwrap();
        assert_eq!(loaded, draft);

        // 跨天不加载
        assert_eq!(store.load_today("2026-09-16"), None);

        // 清理后为空
        store.clear().unwrap();
        assert_eq!(store.load_today("2026-09-15"), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
