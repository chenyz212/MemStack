//! Tauri Command 层：既有服务的薄映射（无新业务逻辑，总计划 §5）。
//!
//! 约定：
//! - 参数 camelCase（Tauri 自动映射 snake_case 形参）；DTO 已由 memory-domain serde 保证 camelCase。
//! - 统一返回 `Result<T, CommandError>`；错误对象与 C# 现网 ApiErrorBody 1:1。
//! - 每个命令委托到 `*_impl(&AppState, ...)` 自由函数，供单测直调（temp 库）。
//! - 命令为 async：在 Tauri 异步运行时线程执行，避免阻塞主线程 UI。

pub mod candidates;
pub mod embedding;
pub mod graph;
pub mod mcp;
pub mod memory;
pub mod projects;
pub mod search;
pub mod workspaces;
