//! MCP 工具契约层：21 个工具定义（16 既有 + 5 项目文档/结论卡片）、权限与项目范围校验、参数到应用服务的分发。
//!
//! - `registry`：以 `contracts/mcp-tools-list.json` 快照为唯一权威内嵌工具清单。
//! - `permissions`：`McpAccessService` 三个静态守卫（写权限 / 项目范围 / 记忆范围）。
//! - `dispatch`：16 工具「参数 DTO → memory-application 调用 → tools/call 结果」映射
//!   （以 C# `McpTools.cs` 为唯一权威，含来源强制 `caller.display_name`）。

pub mod dispatch;
pub mod permissions;
pub mod registry;
