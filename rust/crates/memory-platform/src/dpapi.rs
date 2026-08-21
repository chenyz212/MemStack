//! Windows DPAPI 凭据保护，与 C# `SecretProtector` 双向兼容。
//!
//! 兼容约定：
//! - 作用域 `CurrentUser`（仅当前 Windows 账户可解密）。
//! - Entropy = UTF-8 字节 `"MemStack.Desktop.0.4.0"`（0.4.0 品牌升级后变更，旧 Token 需重签）。
//! - 存储格式为 Base64 文本（对应 `mcp_token.token_ciphertext` 等字段）。

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use memory_domain::{BusinessError, ErrorCode};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::{PCWSTR, PWSTR};

/// DPAPI entropy：0.4.0 品牌升级后与 `MemStack.Desktop.0.4.0` 一致。
const ENTROPY: &[u8] = b"MemStack.Desktop.0.4.0";

/// DPAPI 操作错误分类，映射为稳定业务错误码。
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct DpapiError {
    pub kind: DpapiErrorKind,
    pub message: String,
}

/// 错误种类：调用方据此返回稳定错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpapiErrorKind {
    /// 输入不是合法 Base64。
    InvalidBase64,
    /// 密文损坏、entropy 不匹配或 DPAPI 解密失败。
    CryptFailure,
}

impl From<DpapiError> for BusinessError {
    fn from(error: DpapiError) -> Self {
        let code = match error.kind {
            DpapiErrorKind::InvalidBase64 => ErrorCode::InvalidArgument,
            DpapiErrorKind::CryptFailure => ErrorCode::InternalError,
        };
        BusinessError::with_message(code, error.message)
    }
}

fn crypt_blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
        pbData: bytes.as_ptr().cast_mut(),
    }
}

fn entropy_blob() -> CRYPT_INTEGER_BLOB {
    crypt_blob(ENTROPY)
}

/// 释放 DPAPI 输出缓冲区。
unsafe fn free_blob(blob: CRYPT_INTEGER_BLOB) {
    if !blob.pbData.is_null() {
        // LocalFree 接受 HLOCAL；DPAPI 输出必须用 LocalFree 释放。
        unsafe {
            let _ = LocalFree(Some(HLOCAL(blob.pbData.cast())));
        }
    }
}

/// 使用 DPAPI CurrentUser 作用域加密，返回 Base64 密文（与 C# `Protect` 输出等价）。
pub fn protect(plaintext: &str) -> Result<String, DpapiError> {
    let input = crypt_blob(plaintext.as_bytes());
    let mut output = CRYPT_INTEGER_BLOB::default();
    let result = unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            Some(&entropy_blob()),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    match result {
        Ok(()) => {
            let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) };
            let ciphertext = BASE64.encode(bytes);
            unsafe { free_blob(output) };
            Ok(ciphertext)
        }
        Err(error) => Err(DpapiError {
            kind: DpapiErrorKind::CryptFailure,
            message: format!("DPAPI 加密失败：{error}"),
        }),
    }
}

/// 解密 C# `SecretProtector.Protect` 或本模块 `protect` 生成的 Base64 密文。
pub fn unprotect(ciphertext_base64: &str) -> Result<String, DpapiError> {
    let ciphertext = BASE64.decode(ciphertext_base64.trim()).map_err(|error| DpapiError {
        kind: DpapiErrorKind::InvalidBase64,
        message: format!("密文不是合法 Base64：{error}"),
    })?;
    let input = crypt_blob(&ciphertext);
    let mut output = CRYPT_INTEGER_BLOB::default();
    let mut description = PWSTR::null();
    let result = unsafe {
        CryptUnprotectData(
            &input,
            Some(&mut description),
            Some(&entropy_blob()),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if !description.is_null() {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(description.as_ptr().cast())));
        }
    }
    match result {
        Ok(()) => {
            let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) };
            let plaintext = String::from_utf8_lossy(bytes).into_owned();
            unsafe { free_blob(output) };
            Ok(plaintext)
        }
        Err(error) => Err(DpapiError {
            kind: DpapiErrorKind::CryptFailure,
            message: format!("DPAPI 解密失败：{error}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protect_unprotect_roundtrip() {
        let plaintext = "uam_roundtrip_sample";
        let ciphertext = protect(plaintext).unwrap();
        assert_ne!(plaintext, ciphertext);
        assert_eq!(unprotect(&ciphertext).unwrap(), plaintext);
    }

    #[test]
    fn roundtrip_handles_chinese_and_special_characters() {
        for plaintext in ["中文密钥样本：忆栈迁移", "p@$$w0rd!#%&*", ""] {
            let ciphertext = protect(plaintext).unwrap();
            assert_eq!(unprotect(&ciphertext).unwrap(), plaintext);
        }
    }

    #[test]
    fn invalid_base64_returns_stable_error() {
        let error = unprotect("not-valid-base64!!!").unwrap_err();
        assert_eq!(error.kind, DpapiErrorKind::InvalidBase64);
        let business: BusinessError = error.into();
        assert_eq!(business.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn corrupted_ciphertext_returns_crypt_failure() {
        let ciphertext = protect("sample").unwrap();
        let mut bytes = BASE64.decode(&ciphertext).unwrap();
        bytes[0] ^= 0xFF;
        let corrupted = BASE64.encode(&bytes);
        let error = unprotect(&corrupted).unwrap_err();
        assert_eq!(error.kind, DpapiErrorKind::CryptFailure);
    }

    #[test]
    fn ciphertext_is_never_plaintext_recoverable_without_dpapi() {
        // 长度启发式：密文（含 DPAPI 头）总是显著长于明文。
        let plaintext = "short";
        let ciphertext = protect(plaintext).unwrap();
        assert!(ciphertext.len() > plaintext.len() * 3);
    }
}
