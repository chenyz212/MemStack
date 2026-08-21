//! Command 统一错误模型：与 C# 现网 `DesktopApiHost` 错误响应 1:1 对齐。
//!
//! C# 实测形状（DesktopApiHost.cs 三处一致）：
//! `{"code":"...","message":"...","details":{}}` —— `details` 恒为空对象。
//! `BusinessError.details` 为 `Value::Null` 时序列化为 `{}`，非空时原样透传。

use serde::Serialize;
use serde_json::{Value, json};

use memory_domain::{BusinessError, ErrorCode};

/// Command 错误对象：`invoke` reject 后前端按 `code` 执行恢复。
#[derive(Debug, Clone, Serialize)]
pub struct CommandError {
    /// 稳定业务错误码（SCREAMING_SNAKE_CASE）。
    pub code: String,
    /// 面向用户的中文消息。
    pub message: String,
    /// 结构化补充信息（对齐 C#：默认空对象）。
    pub details: Value,
}

impl From<BusinessError> for CommandError {
    fn from(error: BusinessError) -> Self {
        Self {
            code: error.code.as_str().to_string(),
            message: error.message,
            details: if error.details.is_null() {
                json!({})
            } else {
                error.details
            },
        }
    }
}

impl CommandError {
    /// 以错误码与自定义消息构造。
    pub fn with_message(code: ErrorCode, message: impl Into<String>) -> Self {
        BusinessError::with_message(code, message).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_error_serializes_csharp_shape_with_empty_details() {
        let error = CommandError::from(BusinessError::new(ErrorCode::Conflict));
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["code"], "CONFLICT");
        assert_eq!(json["message"], "内容已被其他会话修改，请刷新后重试");
        // C# 现网 details 恒为空对象。
        assert_eq!(json["details"], json!({}));
    }

    #[test]
    fn command_error_keeps_non_null_details() {
        let business = BusinessError::new(ErrorCode::NotFound).with_details(json!({ "memoryId": "abc" }));
        let error = CommandError::from(business);
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["details"]["memoryId"], "abc");
    }

    #[test]
    fn command_error_message_override() {
        let error = CommandError::with_message(ErrorCode::InternalError, "自定义消息");
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["code"], "INTERNAL_ERROR");
        assert_eq!(json["message"], "自定义消息");
    }
}
