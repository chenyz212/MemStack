//! 权限守卫：与 C# `McpAccessService` 静态方法（L565-590）逐条对齐。
//!
//! 三个守卫已随 `memory_application::mcp_access` 迁移并在第二轮契约验证；
//! 此处作为 MCP 契约层表面再导出，供分发器与 stdio 应用统一引用。

pub use memory_application::mcp_access::{require_memory_scope, require_project_scope, require_write};

#[cfg(test)]
mod tests {
    use memory_domain::{BusinessError, ErrorCode, McpCallerContext, McpPermission, MemoryItem, MemoryScope};

    fn caller(permission: McpPermission, project_id: Option<&str>) -> McpCallerContext {
        McpCallerContext {
            token_id: "00000000-0000-4000-8000-000000000001".to_string(),
            display_name: "守卫测试".to_string(),
            permission,
            project_id: project_id.map(str::to_string),
        }
    }

    fn memory(scope: MemoryScope, project_id: Option<&str>) -> MemoryItem {
        MemoryItem {
            id: "00000000-0000-4000-8000-000000000002".to_string(),
            scope,
            project_id: project_id.map(str::to_string),
            project_name: None,
            title: "t".to_string(),
            summary: String::new(),
            content: "c".to_string(),
            memory_type: "NOTE".to_string(),
            keywords: vec![],
            tags: vec![],
            importance: 3,
            is_favorite: false,
            is_pinned: false,
            cloud_processing_allowed: false,
            status: memory_domain::MemoryStatus::Active,
            version: 1,
            created_source: "s".to_string(),
            updated_source: "s".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            archived_at: None,
        }
    }

    #[test]
    fn read_only_caller_cannot_write() {
        let error = super::require_write(&caller(McpPermission::Read, None)).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpPermissionDenied);
        assert_eq!(error.message, "当前 Token 仅有读取权限");
        assert!(super::require_write(&caller(McpPermission::ReadWrite, None)).is_ok());
    }

    #[test]
    fn project_token_is_pinned_to_its_project() {
        let bound = caller(McpPermission::ReadWrite, Some("p1"));
        // 绑定 Token 读个人范围 → 拒绝。
        assert_eq!(
            super::require_project_scope(&bound, MemoryScope::Personal, None)
                .unwrap_err()
                .code,
            ErrorCode::McpProjectScopeDenied
        );
        // 项目范围但不同项目 → 拒绝。
        assert_eq!(
            super::require_project_scope(&bound, MemoryScope::Project, Some("p2"))
                .unwrap_err()
                .code,
            ErrorCode::McpProjectScopeDenied
        );
        // 绑定项目本身 → 放行。
        assert!(super::require_project_scope(&bound, MemoryScope::Project, Some("p1")).is_ok());
        // 全局 Token → 全放行。
        let global = caller(McpPermission::ReadWrite, None);
        assert!(super::require_project_scope(&global, MemoryScope::Personal, None).is_ok());
    }

    #[test]
    fn memory_scope_delegates_to_project_scope() {
        let bound = caller(McpPermission::ReadWrite, Some("p1"));
        let foreign = memory(MemoryScope::Project, Some("p2"));
        let error: BusinessError = super::require_memory_scope(&bound, &foreign).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpProjectScopeDenied);
        let own = memory(MemoryScope::Project, Some("p1"));
        assert!(super::require_memory_scope(&bound, &own).is_ok());
    }
}
