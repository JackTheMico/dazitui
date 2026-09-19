//! 虎码杯（race.tiger-code.com）HTTP 客户端实现。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::share::TigerScorePayload;
use super::token::{TigerCredentials, TigerTokenStore};
use crate::online::client::{ApiError, CompetitionText};

/// 官方线上比赛网关根地址。
pub const TIGER_BASE_URL: &str = "https://race.tiger-code.com";

/// 虎码杯登录响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TigerLoginResult {
    pub token: String,
    pub user_id: u64,
    pub username: String,
}

/// 虎码杯上传响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TigerUploadResponse {
    pub success: bool,
    pub message: String,
    #[serde(default)]
    pub jingyan: Option<i64>,
}

/// 虎码杯每日排行榜条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TigerLeaderboardEntry {
    pub rank: u32,
    pub username: String,
    pub speed: f64,
    pub hit_rate: f64,
    pub kpw: f64,
    pub time: f64,
    pub accuracy: f64,
    #[serde(default)]
    pub input_method: String,
    #[serde(default)]
    pub word_ratio: f64,
    #[serde(default)]
    pub total_keys: u32,
    #[serde(default)]
    pub correction_count: u32,
    #[serde(default)]
    pub weighted_speed: f64,
    #[serde(default)]
    pub tier: String,
}

/// 虎码杯 API 客户端。
#[derive(Debug, Clone)]
pub struct TigerCupClient {
    agent: ureq::Agent,
    base_url: String,
    creds_store: Option<TigerTokenStore>,
    cached_creds: Arc<Mutex<Option<TigerCredentials>>>,
}

impl TigerCupClient {
    /// 创建指向默认网关的客户端。
    pub fn new() -> Self {
        let store = TigerTokenStore::with_default_path();
        let base_url = std::env::var("TIGER_BASE_URL").unwrap_or_else(|_| TIGER_BASE_URL.to_string());
        Self::with_base_url_and_store(&base_url, Some(store))
    }

    /// 指定网关地址（测试用）。
    pub fn with_base_url(base_url: &str) -> Self {
        Self::with_base_url_and_store(base_url, None)
    }

    /// 指定存储。
    pub fn with_store(store: TigerTokenStore) -> Self {
        let base_url = std::env::var("TIGER_BASE_URL").unwrap_or_else(|_| TIGER_BASE_URL.to_string());
        Self::with_base_url_and_store(&base_url, Some(store))
    }

    /// 指定网关地址与凭据存储。
    pub fn with_base_url_and_store(base_url: &str, store: Option<TigerTokenStore>) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(12)))
            .build()
            .new_agent();
        let initial_creds = store.as_ref().and_then(|s| s.load().ok().flatten());
        Self {
            agent,
            base_url: base_url.trim_end_matches('/').to_string(),
            creds_store: store,
            cached_creds: Arc::new(Mutex::new(initial_creds)),
        }
    }

    /// 当前是否已保存凭据。
    pub fn is_logged_in(&self) -> bool {
        self.current_credentials().is_some()
    }

    /// 获取当前凭据。
    pub fn current_credentials(&self) -> Option<TigerCredentials> {
        self.cached_creds.lock().ok().and_then(|c| c.clone())
    }

    /// 更新并持久化凭据。
    pub fn set_credentials(&self, creds: Option<TigerCredentials>) {
        if let Ok(mut lock) = self.cached_creds.lock() {
            *lock = creds.clone();
        }
        if let Some(ref store) = self.creds_store {
            if let Some(ref c) = creds {
                let _ = store.save(c);
            } else {
                let _ = store.clear();
            }
        }
    }

    /// 登出。
    pub fn logout(&self) {
        self.set_credentials(None);
    }

    /// 登录验证并获取 JWT token。
    pub fn login(&self, username: &str, password: &str) -> Result<TigerLoginResult, ApiError> {
        let url = format!("{}/api/auth/login", self.base_url);
        let payload = serde_json::json!({
            "username": username,
            "password": password,
        });

        let resp_str = self.post_json(&url, &payload)?;
        let val: Value = serde_json::from_str(&resp_str)
            .map_err(|e| ApiError::Parse(format!("JSON 解析失败: {e}")))?;

        if val.get("success").and_then(Value::as_bool) == Some(true) {
            let token = val.get("token").and_then(Value::as_str).unwrap_or("").to_string();
            let user_obj = val.get("user");
            let user_id = user_obj
                .and_then(|u| u.get("id"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let uname = user_obj
                .and_then(|u| u.get("username"))
                .and_then(Value::as_str)
                .unwrap_or(username)
                .to_string();

            let creds = TigerCredentials {
                username: uname.clone(),
                password: password.to_string(),
                token: Some(token.clone()),
                user_id: Some(user_id),
            };
            self.set_credentials(Some(creds));

            Ok(TigerLoginResult {
                token,
                user_id,
                username: uname,
            })
        } else {
            let msg = val
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("登录失败，请检查账号密码");
            Err(ApiError::Server(msg.to_string()))
        }
    }

    /// 获取当日比赛赛文（生稿赛，每天每账号限拉取一次）。
    pub fn fetch_daily_article(&self) -> Result<CompetitionText, ApiError> {
        let creds = self
            .current_credentials()
            .ok_or_else(|| ApiError::Server("请先登录虎码杯账号".to_string()))?;
        self.fetch_daily_article_with_creds(&creds.username, &creds.password)
    }

    /// 使用指定账号密码获取当日比赛赛文。
    pub fn fetch_daily_article_with_creds(
        &self,
        username: &str,
        password: &str,
    ) -> Result<CompetitionText, ApiError> {
        let url = format!("{}/api/daily-article", self.base_url);
        let payload = serde_json::json!({
            "username": username,
            "password": password,
        });

        let resp_str = self.post_json(&url, &payload)?;
        parse_tiger_article_response(&resp_str)
    }

    /// 上传成绩。
    pub fn upload_score(&self, payload: &TigerScorePayload) -> Result<TigerUploadResponse, ApiError> {
        let creds = self
            .current_credentials()
            .ok_or_else(|| ApiError::Server("请先登录虎码杯账号".to_string()))?;
        self.upload_score_with_creds(&creds.username, &creds.password, payload)
    }

    /// 使用指定账号密码上传成绩。
    pub fn upload_score_with_creds(
        &self,
        username: &str,
        password: &str,
        payload: &TigerScorePayload,
    ) -> Result<TigerUploadResponse, ApiError> {
        let url = format!("{}/api/upload-score", self.base_url);
        let mut map = match serde_json::to_value(payload) {
            Ok(Value::Object(m)) => m,
            _ => return Err(ApiError::Parse("序列化成绩载荷失败".into())),
        };
        map.insert("username".to_string(), Value::String(username.to_string()));
        map.insert("password".to_string(), Value::String(password.to_string()));

        let resp_str = self.post_json(&url, &Value::Object(map))?;
        let val: Value = serde_json::from_str(&resp_str)
            .map_err(|e| ApiError::Parse(format!("成绩上传响应不是合法 JSON: {e}")))?;

        if val.get("success").and_then(Value::as_bool) == Some(true) {
            let jingyan = val.get("jingyan").and_then(Value::as_i64);
            Ok(TigerUploadResponse {
                success: true,
                message: "成绩上传成功".to_string(),
                jingyan,
            })
        } else {
            let msg = val
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("成绩上传失败");
            Err(ApiError::Server(msg.to_string()))
        }
    }

    /// 查询指定日期的每日排行榜（格式：YYYY-MM-DD，无需登录）。
    pub fn get_leaderboard(
        &self,
        date: &str,
        limit: usize,
    ) -> Result<Vec<TigerLeaderboardEntry>, ApiError> {
        let url = format!(
            "{}/api/leaderboard/date/{}?limit={}",
            self.base_url, date, limit
        );
        let resp_str = self.get_json(&url)?;
        parse_tiger_leaderboard_response(&resp_str)
    }

    /// 查询历史某天的赛文（格式：YYYY-MM-DD）。
    pub fn get_historical_article(&self, date: &str) -> Result<CompetitionText, ApiError> {
        let url = format!("{}/api/daily-article/date/{}", self.base_url, date);
        let resp_str = self.get_json(&url)?;
        parse_tiger_article_response(&resp_str)
    }

    fn post_json(&self, url: &str, body: &Value) -> Result<String, ApiError> {
        let body_str = serde_json::to_string(body)
            .map_err(|e| ApiError::Parse(format!("序列化请求失败: {e}")))?;
        let req = self
            .agent
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");

        let mut resp = req
            .send(body_str.as_str())
            .map_err(|e| ApiError::Transport(format!("网络请求失败 ({url}): {e}")))?;

        resp.body_mut()
            .read_to_string()
            .map_err(|e| ApiError::Transport(format!("读取响应失败: {e}")))
    }

    fn get_json(&self, url: &str) -> Result<String, ApiError> {
        let req = self
            .agent
            .get(url)
            .header("Accept", "application/json");

        let mut resp = req
            .call()
            .map_err(|e| ApiError::Transport(format!("网络请求失败 ({url}): {e}")))?;

        resp.body_mut()
            .read_to_string()
            .map_err(|e| ApiError::Transport(format!("读取响应失败: {e}")))
    }
}


/// 解析虎码杯赛文响应。
pub fn parse_tiger_article_response(body: &str) -> Result<CompetitionText, ApiError> {
    let val: Value = serde_json::from_str(body)
        .map_err(|e| ApiError::Parse(format!("赛文响应不是有效 JSON: {e}")))?;

    if val.get("success").and_then(Value::as_bool) == Some(false) {
        let msg = val
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("获取赛文失败");
        return Err(ApiError::Server(msg.to_string()));
    }

    let data = val.get("data").unwrap_or(&val);
    let title = data
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("虎码杯赛文")
        .to_string();
    let content = data
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    if content.is_empty() {
        return Err(ApiError::Parse("赛文内容为空".to_string()));
    }

    let word_num = content.chars().count();
    Ok(CompetitionText {
        content,
        title,
        author: "虎码杯官方".to_string(),
        word_num,
    })
}

/// 解析虎码杯排行榜响应。
pub fn parse_tiger_leaderboard_response(body: &str) -> Result<Vec<TigerLeaderboardEntry>, ApiError> {
    let val: Value = serde_json::from_str(body)
        .map_err(|e| ApiError::Parse(format!("排行榜响应不是有效 JSON: {e}")))?;

    let lb_val = val
        .get("data")
        .and_then(|d| d.get("leaderboard"))
        .or_else(|| val.get("leaderboard"));

    let Some(arr) = lb_val.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    let mut entries = Vec::with_capacity(arr.len());
    for item in arr {
        let rank = item.get("rank").and_then(Value::as_u64).unwrap_or(0) as u32;
        let username = item
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let speed = item.get("speed").and_then(Value::as_f64).unwrap_or(0.0);
        let hit_rate = item.get("hit_rate").and_then(Value::as_f64).unwrap_or(0.0);
        let kpw = item.get("kpw").and_then(Value::as_f64).unwrap_or(0.0);
        let time = item.get("time").and_then(Value::as_f64).unwrap_or(0.0);
        let accuracy = item.get("accuracy").and_then(Value::as_f64).unwrap_or(0.0);
        let input_method = item
            .get("input_method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let word_ratio = item.get("word_ratio").and_then(Value::as_f64).unwrap_or(0.0);
        let total_keys = item.get("total_keys").and_then(Value::as_u64).unwrap_or(0) as u32;
        let correction_count = item
            .get("correction_count")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let weighted_speed = item
            .get("weighted_speed")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let tier = item
            .get("tier")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        entries.push(TigerLeaderboardEntry {
            rank,
            username,
            speed,
            hit_rate,
            kpw,
            time,
            accuracy,
            input_method,
            word_ratio,
            total_keys,
            correction_count,
            weighted_speed,
            tier,
        });
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_article_response_success() {
        let json = r#"{"success":true,"data":{"title":"2026-09-15 每日赛文","content":"床前明月光"}}"#;
        let article = parse_tiger_article_response(json).unwrap();
        assert_eq!(article.title, "2026-09-15 每日赛文");
        assert_eq!(article.content, "床前明月光");
        assert_eq!(article.word_num, 5);
    }

    #[test]
    fn parse_article_response_rate_limit_error() {
        let json = r#"{"success":false,"message":"您今天已经获取过文章了，请明天再来"}"#;
        let err = parse_tiger_article_response(json).unwrap_err();
        assert_eq!(
            err,
            ApiError::Server("您今天已经获取过文章了，请明天再来".to_string())
        );
    }

    #[test]
    fn parse_leaderboard_response_success() {
        let json = r#"{
            "success": true,
            "data": {
                "leaderboard": [
                    {
                        "rank": 1,
                        "username": "摸鱼侠",
                        "speed": 186.18,
                        "hit_rate": 7.34,
                        "kpw": 2.37,
                        "time": 189.817,
                        "accuracy": 95.3,
                        "input_method": "虎码",
                        "word_ratio": 0.5427,
                        "total_keys": 1394,
                        "correction_count": 14,
                        "weighted_speed": 178.71,
                        "tier": "💎钻石·4"
                    }
                ]
            }
        }"#;
        let lb = parse_tiger_leaderboard_response(json).unwrap();
        assert_eq!(lb.len(), 1);
        let entry = &lb[0];
        assert_eq!(entry.rank, 1);
        assert_eq!(entry.username, "摸鱼侠");
        assert_eq!(entry.speed, 186.18);
        assert_eq!(entry.hit_rate, 7.34);
        assert_eq!(entry.kpw, 2.37);
        assert_eq!(entry.tier, "💎钻石·4");
    }
}
