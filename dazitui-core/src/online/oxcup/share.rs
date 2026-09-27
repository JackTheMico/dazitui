//! 牛杯（ox-cup）打卡分享文本格式化（兼容打字 QQ 群小稽机器人标准格式）。

use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{Stats, Text, format_time};

/// 计算牛杯成绩打卡凭证（SHA-256 前 5 位十六进制）。
pub fn calculate_ox_proof(
    segment: u32,
    speed: f64,
    kps: f64,
    key_length: f64,
    total_chars: usize,
    edits: u32,
    backspace: u32,
) -> String {
    let mut hasher = Sha256::new();
    let msg = format!(
        "{}:{:.2}:{:.2}:{:.2}:{}:{}:{}",
        segment, speed, kps, key_length, total_chars, edits, backspace
    );
    hasher.update(msg.as_bytes());
    let result = hasher.finalize();
    let hex_str = format!("{:x}", result);
    hex_str.chars().take(5).collect()
}

/// 格式化牛杯成绩打卡文本（写入剪贴板供粘贴至 QQ 群）。
///
/// 格式遵循主流打字群机器人（小稽机器人等）正则识别规范：
/// `第666段 速度186.18 击键7.34 码长2.37 键准95.30% 回改14 回车0 退格19 字数141 键数358 时间01:52.357 打词82.71% 凭证xxxxx dazitui`
pub fn format_ox_share_text(text: &Text, stats: &Stats, elapsed: Duration) -> String {
    let segment = 666u32;
    let elapsed_secs = elapsed.as_secs_f64().max(0.001);
    let total_keys: u32 = stats.key_frequency.iter().map(|(_, n)| n).sum();
    let strokes = if stats.total_strokes > 0 {
        stats.total_strokes
    } else {
        total_keys
    };

    let backspace = stats
        .key_frequency
        .iter()
        .find(|(k, _)| k == "Backspace")
        .map(|(_, n)| *n)
        .unwrap_or(stats.edits);

    let kps = if stats.kps > 0.0 {
        stats.kps
    } else {
        strokes as f64 / elapsed_secs
    };

    let key_length = if stats.key_length > 0.0 {
        stats.key_length
    } else if stats.typed_chars > 0 {
        strokes as f64 / stats.typed_chars as f64
    } else {
        0.0
    };

    let wasted_keys = backspace as f64 + (stats.edits as f64) * key_length;
    let accuracy = if strokes == 0 {
        100.0
    } else {
        let valid = (strokes as f64 - wasted_keys).max(0.0);
        ((valid / strokes as f64) * 100.0).clamp(0.0, 100.0)
    };

    let total_chars = text.content.chars().count();
    let word_ratio = if total_chars > 0 {
        (stats.phrase_chars as f64 / total_chars as f64 * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };

    let proof = calculate_ox_proof(
        segment,
        stats.wpm,
        kps,
        key_length,
        total_chars,
        stats.edits,
        backspace,
    );

    format!(
        "第{}段 速度{:.2} 击键{:.2} 码长{:.2} 键准{:.2}% 回改{} 回车0 退格{} 字数{} 键数{} 时间{} 打词{:.2}% 凭证{} dazitui",
        segment,
        stats.wpm,
        kps,
        key_length,
        accuracy,
        stats.edits,
        backspace,
        total_chars,
        strokes,
        format_time(elapsed),
        word_ratio,
        proof
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TextSource;

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
            phrase_chars: 19,
            key_frequency: vec![("a".to_string(), 80), ("Backspace".to_string(), 4)],
            edit_details: vec![],
            speed_samples: vec![],
            kps_samples: vec![],
            error_points: vec![],
        }
    }

    #[test]
    fn format_ox_share_text_contains_bot_fields() {
        let text = Text {
            title: "2026-09-24 牛杯赛文".into(),
            content: "这是一段牛杯测试文本一共十六个字".into(),
            source: TextSource::OxCup,
            word_boundaries: None,
            shuffled: false,
        };
        let stats = sample_stats();
        let share = format_ox_share_text(&text, &stats, Duration::from_secs(30));

        assert!(share.starts_with("第666段 "));
        assert!(share.contains("速度120.50 "));
        assert!(share.contains("击键5.20 "));
        assert!(share.contains("码长2.60 "));
        assert!(share.contains("回改2 "));
        assert!(share.contains("退格4 "));
        assert!(share.contains("字数16 "));
        assert!(share.contains("时间00:30.000 "));
        assert!(share.contains("打词"));
        assert!(share.contains("凭证"));
        assert!(share.ends_with(" dazitui"));
    }
}
