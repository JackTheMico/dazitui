//! 52dazi.cn 传输层：ureq 同步 HTTP 客户端，封装登录/载文/上传三个 API。
//!
//! 响应解析与请求构造为纯函数（可独立测试），HTTP 调用是薄壳。
//! 协议细节见 ADR-0002：请求体 AES 加密，响应为明文 JSON `{"error":0,"msg":…}`。

use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use super::protocol::encrypt_value;
use crate::CompetitionType;

/// 52dazi 网关根地址。
pub const BASE_URL: &str = "http://www.jsxiaoshi.com/index.php";

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

/// 比赛赛文（getContent 响应 `msg` 数组）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompetitionText {
    /// 赛文内容。
    pub content: String,
    /// 作者。
    pub author: String,
    /// 字数。
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

/// 解析载文响应：`msg` 为数组 `[content, author, …, word_num]`。
pub fn parse_content_response(body: &str) -> Result<CompetitionText, ApiError> {
    let arr: Vec<Value> = parse_api_response(body)?;
    let content = arr
        .first()
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let author = arr.get(1).and_then(Value::as_str).unwrap_or("").to_string();
    let word_num = arr
        .get(6)
        .and_then(|v| {
            v.as_u64()
                .map(|n| n as usize)
                .or_else(|| v.as_str().and_then(|s| s.parse::<usize>().ok()))
        })
        .unwrap_or(0);
    Ok(CompetitionText {
        content,
        author,
        word_num,
    })
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

/// 请求体公共字段：`version` + `subversions` + 可选 `token`。
fn base_fields(token: Option<&str>) -> Map<String, Value> {
    let mut m = Map::new();
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

/// 52dazi 客户端。
#[derive(Debug, Clone)]
pub struct ApiClient {
    agent: ureq::Agent,
    base_url: String,
}

impl Default for ApiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiClient {
    /// 指向 52dazi 网关的客户端（10 秒超时）。
    pub fn new() -> Self {
        Self::with_base_url(BASE_URL)
    }

    /// 指定网关根地址（测试时指向本地 mock）。
    pub fn with_base_url(base_url: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .new_agent();
        Self {
            agent,
            base_url: base_url.to_string(),
        }
    }

    /// 登录：返回 token。
    pub fn login(&self, username: &str, password: &str) -> Result<LoginResult, ApiError> {
        let body = login_payload(username, password);
        let resp = self.post("Api/User/login", &body)?;
        parse_login_response(&resp)
    }

    /// 按比赛类型载入赛文（需登录）。
    pub fn get_content(
        &self,
        token: &str,
        competition_type: CompetitionType,
    ) -> Result<CompetitionText, ApiError> {
        let body = content_payload(token, competition_type);
        let resp = self.post("Api/Text/getContent", &body)?;
        parse_content_response(&resp)
    }

    /// 上传成绩（`payload` 为业务字段，公共字段与 token 自动合并）。
    pub fn upload_result(&self, token: &str, payload: &Value) -> Result<RankResult, ApiError> {
        let body = upload_payload(token, payload);
        let resp = self.post("Api/Rank/uploadResult", &body)?;
        parse_upload_response(&resp)
    }

    /// 发送加密请求体，返回响应文本。
    fn post(&self, path: &str, body: &str) -> Result<String, ApiError> {
        let url = format!("{}/{path}", self.base_url);
        let mut resp = self
            .agent
            .post(&url)
            .send(body)
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        resp.body_mut()
            .read_to_string()
            .map_err(|e| ApiError::Transport(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 响应解析 ----

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
    fn parse_content_extracts_content_author_word_num() {
        // msg 数组结构来自前端逆向：[0]=内容 [1]=作者 [6]=字数
        let body = r#"{"error":0,"msg":["你好世界","作者甲",null,"x",null,null,"12"]}"#;
        let r = parse_content_response(body).unwrap();
        assert_eq!(r.content, "你好世界");
        assert_eq!(r.author, "作者甲");
        assert_eq!(r.word_num, 12);
    }

    #[test]
    fn parse_content_short_array_does_not_panic() {
        let body = r#"{"error":0,"msg":["只有内容"]}"#;
        let r = parse_content_response(body).unwrap();
        assert_eq!(r.content, "只有内容");
        assert_eq!(r.author, "");
        assert_eq!(r.word_num, 0);
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
}
