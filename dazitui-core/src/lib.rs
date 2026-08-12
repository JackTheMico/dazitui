use std::path::Path;

/// 赛文：练习/比赛用的文字内容，来自本地文件或 52dazi.cn。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    /// 赛文的标题（本地文件载入时为文件名）。
    pub title: String,
    /// 赛文内容。
    pub content: String,
}

/// 载文失败的分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// 文件不存在。
    NotFound,
    /// 文件存在但内容为空。
    Empty,
    /// 读取失败（权限等）。
    ReadFailed,
}

/// 从本地文件载入赛文。
pub fn load_text_from_file(path: &Path) -> Result<Text, LoadError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            LoadError::NotFound
        } else {
            LoadError::ReadFailed
        }
    })?;
    if content.is_empty() {
        return Err(LoadError::Empty);
    }
    let title = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(Text { title, content })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(suffix: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("dazitui-test-{stamp}-{suffix}"))
    }

    #[test]
    fn loads_existing_file_into_text() {
        let path = temp_path("exists.txt");
        fs::write(&path, "你好，世界。\n这是第二行。").unwrap();

        let text = load_text_from_file(&path).expect("载入应成功");
        assert!(text.title.ends_with("exists.txt"), "title 应为文件名，得到: {}", text.title);
        assert_eq!(text.content, "你好，世界。\n这是第二行。");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_not_found_error() {
        let path = temp_path("missing.txt");
        let _ = fs::remove_file(&path); // 确保不存在

        let err = load_text_from_file(&path).expect_err("缺失文件应报错");
        assert_eq!(err, LoadError::NotFound);
    }

    #[test]
    fn empty_file_is_empty_error() {
        let path = temp_path("empty.txt");
        fs::write(&path, "").unwrap();

        let err = load_text_from_file(&path).expect_err("空文件应报错");
        assert_eq!(err, LoadError::Empty);

        let _ = fs::remove_file(&path);
    }
}
