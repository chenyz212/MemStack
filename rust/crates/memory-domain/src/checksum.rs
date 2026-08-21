//! 内容校验和：与 C# `MemoryService.CalculateChecksum` /
//! `MemoryCandidateService` 同名实现逐字节一致。
//!
//! 算法：`content.trim()` → Unicode NFKC 规范化 → UTF-8 字节 SHA-256 → 小写 hex。
//! 用于「相同范围内内容一致」的去重判断，两侧实现必须产出完全相同的摘要。

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// 计算标准化正文哈希（NFKC + SHA-256 小写 hex）。
pub fn content_checksum(content: &str) -> String {
    let normalized: String = content.trim().nfkc().collect();
    let digest = Sha256::digest(normalized.as_bytes());
    hex_lower(&digest)
}

/// 将字节序列编码为小写 hex（等价 C# `Convert.ToHexString(...).ToLowerInvariant()`）。
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    /// C# Tools 生成的黄金数据（`testdata/golden/checksum-golden.json`）；
    /// 文件缺失时跳过，保证 Rust 侧可独立运行。
    #[derive(Deserialize)]
    struct GoldenEntry {
        content: String,
        checksum: String,
    }

    #[test]
    fn checksum_is_stable_sha256_lower_hex() {
        // SHA-256("abc") 的已知摘要，验证 hex 编码与小写。
        assert_eq!(
            content_checksum("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn checksum_trims_whitespace() {
        assert_eq!(content_checksum("  abc  "), content_checksum("abc"));
        assert_eq!(content_checksum("\u{3000}abc\u{a0}"), content_checksum("abc"));
    }

    #[test]
    fn checksum_nfkc_normalizes_fullwidth() {
        // NFKC 将全角字母折叠为半角；与 C# FormKC 行为一致。
        assert_eq!(content_checksum("ＡＢＣ"), content_checksum("ABC"));
        // 组合字符与预组合字符在 NFKC 下等价。
        assert_eq!(content_checksum("é"), content_checksum("e\u{0301}"));
    }

    #[test]
    fn checksum_supports_chinese_and_special_characters() {
        assert_eq!(content_checksum("统一 AI 记忆").len(), 64);
        assert!(content_checksum("统一 AI 记忆").chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(content_checksum("记忆 A"), content_checksum("记忆 B"));
    }

    #[test]
    fn checksum_matches_csharp_golden_file_when_present() {
        let golden_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("testdata/golden/checksum-golden.json");
        let Ok(raw) = std::fs::read_to_string(&golden_path) else {
            return; // 黄金文件缺失：跳过（C# 侧生成后自动生效）
        };
        let entries: Vec<GoldenEntry> = serde_json::from_str(&raw).expect("checksum-golden.json 格式无效");
        assert!(!entries.is_empty(), "checksum-golden.json 不应为空");
        for entry in &entries {
            assert_eq!(
                content_checksum(&entry.content),
                entry.checksum,
                "内容「{}」的校验和与 C# 黄金值不一致",
                entry.content
            );
        }
    }
}
