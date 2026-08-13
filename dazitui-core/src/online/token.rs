//! 52dazi.cn 登录 token 持久化（纯文件读写，零网络依赖）。

use std::io;
use std::path::{Path, PathBuf};

/// token 文件读写。文件不存在或为空视为未登录。
#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    /// 默认存储路径：`~/.config/dazitui/token`。
    pub fn with_default_path() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::new(home.join(".config").join("dazitui").join("token"))
    }

    /// 指定路径的存储。
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 存储路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 保存 token（自动创建父目录）。
    pub fn save(&self, token: &str) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, token)
    }

    /// 读取 token；文件不存在或为空返回 `None`（未登录）。
    pub fn load(&self) -> Option<String> {
        let s = std::fs::read_to_string(&self.path).ok()?;
        let s = s.trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(suffix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("dazitui-token-{stamp}-{suffix}"))
    }

    #[test]
    fn save_writes_token_and_load_reads_it_back() {
        let store = TokenStore::new(temp_path("save-load"));
        store.save("token-abc-123").unwrap();
        assert_eq!(store.load(), Some("token-abc-123".to_string()));
        let _ = std::fs::remove_file(store.path());
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = temp_path("nested-dir");
        let store = TokenStore::new(dir.join("sub").join("token"));
        store.save("xyz").unwrap();
        assert_eq!(store.load(), Some("xyz".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_logged_out() {
        let store = TokenStore::new(temp_path("missing"));
        assert_eq!(store.load(), None);
    }

    #[test]
    fn empty_file_is_logged_out() {
        let store = TokenStore::new(temp_path("empty"));
        std::fs::write(store.path(), "").unwrap();
        assert_eq!(store.load(), None);
        let _ = std::fs::remove_file(store.path());
    }

    #[test]
    fn whitespace_only_file_is_logged_out() {
        let store = TokenStore::new(temp_path("blank"));
        std::fs::write(store.path(), "  \n\t").unwrap();
        assert_eq!(store.load(), None);
        let _ = std::fs::remove_file(store.path());
    }
}
