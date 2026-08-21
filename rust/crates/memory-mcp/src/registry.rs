//! 工具清单注册表：内嵌 `contracts/mcp-tools-list.json` 快照作为唯一权威。
//!
//! 快照是 C# MCP SDK `tools/list` 的原始响应帧（含 `result.tools`）；
//! 这里抽出 `tools` 数组做 Value 级服务（JSON 对象键序无关，决策 6）。

use serde_json::{Value, json};

/// C# 0.4.0 实测捕获的 tools/list 契约快照（字节级权威）。
const TOOLS_LIST_SNAPSHOT: &str = include_str!("../../../../contracts/mcp-tools-list.json");

/// 返回 `{"tools":[...]}`（与 C# tools/list 的 result 形状一致）。
pub fn tools_list_json() -> Value {
    let snapshot: Value =
        serde_json::from_str(TOOLS_LIST_SNAPSHOT).expect("contracts/mcp-tools-list.json 必须是合法 JSON");
    json!({ "tools": snapshot["result"]["tools"].clone() })
}

/// 全部工具名（快照顺序）。
pub fn tool_names() -> Vec<String> {
    tools_list_json()["tools"]
        .as_array()
        .expect("快照 tools 必须是数组")
        .iter()
        .map(|tool| tool["name"].as_str().expect("工具必须有名").to_string())
        .collect()
}

/// 分发前置校验：是否为已注册工具。
pub fn is_known_tool(name: &str) -> bool {
    tool_names().iter().any(|tool| tool == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16 个单一职责工具（对应迁移计划阶段 5）。
    const EXPECTED: [&str; 16] = [
        "memory_search",
        "memory_get",
        "memory_recent",
        "memory_context",
        "memory_related",
        "memory_create",
        "memory_update",
        "memory_archive",
        "memory_candidate_submit",
        "memory_candidate_list",
        "memory_candidate_confirm",
        "memory_candidate_reject",
        "project_list",
        "project_resolve",
        "project_create",
        "project_update",
    ];

    #[test]
    fn snapshot_contains_exactly_sixteen_tools() {
        let snapshot = tools_list_json();
        let tools = snapshot["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 16, "工具数量必须是 16：{tools:?}");
        let mut names: Vec<&str> = tools.iter().map(|tool| tool["name"].as_str().unwrap()).collect();
        names.sort_unstable();
        let mut expected = EXPECTED;
        expected.sort_unstable();
        assert_eq!(names, expected);
    }

    #[test]
    fn every_tool_declares_input_schema() {
        let snapshot = tools_list_json();
        for tool in snapshot["tools"].as_array().unwrap() {
            assert!(
                tool["inputSchema"]["type"].is_string(),
                "{} 缺少 inputSchema",
                tool["name"]
            );
        }
    }

    #[test]
    fn known_tool_check_rejects_unknown_names() {
        assert!(is_known_tool("memory_search"));
        assert!(is_known_tool("project_update"));
        assert!(!is_known_tool("memory_delete"));
        assert!(!is_known_tool(""));
    }

    /// 递归校验：任何 schema 节点的 required 不得包含可空字段。
    /// stdio 层 strip_nulls 会剔除 null 属性（对齐 C# SDK 序列化语义），
    /// 若 required 含可空字段，MCP 客户端（TraeWork/WorkBuddy SDK）校验
    /// structuredContent 必然失败——实测曾致 project_resolve 全量调用报错。
    #[test]
    fn output_schema_required_never_nullable() {
        fn check(node: &Value, path: String) {
            let Some(object) = node.as_object() else { return };
            if let (Some(required), Some(properties)) = (object.get("required"), object.get("properties"))
                && let (Some(required), Some(properties)) = (required.as_array(), properties.as_object())
            {
                for field in required {
                    let name = field.as_str().unwrap_or_default();
                    let Some(property) = properties.get(name) else { continue };
                    let Some(types) = property.get("type") else { continue };
                    let nullable = types
                        .as_array()
                        .map(|list| list.iter().any(|item| item == "null"))
                        .unwrap_or(false);
                    assert!(
                        !nullable,
                        "{path} 的 required 字段 {name} 可空（type 含 null）：strip_nulls 会剔除 null 属性，MCP 客户端校验必失败"
                    );
                }
            }
            for (key, value) in object {
                check(value, format!("{path}.{key}"));
            }
        }
        for tool in tools_list_json()["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            check(&tool["outputSchema"], name.to_string());
            check(&tool["inputSchema"], format!("{name} input"));
        }
    }
}
