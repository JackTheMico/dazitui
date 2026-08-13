//! 登录凭据与 token 生命周期（纯逻辑，网络调用由调用方完成）。

use super::client::ApiError;

/// 从环境变量读取登录凭据。
///
/// 只有 `DAZITUI_USER` 与 `DAZITUI_PASS` 都有值才返回 `Some`；
/// 任一缺失视为「未配置」，走手动输入。
///
/// `get` 为环境读取闭包（测试注入，生产用 `std::env::var`）。
pub fn env_credentials(get: impl Fn(&str) -> Option<String>) -> Option<(String, String)> {
    let user = get("DAZITUI_USER")?;
    let pass = get("DAZITUI_PASS")?;
    Some((user, pass))
}

/// 判断 API 错误是否表示登录失效（token 过期/未登录），需要提示重新登录。
///
/// 真实网关 token 失效时返回业务错误，文案通常包含「登录」「过期」「失效」。
/// 传输失败、响应格式错误、以及其它业务错误不算登录失效。
pub fn is_auth_failure(err: &ApiError) -> bool {
    match err {
        ApiError::Server(msg) => {
            msg.contains("登录") || msg.contains("过期") || msg.contains("失效")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(map: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = map
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn env_credentials_returns_both_when_set() {
        let get = env_of(&[("DAZITUI_USER", "alice"), ("DAZITUI_PASS", "s3cret")]);
        assert_eq!(
            env_credentials(get),
            Some(("alice".to_string(), "s3cret".to_string()))
        );
    }

    #[test]
    fn env_credentials_none_when_user_missing() {
        let get = env_of(&[("DAZITUI_PASS", "s3cret")]);
        assert_eq!(env_credentials(get), None);
    }

    #[test]
    fn env_credentials_none_when_pass_missing() {
        let get = env_of(&[("DAZITUI_USER", "alice")]);
        assert_eq!(env_credentials(get), None);
    }

    #[test]
    fn env_credentials_none_when_both_missing() {
        let get = env_of(&[]);
        assert_eq!(env_credentials(get), None);
    }

    #[test]
    fn is_auth_failure_detects_login_expired_message() {
        let err = ApiError::Server("登录已过期，请重新登录".into());
        assert!(is_auth_failure(&err));
    }

    #[test]
    fn is_auth_failure_detects_relogin_message() {
        let err = ApiError::Server("token 失效".into());
        assert!(is_auth_failure(&err));
    }

    #[test]
    fn is_auth_failure_ignores_other_server_errors() {
        let err = ApiError::Server("用户名不能为空！".into());
        assert!(!is_auth_failure(&err));
    }

    #[test]
    fn is_auth_failure_ignores_transport_and_parse() {
        assert!(!is_auth_failure(&ApiError::Transport("连接失败".into())));
        assert!(!is_auth_failure(&ApiError::Parse("无效 JSON".into())));
    }
}
