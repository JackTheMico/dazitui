//! 52dazi.cn 协议层：请求加密、响应解析（纯函数，零 I/O）。
//!
//! 逆向协议见 ADR-0002：AES-128-CBC（ZeroPadding），key/iv 为 16 字节 ASCII，
//! 加密后 base64 编码作为请求体；响应为明文 JSON。

use aes::Aes128;
use base64::Engine;
use cbc::Decryptor;
use cbc::Encryptor;
use cbc::cipher::block_padding::NoPadding;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use serde::de::DeserializeOwned;

/// AES-128 密钥（16 字节 ASCII）。
const AES_KEY: &[u8; 16] = b"c9ec834c80f77237";
/// AES-CBC 初始向量（16 字节 ASCII）。
const AES_IV: &[u8; 16] = b"db4d6bfde3057dca";

/// AES 块大小。
const BLOCK: usize = 16;

type Aes128CbcEnc = Encryptor<Aes128>;
type Aes128CbcDec = Decryptor<Aes128>;

/// 加密/解析失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// 加解密失败。
    Crypto(String),
    /// JSON 解析失败。
    Parse(String),
}

/// CryptoJS 风格的 ZeroPadding：补 0x00 到下一个块边界；空输入也补一个全零块。
fn zero_pad(data: &[u8]) -> Vec<u8> {
    let pad = BLOCK - (data.len() % BLOCK);
    let mut out = data.to_vec();
    out.resize(data.len() + pad, 0);
    out
}

/// 去掉末尾连续的 0x00（ZeroPadding 的逆操作）。
fn zero_unpad(data: &[u8]) -> &[u8] {
    let end = data.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    &data[..end]
}

/// 用 52dazi 约定加密明文：AES-128-CBC（ZeroPadding）+ base64。
pub fn encrypt(plaintext: &str) -> String {
    let padded = zero_pad(plaintext.as_bytes());
    let cipher = Aes128CbcEnc::new_from_slices(AES_KEY, AES_IV).expect("key/iv 长度固定为 16 字节");
    let ct = cipher.encrypt_padded_vec_mut::<NoPadding>(&padded);
    base64::engine::general_purpose::STANDARD.encode(ct)
}

/// 解密 52dazi 密文（base64 → AES-CBC，去 ZeroPadding）。用于测试与调试。
pub fn decrypt(ciphertext_b64: &str) -> String {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(ciphertext_b64)
        .expect("base64 解码失败");
    let cipher = Aes128CbcDec::new_from_slices(AES_KEY, AES_IV).expect("key/iv 长度固定为 16 字节");
    let pt = cipher
        .decrypt_padded_vec_mut::<NoPadding>(&bytes)
        .expect("AES 解密失败");
    String::from_utf8(zero_unpad(&pt).to_vec()).expect("明文非 UTF-8")
}

/// 把请求字段构造成 JSON 对象并加密为请求体。
pub fn build_request(fields: &[(&str, &str)]) -> String {
    let map: serde_json::Map<String, serde_json::Value> = fields
        .iter()
        .map(|(k, v)| {
            (
                (*k).to_string(),
                serde_json::Value::String((*v).to_string()),
            )
        })
        .collect();
    let json = serde_json::Value::Object(map).to_string();
    encrypt(&json)
}

/// 把 JSON Value 序列化并加密（用于含数字字段的请求体）。
pub fn encrypt_value(value: &serde_json::Value) -> String {
    encrypt(&value.to_string())
}

/// 把明文 JSON 反序列化为强类型。
pub fn parse_json<T: DeserializeOwned>(json: &str) -> Result<T, ProtocolError> {
    serde_json::from_str(json).map_err(|e| ProtocolError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    // 期望值来自独立的 cryptography 库（非本代码计算），避免同义反复。
    #[test]
    fn encrypt_matches_known_vector_ascii() {
        assert_eq!(encrypt("test"), "cobkDjdkjYcd+dKIWQvMkw==");
    }

    #[test]
    fn encrypt_matches_known_vector_cn() {
        assert_eq!(encrypt("你好世界"), "mAEtXj+971NMkdYhudo1Tg==");
    }

    #[test]
    fn encrypt_empty_still_pads_one_block() {
        assert_eq!(encrypt(""), "1hOp2mXJlkI5Mti018cSWQ==");
    }

    #[test]
    fn build_request_encrypts_json_object() {
        let encoded = build_request(&[("username", "alice"), ("password", "secret")]);
        // 解密回来验证 JSON 结构（键有序，BTreeMap）
        let decrypted = decrypt(&encoded);
        assert_eq!(decrypted, r#"{"password":"secret","username":"alice"}"#);
    }

    #[test]
    fn encrypt_value_handles_numeric_and_nested_fields() {
        let v = serde_json::json!({"speed": 85.2, "wordNum": 100});
        let encoded = encrypt_value(&v);
        let decrypted = decrypt(&encoded);
        // 数字保持数字类型（不字符串化）
        assert_eq!(decrypted, r#"{"speed":85.2,"wordNum":100}"#);
    }

    #[derive(Deserialize, Debug)]
    struct FakePayload {
        name: String,
        count: u32,
    }

    #[test]
    fn parse_json_deserializes_into_typed_value() {
        let v: FakePayload = parse_json(r#"{"name":"alice","count":3}"#).unwrap();
        assert_eq!(v.name, "alice");
        assert_eq!(v.count, 3);
    }

    #[test]
    fn parse_json_reports_bad_json() {
        let err = parse_json::<FakePayload>("not json").unwrap_err();
        assert!(matches!(err, ProtocolError::Parse(_)));
    }
}
