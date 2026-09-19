//! 虎码杯（race.tiger-code.com）协议层与客户端集成。
//!
//! 包含凭据持久化、生稿赛草稿保护、成绩指标折算、剪贴板分享格式化以及 HTTP 客户端。

pub mod client;
pub mod draft;
pub mod share;
pub mod token;

pub use client::{
    TIGER_BASE_URL, TigerCupClient, TigerLeaderboardEntry, TigerLoginResult, TigerUploadResponse,
    parse_tiger_article_response, parse_tiger_leaderboard_response,
};
pub use draft::{TigerDraft, TigerDraftStore};
pub use share::{TigerScorePayload, build_tiger_payload, format_tiger_share_text};
pub use token::{TigerCredentials, TigerTokenStore};
