//! 虎码杯（race.tiger-code.com）数据指标折算、Payload 构建与剪贴板分享格式化。

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{Stats, Text, format_time};

/// 虎码杯上传成绩的指标载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TigerScorePayload {
    /// 速度（WPM）。
    pub speed: f64,
    /// 击键（KPS，按键数/秒）。
    pub keystrokes: f64,
    /// 码长（总击键/上屏字数）。
    #[serde(rename = "codeLength")]
    pub code_length: f64,
    /// 用时（秒数浮点）。
    pub time: f64,
    /// 回改次数。
    pub corrections: u32,
    /// 总按键数。
    #[serde(rename = "keyCount")]
    pub key_count: u32,
    /// 键准（0.0 - 100.0 百分比浮点）。
    #[serde(rename = "keyAccuracy")]
    pub key_accuracy: f64,
    /// 打词率（0.0 - 1.0 比例浮点）。
    #[serde(rename = "wordRatio")]
    pub word_ratio: f64,
    /// 退格键按键数。
    #[serde(rename = "backsCount")]
    pub backs_count: u32,
    /// 参赛输入法名称。
    #[serde(rename = "inputMethod")]
    pub input_method: String,
    /// 赛文标题。
    #[serde(rename = "userArticle")]
    pub user_article: String,
    /// 按键日志（防作弊审计字段，可为空串）。
    #[serde(default)]
    pub key_log: String,
}

/// 根据本地跟打结果构建虎码杯成绩载荷。
pub fn build_tiger_payload(
    text: &Text,
    stats: &Stats,
    elapsed: Duration,
    input_method: &str,
) -> TigerScorePayload {
    let elapsed_secs = elapsed.as_secs_f64().max(0.001);
    let total_keys: u32 = stats.key_frequency.iter().map(|(_, n)| n).sum();
    let strokes = if stats.total_strokes > 0 {
        stats.total_strokes
    } else {
        total_keys
    };

    let backs_count = stats
        .key_frequency
        .iter()
        .find(|(k, _)| k == "Backspace")
        .map(|(_, n)| *n)
        .unwrap_or(stats.edits);

    let keystrokes = if stats.kps > 0.0 {
        stats.kps
    } else {
        strokes as f64 / elapsed_secs
    };

    let code_length = if stats.key_length > 0.0 {
        stats.key_length
    } else if stats.typed_chars > 0 {
        strokes as f64 / stats.typed_chars as f64
    } else {
        0.0
    };

    // 键准计算：(有效击数 / 总击数) * 100%
    let wasted_keys = backs_count as f64 + (stats.edits as f64) * code_length;
    let key_accuracy = if strokes == 0 {
        100.0
    } else {
        let valid_keys = (strokes as f64 - wasted_keys).max(0.0);
        ((valid_keys / strokes as f64) * 100.0).clamp(0.0, 100.0)
    };

    // 打词率计算：打词字符数 / 赛文总字数
    let total_chars = text.content.chars().count();
    let word_ratio = if total_chars > 0 {
        (stats.phrase_chars as f64 / total_chars as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let effective_input_method = if input_method.trim().is_empty() {
        "虎码"
    } else {
        input_method.trim()
    };

    TigerScorePayload {
        speed: (stats.wpm * 100.0).round() / 100.0,
        keystrokes: (keystrokes * 100.0).round() / 100.0,
        code_length: (code_length * 100.0).round() / 100.0,
        time: (elapsed_secs * 100.0).round() / 100.0,
        corrections: stats.edits,
        key_count: strokes,
        key_accuracy: (key_accuracy * 100.0).round() / 100.0,
        word_ratio: (word_ratio * 10000.0).round() / 10000.0,
        backs_count,
        input_method: effective_input_method.to_string(),
        user_article: text.title.clone(),
        key_log: String::new(),
    }
}

/// 格式化虎码杯分享文本（写入剪贴板）。
pub fn format_tiger_share_text(
    payload: &TigerScorePayload,
    elapsed: Duration,
    tier: Option<&str>,
    rank: Option<u32>,
    total_participants: Option<u32>,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!("【虎码杯】{}", payload.user_article));

    let mut meta = Vec::new();
    if let Some(t) = tier && !t.is_empty() {
        meta.push(format!("段位: {t}"));
    }
    if let Some(r) = rank {
        if let Some(total) = total_participants {
            meta.push(format!("第 {r} 名 / 共 {total} 人"));
        } else {
            meta.push(format!("第 {r} 名"));
        }
    }
    if !meta.is_empty() {
        lines.push(meta.join("  "));
    }

    lines.push(format!(
        "速度: {:.2} WPM | 击键: {:.2} | 码长: {:.2}",
        payload.speed, payload.keystrokes, payload.code_length
    ));
    lines.push(format!(
        "用时: {} | 键数: {} | 键准: {:.2}%",
        format_time(elapsed),
        payload.key_count,
        payload.key_accuracy
    ));
    lines.push(format!(
        "打词: {:.2}% | 回改: {} | 退格: {}",
        payload.word_ratio * 100.0,
        payload.corrections,
        payload.backs_count
    ));
    lines.push(format!("输入法: {}", payload.input_method));
    lines.push("—— 由 dazitui 强力驱动".to_string());

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stats() -> Stats {
        Stats {
            wpm: 120.5,
            kps: 5.2,
            key_length: 2.6,
            total_strokes: 100,
            correct_chars: 38,
            wrong_chars: 0,
            edits: 2,
            wrong_total: 2,
            typed_chars: 38,
            phrase_chars: 5,
            key_frequency: vec![("a".to_string(), 80), ("b".to_string(), 20)],
            edit_details: vec![],
            speed_samples: vec![],
            kps_samples: vec![],
            error_points: vec![],
        }
    }

    #[test]
    fn tiger_payload_and_share_formatting() {
        let text = Text {
            title: "2026-09-15 每日赛文".into(),
            content: "生稿测试正文共十个汉字".into(),
            source: crate::TextSource::TigerCup,
            word_boundaries: None,
            shuffled: false,
        };
        let stats = sample_stats();

        let payload = build_tiger_payload(&text, &stats, Duration::from_secs(30), "虎码");

        assert_eq!(payload.speed, 120.5);
        assert_eq!(payload.keystrokes, 5.2);
        assert_eq!(payload.code_length, 2.6);
        assert_eq!(payload.corrections, 2);
        assert_eq!(payload.input_method, "虎码");
        assert_eq!(payload.user_article, "2026-09-15 每日赛文");

        let share = format_tiger_share_text(
            &payload,
            Duration::from_secs(30),
            Some("💎钻石·4"),
            Some(3),
            Some(50),
        );
        assert!(share.contains("【虎码杯】2026-09-15 每日赛文"));
        assert!(share.contains("段位: 💎钻石·4  第 3 名 / 共 50 人"));
        assert!(share.contains("速度: 120.50 WPM"));
        assert!(share.contains("输入法: 虎码"));
    }
}
