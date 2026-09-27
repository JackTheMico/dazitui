//! 牛杯（ox-cup）赛文拉取、榜单查询与打卡分享格式化。

pub mod client;
pub mod share;

pub use client::{
    OX_CUP_ARTICLE_BASE_URL, OX_CUP_SCORE_BASE_URL, OxCupArticle, OxCupClient, OxCupScoreEntry,
    parse_ox_cup_article_response, parse_ox_cup_leaderboard_response,
};
pub use share::{calculate_ox_proof, format_ox_share_text};
