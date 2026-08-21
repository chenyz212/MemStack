# 第三轮连接 AI 动态化修订执行计划

## 修订背景

Java 版连接 AI 采用动态客户端卡片和统一新增/编辑弹窗，用户自行填写 AI 工具名称。当前桌面版在 `App.vue` 固定写死 Codex、Claude、Cursor、Trae 和通用 MCP，不能创建任意工具，且每张固定卡片重复显示创建表单。本轮将 `McpAssistantType` 降级为兼容字段，不再作为业务身份。

## 目标

用户可以创建、命名、编辑和管理任意 AI 工具；每个工具拥有独立会话、Token、权限、项目范围、有效期、配置和最近调用状态。初始无数据时只显示“连接你的 AI”入口，不显示固定助手卡片。已知客户端目录只提供图标、颜色和说明，未知名称使用通用样式。保留 DPAPI 可恢复 Token、默认雾化、悬停/聚焦重复查看和复制，以及 `10212` MCP、`18461` REST、16 个工具和候选流程。

## 数据模型

新增 `mcp_client_session`：`id`、`client_key`、`display_name`、`assistant_type`、`icon_key`、`color_key`、`created_at`、`updated_at`、`revoked_at`。`display_name` 是唯一可信的 MCP 来源；`client_key` 是不区分大小写的规范化名称；`assistant_type` 仅兼容旧数据。

`mcp_token` 增加 `session_id` 外键并保留旧字段。迁移必须无损转换第二轮数据；旧 Token 按显示名称创建会话，无法推断时使用兼容名称并允许编辑；新 Token 必须关联会话。

## 后端接口

在 `127.0.0.1:18461` 增加客户端管理接口：

```text
GET /api/mcp/clients
POST /api/mcp/clients
GET /api/mcp/clients/{id}
PUT /api/mcp/clients/{id}
POST /api/mcp/clients/{id}/revoke
POST /api/mcp/clients/{id}/regenerate
GET /api/mcp/clients/{id}/secret
GET /api/mcp/clients/{id}/config
POST /api/mcp/clients/{id}/test
```

客户端资料、密钥、配置和测试使用独立方法。名称去空白并校验长度，重复返回冲突；吊销清除密文；明文接口只接受桌面临时管理密钥并设置 `Cache-Control: no-store`。配置生成拆分为 HTTP、stdio、JSON、TOML 和命令行转义函数，HTTP 地址固定为 `http://127.0.0.1:10212/mcp`。

## 前端 UI

页面分为 MCP 状态、已接入 AI 和新增入口三层。已接入区域只渲染真实会话；无数据显示空状态。动态卡片显示用户名称、图标、状态、权限、项目范围、有效期、最近调用、Token 雾化区域、复制 Token、复制完整配置、编辑、测试、重新生成和吊销。

统一弹窗包含“编辑 AI 工具”和“接入配置”两个标签。创建保存会话和 Token；编辑名称不重复创建 Token；危险操作需要确认。长名称换行且卡片宽度稳定，宽屏双列、窄屏单列。

Token 默认使用当前占位区域的雾蒙蒙效果；悬停或键盘聚焦显示完整值，移出、失焦、窗口失焦或按 Esc 恢复雾化。雾化状态可直接复制，允许重复查看；离开页面清空前端明文状态，显示隐藏不改变布局。

## MCP 能力与端口

继续实现 16 个单一职责工具：`memory_search`、`memory_get`、`memory_recent`、`memory_context`、`memory_related`、`memory_create`、`memory_update`、`memory_archive`、`memory_candidate_submit`、`memory_candidate_list`、`memory_candidate_confirm`、`memory_candidate_reject`、`project_list`、`project_resolve`、`project_create`、`project_update`。

只读 Token 仅允许读取、候选列表和项目解析；读写 Token 额外允许正式记忆、候选和项目写入；项目范围 Token 只能访问绑定项目。来源严格读取会话 `display_name`，不得被握手名称、模型名或工具参数覆盖；候选确认前不得进入正式检索。

MCP Streamable HTTP 只监听 `127.0.0.1:10212/mcp`，REST 只监听 `127.0.0.1:18461`。端口占用时禁止随机切换，释放后可单独重启。`统一AI记忆.exe --stdio` 读取 `UNIFIED_AI_MEMORY_TOKEN`，不创建窗口、托盘或 HTTP 监听。

## 执行顺序

1. 盘点固定助手耦合点并完成会话表、旧数据转换和 Token 关联。
2. 实现会话服务、名称规范化、DPAPI 查看、吊销和轮换。
3. 拆分并固定双端口，实现占用检测、单独重启和 stdio。
4. 引入 MCP SDK 2.0.0，实现权限、项目上下文、16 个工具和候选事务。
5. 实现通用配置生成器和转义函数。
6. 重做动态卡片、空状态、统一弹窗和 Token 雾化交互。
7. 更新总览、托盘状态，运行类型、Vitest、后端、协议和视觉回归测试。
8. 构建 `0.4.0` Windows x64 绿色版并生成中文验收报告。

## 验收标准

- 任意名称创建、改名、重复校验、未知客户端样式和配置均正确。
- Token 不明文落盘；默认雾化，可悬停/聚焦重复查看和复制，离开页面清理状态。
- MCP/REST 双端口隔离、端口占用不随机切换、stdio 不监听端口。
- 工具固定 16 个，权限、项目范围、来源和候选边界正确，第二轮数据无损升级。
- Playwright 验证宽屏、平板、移动端，长名称、空状态、加载、过期、吊销和错误状态无重叠。

## 范围边界

不实现正式知识图谱、旧 SSE、OAuth、多用户、备份恢复和安装程序；不修改 `desktop-client` 外 Java 版用户改动；继续使用单项目范围模型。所有动态会话、Token、配置、MCP、候选、UI 和 `0.4.0` 构建验收通过后视为完成。
