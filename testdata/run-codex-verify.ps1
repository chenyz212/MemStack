# 第一轮 T8 Codex 真实验证脚本（需在用户本机 PowerShell 直接运行，不要经由 AI 沙箱终端）。
# 前提：config.toml 已注入 [mcp_servers.memstack]（docs/codex-config-memstack-injected.toml 有副本）。
# 动作：codex exec 发起真实会话 → 输出存 testdata/codex-verify-output.txt → 恢复原 config.toml。

$ErrorActionPreference = 'Continue'
$outFile = "e:\AICoding\mcp-ai-memory\desktop-client\testdata\codex-verify-output.txt"

Write-Host "== Codex MCP 真实验证开始（可能需要 1-3 分钟）=="
codex exec --skip-git-repo-check -C "e:\AICoding\mcp-ai-memory\desktop-client" "请用 memstack MCP 服务器的 memory_search 工具搜索关键词'迁移基线'，并把返回结果中记忆的 title 字段原样告诉我。" 2>&1 | Tee-Object -FilePath $outFile

Write-Host "`n== 恢复原 config.toml =="
python "e:\AICoding\mcp-ai-memory\desktop-client\testdata\tmp-inject-codex.py" restore
Write-Host "完成。请回到 TRAE 告知结果，或直接确认 testdata\codex-verify-output.txt 已生成。"
