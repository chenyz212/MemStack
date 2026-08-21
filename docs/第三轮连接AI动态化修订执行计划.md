# 第三轮连接 AI 动态化修订执行计划

## 一、问题结论

Java 版的连接 AI 实现模式是：

- 页面只展示已经创建的 AI 工具卡片。
- 页面末尾提供“连接你的 AI”新增卡片。
- 用户自行填写 AI 工具名称，例如 `Codex`、`Claude`、`WorkBuddy` 或任意自定义名称。
- 已知 AI 名称只通过 `mcpClientCatalog.ts` 提供图标、颜色、传输方式和说明，不负责创建固定助手。
- 未识别名称使用通用 MCP 兜底展示。
- 新增和编辑都在统一弹窗中完成，不把完整表单重复塞进每张卡片。
- 每个 AI 工具拥有稳定的客户端会话和独立 Token。
- 支持编辑名称、项目范围、权限、有效期、吊销和轮换。
- 配置页通过 HTTP/stdio 标签切换，并复制当前 AI 工具的完整配置。

当前桌面版的问题是：

- `assistantCards` 把 Codex、Claude、Cursor、Trae、通用 MCP 写死成五张业务卡片。
- 用户不能真正创建任意 AI 工具。
- Token 创建表单重复出现在每张固定卡片中，导致页面拥挤、层级混乱。
- `assistant_type` 被错误地当成助手身份，无法表达用户自定义工具。
- 没有 Java 版的“动态已接入列表 + 新增卡片 + 统一弹窗”结构。

本次只修订连接 AI 的数据模型、接口和 UI；10212 MCP 固定端口、18461 REST、DPAPI、权限、有效期、16 个工具、候选记忆和其他第三轮功能保持不变。

## 二、目标结构

### 1. 动态助手模型

新增动态 AI 客户端会话概念，参考 Java 版：

```
mcp_client_session
id
client_key
display_name
client_version
transport
last_seen_at
call_count
created_at
updated_at
```

Token 通过 `session_id` 关联客户端会话：

```
mcp_token.session_id
```

规则：

- `display_name` 是用户填写的 AI 工具名。
- `client_key` 是用于去重和关联的规范化名称。
- 名称规范化：去首尾空格、转小写、移除空白字符。
- 同名 AI 工具不允许重复创建。
- 名称最大长度 80 个字符。
- 中文名称、英文名称、带空格名称和自定义工具名全部支持。
- 已知名称只影响展示样式，不影响业务身份。
- `assistant_type` 作为旧数据兼容字段保留，但不再作为前端业务约束。
- MCP 写入来源始终使用 Token 对应的 `display_name`。
- 改名只影响后续写入来源，历史记忆中的来源名称不回写。

### 2. Token 生命周期

每个动态客户端会话最多保留一个有效 Token：

- 创建 AI 工具：事务中创建会话和 Token。
- 编辑 AI 工具：修改名称、权限、项目范围和有效期，不改变 Token 明文。
- 轮换 Token：旧 Token 立即吊销，生成新 Token，继续绑定原会话。
- 吊销 Token：保留客户端卡片，状态变为“未启用”，允许重新生成。
- 删除客户端：本轮不实现，避免破坏 Java 版“卡片保留、Token 可重建”的行为。
- Token 明文继续使用 DPAPI `CurrentUser` 加密保存。
- Token 页面默认雾化，悬停、聚焦或复制时按需解密。
- 离开页面、窗口失焦、切换菜单或按 Esc 后清除前端明文。

## 三、接口修订

### 1. 动态客户端接口

新增桌面管理接口：

```
GET    /api/mcp/clients
POST   /api/mcp/clients
GET    /api/mcp/clients/{sessionId}
PATCH  /api/mcp/clients/{sessionId}
POST   /api/mcp/clients/{sessionId}/rotate
POST   /api/mcp/clients/{sessionId}/revoke
GET    /api/mcp/clients/{sessionId}/secret
POST   /api/mcp/clients/{sessionId}/test
```

创建请求：

```
{
  "displayName": "WorkBuddy",
  "permission": "ReadWrite",
  "projectId": null,
  "expiresAt": null
}
```

返回的安全卡片数据包含：

```
sessionId
clientKey
displayName
permission
projectId
projectName
tokenPrefix
expiresAt
lastUsedAt
callCount
revokedAt
status
```

明文 Token 只在单独的 secret 接口返回，并设置：

```
Cache-Control: no-store
```

现有 `/api/mcp/tokens` 接口保留兼容，但前端连接页面改用 `/api/mcp/clients`，避免把 Token 表直接当成助手卡片。

### 2. 数据库迁移

新增一次 SQLite 增量迁移：

- 创建 `mcp_client_session`。
- 为 `mcp_token` 增加 `session_id`。
- 为已有 Token 自动创建对应客户端会话。
- 旧 `assistant_type` 映射为历史兼容字段。
- 为 `client_key` 创建唯一约束。
- 保留已有 Token、权限、项目范围、有效期、吊销状态和密文。
- 无法安全推导名称的旧记录使用原 `display_name`，禁止丢失。
- 迁移必须支持第二轮和当前第三轮数据库无损升级。

## 四、连接 AI 页面重做

### 1. 页面结构

删除当前固定的：

```
Codex
Claude
Cursor
Trae
通用 MCP
```

改为：

```
连接 AI 页面
├── 页面标题与 MCP 服务状态
├── 已接入的 AI
│   ├── 动态客户端卡片
│   ├── 动态客户端卡片
│   └── 连接你的 AI 新增卡片
└── 连接配置弹窗
```

卡片数量完全由数据库中的客户端会话决定。

初次安装没有数据时：

- 不自动创建 Codex、Claude、Cursor、Trae 或通用 MCP。
- 只显示“连接你的 AI”新增卡片。
- 用户创建什么名称，页面就显示什么名称。

### 2. 动态客户端卡片

每张卡片显示：

- 用户自定义的 AI 工具名称。
- 根据名称生成首字母或简短标识。
- 已知名称使用 Java 目录中的颜色和图标提示。
- 未知名称使用通用 MCP 样式。
- 连接状态：未启用、已过期、尚未调用、最近调用。
- 权限：只读或读写。
- 项目范围：全部项目或指定项目。
- 有效期。
- 最近调用时间。
- 操作：编辑、查看配置。

卡片不直接展开完整创建表单，只保留清晰的主操作。

### 3. 新增客户端卡片

“连接你的 AI”卡片点击后打开统一弹窗，表单包含：

- AI 工具名称。
- 权限选择。
- 项目范围选择。
- 有效期选择。
- 生成 Token。

示例名称允许：

```
Codex
Claude Desktop
Cursor
Trae
WorkBuddy
我的本地助手
任意自定义 MCP 客户端
```

### 4. 统一连接弹窗

弹窗分为两个标签：

#### 编辑 AI 工具

- 新建时填写工具名称并生成 Token。
- 已有客户端可修改名称、权限、项目范围和有效期。
- 显示 Token 前缀和当前状态。
- 提供重新生成、吊销和保存修改。
- 生成或轮换后进入 Token 查看区域。

#### 接入配置

- 选择当前客户端 Token。
- 提供 `Streamable HTTP` 与 `stdio` 分段控制。
- HTTP 固定使用：

```
http://127.0.0.1:10212/mcp
```

- stdio 使用：

```
统一AI记忆.exe --stdio
```

- 配置中的 Token 默认雾化。
- 悬停或键盘聚焦显示完整 Token。
- “复制配置”必须先按需解密，不能复制占位符。
- 每个自定义 AI 工具都能生成两类通用 MCP 配置。
- 不根据名称硬编码客户端配置类型；名称与配置传输方式解耦。

## 五、视觉与交互规范

设计基于当前桌面端浅色主题，不新增一套孤立设计系统。

### 布局

- 桌面端客户端卡片使用两列布局。
- 平板端允许两列压缩。
- 手机端切换为单列。
- 内容区域最大宽度约 1200px。
- 卡片内部使用紧凑的 8/12/16/24px 间距。
- 卡片圆角控制在 8px 以内，弹窗可使用更明显的层级圆角。
- 不在卡片内部继续嵌套装饰性卡片。

### 视觉层级

- 页面标题是一级信息。
- “连接你的 AI”是新增客户端的唯一主操作。
- 卡片操作按钮为次级操作。
- 吊销使用危险色，但必须保留恢复方式。
- Token 区域使用稳定宽度，雾化与明文切换不能改变布局。
- 长名称使用换行或省略，不得撑破卡片。
- 状态不能只依赖颜色，同时显示文字。

### 无障碍

- 所有输入框都有明确 label。
- 所有弹窗按钮支持键盘操作。
- Tab 标签使用 `role="tablist"`、`aria-selected`。
- Token 区域可通过键盘聚焦查看。
- Esc 关闭弹窗并恢复 Token 雾化。
- 错误信息显示在当前操作附近，并保留全局提示。
- 手机端不依赖 hover，复制和聚焦必须可用。

## 六、实现顺序

1. 保存本计划为：

```
docs/第三轮连接AI动态化修订执行计划.md
```

1. 继续只读分析 Java 版相关实现，确认接口字段、卡片状态和配置弹窗行为。
2. 新增 `mcp_client_session` 数据模型及 SQLite 增量迁移。
3. 将现有 Token 自动迁移为动态客户端会话。
4. 重构 Token 服务，支持创建、查询、更新、轮换、吊销和会话关联。
5. 增加动态客户端 REST 接口，保留旧 Token 接口兼容。
6. 删除前端固定 `assistantCards`、`McpAssistantType` 业务分支和固定五类创建逻辑。
7. 新增动态客户端列表、空状态和新增卡片。
8. 实现统一的“编辑 AI 工具 / 接入配置”弹窗。
9. 接入名称、权限、项目范围和有效期表单。
10. 保留 DPAPI Token 可恢复解密、雾化、聚焦、复制和失焦清理。
11. 将 HTTP/stdio 配置生成改为与客户端名称无关的通用生成器。
12. 补齐客户端卡片的测试连接、编辑、轮换和吊销操作。
13. 增加动态名称、重复名称、过期 Token、吊销 Token 和旧数据迁移测试。
14. 使用 Playwright 验证桌面宽屏、平板和手机布局。
15. 执行 TypeScript、Vitest、后端、协议和 Release 回归。
16. 重新生成 `0.4.0` Windows x64 绿色版和中文验收报告。

## 七、验收标准

必须满足：

- 初次安装不显示固定的五类 AI 卡片。
- 用户可以创建任意名称的 AI 工具。
- 创建 `WorkBuddy`、中文名称和带空格名称均成功。
- 同名工具不能重复创建。
- Java 版已有客户端命名和卡片行为可被当前桌面版理解。
- 已有 Token 和记忆数据无损升级。
- 用户可以编辑名称、权限、项目范围和有效期。
- 改名后新的 MCP 写入来源严格等于新名称。
- 旧记忆来源不被批量修改。
- 吊销后客户端卡片仍保留并可重新生成。
- Token 默认雾化，可重复悬停、聚焦和复制。
- 页面切换、窗口失焦和 Esc 后 Token 恢复雾化。
- HTTP 配置始终使用 10212。
- stdio 不监听任何端口。
- REST 继续使用 18461。
- 16 个 MCP 工具、权限、候选事务和固定端口规则全部保持原计划行为。
- 桌面端连接页不再出现固定五卡片表单布局。
- 桌面端和移动端截图中不出现文字重叠、卡片溢出、按钮换行错位或弹窗超出视口。

## 八、明确假设

- 保留此前已经确定的 DPAPI 可恢复 Token 行为，不改回 Java 版“只展示一次”。
- 保留第三轮计划的单项目范围模型：空值表示全部项目，非空表示一个绑定项目。
- Java 版的多项目勾选能力暂不扩展到本轮数据库模型。
- 已知客户端目录只作为视觉和说明目录，不能预创建业务客户端。
- 所有自定义客户端默认都能生成通用 Streamable HTTP 和 stdio 配置。
- Codex、Claude、Cursor、Trae 等名称仅作为用户可填写的名称，不再作为固定业务枚举。
- 不修改 `desktop-client` 目录外的 Java 版用户改动。