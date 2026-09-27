//! 牛杯（ox-cup）赛文拉取与排行榜客户端实现。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::online::client::{ApiError, today_ymd};

/// 牛杯赛文 API 根地址。
pub const OX_CUP_ARTICLE_BASE_URL: &str = "http://tiger-code.com:8000";

/// 牛杯排行榜 API 根地址（小稽机器人汇聚接口）。
pub const OX_CUP_SCORE_BASE_URL: &str = "http://123.60.58.190:6190";

/// 牛杯赛文内容数据模型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OxCupArticle {
    /// 赛文 ID。
    pub id: u64,
    /// 赛文标题，形如 `YYYY-MM-DD 牛杯赛文`。
    pub title: String,
    /// 赛文正文内容。
    pub content: String,
    /// 期次日期，形如 `YYYY-MM-DD`。
    pub date: String,
    /// 字符总数。
    pub length: usize,
    /// 赛文哈希校验码。
    pub hash: String,
}

/// 牛杯排行榜单条成绩记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OxCupScoreEntry {
    /// 排名（1-based）。
    pub rank: u32,
    /// 用户 ID（通常为 QQ 号）。
    pub user_id: String,
    /// 用户昵称。
    pub user_name: String,
    /// 跟打速度（WPM）。
    pub speed: f64,
    /// 击键速度（KPS）。
    pub hit_rate: f64,
    /// 码长（总键数 / 字数）。
    pub kpw: f64,
    /// 键准（百分比，0.0 - 100.0）。
    pub accuracy: f64,
    /// 用时文本，形如 `00:39.17` 或 `36.966`。
    pub time: Option<String>,
    /// 回改次数。
    pub corrections: u32,
    /// 打词率（百分比，0.0 - 100.0）。
    pub word_ratio: Option<f64>,
    /// 打卡来源 QQ 群名称（如 `聚贤阁`、`百鹤岛`）。
    pub group_name: String,
    /// 打卡来源 QQ 群号。
    pub group_id: Option<String>,
    /// 打卡入库时间戳（ISO 8601）。
    pub timestamp: Option<String>,
}

/// 解析牛杯赛文 JSON 响应。
pub fn parse_ox_cup_article_response(body: &str) -> Result<OxCupArticle, ApiError> {
    let json: Value = serde_json::from_str(body).map_err(|e| ApiError::Parse(e.to_string()))?;
    let code = json.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
    if code != 1 {
        let msg = json
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("获取赛文失败");
        return Err(ApiError::Server(msg.to_string()));
    }

    let data = json
        .get("data")
        .ok_or_else(|| ApiError::Parse("缺少 data 字段".to_string()))?;

    let id = data.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
    let title = data
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let content = data
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Parse("缺少 content 字段".to_string()))?
        .to_string();
    let date = data
        .get("date")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let length = data
        .get("length")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or_else(|| content.chars().count());
    let hash = data
        .get("hash")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(OxCupArticle {
        id,
        title,
        content,
        date,
        length,
        hash,
    })
}

/// 解析牛杯排行榜 JSON 响应。
pub fn parse_ox_cup_leaderboard_response(body: &str) -> Result<Vec<OxCupScoreEntry>, ApiError> {
    let json: Value = serde_json::from_str(body).map_err(|e| ApiError::Parse(e.to_string()))?;
    let status = json.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "success" {
        let msg = json
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("获取排行榜失败");
        return Err(ApiError::Server(msg.to_string()));
    }

    let scores = json
        .get("data")
        .and_then(|d| d.get("scores"))
        .and_then(|s| s.as_array())
        .ok_or_else(|| ApiError::Parse("缺少 scores 列表".to_string()))?;

    let mut entries = Vec::with_capacity(scores.len());
    for (idx, item) in scores.iter().enumerate() {
        let user_id = item
            .get("user_id")
            .and_then(|v| {
                if let Some(s) = v.as_str() {
                    Some(s.to_string())
                } else {
                    v.as_i64().map(|n| n.to_string())
                }
            })
            .unwrap_or_default();
        let user_name = item
            .get("user_name")
            .and_then(|v| v.as_str())
            .unwrap_or("未知玩家")
            .trim()
            .to_string();
        let speed = item.get("speed").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let hit_rate = item
            .get("hit_rate")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let kpw = item.get("kpw").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let accuracy = item
            .get("accuracy")
            .and_then(|v| v.as_f64())
            .unwrap_or(100.0);
        let time = item
            .get("time")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let corrections = item
            .get("corrections")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let word_ratio = item.get("word_ratio").and_then(|v| v.as_f64());
        let group_name = item
            .get("group_name")
            .and_then(|v| v.as_str())
            .unwrap_or("牛杯群")
            .to_string();
        let group_id = item
            .get("group_id")
            .and_then(|v| {
                if let Some(s) = v.as_str() {
                    Some(s.to_string())
                } else {
                    v.as_i64().map(|n| n.to_string())
                }
            });
        let timestamp = item
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        entries.push(OxCupScoreEntry {
            rank: (idx + 1) as u32,
            user_id,
            user_name,
            speed,
            hit_rate,
            kpw,
            accuracy,
            time,
            corrections,
            word_ratio,
            group_name,
            group_id,
            timestamp,
        });
    }

    Ok(entries)
}

/// 牛杯（ox-cup）HTTP 客户端。
#[derive(Debug, Clone)]
pub struct OxCupClient {
    agent: ureq::Agent,
    article_base_url: String,
    score_base_url: String,
}

impl Default for OxCupClient {
    fn default() -> Self {
        Self::new()
    }
}

impl OxCupClient {
    /// 创建指向默认线上网关的牛杯客户端。
    pub fn new() -> Self {
        let article_base = std::env::var("OX_CUP_ARTICLE_BASE_URL")
            .unwrap_or_else(|_| OX_CUP_ARTICLE_BASE_URL.to_string());
        let score_base = std::env::var("OX_CUP_SCORE_BASE_URL")
            .unwrap_or_else(|_| OX_CUP_SCORE_BASE_URL.to_string());
        Self::with_base_urls(&article_base, &score_base)
    }

    /// 指定自定义端点（测试或镜像用）。
    pub fn with_base_urls(article_base: &str, score_base: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .new_agent();
        Self {
            agent,
            article_base_url: article_base.trim_end_matches('/').to_string(),
            score_base_url: score_base.trim_end_matches('/').to_string(),
        }
    }

    /// 获取今日牛杯赛文（自动使用当前北京时间 YYYY-MM-DD）。
    pub fn get_today_article(&self) -> Result<OxCupArticle, ApiError> {
        let date = today_ymd();
        self.get_article(&date)
    }

    /// 获取指定日期的牛杯赛文。
    pub fn get_article(&self, date: &str) -> Result<OxCupArticle, ApiError> {
        let url = format!("{}/ox-cup/article/{}", self.article_base_url, date);
        let resp = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        let body = resp
            .into_body()
            .read_to_string()
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        parse_ox_cup_article_response(&body)
    }

    /// 获取指定日期的牛杯排行榜。
    pub fn get_leaderboard(&self, date: &str) -> Result<Vec<OxCupScoreEntry>, ApiError> {
        let url = format!("{}/api/nb_scores?date={}", self.score_base_url, date);
        let resp = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        let body = resp
            .into_body()
            .read_to_string()
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        parse_ox_cup_leaderboard_response(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_article_response_success() {
        let body = r#"{
            "code": 1,
            "message": "success",
            "data": {
                "id": 223,
                "title": "2026-09-24 牛杯赛文",
                "content": "公司年会上，老板激动地抽取特等奖。",
                "date": "2026-09-24",
                "length": 18,
                "hash": "d7bbd30c"
            }
        }"#;
        let article = parse_ox_cup_article_response(body).expect("应成功解析赛文");
        assert_eq!(article.id, 223);
        assert_eq!(article.title, "2026-09-24 牛杯赛文");
        assert_eq!(article.content, "公司年会上，老板激动地抽取特等奖。");
        assert_eq!(article.date, "2026-09-24");
        assert_eq!(article.length, 18);
        assert_eq!(article.hash, "d7bbd30c");
    }

    #[test]
    fn parse_article_response_error_code() {
        let body = r#"{"code": 0, "message": "日期格式应为 YYYY-MM-DD", "data": null}"#;
        let err = parse_ox_cup_article_response(body).unwrap_err();
        assert!(matches!(err, ApiError::Server(msg) if msg.contains("日期格式")));
    }

    #[test]
    fn parse_leaderboard_response_success() {
        let body = r#"{
            "status": "success",
            "message": "获取成功",
            "data": {
                "date": "2026-09-24",
                "count": 2,
                "scores": [
                    {
                        "user_id": "1311804709",
                        "user_name": "小铭同学",
                        "speed": 283.18,
                        "hit_rate": 9.04,
                        "kpw": 1.91,
                        "accuracy": 97.87,
                        "time": "00:42.50",
                        "corrections": 2,
                        "word_ratio": 50.0,
                        "group_name": "聚贤阁",
                        "group_id": "12941287",
                        "timestamp": "2026-09-24T11:26:06.931779"
                    },
                    {
                        "user_id": 1543953053,
                        "user_name": "靓仔",
                        "speed": 273.87,
                        "hit_rate": 8.68,
                        "kpw": 1.9,
                        "accuracy": null,
                        "time": null,
                        "corrections": null,
                        "word_ratio": null,
                        "group_name": "百鹤岛",
                        "group_id": 835527851,
                        "timestamp": null
                    }
                ]
            }
        }"#;
        let list = parse_ox_cup_leaderboard_response(body).expect("应成功解析排行榜");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].rank, 1);
        assert_eq!(list[0].user_name, "小铭同学");
        assert_eq!(list[0].group_name, "聚贤阁");
        assert_eq!(list[0].speed, 283.18);
        assert_eq!(list[0].accuracy, 97.87);
        assert_eq!(list[0].corrections, 2);

        assert_eq!(list[1].rank, 2);
        assert_eq!(list[1].user_name, "靓仔");
        assert_eq!(list[1].user_id, "1543953053");
        assert_eq!(list[1].group_name, "百鹤岛");
        assert_eq!(list[1].accuracy, 100.0);
        assert_eq!(list[1].corrections, 0);
    }
}
