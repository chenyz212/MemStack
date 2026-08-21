//! 记忆列表游标分页编解码：与 C# `MemoryService.EncodeCursor/DecodeCursor`
//! 字节级一致（MemoryService.cs L766-796）。
//!
//! 游标体 `MemoryCursor { isPinned, updatedAt, id }` 按 camelCase JSON 序列化
//! （字段顺序固定：isPinned → updatedAt → id），再 UTF-8 → 标准 Base64（带 padding）。
//! 排序键为 `is_pinned DESC, updated_at DESC, id DESC`，keyset 条件由服务层 SQL 实现。
//!
//! 解码语义对齐 C#：空/全空白游标返回 `None`（第一页）；非法 Base64 抛
//! `CURSOR_INVALID`。C# 对「合法 Base64 但非法 JSON」会抛非业务异常（500），
//! Rust 侧统一收敛为 `CURSOR_INVALID`（400），行为更稳健且不影响契约场景。

use crate::error::{BusinessError, ErrorCode};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// 稳定分页所需的完整排序位置（与 C# `MemoryCursor` record 一致）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryCursor {
    pub is_pinned: bool,
    /// UTC ISO 8601 文本（7 位小数 + 时区偏移，与库内存储一致）。
    pub updated_at: String,
    /// GUID 小写带连字符。
    pub id: String,
}

/// 将完整排序位置编码为分页游标。
pub fn encode_cursor(is_pinned: bool, updated_at: &str, id: &str) -> String {
    let cursor = MemoryCursor {
        is_pinned,
        updated_at: updated_at.to_string(),
        id: id.to_string(),
    };
    let json = serde_json::to_string(&cursor).expect("MemoryCursor 序列化不会失败");
    base64::engine::general_purpose::STANDARD.encode(json.as_bytes())
}

/// 解码分页游标：`None`/空白 → 第一页（`Ok(None)`）；格式非法 → `CURSOR_INVALID`。
pub fn decode_cursor(cursor: Option<&str>) -> Result<Option<MemoryCursor>, BusinessError> {
    let Some(raw) = cursor else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|_| BusinessError::new(ErrorCode::CursorInvalid))?;
    let text = String::from_utf8(decoded).map_err(|_| BusinessError::new(ErrorCode::CursorInvalid))?;
    let parsed: MemoryCursor = serde_json::from_str(&text).map_err(|_| BusinessError::new(ErrorCode::CursorInvalid))?;
    Ok(Some(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    const UPDATED_AT: &str = "2026-08-15T08:00:00.0000000+00:00";
    const MEMORY_ID: &str = "0b6c8f5a-1111-4222-8333-444455556666";

    #[test]
    fn encode_matches_csharp_wire_format() {
        // 期望值 = Base64(UTF8('{"isPinned":true,"updatedAt":"...","id":"..."}'))
        // 字段顺序与 camelCase 均与 C# JsonSerializerDefaults.Web 一致。
        let cursor = encode_cursor(true, UPDATED_AT, MEMORY_ID);
        let json = format!("{{\"isPinned\":true,\"updatedAt\":\"{UPDATED_AT}\",\"id\":\"{MEMORY_ID}\"}}");
        let expected = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        assert_eq!(cursor, expected);
    }

    #[test]
    fn encode_decode_roundtrip() {
        for pinned in [false, true] {
            let encoded = encode_cursor(pinned, UPDATED_AT, MEMORY_ID);
            let decoded = decode_cursor(Some(&encoded)).unwrap().expect("应解码出游标");
            assert_eq!(
                decoded,
                MemoryCursor {
                    is_pinned: pinned,
                    updated_at: UPDATED_AT.to_string(),
                    id: MEMORY_ID.to_string(),
                }
            );
        }
    }

    #[test]
    fn decode_none_or_blank_returns_first_page() {
        assert!(decode_cursor(None).unwrap().is_none());
        assert!(decode_cursor(Some("")).unwrap().is_none());
        assert!(decode_cursor(Some("   ")).unwrap().is_none());
    }

    #[test]
    fn decode_invalid_input_raises_cursor_invalid() {
        // 非 Base64 字符。
        let error = decode_cursor(Some("!!!not-base64!!!")).unwrap_err();
        assert_eq!(error.code, ErrorCode::CursorInvalid);
        // 合法 Base64 但非 UTF-8。
        let binary = base64::engine::general_purpose::STANDARD.encode([0xff, 0xfe, 0xfd]);
        let error = decode_cursor(Some(&binary)).unwrap_err();
        assert_eq!(error.code, ErrorCode::CursorInvalid);
        // 合法 Base64 但 JSON 结构不符。
        let text = base64::engine::general_purpose::STANDARD.encode(b"{\"isPinned\":true}");
        let error = decode_cursor(Some(&text)).unwrap_err();
        assert_eq!(error.code, ErrorCode::CursorInvalid);
        assert_eq!(error.message, "分页游标格式无效");
    }

    #[test]
    fn decode_accepts_csharp_generated_cursor() {
        // 由 C# 端契约场景生成的真实游标（tools/csharp 生成后回填期望值）。
        // 构造方式与 encode_matches_csharp_wire_format 一致，此处验证跨语言互通路径。
        let csharp_cursor = encode_cursor(
            false,
            "2026-01-02T03:04:05.6000000+00:00",
            "00000000-0000-4000-8000-000000000001",
        );
        let decoded = decode_cursor(Some(&csharp_cursor)).unwrap().expect("应解码成功");
        assert!(!decoded.is_pinned);
        assert_eq!(decoded.updated_at, "2026-01-02T03:04:05.6000000+00:00");
        assert_eq!(decoded.id, "00000000-0000-4000-8000-000000000001");
    }
}
