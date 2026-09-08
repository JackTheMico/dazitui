use std::ffi::OsString;
use std::path::PathBuf;

/// 获取用户主目录。
///
/// 优先级：
/// 1. `HOME`（Linux / macOS / MSYS2 / WSL）
/// 2. `USERPROFILE`（原生 Windows）
/// 3. `HOMEDRIVE` + `HOMEPATH`（Windows 兼容回退）
/// 4. `"."`（无环境变量时的最后兜底）
pub fn user_home_dir() -> PathBuf {
    user_home_dir_from(|k| std::env::var_os(k))
}

pub(crate) fn user_home_dir_from(get_env: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    if let Some(home) = get_env("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home);
        }
    }
    if let Some(userprofile) = get_env("USERPROFILE") {
        if !userprofile.is_empty() {
            return PathBuf::from(userprofile);
        }
    }
    if let (Some(drive), Some(path)) = (get_env("HOMEDRIVE"), get_env("HOMEPATH")) {
        let d = drive.to_string_lossy();
        let p = path.to_string_lossy();
        let combined = format!("{d}{p}");
        if !combined.is_empty() {
            return PathBuf::from(combined);
        }
    }
    PathBuf::from(".")
}

/// 获取用户配置根目录。
///
/// 优先级：
/// 1. `XDG_CONFIG_HOME`（优先尊重显式设置的 XDG 变量）
/// 2. 若在 Windows 环境或存在 `APPDATA`，使用 `APPDATA`（Roaming，适合跨设备漫游的配置文件）
/// 3. 回退为 `~/.config`（类 Unix 默认）
pub fn config_dir() -> PathBuf {
    config_dir_from(|k| std::env::var_os(k), cfg!(windows))
}

pub(crate) fn config_dir_from(get_env: impl Fn(&str) -> Option<OsString>, is_windows: bool) -> PathBuf {
    if let Some(xdg) = get_env("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg);
        }
    }
    if is_windows {
        if let Some(appdata) = get_env("APPDATA") {
            if !appdata.is_empty() {
                return PathBuf::from(appdata);
            }
        }
    } else if get_env("HOME").is_none() {
        // 非 Windows 但无 HOME 时，若存在 APPDATA 也可兜底
        if let Some(appdata) = get_env("APPDATA") {
            if !appdata.is_empty() {
                return PathBuf::from(appdata);
            }
        }
    }
    user_home_dir_from(&get_env).join(".config")
}

/// 获取用户数据根目录。
///
/// 优先级：
/// 1. `XDG_DATA_HOME`（优先尊重显式设置的 XDG 变量）
/// 2. 若在 Windows 环境或存在 `LOCALAPPDATA` / `APPDATA`，优先使用 `LOCALAPPDATA`（Local 适合存放 SQLite 等本地机器专有数据），次选 `APPDATA`
/// 3. 回退为 `~/.local/share`（类 Unix 默认）
pub fn data_dir() -> PathBuf {
    data_dir_from(|k| std::env::var_os(k), cfg!(windows))
}

pub(crate) fn data_dir_from(get_env: impl Fn(&str) -> Option<OsString>, is_windows: bool) -> PathBuf {
    if let Some(xdg) = get_env("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg);
        }
    }
    if is_windows {
        if let Some(local_appdata) = get_env("LOCALAPPDATA") {
            if !local_appdata.is_empty() {
                return PathBuf::from(local_appdata);
            }
        }
        if let Some(appdata) = get_env("APPDATA") {
            if !appdata.is_empty() {
                return PathBuf::from(appdata);
            }
        }
    } else if get_env("HOME").is_none() {
        if let Some(local_appdata) = get_env("LOCALAPPDATA") {
            if !local_appdata.is_empty() {
                return PathBuf::from(local_appdata);
            }
        }
    }
    user_home_dir_from(&get_env).join(".local").join("share")
}

/// dazitui 的配置目录（包含 settings、token 等）。
pub fn dazitui_config_dir() -> PathBuf {
    config_dir().join("dazitui")
}

/// dazitui 的数据目录（包含 stats.db 等）。
pub fn dazitui_data_dir() -> PathBuf {
    data_dir().join("dazitui")
}

/// 默认设置文件路径。
pub fn default_settings_path() -> PathBuf {
    dazitui_config_dir().join("settings")
}

/// 默认统计数据库文件路径。
pub fn default_stats_db_path() -> PathBuf {
    dazitui_data_dir().join("stats.db")
}

/// 默认 52dazi Token 文件路径。
pub fn default_token_path() -> PathBuf {
    dazitui_config_dir().join("token")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn mock_env(map: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<OsString> {
        move |k| map.get(k).map(|v| OsString::from(*v))
    }

    #[test]
    fn test_user_home_dir_unix() {
        let env = HashMap::from([("HOME", "/home/alice")]);
        let home = user_home_dir_from(mock_env(env));
        assert_eq!(home, PathBuf::from("/home/alice"));
    }

    #[test]
    fn test_user_home_dir_windows() {
        let env = HashMap::from([("USERPROFILE", "C:\\Users\\alice")]);
        let home = user_home_dir_from(mock_env(env));
        assert_eq!(home, PathBuf::from("C:\\Users\\alice"));
    }

    #[test]
    fn test_user_home_dir_windows_drive_path() {
        let env = HashMap::from([("HOMEDRIVE", "D:"), ("HOMEPATH", "\\Users\\bob")]);
        let home = user_home_dir_from(mock_env(env));
        assert_eq!(home, PathBuf::from("D:\\Users\\bob"));
    }

    #[test]
    fn test_user_home_dir_fallback() {
        let env = HashMap::new();
        let home = user_home_dir_from(mock_env(env));
        assert_eq!(home, PathBuf::from("."));
    }

    #[test]
    fn test_config_dir_xdg_precedence() {
        let env = HashMap::from([
            ("XDG_CONFIG_HOME", "/custom/config"),
            ("HOME", "/home/alice"),
            ("APPDATA", "C:\\Users\\alice\\AppData\\Roaming"),
        ]);
        let cfg = config_dir_from(mock_env(env), true);
        assert_eq!(cfg, PathBuf::from("/custom/config"));
    }

    #[test]
    fn test_config_dir_windows() {
        let env = HashMap::from([
            ("USERPROFILE", "C:\\Users\\alice"),
            ("APPDATA", "C:\\Users\\alice\\AppData\\Roaming"),
        ]);
        let cfg = config_dir_from(mock_env(env), true);
        assert_eq!(cfg, PathBuf::from("C:\\Users\\alice\\AppData\\Roaming"));
    }

    #[test]
    fn test_data_dir_windows() {
        let env = HashMap::from([
            ("USERPROFILE", "C:\\Users\\alice"),
            ("LOCALAPPDATA", "C:\\Users\\alice\\AppData\\Local"),
            ("APPDATA", "C:\\Users\\alice\\AppData\\Roaming"),
        ]);
        let data = data_dir_from(mock_env(env), true);
        assert_eq!(data, PathBuf::from("C:\\Users\\alice\\AppData\\Local"));
    }

    #[test]
    fn test_data_dir_unix() {
        let env = HashMap::from([("HOME", "/home/alice")]);
        let data = data_dir_from(mock_env(env), false);
        assert_eq!(data, PathBuf::from("/home/alice/.local/share"));
    }
}
