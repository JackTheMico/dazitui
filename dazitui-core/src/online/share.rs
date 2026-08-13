//! 52dazi.cn 上传字段映射与分享文本格式化。

use std::time::Duration;

use crate::{Stats, TextSource};

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
}
