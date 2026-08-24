//! 52dazi.cn 登录会话持久化（支持 Token + Session Cookie，纯文件读写，零网络依赖）。

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 登录会话数据：包含接口 token 与可选的 HTTP 会话 cookie（PHPSESSID）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthSession {
    /// 接口认证 token。
    pub token: String,
    /// HTTP Cookie 头（如 "PHPSESSID=xxx"），用于维持服务端 PHP 会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<String>,
}

impl AuthSession {
    /// 仅包含 token 的会话。
    pub fn from_token(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            cookie: None,
        }
    }

    /// 包含 token 与 cookie 的会话。
    pub fn new(token: impl Into<String>, cookie: Option<String>) -> Self {
        Self {
            token: token.into(),
            cookie,
        }
    }
}

/// 会话文件读写。文件不存在或为空视为未登录。
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

    /// 保存完整会话（JSON 格式，自动创建父目录）。
    pub fn save_session(&self, session: &AuthSession) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json_str = serde_json::to_string(session)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(&self.path, json_str)
    }

    /// 读取完整会话：兼容 JSON 格式与历史纯 token 文本文件。
    pub fn load_session(&self) -> Option<AuthSession> {
        let s = std::fs::read_to_string(&self.path).ok()?;
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        // 尝试解析 JSON 会话格式
        if let Ok(sess) = serde_json::from_str::<AuthSession>(trimmed)
            && !sess.token.is_empty()
        {
            return Some(sess);
        }
        // 兼容旧格式：纯文本 token
        Some(AuthSession::from_token(trimmed))
    }

    /// 保存 token（兼容旧接口：若已有 cookie 则保留）。
    pub fn save(&self, token: &str) -> io::Result<()> {
        let existing_cookie = self.load_session().and_then(|s| s.cookie);
        self.save_session(&AuthSession::new(token, existing_cookie))
    }

    /// 读取 token；文件不存在或为空返回 `None`（未登录）。
    pub fn load(&self) -> Option<String> {
        self.load_session().map(|s| s.token)
    }

    /// 清空持久化会话。
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
    fn save_session_and_load_session_roundtrip_with_cookie() {
        let store = TokenStore::new(temp_path("session-roundtrip"));
        let session = AuthSession::new("tok-999", Some("PHPSESSID=session_xyz".to_string()));
        store.save_session(&session).unwrap();
        assert_eq!(store.load_session(), Some(session));
        let _ = std::fs::remove_file(store.path());
    }

    #[test]
    fn load_session_parses_legacy_plain_text_token() {
        let store = TokenStore::new(temp_path("legacy-plain"));
        std::fs::write(store.path(), "legacy-plain-token-123\n").unwrap();
        let loaded = store.load_session().unwrap();
        assert_eq!(loaded.token, "legacy-plain-token-123");
        assert_eq!(loaded.cookie, None);
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
        assert_eq!(store.load_session(), None);
    }

    #[test]
    fn empty_file_is_logged_out() {
        let store = TokenStore::new(temp_path("empty"));
        std::fs::write(store.path(), "").unwrap();
        assert_eq!(store.load(), None);
        assert_eq!(store.load_session(), None);
        let _ = std::fs::remove_file(store.path());
    }

    #[test]
    fn whitespace_only_file_is_logged_out() {
        let store = TokenStore::new(temp_path("blank"));
        std::fs::write(store.path(), "  \n\t").unwrap();
        assert_eq!(store.load(), None);
        assert_eq!(store.load_session(), None);
        let _ = std::fs::remove_file(store.path());
    }
}
