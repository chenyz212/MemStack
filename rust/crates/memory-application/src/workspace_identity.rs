//! 工作空间标识规范化：移植 C# `Infrastructure\WorkspaceIdentity`。
//!
//! - `normalize_identifier`：从标识、路径或 file URI 提取最后一级名称（NFKC + trim，
//!   长度 1-120，拒绝 `.` / `..`）。
//! - `calculate_key`：NFKC + 大写 → 不区分大小写的稳定匹配键。

use memory_domain::{BusinessError, ErrorCode};
use unicode_normalization::UnicodeNormalization;

/// 从标识、路径或文件 URI 中提取工作空间最后一级名称。
///
/// 与 C# 差异说明：C# 用 `System.Uri` 解析绝对 file URI；Rust 侧对
/// `file://` 前缀做等价转换（percent-decode + 取路径），Windows 盘符路径
/// 保持原样——归一为 `/` 分隔后取末段，两者结果一致。
pub fn normalize_identifier(workspace_identifier: &str) -> Result<String, BusinessError> {
    let invalid = || BusinessError::new(ErrorCode::WorkspaceIdentifierInvalid);
    if workspace_identifier.trim().is_empty() {
        return Err(invalid());
    }
    let normalized: String = workspace_identifier.trim().nfkc().collect();
    let value = if let Some(rest) = normalized.strip_prefix("file://") {
        // file URI：去 authority（localhost）与查询串，percent-decode。
        let path = rest.split('?').next().unwrap_or("");
        let path = path.strip_prefix("localhost/").unwrap_or(path);
        percent_decode(path)
    } else {
        normalized
    };
    let normalized_separators = value.replace('\\', "/");
    let normalized_separators = normalized_separators.trim_end_matches('/');
    let identifier = match normalized_separators.rfind('/') {
        Some(index) => &normalized_separators[index + 1..],
        None => normalized_separators,
    };
    let identifier = identifier.trim();
    // 长度按 UTF-16 code unit 计数（与 C# `string.Length` 一致）。
    let length: usize = identifier.chars().map(|c| c.len_utf16()).sum();
    if identifier.is_empty() || length > 120 || identifier == "." || identifier == ".." {
        return Err(invalid());
    }
    Ok(identifier.to_string())
}

/// 为工作空间标识生成不区分大小写的稳定匹配键（NFKC + 大写）。
pub fn calculate_key(workspace_identifier: &str) -> String {
    workspace_identifier.nfkc().flat_map(|c| c.to_uppercase()).collect()
}

/// 简易 percent-decoding（`%XX` → 字节），无效转义原样保留。
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                output.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_name_stays() {
        assert_eq!(normalize_identifier("my-workspace").unwrap(), "my-workspace");
        assert_eq!(normalize_identifier("  记忆工作区  ").unwrap(), "记忆工作区");
    }

    #[test]
    fn path_takes_last_segment() {
        assert_eq!(normalize_identifier(r"E:\AICoding\memstack").unwrap(), "memstack");
        assert_eq!(normalize_identifier("/home/user/project").unwrap(), "project");
        assert_eq!(normalize_identifier("a/b/c/").unwrap(), "c");
    }

    #[test]
    fn file_uri_is_decoded() {
        assert_eq!(normalize_identifier("file:///E:/AICoding/my%20ws").unwrap(), "my ws");
        assert_eq!(normalize_identifier("file:///home/user/ws").unwrap(), "ws");
    }

    #[test]
    fn nfkc_normalizes_fullwidth() {
        assert_eq!(normalize_identifier("ＷＳ名称").unwrap(), "WS名称");
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        assert!(normalize_identifier("   ").is_err());
        assert!(normalize_identifier("/").is_err());
        assert!(normalize_identifier("a/b/..").is_err());
        assert!(normalize_identifier("a/b/.").is_err());
        let long = "x".repeat(121);
        assert!(normalize_identifier(&long).is_err());
        assert!(normalize_identifier(&"x".repeat(120)).is_ok());
    }

    #[test]
    fn key_is_uppercase_for_matching() {
        assert_eq!(calculate_key("MyWorkspace"), calculate_key("myworkspace"));
        assert_eq!(calculate_key("记忆区"), "记忆区");
        // NFKC 折叠全角。
        assert_eq!(calculate_key("ＡＢ"), "AB");
    }
}
