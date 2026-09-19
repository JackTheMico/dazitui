//! 虎码杯（race.tiger-code.com）凭据持久化（纯文件读写，零网络依赖）。

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 虎码杯登录凭据与会话信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TigerCredentials {
    /// 比赛平台用户名。
    pub username: String,
    /// 比赛平台密码（服务端获取生稿与提交成绩时需携带认证）。
    pub password: String,
    /// 可选的 JWT 认证令牌。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// 用户 ID。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<u64>,
}

impl TigerCredentials {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            token: None,
            user_id: None,
        }
    }
}

/// 虎码杯凭据文件读写。
#[derive(Debug, Clone)]
pub struct TigerTokenStore {
    path: PathBuf,
}

impl TigerTokenStore {
    /// 默认存储路径：~/.config/dazitui/tokens/tigercup.json。
    pub fn with_default_path() -> Self {
        Self::new(crate::paths::default_tigercup_token_path())
    }

    /// 指定路径的存储。
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 存储路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 是否已登录并持有凭据。
    pub fn is_logged_in(&self) -> bool {
        self.load().ok().flatten().is_some()
    }

    /// 保存凭据（JSON 格式，自动创建父目录，并设置 0600 权限保障安全）。
    pub fn save(&self, creds: &TigerCredentials) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(creds)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        std::fs::write(&self.path, json.as_bytes())?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }

        Ok(())
    }

    /// 读取凭据。文件不存在或内容无效返回 `Ok(None)`。
    pub fn load(&self) -> io::Result<Option<TigerCredentials>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let content = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        match serde_json::from_str::<TigerCredentials>(trimmed) {
            Ok(c) if !c.username.is_empty() => Ok(Some(c)),
            _ => Ok(None),
        }
    }

    /// 清理凭据（登出）。
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
    fn tiger_token_store_roundtrip() {
        let dir = std::env::temp_dir().join("dazitui_test_tigertoken");
        let path = dir.join("tigercup.json");
        let _ = std::fs::remove_file(&path);

        let store = TigerTokenStore::new(path.clone());
        assert!(!store.is_logged_in());
        assert_eq!(store.load().unwrap(), None);

        let creds = TigerCredentials {
            username: "摸鱼侠".into(),
            password: "password123".into(),
            token: Some("jwt.token.abc".into()),
            user_id: Some(624),
        };
        store.save(&creds).unwrap();
        assert!(store.is_logged_in());

        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded, creds);

        store.clear().unwrap();
        assert!(!store.is_logged_in());
        assert_eq!(store.load().unwrap(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
