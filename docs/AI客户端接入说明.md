# 忆栈 AI 客户端接入说明（MCP stdio）

> 适用版本：忆栈 0.4.0（MemStack）
> 接入方式：标准 MCP stdio（本地进程，无 HTTP 端口）

## 一、接入原理

每个 AI 客户端通过标准 stdio 协议拉起独立的 MCP 服务进程：

```text
AI 客户端（Codex / Claude / Cursor …）
    ↓ MCP stdio（JSON-RPC over stdin/stdout）
MemStack-MCP.exe --session-id <客户端会话GUID>
    ↓
同一 SQLite 数据库（schema 7）
```

- 每个客户端一个独立**会话 ID**，权限（读/写）、项目范围、有效期互相隔离；
- 会话 ID 即接入身份，**无需在客户端配置中填写明文令牌**；
- 桌面程序未运行时，MCP 进程仍可独立读写数据库。

## 二、统一接入流程（推荐）

1. 打开忆栈 → 左侧「连接你的 AI」→ 点击「＋ 连接你的 AI」。
2. 填写显示名（如 `Codex`）、权限（读写/只读）、可选项目范围与有效期 → 生成令牌。
3. 切换到「接入配置」标签：
   - 查看 stdio 配置预览（含真实 exe 路径与该客户端的会话 ID）；
   - 点击「写入配置」：自动备份原配置 → 结构化写入 → 重读验证；
   - 或点击「复制配置」手动粘贴到目标客户端。
4. 重启目标 AI 客户端，使其加载新的 MCP 配置。
5. 回到忆栈页面，卡片状态变为「已连接」即接入成功（首次 initialize 成功后回填）。

## 三、各客户端配置位置

### Codex（自动写入）

```text
文件：%USERPROFILE%\.codex\config.toml
```

写入的配置段（示例）：

```toml
[mcp_servers.unified_ai_memory]
command = 'C:\...\MemStack-MCP.exe'
args = ['--session-id', '<会话GUID>']
enabled = true
required = false
startup_timeout_sec = 10
tool_timeout_sec = 60
```

说明：TOML 结构化写入，既有段落与注释完整保留；写入前生成 `config.toml.bak.<时间戳>` 备份。

### Claude Desktop（自动写入）

```text
文件：%APPDATA%\Claude\claude_desktop_config.json
```

```json
{
  "mcpServers": {
    "unifiedAiMemory": {
      "command": "C:\\...\\MemStack-MCP.exe",
      "args": ["--session-id", "<会话GUID>"]
    }
  }
}
```

### Cursor（自动写入）

```text
文件：%USERPROFILE%\.cursor\mcp.json
```

结构同 Claude（`mcpServers.unifiedAiMemory`）。

### 其他客户端（Trae / WorkBuddy / Qoder 等）

暂不支持自动写入，页面提供**可复制的通用 stdio 配置**（JSON 形态同上），请在对应客户端的 MCP 设置中手动添加 `command` + `args`。

## 四、可用的 16 个 MCP 工具

| 类别 | 工具 |
|---|---|
| 读取 | memory_search / memory_get / memory_recent / memory_context / memory_related |
| 记忆 | memory_create / memory_update / memory_archive |
| 候选 | memory_candidate_submit / memory_candidate_list / memory_candidate_confirm / memory_candidate_reject |
| 项目 | project_list / project_resolve / project_create / project_update |

权限约束：只读客户端无法调用写工具；项目范围客户端只能访问绑定项目；记忆来源始终取客户端显示名，不可被参数覆盖。

## 五、路径失效与一键修复（绿色版）

绿色版移动目录后，已写入配置中的 exe 路径失效：

- 「连接你的 AI」页面对应客户端卡片出现黄色警示「程序位置已变化」；
- 点击**一键重新注册**：自动备份原配置 → 写入新路径（会话 ID 不变）→ 验证通过后警示消失；
- 修复后重启对应 AI 客户端。

## 六、旧版环境变量令牌迁移（重要）

0.4.0 仍兼容 `UNIFIED_AI_MEMORY_TOKEN` 环境变量接入形态，但**将于 0.5.0 移除**：

- 页面检测到旧形态时，客户端卡片显示蓝色提示「仍在使用环境变量令牌接入」；
- 点击**改用会话 ID**：一键重写为 `--session-id` 形态（令牌本身不变，吊销/轮换语义继续生效）；
- 新接入一律使用会话 ID 形态，注册配置不再生成环境变量令牌写法。

## 七、连接测试与状态

- 卡片「编辑」内可执行**连接测试**：真实 spawn MCP exe 完成 initialize + tools/list 握手（16 工具校验）；
- 连接活动（调用次数、最近连接时间）由真实 MCP 初始化与工具调用自动回填；
- 轮换会话 ID 后旧 ID 立即失效，需重新「写入配置」并重启客户端。
