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

/// 比赛赛文（getContent 响应 `msg` 对象）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompetitionText {
    /// 赛文内容（`msg["0"]`）。
    pub content: String,
    /// 赛文标题（`msg["7"]`，极速杯等类型可能缺失）。
    pub title: String,
    /// 作者（`msg["1"]`）。
    pub author: String,
    /// 字数（`msg["6"]`）。
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

/// 解析载文响应：`msg` 为对象，字段 `"0"`=内容、`"1"`=作者、`"6"`=字数、`"7"`=标题。
/// 除内容外其余字段可能缺失（极速杯只返回 `"0"`），缺失时用空串/0 兜底。
pub fn parse_content_response(body: &str) -> Result<CompetitionText, ApiError> {
    let obj: Map<String, Value> = parse_api_response(body)?;
    let content = obj
        .get("0")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let title = obj
        .get("7")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let author = obj
        .get("1")
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
        .unwrap_or(0);
    Ok(CompetitionText {
        content,
        title,
        author,
        word_num,
    })
}

/// 标题为空时用比赛类型名兜底（极速杯不返回标题字段，验收要求「标题显示比赛类型」）。
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
    ///
    /// 网关地址可用 `DAZITUI_BASE_URL` 环境变量覆盖（调试/抓包用，如反向代理）。
    pub fn new() -> Self {
        let base_url =
            std::env::var("DAZITUI_BASE_URL").unwrap_or_else(|_| BASE_URL.to_string());
        Self::with_base_url(&base_url)
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
        Ok(with_title_fallback(
            parse_content_response(&resp)?,
            competition_type,
        ))
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
    fn parse_content_missing_optional_fields_does_not_panic() {
        // 极速杯实测：只返回 "0"（内容），其余字段缺失。
        let body = r#"{"error":0,"msg":{"0":"只有内容"}}"#;
        let r = parse_content_response(body).unwrap();
        assert_eq!(r.content, "只有内容");
        assert_eq!(r.title, "");
        assert_eq!(r.author, "");
        assert_eq!(r.word_num, 0);
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
            title: "锦标赛第3279期".into(),
            author: "".into(),
            word_num: 0,
        };
        let filled = with_title_fallback(text, CompetitionType::Jinbiao);
        assert_eq!(filled.title, "锦标赛第3279期");
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
}
