//! 52dazi.cn 传输层：ureq 同步 HTTP 客户端，封装登录/载文/上传三个 API。
//!
//! 响应解析与请求构造为纯函数（可独立测试），HTTP 调用是薄壳。
//! 协议细节见 ADR-0002：请求体 AES 加密，响应为明文 JSON `{"error":0,"msg":…}`。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use super::protocol::encrypt_value;
use super::token::{AuthSession, TokenStore};
use crate::{CompetitionType, Stats, Text};

/// 52dazi 网关根地址。
pub const BASE_URL: &str = "https://www.jsxiaoshi.com/index.php";

/// 请求体公共字段（前端固定携带）。
const VERSION: &str = "v2.1.6";
const SUBVERSIONS: u32 = 17108;

/// 传输层错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// 网络/传输失败（连接失败、超时、HTTP 非 2xx 等）。
    Transport(String),
    /// 服务器返回业务错误（响应 `error != 0`）。
    Server(String),
    /// 响应不是预期格式（无效 JSON、字段缺失/类型错误）。
    Parse(String),
}

/// 登录结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginResult {
    /// 会话 token，后续请求携带。
    pub token: String,
}

/// 比赛赛文（getContent 响应 `msg` 对象）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompetitionText {
    /// 赛文内容（`msg["a_content"]` 或 `msg["0"]`）。
    pub content: String,
    /// 赛文标题（`msg["a_name"]` 或 `msg["7"]`）。
    pub title: String,
    /// 作者（`msg["a_author"]` 或 `msg["1"]`）。
    pub author: String,
    /// 字数（`msg["6"]` 或字符数）。
    pub word_num: usize,
}

/// 上传结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankResult {
    /// 服务器返回的提示文本（通常包含排名信息）。
    pub message: String,
    /// 结构化排名（仅当服务器返回对象形式的 `msg` 时有值）。
    pub ranking: Option<String>,
}

/// 上传成绩高层结果（包含排名、提示文本与格式化分享文本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadOutcome {
    /// 结构化排名（若有）。
    pub ranking: Option<String>,
    /// 格式化分享文本。
    pub share_text: String,
    /// 服务器返回的提示文本。
    pub message: String,
}

/// 统一响应外壳：成功 `error == 0` 且 `msg` 为数据，失败 `msg` 为错误文本。
#[derive(Deserialize)]
struct RawResponse {
    error: i32,
    msg: Value,
}

/// 解析统一响应：`error != 0` → `Server` 错误，否则把 `msg` 反序列化为 `T`。
fn parse_api_response<T: DeserializeOwned>(body: &str) -> Result<T, ApiError> {
    let raw: RawResponse = serde_json::from_str(body)
        .map_err(|e| ApiError::Parse(format!("响应不是有效 JSON: {e}")))?;
    if raw.error != 0 {
        let msg = raw.msg.as_str().unwrap_or("未知错误").to_string();
        return Err(ApiError::Server(msg));
    }
    serde_json::from_value(raw.msg).map_err(|e| ApiError::Parse(format!("msg 字段解析失败: {e}")))
}

/// 解析登录响应：`msg.token`。
pub fn parse_login_response(body: &str) -> Result<LoginResult, ApiError> {
    #[derive(Deserialize)]
    struct LoginMsg {
        token: String,
    }
    let m: LoginMsg = parse_api_response(body)?;
    Ok(LoginResult { token: m.token })
}

/// 解析载文响应：优先读取真实文章字段 `a_name`/`a_content`，兼容数字键 `"0"`/`"7"`。
pub fn parse_content_response(body: &str) -> Result<CompetitionText, ApiError> {
    let obj: Map<String, Value> = parse_api_response(body)?;
    let content = obj
        .get("a_content")
        .or_else(|| obj.get("0"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let title = obj
        .get("a_name")
        .or_else(|| obj.get("7"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let author = obj
        .get("a_author")
        .or_else(|| obj.get("1"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let word_num = obj
        .get("6")
        .and_then(|v| {
            v.as_u64()
                .map(|n| n as usize)
                .or_else(|| v.as_str().and_then(|s| s.parse::<usize>().ok()))
        })
        .unwrap_or_else(|| content.chars().count());
    Ok(CompetitionText {
        content,
        title,
        author,
        word_num,
    })
}

/// 标题为空时用比赛类型名兜底（验收要求「无标题时显示比赛类型」）。
fn with_title_fallback(
    mut text: CompetitionText,
    competition_type: CompetitionType,
) -> CompetitionText {
    if text.title.is_empty() {
        text.title = competition_type.name().to_string();
    }
    text
}

/// 解析上传响应：`msg` 为提示文本（字符串）或对象（含 `ranking`/`rankTips`）。
pub fn parse_upload_response(body: &str) -> Result<RankResult, ApiError> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RankMsg {
        Text(String),
        Object {
            #[serde(rename = "rankTips")]
            rank_tips: Option<String>,
            ranking: Option<Value>,
        },
    }
    let m: RankMsg = parse_api_response(body)?;
    match m {
        RankMsg::Text(s) => Ok(RankResult {
            message: s,
            ranking: None,
        }),
        RankMsg::Object { rank_tips, ranking } => {
            let message = rank_tips.unwrap_or_default();
            let ranking = ranking.map(|v| match v {
                Value::Number(n) => n.to_string(),
                Value::String(s) => s,
                other => other.to_string(),
            });
            Ok(RankResult { message, ranking })
        }
    }
}

/// 请求体公共字段：`from` + `timestamp` + `version` + `subversions` + 可选 `token`。
fn base_fields(token: Option<&str>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("from".into(), json!("web"));
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    m.insert("timestamp".into(), json!(timestamp));
    m.insert("version".into(), json!(VERSION));
    m.insert("subversions".into(), json!(SUBVERSIONS));
    if let Some(t) = token {
        m.insert("token".into(), json!(t));
    }
    m
}

/// 登录请求体（加密后）。
fn login_payload(username: &str, password: &str) -> String {
    let mut m = base_fields(None);
    m.insert("username".into(), json!(username));
    m.insert("password".into(), json!(password));
    encrypt_value(&Value::Object(m))
}

/// 载文请求体（加密后）：`competitionType` + `snumflag=1`。
fn content_payload(token: &str, competition_type: CompetitionType) -> String {
    let mut m = base_fields(Some(token));
    m.insert("competitionType".into(), json!(competition_type.code()));
    m.insert("snumflag".into(), json!("1"));
    encrypt_value(&Value::Object(m))
}

/// 上传请求体（加密后）：合并业务字段与公共字段。
fn upload_payload(token: &str, payload: &Value) -> String {
    let mut m = base_fields(Some(token));
    if let Value::Object(obj) = payload {
        for (k, v) in obj {
            m.insert(k.clone(), v.clone());
        }
    }
    encrypt_value(&Value::Object(m))
}


/// 52dazi 客户端（深模块：封装会话持久化、Cookie 回传、自动重登与上传全流程）。
#[derive(Debug, Clone)]
pub struct ApiClient {
    agent: ureq::Agent,
    base_url: String,
    session: Arc<Mutex<Option<AuthSession>>>,
    token_store: Option<TokenStore>,
}

impl Default for ApiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiClient {
    /// 指向 52dazi 网关的客户端（自动从默认路径加载与保存会话，10 秒超时）。
    ///
    /// 网关地址可用 `DAZITUI_BASE_URL` 环境变量覆盖（调试/抓包用，如反向代理）。
    pub fn new() -> Self {
        let store = TokenStore::with_default_path();
        let base_url = std::env::var("DAZITUI_BASE_URL").unwrap_or_else(|_| BASE_URL.to_string());
        Self::with_base_url_and_store(&base_url, Some(store))
    }

    /// 指定网关根地址（测试时指向本地 mock，不持久化到默认文件）。
    pub fn with_base_url(base_url: &str) -> Self {
        Self::with_base_url_and_store(base_url, None)
    }

    /// 指定会话存储（使用默认网关地址）。
    pub fn with_store(token_store: TokenStore) -> Self {
        let base_url = std::env::var("DAZITUI_BASE_URL").unwrap_or_else(|_| BASE_URL.to_string());
        Self::with_base_url_and_store(&base_url, Some(token_store))
    }

    /// 指定网关根地址与会话存储。
    pub fn with_base_url_and_store(base_url: &str, token_store: Option<TokenStore>) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .new_agent();
        let initial_session = token_store.as_ref().and_then(|s| s.load_session());
        Self {
            agent,
            base_url: base_url.to_string(),
            session: Arc::new(Mutex::new(initial_session)),
            token_store,
        }
    }

    /// 当前是否处于已登录状态（持有有效 token）。
    pub fn is_logged_in(&self) -> bool {
        self.current_token().is_some()
    }

    /// 获取当前登录 token。
    pub fn current_token(&self) -> Option<String> {
        self.session
            .lock()
            .ok()
            .and_then(|s| s.as_ref().and_then(|x| {
                if x.token.is_empty() {
                    None
                } else {
                    Some(x.token.clone())
                }
            }))
    }

    /// 获取当前完整会话（token + cookie）。
    pub fn current_session(&self) -> Option<AuthSession> {
        self.session.lock().ok().and_then(|s| s.clone())
    }

    /// 设置并同步持久化会话。
    pub fn set_session(&self, session: Option<AuthSession>) {
        if let Ok(mut lock) = self.session.lock() {
            *lock = session.clone();
        }
        if let Some(ref store) = self.token_store {
            if let Some(ref sess) = session {
                let _ = store.save_session(sess);
            } else {
                let _ = store.clear();
            }
        }
    }

    /// 注销登录：清空内存与磁盘会话。
    pub fn logout(&self) {
        self.set_session(None);
    }

    /// 登录：调用网关，提取 token 与 session cookie 并自动持久化。
    pub fn login(&self, username: &str, password: &str) -> Result<LoginResult, ApiError> {
        let body = login_payload(username, password);
        let resp = self.post("Api/User/login", &body)?;
        let result = parse_login_response(&resp)?;
        let existing_cookie = self
            .session
            .lock()
            .ok()
            .and_then(|s| s.as_ref().and_then(|x| x.cookie.clone()));
        let session = AuthSession::new(&result.token, existing_cookie);
        self.set_session(Some(session));
        Ok(result)
    }

    /// 按比赛类型载入赛文（自动使用当前登录 token）。
    pub fn get_content(&self, competition_type: CompetitionType) -> Result<CompetitionText, ApiError> {
        let token = self
            .current_token()
            .ok_or_else(|| ApiError::Server("请先登录 52dazi".into()))?;
        self.get_content_with_token(&token, competition_type)
    }

    /// 按比赛类型与指定 token 载入赛文。
    pub fn get_content_with_token(
        &self,
        token: &str,
        competition_type: CompetitionType,
    ) -> Result<CompetitionText, ApiError> {
        let body = content_payload(token, competition_type);
        let resp = self.post("Api/Text/getContent", &body)?;
        Ok(with_title_fallback(
            parse_content_response(&resp)?,
            competition_type,
        ))
    }

    /// 校验 token 是否有效（通过 getBaseInfo 的 isLogin 字段探测）。
    pub fn validate_token(&self, token: &str) -> Result<bool, ApiError> {
        let body = encrypt_value(&Value::Object(base_fields(Some(token))));
        let resp = self.post("Api/System/getBaseInfo", &body)?;
        let obj: Map<String, Value> = parse_api_response(&resp)?;
        let is_login = obj.get("isLogin").and_then(Value::as_i64).unwrap_or(0);
        Ok(is_login == 1)
    }

    /// 校验当前会话是否处于有效登录态。
    pub fn validate_current_session(&self) -> Result<bool, ApiError> {
        let Some(token) = self.current_token() else {
            return Ok(false);
        };
        self.validate_token(&token)
    }

    /// 上传成绩（`payload` 为业务字段，公共字段与 token 自动合并）。
    pub fn upload_result(&self, token: &str, payload: &Value) -> Result<RankResult, ApiError> {
        let body = upload_payload(token, payload);
        let resp = self.post("Api/Rank/uploadResult", &body)?;
        parse_upload_response(&resp)
    }

    /// 一站式完成跟打成绩上传（深模块核心接口）：
    /// 自动校验登录态、计算指标、构造 payload、上传结果；
    /// 若 token 失效且配置了环境变量凭据则自动重登重试一次；
    /// 成功后自动格式化分享文本并返回 `UploadOutcome`。
    pub fn upload_session(
        &self,
        text: &Text,
        stats: &Stats,
        elapsed: Duration,
    ) -> Result<UploadOutcome, ApiError> {
        let token = self
            .current_token()
            .ok_or_else(|| ApiError::Server("未登录，无法上传成绩".into()))?;
        let upload_stats = super::share::to_upload_stats(stats, elapsed);
        let payload = super::share::build_upload_payload(text, stats, &upload_stats, elapsed);
        let rank_res = match self.upload_result(&token, &payload) {
            Ok(r) => Ok(r),
            Err(e) => {
                if super::auth::is_auth_failure(&e) {
                    if let Some((user, pass)) =
                        super::auth::env_credentials(|k| std::env::var(k).ok())
                    {
                        if let Ok(new_login) = self.login(&user, &pass) {
                            let new_payload = super::share::build_upload_payload(
                                text,
                                stats,
                                &upload_stats,
                                elapsed,
                            );
                            self.upload_result(&new_login.token, &new_payload)
                        } else {
                            Err(e)
                        }
                    } else {
                        Err(e)
                    }
                } else {
                    Err(e)
                }
            }
        }?;
        let ranking = rank_res.ranking.clone();
        let rank_num = ranking.as_deref().and_then(|s| s.parse::<u32>().ok());
        let share_text = super::share::format_share_text(&text.source, rank_num, &upload_stats);
        Ok(UploadOutcome {
            ranking,
            share_text,
            message: rank_res.message,
        })
    }

    /// 发送加密请求体，维护 Session Cookie，返回响应文本。
    fn post(&self, path: &str, body: &str) -> Result<String, ApiError> {
        let url = format!("{}/{path}", self.base_url);
        let mut req = self.agent.post(&url);
        if let Some(cookie) = self
            .session
            .lock()
            .ok()
            .and_then(|s| s.as_ref().and_then(|x| x.cookie.clone()))
        {
            req = req.header("Cookie", &cookie);
        }
        let mut resp = req.send(body).map_err(|e| ApiError::Transport(e.to_string()))?;
        let new_cookie = resp
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).trim().to_string());
        if let Some(cookie_val) = new_cookie {
            if let Ok(mut lock) = self.session.lock() {
                if let Some(sess) = lock.as_mut() {
                    sess.cookie = Some(cookie_val);
                    if let Some(ref store) = self.token_store {
                        let _ = store.save_session(sess);
                    }
                } else {
                    *lock = Some(AuthSession::new("", Some(cookie_val)));
                }
            }
        }
        resp.body_mut()
            .read_to_string()
            .map_err(|e| ApiError::Transport(e.to_string()))
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_ureq_header_api() {
        let resp = ureq::http::Response::builder()
            .header("Set-Cookie", "PHPSESSID=test12345; path=/")
            .body(())
            .unwrap();
        let cookie_str = resp.headers().get("set-cookie").and_then(|v| v.to_str().ok());
        assert_eq!(cookie_str, Some("PHPSESSID=test12345; path=/"));
    }

    #[test]
    fn parse_login_returns_token() {
        let body = r#"{"error":0,"msg":{"token":"tok-123","name":"张三"}}"#;
        let r = parse_login_response(body).unwrap();
        assert_eq!(r.token, "tok-123");
    }

    #[test]
    fn parse_login_server_error_message() {
        let body = r#"{"error":1,"msg":"您的用户名或密码错误！"}"#;
        let err = parse_login_response(body).unwrap_err();
        assert_eq!(err, ApiError::Server("您的用户名或密码错误！".into()));
    }

    #[test]
    fn parse_content_extracts_content_title_author_word_num() {
        // msg 对象结构（锦标赛/键神杯实测）："0"=内容 "1"=作者 "6"=字数 "7"=标题
        let body = r#"{"error":0,"msg":{"0":"你好世界","1":"消逝","2":"beiyiwang12","3":"170.91","4":"尊极境七重","6":698,"7":"锦标赛第3279期"}}"#;
        let r = parse_content_response(body).unwrap();
        assert_eq!(r.content, "你好世界");
        assert_eq!(r.title, "锦标赛第3279期");
        assert_eq!(r.author, "消逝");
        assert_eq!(r.word_num, 698);
    }

    #[test]
    fn parse_content_response_with_named_fields_extracts_title_and_content() {
        // 52dazi 实测响应：a_name 与 a_content
        let body = r#"{"error":0,"msg":{"a_name":"市井人间烟火的生活本真","a_content":"市井人间烟火，是最真实的生活本真","number":99999}}"#;
        let r = parse_content_response(body).unwrap();
        assert_eq!(r.title, "市井人间烟火的生活本真");
        assert_eq!(r.content, "市井人间烟火，是最真实的生活本真");
        assert_eq!(r.word_num, "市井人间烟火，是最真实的生活本真".chars().count());
    }

    #[test]
    fn parse_content_missing_optional_fields_does_not_panic() {
        // 兼容历史数字键格式：只返回 "0"（内容）
        let body = r#"{"error":0,"msg":{"0":"只有内容"}}"#;
        let r = parse_content_response(body).unwrap();
        assert_eq!(r.content, "只有内容");
        assert_eq!(r.title, "");
        assert_eq!(r.author, "");
        assert_eq!(r.word_num, "只有内容".chars().count());
    }

    #[test]
    fn with_title_fallback_uses_competition_name_when_empty() {
        let text = CompetitionText {
            content: "x".into(),
            title: "".into(),
            author: "".into(),
            word_num: 0,
        };
        let filled = with_title_fallback(text, CompetitionType::Jisu);
        assert_eq!(filled.title, "极速杯");
    }

    #[test]
    fn with_title_fallback_keeps_server_title_when_present() {
        let text = CompetitionText {
            content: "x".into(),
            title: "市井人间烟火的生活本真".into(),
            author: "".into(),
            word_num: 0,
        };
        let filled = with_title_fallback(text, CompetitionType::Jisu);
        assert_eq!(filled.title, "市井人间烟火的生活本真");
    }

    #[test]
    fn parse_upload_text_msg() {
        let body = r#"{"error":0,"msg":"上传成功，你在极速杯排名第5名！"}"#;
        let r = parse_upload_response(body).unwrap();
        assert_eq!(r.message, "上传成功，你在极速杯排名第5名！");
        assert_eq!(r.ranking, None);
    }

    #[test]
    fn parse_upload_object_msg_with_ranking() {
        let body = r#"{"error":0,"msg":{"ranking":5,"rankTips":"恭喜获得第5名"}}"#;
        let r = parse_upload_response(body).unwrap();
        assert_eq!(r.message, "恭喜获得第5名");
        assert_eq!(r.ranking, Some("5".into()));
    }

    #[test]
    fn parse_invalid_json_is_parse_error() {
        let err = parse_login_response("not json").unwrap_err();
        assert!(matches!(err, ApiError::Parse(_)));
    }

    #[test]
    fn parse_missing_msg_field_is_parse_error() {
        let err = parse_login_response(r#"{"error":0}"#).unwrap_err();
        assert!(matches!(err, ApiError::Parse(_)));
    }

    // ---- 请求构造 ----

    #[test]
    fn login_payload_contains_fields_and_common() {
        let encoded = login_payload("alice", "s3cret");
        let decrypted = super::super::protocol::decrypt(&encoded);
        let v: Value = serde_json::from_str(&decrypted).unwrap();
        assert_eq!(v["username"], "alice");
        assert_eq!(v["password"], "s3cret");
        assert_eq!(v["from"], "web");
        assert!(v["timestamp"].is_number(), "应包含秒级时间戳");
        assert_eq!(v["version"], VERSION);
        assert_eq!(v["subversions"], SUBVERSIONS);
        assert!(v.get("token").is_none(), "登录请求不应带 token");
    }

    #[test]
    fn content_payload_contains_competition_type_and_token() {
        let encoded = content_payload("tok-9", CompetitionType::Jinbiao);
        let decrypted = super::super::protocol::decrypt(&encoded);
        let v: Value = serde_json::from_str(&decrypted).unwrap();
        assert_eq!(v["competitionType"], 2); // 锦标赛 = 2
        assert_eq!(v["snumflag"], "1");
        assert_eq!(v["token"], "tok-9");
    }

    #[test]
    fn upload_payload_merges_business_fields_and_common() {
        let payload = json!({"speed": 85.2, "wordNum": 100, "textTitle": "赛文"});
        let encoded = upload_payload("tok-9", &payload);
        let decrypted = super::super::protocol::decrypt(&encoded);
        let v: Value = serde_json::from_str(&decrypted).unwrap();
        assert_eq!(v["speed"], 85.2);
        assert_eq!(v["wordNum"], 100);
        assert_eq!(v["textTitle"], "赛文");
        assert_eq!(v["token"], "tok-9");
        assert_eq!(v["version"], VERSION);
    }

    // ---- HTTP 壳 ----

    #[test]
    fn network_failure_returns_transport_error_not_panic() {
        // 127.0.0.1:1 几乎必然连接拒绝，验证网络错误被友好化。
        let client = ApiClient::with_base_url("http://127.0.0.1:1");
        let err = client.login("a", "b").unwrap_err();
        assert!(matches!(err, ApiError::Transport(_)), "得到: {err:?}");
    }

    #[test]
    fn session_cookie_is_replayed_between_requests() {
        // 核心回归：ureq 启用 cookies feature 后，login 返回的 Set-Cookie（PHPSESSID）
        // 必须在同一 ApiClient 实例的后续请求中自动回传，否则登录后上传仍会「用户名不能为空」。
        use std::io::{Read, Write};
        use std::net::TcpListener;

        fn read_http_request(conn: &mut std::net::TcpStream) -> String {
            conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut total = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = conn.read(&mut buf).expect("读取请求失败");
                if n == 0 {
                    break;
                }
                total.extend_from_slice(&buf[..n]);
                // 找到 header 结尾后，按 Content-Length 判断 body 是否读完
                if let Some(pos) = total.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&total[..pos]);
                    let content_len = headers
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                        .and_then(|l| l.split(':').nth(1))
                        .and_then(|s| s.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if total.len() >= pos + 4 + content_len {
                        break;
                    }
                }
            }
            String::from_utf8_lossy(&total).to_string()
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            // 请求 1：login，返回 Set-Cookie
            let (mut conn, _) = listener.accept().unwrap();
            let req1 = read_http_request(&mut conn);
            assert!(req1.contains("Api/User/login"), "请求1 应为 login: {req1}");
            assert!(
                !req1.to_ascii_lowercase().contains("cookie:"),
                "请求1 不应带 cookie: {req1}"
            );
            let body1 = r#"{"error":0,"msg":{"token":"t1"}}"#;
            conn.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nSet-Cookie: PHPSESSID=abc123; path=/\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body1}",
                    body1.len()
                )
                .as_bytes(),
            )
            .unwrap();
            drop(conn);

            // 请求 2：uploadResult，应回传 PHPSESSID
            let (mut conn2, _) = listener.accept().unwrap();
            let req2 = read_http_request(&mut conn2);
            assert!(
                req2.contains("Api/Rank/uploadResult"),
                "请求2 应为 uploadResult: {req2}"
            );
            let cookie = req2
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("cookie:"))
                .unwrap_or("")
                .to_string();
            assert!(
                cookie.contains("PHPSESSID=abc123"),
                "请求2 未回传会话 cookie。Cookie 头: {cookie:?}\n完整请求:\n{req2}"
            );
            let body2 = r#"{"error":0,"msg":"上传成功"}"#;
            conn2.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body2}",
                    body2.len()
                )
                .as_bytes(),
            )
            .unwrap();
        });

        let client = ApiClient::with_base_url(&format!("http://{addr}"));
        let login = client.login("u", "p").unwrap();
        assert_eq!(login.token, "t1");
        let rank = client.upload_result("t1", &json!({"speed": 85.0})).unwrap();
        assert_eq!(rank.message, "上传成功");

        server.join().unwrap();
    }

    /// 三步链路回归：登录 → 载文 → 上传成绩，全程同一 ApiClient（同一 cookie store）。
    ///
    /// 真实流程为登录后先 F1 载入赛文（`getContent`），打完后才 `uploadResult`。
    /// 现行 `session_cookie_is_replayed_between_requests` 只验证 login → uploadResult 两步，
    /// 没有覆盖中间的 `getContent`，故即便 cookie 在 GET/POST 间丢失也测不出。
    /// 本测试追加中间一步，断言 login 后下发的 PHPSESSID 在 getContent 与 uploadResult
    /// 两次后续请求中都仍然回传。
    #[test]
    fn session_cookie_survives_three_step_login_get_content_upload_result() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        fn read_http_request(conn: &mut std::net::TcpStream) -> String {
            conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut total = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = conn.read(&mut buf).expect("读取请求失败");
                if n == 0 {
                    break;
                }
                total.extend_from_slice(&buf[..n]);
                if let Some(pos) = total.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&total[..pos]);
                    let content_len = headers
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                        .and_then(|l| l.split(':').nth(1))
                        .and_then(|s| s.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if total.len() >= pos + 4 + content_len {
                        break;
                    }
                }
            }
            String::from_utf8_lossy(&total).to_string()
        }

        /// 从原始 HTTP 请求行里取出 cookie 头（小写键名），找不到返回空串。
        fn cookie_header(req: &str) -> String {
            req.lines()
                .find(|l| l.to_ascii_lowercase().starts_with("cookie:"))
                .unwrap_or("")
                .to_string()
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            // 请求 1：login，下发 Set-Cookie: PHPSESSID=abc123
            let (mut conn, _) = listener.accept().unwrap();
            let req1 = read_http_request(&mut conn);
            assert!(req1.contains("Api/User/login"), "请求1 应为 login: {req1}");
            assert!(
                !req1.to_ascii_lowercase().contains("cookie:"),
                "请求1 不应带 cookie: {req1}"
            );
            let body1 = r#"{"error":0,"msg":{"token":"t1"}}"#;
            conn.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nSet-Cookie: PHPSESSID=abc123; path=/\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body1}",
                    body1.len()
                )
                .as_bytes(),
            )
            .unwrap();
            drop(conn);

            // 请求 2：getContent，应已回传 PHPSESSID（此为现行两步测试所缺校验）
            let (mut conn2, _) = listener.accept().unwrap();
            let req2 = read_http_request(&mut conn2);
            assert!(
                req2.contains("Api/Text/getContent"),
                "请求2 应为 getContent: {req2}"
            );
            let cookie2 = cookie_header(&req2);
            assert!(
                cookie2.contains("PHPSESSID=abc123"),
                "请求2（getContent）未回传会话 cookie。Cookie 头: {cookie2:?}\n完整请求:\n{req2}"
            );
            // 极速杯风格：只返回 "0" 内容字段
            let body2 = r#"{"error":0,"msg":{"0":"户外溯溪玩水的夏日治愈"}}"#;
            conn2.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body2}",
                    body2.len()
                )
                .as_bytes(),
            )
            .unwrap();
            drop(conn2);

            // 请求 3：uploadResult，应仍回传 PHPSESSID
            let (mut conn3, _) = listener.accept().unwrap();
            let req3 = read_http_request(&mut conn3);
            assert!(
                req3.contains("Api/Rank/uploadResult"),
                "请求3 应为 uploadResult: {req3}"
            );
            let cookie3 = cookie_header(&req3);
            assert!(
                cookie3.contains("PHPSESSID=abc123"),
                "请求3（uploadResult）未回传会话 cookie。Cookie 头: {cookie3:?}\n完整请求:\n{req3}"
            );
            let body3 = r#"{"error":0,"msg":"上传成功"}"#;
            conn3.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body3}",
                    body3.len()
                )
                .as_bytes(),
            )
            .unwrap();
        });

        let client = ApiClient::with_base_url(&format!("http://{addr}"));
        // 步骤 1：登录，cookie 落到 client.agent 的 cookie store
        let login = client.login("u", "p").unwrap();
        assert_eq!(login.token, "t1");
        // 步骤 2：载入赛文——这是现行两步测试缺的关键一步
        let text = client
            .get_content_with_token("t1", CompetitionType::Jisu)
            .expect("getContent 应成功（cookie 应仍有效）");
        assert_eq!(text.content, "户外溯溪玩水的夏日治愈");
        // 步骤 3：上传成绩——真实 bug 现场：服务端在此步回「用户名不能为空！」
        let rank = client
            .upload_result("t1", &json!({"speed": 48.34, "wordNum": 369}))
            .expect("upload_result 应成功（cookie 应仍有效）");
        assert_eq!(rank.message, "上传成功");

        server.join().unwrap();
    }

    /// 针对性回归：mock 服务收 uploadResult，解密 AES-CBC 请求体，
    /// 断言 build_upload_payload 产出的所有前端 schema 必填字段都在（jianZhun/repeatNum/daCi/xuanChong/keyMethod 等）。
    ///
    /// 这条测试锚定 wire format：未来任何人改 build_upload_payload 漏掉某个字段时
    /// （这是历史上 `Server("用户名不能为空！")` 错误的诱因之一），这测试会红。
    #[test]
    fn upload_result_encrypts_all_frontend_schema_fields() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        fn read_http_request(conn: &mut std::net::TcpStream) -> String {
            conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut total = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = conn.read(&mut buf).expect("读取请求失败");
                if n == 0 {
                    break;
                }
                total.extend_from_slice(&buf[..n]);
                if let Some(pos) = total.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&total[..pos]);
                    let content_len = headers
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                        .and_then(|l| l.split(':').nth(1))
                        .and_then(|s| s.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if total.len() >= pos + 4 + content_len {
                        break;
                    }
                }
            }
            String::from_utf8_lossy(&total).to_string()
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let req = read_http_request(&mut conn);
            assert!(
                req.contains("Api/Rank/uploadResult"),
                "应为 uploadResult: {req}"
            );
            // body 是请求头 \r\n\r\n 之后的部分
            let body = req
                .split("\r\n\r\n")
                .nth(1)
                .unwrap_or("")
                .to_string();
            assert!(!body.is_empty(), "请求体为空");
            // 解密 AES-CBC 加密的请求体（前端格式）
            let plaintext = super::super::protocol::decrypt(&body);
            let v: Value = serde_json::from_str(&plaintext)
                .unwrap_or_else(|e| panic!("解密后不是有效 JSON: {e}\n明文: {plaintext}"));
            // 公共字段（由 upload_payload 合并）
            assert_eq!(v["from"], "web");
            assert!(v["timestamp"].is_number(), "应包含时间戳");
            assert_eq!(v["version"], VERSION);
            assert_eq!(v["subversions"], SUBVERSIONS);
            assert_eq!(v["token"], "tok-9");
            // 业务字段：前端 resultPostData 完整 schema（含新补的字段）
            for key in [
                "textTitle", "speed", "keystrokes", "maChang", "wordNum", "typingTime",
                "huiGai", "huiChe", "jianShu", "jianZhun", "accuracy", "repeatNum", "daCi",
                "wrongNum", "inputMethod", "backspace", "xuanChong", "keyMethod",
                "challengeFlag", "isFirstSubmit", "isGroupText",
            ] {
                assert!(
                    v.get(key).is_some(),
                    "uploadResult 请求体缺字段 `{key}`（与前端 resultPostData schema 对齐）。完整:\n{plaintext}"
                );
            }
            // 响应成功，随便返个 msg
            let resp = r#"{"error":0,"msg":"上传成功"}"#;
            conn.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                    resp.len()
                )
                .as_bytes(),
            )
            .unwrap();
        });

        let client = ApiClient::with_base_url(&format!("http://{addr}"));
        // 与 share.rs build_upload_payload 同源字段：speed/wordNum/textTitle 是最简单的可充值 payload
        let payload = json!({
            "textTitle": "极速杯",
            "speed": 48.34,
            "keystrokes": 3.5,
            "maChang": 2.8,
            "wordNum": 369,
            "typingTime": "05:32.410",
            "huiGai": 0,
            "huiChe": 0,
            "jianShu": 1024,
            "jianZhun": "100.00%",
            "accuracy": 100.0,
            "repeatNum": 0,
            "daCi": "0%",
            "wrongNum": 0,
            "inputMethod": "",
            "backspace": 0,
            "xuanChong": 0,
            "keyMethod": "0%",
            "challengeFlag": 0,
            "isFirstSubmit": 1,
            "isGroupText": 0,
        });
        let rank = client
            .upload_result("tok-9", &payload)
            .expect("upload_result 应成功");
        assert_eq!(rank.message, "上传成功");

        server.join().unwrap();
    }

    #[test]
    fn saved_session_cookie_persists_and_replays_after_restart() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::{SystemTime, UNIX_EPOCH};

        fn temp_path(suffix: &str) -> std::path::PathBuf {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            std::env::temp_dir().join(format!("dazitui-client-test-{stamp}-{suffix}"))
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let store_path = temp_path("session-cookie-restart");
        let store = TokenStore::new(store_path.clone());

        let server = std::thread::spawn(move || {
            // 请求 1：login
            let (mut conn1, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn1.read(&mut buf).unwrap();
            let body1 = r#"{"error":0,"msg":{"token":"tok-live"}}"#;
            conn1.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nSet-Cookie: PHPSESSID=session_persisted_999; path=/\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body1}",
                    body1.len()
                )
                .as_bytes(),
            )
            .unwrap();
            drop(conn1);

            // 请求 2：由新的 ApiClient 实例发起（模拟重启），必须回传 PHPSESSID=session_persisted_999
            let (mut conn2, _) = listener.accept().unwrap();
            let mut total2 = Vec::new();
            loop {
                let n = conn2.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                total2.extend_from_slice(&buf[..n]);
                if total2.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let req2 = String::from_utf8_lossy(&total2);
            assert!(
                req2.contains("PHPSESSID=session_persisted_999"),
                "重启后的新 ApiClient 未能回传已持久化的 PHPSESSID cookie！请求为:\n{req2}"
            );
            let body2 = r#"{"error":0,"msg":"上传成功"}"#;
            conn2.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body2}",
                    body2.len()
                )
                .as_bytes(),
            )
            .unwrap();
        });

        // 第一次运行：登录并保存
        let client1 = ApiClient::with_base_url_and_store(&format!("http://{addr}"), Some(store.clone()));
        let login_res = client1.login("alice", "pass").unwrap();
        assert_eq!(login_res.token, "tok-live");
        assert!(client1.is_logged_in());

        // 模拟进程重启：创建全新 ApiClient 实例，仅挂载同一 store
        let client2 = ApiClient::with_base_url_and_store(&format!("http://{addr}"), Some(store));
        assert!(client2.is_logged_in(), "新实例应自动加载会话并判定为已登录");
        assert_eq!(client2.current_token().as_deref(), Some("tok-live"));

        // 发起上传，验证 cookie 成功回传
        let rank = client2
            .upload_result("tok-live", &json!({"speed": 60.0}))
            .expect("使用持久化会话上传应成功");
        assert_eq!(rank.message, "上传成功");

        server.join().unwrap();
        let _ = std::fs::remove_file(store_path);
    }

    #[test]
    fn upload_session_auto_retries_and_returns_outcome() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use crate::TextSource;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            // 请求 1：首次 uploadResult，返回 token 过期错误
            let (mut conn1, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn1.read(&mut buf).unwrap();
            let body1 = r#"{"error":1,"msg":"登录已过期，请重新登录"}"#;
            conn1.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body1}",
                    body1.len()
                )
                .as_bytes(),
            )
            .unwrap();
            drop(conn1);

            // 请求 2：自动重登 login
            let (mut conn2, _) = listener.accept().unwrap();
            let _ = conn2.read(&mut buf).unwrap();
            let body2 = r#"{"error":0,"msg":{"token":"tok-new-auto"}}"#;
            conn2.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nSet-Cookie: PHPSESSID=new_cookie_auto; path=/\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body2}",
                    body2.len()
                )
                .as_bytes(),
            )
            .unwrap();
            drop(conn2);

            // 请求 3：重试 uploadResult，成功并返回排名
            let (mut conn3, _) = listener.accept().unwrap();
            let _ = conn3.read(&mut buf).unwrap();
            let body3 = r#"{"error":0,"msg":{"ranking":3,"rankTips":"第3名"}}"#;
            conn3.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body3}",
                    body3.len()
                )
                .as_bytes(),
            )
            .unwrap();
        });

        unsafe {
            std::env::set_var("DAZITUI_USER", "test_user");
            std::env::set_var("DAZITUI_PASS", "test_pass");
        }

        let client = ApiClient::with_base_url(&format!("http://{addr}"));
        client.set_session(Some(AuthSession::from_token("tok-old-expired")));

        let text = Text {
            title: "极速杯第100期".into(),
            content: "打字练习测试".into(),
            source: TextSource::Online { competition_type: CompetitionType::Jisu },
            word_boundaries: None,
            shuffled: false,
        };
        let stats = Stats {
            wpm: 92.5,
            typed_chars: 6,
            correct_chars: 6,
            wrong_chars: 0,
            edits: 0,
            wrong_total: 0,
            key_frequency: vec![("a".into(), 10)],
            edit_details: Vec::new(),
        };

        let outcome = client.upload_session(&text, &stats, Duration::from_secs(10)).unwrap();
        assert_eq!(outcome.ranking.as_deref(), Some("3"));
        assert!(outcome.share_text.contains("第3名"));
        assert!(outcome.share_text.contains("WPM 92.5"));
        assert_eq!(client.current_token().as_deref(), Some("tok-new-auto"));

        unsafe {
            std::env::remove_var("DAZITUI_USER");
            std::env::remove_var("DAZITUI_PASS");
        }

        server.join().unwrap();
    }

    #[test]
    #[ignore = "requires live 52dazi network access"]
    fn real_gateway_connection() {
        let client = ApiClient::new();
        let res = client.login("invalid_user_test", "invalid_pass_test");
        assert!(matches!(res, Err(ApiError::Server(_))));
    }

    #[test]
    #[ignore = "requires live 52dazi network access"]
    fn real_gateway_get_content() {
        let client = ApiClient::new();
        // 极速杯公开载文
        let res = client.get_content_with_token("", CompetitionType::Jisu);
        assert!(res.is_ok(), "极速杯载文应当成功: {res:?}");
        let text = res.unwrap();
        assert!(!text.content.is_empty(), "极速杯内容不应为空");
        assert_eq!(text.title, "市井人间烟火的生活本真");
    }

    #[test]
    #[ignore = "requires live 52dazi network access"]
    fn real_gateway_upload_test() {
        let client = ApiClient::new();
        println!("Current token: {:?}", client.current_token());
        let text = client.get_content(CompetitionType::Jisu).expect("get_content failed");
        let stats = Stats {
            wpm: 60.0,
            typed_chars: text.content.chars().count(),
            correct_chars: text.content.chars().count(),
            wrong_chars: 0,
            edits: 0,
            wrong_total: 0,
            key_frequency: vec![("a".into(), 100)],
            edit_details: Vec::new(),
        };
        let upload_stats = crate::online::share::to_upload_stats(&stats, Duration::from_secs(60));
        let payload = crate::online::share::build_upload_payload(&Text {
            title: text.title.clone(),
            content: text.content.clone(),
            source: crate::TextSource::Online { competition_type: CompetitionType::Jisu },
            word_boundaries: None,
            shuffled: false,
        }, &stats, &upload_stats, Duration::from_secs(60));
        println!("Generated payload: {}", serde_json::to_string_pretty(&payload).unwrap());

        if let Some(token) = client.current_token() {
            let base_info_body = encrypt_value(&Value::Object(base_fields(Some(&token))));
            let base_info_resp = client.post("Api/System/getBaseInfo", &base_info_body);
            println!("getBaseInfo with token response: {base_info_resp:?}");
            let res = client.upload_result(&token, &payload);
            println!("Upload result with current token: {res:?}");
        }

        let bogus_res = client.upload_result("bogus_123", &payload);
        println!("Upload result with bogus token: {bogus_res:?}");
    }
}




