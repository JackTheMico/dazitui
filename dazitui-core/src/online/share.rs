//! 52dazi.cn 上传字段映射与分享文本格式化。

use std::time::Duration;

use base64::Engine;
use serde_json::Value;

use crate::{Stats, Text, TextSource};

/// 52dazi.cn 上传字段。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UploadStats {
    /// speed：WPM（每分钟正确字数）。
    pub speed: f64,
    /// keystrokes：击键（每秒按键次数）。
    pub keystrokes: f64,
    /// key_length：码长（总按键数 / 已上屏字数）。
    pub key_length: f64,
}

/// 把本地 `Stats` 映射为 52dazi.cn 上传字段。
pub fn to_upload_stats(stats: &Stats, elapsed: Duration) -> UploadStats {
    let total_keys: u32 = stats.key_frequency.iter().map(|(_, n)| n).sum();
    let keystrokes = if elapsed.is_zero() {
        0.0
    } else {
        total_keys as f64 / elapsed.as_secs_f64()
    };
    let key_length = if stats.typed_chars == 0 {
        0.0
    } else {
        total_keys as f64 / stats.typed_chars as f64
    };
    UploadStats {
        speed: stats.wpm,
        keystrokes,
        key_length,
    }
}

/// 分享文本：`极速杯 第5名 · WPM 85.2 · 击键 3.5 · 码长 2.8`。
///
/// `rank` 为 `None` 时省略排名（如离线赛文）。
pub fn format_share_text(source: &TextSource, rank: Option<u32>, stats: &UploadStats) -> String {
    let name = match source {
        TextSource::File => "本地",
        TextSource::Online { competition_type } => competition_type.name(),
    };
    let rank_part = rank.map(|r| format!(" 第{r}名")).unwrap_or_default();
    format!(
        "{name}{rank_part} · WPM {:.1} · 击键 {:.1} · 码长 {:.1}",
        stats.speed, stats.keystrokes, stats.key_length
    )
}

/// 用时格式化为 `MM:SS.sss`（与前端 `formatTime` 一致，秒保留 3 位小数）。
pub fn format_time(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    let minutes = (secs / 60.0).floor() as u64;
    let seconds = secs - (minutes as f64) * 60.0;
    format!("{minutes:02}:{seconds:06.3}")
}

/// 构造 52dazi.cn 上传请求体（业务字段，公共字段由 `client.upload_result` 自动合并）。
///
/// 字段名与前端 `resultPostData`（app.js 中的 vuex getter）逐项对齐：
/// dazitui 不采集的字段用与前端一致的兜底值——`repeatNum`/`xuanChong`=0、
/// `daCi`/`keyMethod`="0%"、`inputMethod`/`challengeFlag`/`isFirstSubmit`/`isGroupText`
/// 沿用前端默认 0 / 空串。可采集字段：
/// - `jianZhun`（击准率）= 正确字数 / 已上屏字数，百分比字符串（与前端 `e.accuracy+"%"` 同构）；
/// - `wrongNum`/`jianShu`/`backspace`/`huiGai` 仍来自 `Stats`。
///
/// 缺字段会让服务端字段对齐校验失败（错误信息可表现为 token/username 解析异常），
/// 故即便本端无法采集某些指标，也按前端 schema 输出兜底值。
pub fn build_upload_payload(
    text: &Text,
    stats: &Stats,
    upload: &UploadStats,
    elapsed: Duration,
) -> Value {
    let total_keys: u32 = stats.key_frequency.iter().map(|(_, n)| n).sum();
    let backspace: u32 = stats
        .key_frequency
        .iter()
        .find(|(k, _)| k == "Backspace")
        .map(|(_, n)| *n)
        .unwrap_or(0);
    // 击准率 = 正确字数 / 已上屏字数 * 100（与前端 accuracy 同口径）。无上屏时记 0。
    let accuracy_pct = if stats.typed_chars == 0 {
        0.0
    } else {
        stats.correct_chars as f64 / stats.typed_chars as f64 * 100.0
    };
    serde_json::json!({
        "textTitle": text.title,
        "speed": upload.speed,
        "keystrokes": upload.keystrokes,
        "maChang": upload.key_length,
        "wordNum": text.content.chars().count(),
        "typingTime": format_time(elapsed),
        "huiGai": stats.edits,
        "huiChe": 0,
        "jianShu": total_keys,
        "jianZhun": format!("{:.2}%", accuracy_pct),
        "repeatNum": 0,
        "daCi": "0%",
        "wrongNum": stats.wrong_total,
        "inputMethod": "",
        "backspace": backspace,
        "xuanChong": 0,
        "keyMethod": "0%",
        "challengeFlag": 0,
        "isFirstSubmit": 0,
        "isGroupText": 0,
    })
}

/// 生成 OSC 52 剪贴板写入序列（终端收到后把 `text` 写入系统剪贴板）。
pub fn osc52_clipboard(text: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CompetitionType;

    fn sample_stats() -> Stats {
        Stats {
            wpm: 85.2,
            correct_chars: 40,
            wrong_chars: 2,
            edits: 1,
            wrong_total: 3,
            typed_chars: 40,
            key_frequency: vec![("a".to_string(), 100), ("b".to_string(), 40)],
            edit_details: vec![],
        }
    }

    #[test]
    fn upload_stats_maps_speed_keystrokes_key_length() {
        let stats = sample_stats();
        // 总按键 = 100 + 40 = 140；用时 40s → keystrokes = 140/40 = 3.5
        let up = to_upload_stats(&stats, Duration::from_secs(40));
        assert_eq!(up.speed, 85.2);
        assert_eq!(up.keystrokes, 3.5);
        assert_eq!(up.key_length, 3.5); // 140 / 40 字 = 3.5
    }

    #[test]
    fn upload_stats_zero_elapsed_gives_zero_keystrokes() {
        let stats = sample_stats();
        let up = to_upload_stats(&stats, Duration::ZERO);
        assert_eq!(up.keystrokes, 0.0);
        assert_eq!(up.key_length, 3.5); // 码长不依赖时间
    }

    #[test]
    fn upload_stats_zero_typed_gives_zero_key_length() {
        let mut stats = sample_stats();
        stats.typed_chars = 0;
        let up = to_upload_stats(&stats, Duration::from_secs(10));
        assert_eq!(up.key_length, 0.0);
    }

    #[test]
    fn share_text_formats_full_line() {
        let up = UploadStats {
            speed: 85.2,
            keystrokes: 3.5,
            key_length: 2.8,
        };
        let source = TextSource::Online {
            competition_type: CompetitionType::Jisu,
        };
        let text = format_share_text(&source, Some(5), &up);
        assert_eq!(text, "极速杯 第5名 · WPM 85.2 · 击键 3.5 · 码长 2.8");
    }

    #[test]
    fn share_text_omits_rank_when_none() {
        let up = UploadStats {
            speed: 85.2,
            keystrokes: 3.5,
            key_length: 2.8,
        };
        let source = TextSource::File;
        let text = format_share_text(&source, None, &up);
        assert_eq!(text, "本地 · WPM 85.2 · 击键 3.5 · 码长 2.8");
    }

    #[test]
    fn competition_type_names() {
        assert_eq!(CompetitionType::Jisu.name(), "极速杯");
        assert_eq!(CompetitionType::Jinbiao.name(), "锦标赛");
        assert_eq!(CompetitionType::Jianshen.name(), "键神杯");
    }

    #[test]
    fn format_time_minutes_seconds_millis() {
        assert_eq!(format_time(Duration::from_secs_f64(85.23)), "01:25.230");
        assert_eq!(format_time(Duration::from_secs(5)), "00:05.000");
        assert_eq!(format_time(Duration::ZERO), "00:00.000");
    }

    #[test]
    fn build_upload_payload_maps_fields() {
        let stats = sample_stats();
        let up = UploadStats {
            speed: 85.2,
            keystrokes: 3.5,
            key_length: 2.8,
        };
        let text = Text {
            title: "锦标赛第3279期".into(),
            content: "你好世界".into(),
            source: TextSource::Online {
                competition_type: CompetitionType::Jinbiao,
            },
        };
        let v = build_upload_payload(&text, &stats, &up, Duration::from_secs_f64(85.23));
        assert_eq!(v["textTitle"], "锦标赛第3279期");
        assert_eq!(v["speed"], 85.2);
        assert_eq!(v["keystrokes"], 3.5);
        assert_eq!(v["maChang"], 2.8);
        assert_eq!(v["wordNum"], 4);
        assert_eq!(v["typingTime"], "01:25.230");
        assert_eq!(v["huiGai"], 1); // sample_stats edits=1
        assert_eq!(v["jianShu"], 140); // 100 + 40
        assert_eq!(v["wrongNum"], 3); // sample_stats wrong_total=3
        assert_eq!(v["challengeFlag"], 0);
        assert_eq!(v["isFirstSubmit"], 0);
    }

    #[test]
    fn build_upload_payload_includes_frontend_schema_fields() {
        // 与前端 resultPostData 逐项对齐：服务端按字段位置校验，缺字段会被拒
        // （错误信息可能表现为 token/username 解析异常）。此处断言这些字段键存在。
        let stats = sample_stats();
        let up = UploadStats {
            speed: 85.2,
            keystrokes: 3.5,
            key_length: 2.8,
        };
        let text = Text {
            title: "极速杯".into(),
            content: "你好世界".into(),
            source: TextSource::Online {
                competition_type: CompetitionType::Jisu,
            },
        };
        let v = build_upload_payload(&text, &stats, &up, Duration::from_secs(60));
        // 五个之前缺失的新字段：jianZhun / repeatNum / daCi / xuanChong / keyMethod
        assert!(v.get("jianZhun").is_some(), "缺 jianZhun: {v}");
        assert!(v.get("repeatNum").is_some(), "缺 repeatNum: {v}");
        assert!(v.get("daCi").is_some(), "缺 daCi: {v}");
        assert!(v.get("xuanChong").is_some(), "缺 xuanChong: {v}");
        assert!(v.get("keyMethod").is_some(), "缺 keyMethod: {v}");
        // jianZhun 为击准率百分号字符串
        let jian_zhun = v["jianZhun"].as_str().expect("jianZhun 应为字符串");
        assert!(
            jian_zhun.ends_with('%'),
            "jianZhun 应以 % 结尾: {jian_zhun}"
        );
        // sample_stats: correct=40, typed=40 → 100%
        assert_eq!(jian_zhun, "100.00%");
        // daCi / keyMethod 为兜底的 "0%"
        assert_eq!(v["daCi"], "0%");
        assert_eq!(v["keyMethod"], "0%");
        // repeatNum / xuanChong 兜底 0
        assert_eq!(v["repeatNum"], 0);
        assert_eq!(v["xuanChong"], 0);
    }

    #[test]
    fn build_upload_payload_accuracy_zero_when_no_typed_chars() {
        // 无上屏时击准率应记 0% 而非 NaN
        let mut stats = sample_stats();
        stats.typed_chars = 0;
        let up = UploadStats {
            speed: 0.0,
            keystrokes: 0.0,
            key_length: 0.0,
        };
        let text = Text {
            title: "x".into(),
            content: "c".into(),
            source: TextSource::File,
        };
        let v = build_upload_payload(&text, &stats, &up, Duration::from_secs(1));
        assert_eq!(v["jianZhun"], "0.00%");
    }

    #[test]
    fn build_upload_payload_backspace_from_key_frequency() {
        let mut stats = sample_stats();
        stats.key_frequency.push(("Backspace".to_string(), 7));
        let up = UploadStats {
            speed: 1.0,
            keystrokes: 1.0,
            key_length: 1.0,
        };
        let text = Text {
            title: "t".into(),
            content: "c".into(),
            source: TextSource::File,
        };
        let v = build_upload_payload(&text, &stats, &up, Duration::from_secs(1));
        assert_eq!(v["backspace"], 7);
        assert_eq!(v["jianShu"], 147); // 140 + 7
    }

    #[test]
    fn osc52_clipboard_wraps_base64() {
        let seq = osc52_clipboard("你好");
        // base64("你好") = 5L2g5aW9
        assert_eq!(seq, "\x1b]52;c;5L2g5aW9\x07");
        // 空文本也是合法序列
        assert_eq!(osc52_clipboard(""), "\x1b]52;c;\x07");
    }
}
