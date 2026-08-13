//! 52dazi.cn 协议层：请求加密、响应解析（纯函数，零 I/O）。
//!
//! 逆向协议见 ADR-0002：AES-128-CBC（ZeroPadding），key/iv 为 16 字节 ASCII，
//! 加密后 base64 编码作为请求体；响应为明文 JSON。

pub mod auth;
pub mod client;
pub mod protocol;
pub mod share;
pub mod token;
