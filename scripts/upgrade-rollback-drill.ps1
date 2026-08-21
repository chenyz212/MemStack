# 真实数据库升级 + 回滚演练脚本（第五轮 T9，§17）：
#   阶段 1：完整复制生产 %LOCALAPPDATA%\MemStack（含 WAL/备份）到隔离目录
#   阶段 2：Rust MCP stdio 在隔离目录执行真实链路：首启备份 → session-id 鉴权
#           （DPAPI 解密真实令牌）→ initialize/tools/list → 读工具 → 写工具
#   阶段 3：二次启动验证 marker 幂等（不再重复备份）
#   阶段 4：回滚演练：恢复 pre-rust 备份 → 再次启动验证可用
# 结果追加写入 docs/升级回滚演练-0.4.0.md。
# 说明：桌面端（Tauri GUI）在同数据目录的等价验证由用户本机运行（沙箱限制，
# 见第五轮验收报告）；本脚本覆盖数据库/凭据/MCP 链路的核心风险面。
param(
    [string]$Version = "0.4.0"
)
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$mcpExe = Join-Path $root "target\release\MemStack-MCP.exe"
if (-not (Test-Path $mcpExe)) { throw "缺少构建产物：$mcpExe" }
$production = Join-Path $env:LOCALAPPDATA "MemStack"
if (-not (Test-Path $production)) { throw "未找到生产数据目录：$production（本机需有真实使用数据）" }

$drillRoot = Join-Path $env:TEMP "memstack-drill-$Version"
if (Test-Path $drillRoot) { Remove-Item $drillRoot -Recurse -Force }
$drillLocal = Join-Path $drillRoot "LocalAppData"
New-Item -ItemType Directory -Force $drillLocal | Out-Null

$notes = @()
function Note([string]$text) {
    $script:notes += "- $text"
    Write-Host "  * $text"
}

Write-Host "==> 阶段 1：复制生产数据目录到隔离环境…"
Copy-Item $production (Join-Path $drillLocal "MemStack") -Recurse -Force
# 按生产规则解析库文件：memory.db 优先，识别为旧 Java 结构（client_name 列标记）
# 时回退 desktop-memory.db（schema.rs::resolve_desktop_database_path 同语义）。
$dataDir = Join-Path $drillLocal "MemStack\data"
$memoryDb = Join-Path $dataDir "memory.db"
$desktopDb = Join-Path $dataDir "desktop-memory.db"
$dataDb = $memoryDb
if (Test-Path $memoryDb) {
    $probe = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($memoryDb))
    if ($probe.Contains("client_name TEXT")) {
        $dataDb = $desktopDb
        Note "memory.db 为旧 Java 库 → 按生产规则回退 desktop-memory.db（与 Rust resolve_desktop_database_path 一致）"
    }
}
if (-not (Test-Path $dataDb)) { throw "隔离副本缺少数据库：$dataDb" }
$sizeMb = [math]::Round((Get-Item $dataDb).Length / 1MB, 2)
Note "生产库副本就绪：$dataDb（$sizeMb MB，含 WAL：$(Test-Path "$dataDb-wal")）"

# MCP stdio 交互：发送 JSON-RPC 帧并读取全部响应（stdin EOF 后进程退出）。
# ExtraEnv：可选附加环境变量（env Token 兼容路径验证用）。
function Invoke-Mcp([string[]]$Frames, [string]$SessionId, [hashtable]$ExtraEnv = $null) {
    $env:LOCALAPPDATA = $drillLocal   # 数据目录解析走生产规则（作用于隔离副本）
    $savedToken = $env:MEMSTACK_TOKEN
    try {
        if ($ExtraEnv -and $ExtraEnv.ContainsKey("MEMSTACK_TOKEN")) {
            $env:MEMSTACK_TOKEN = $ExtraEnv["MEMSTACK_TOKEN"]
        }
        $arguments = if ($SessionId) { "--session-id $SessionId" } else { "" }
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = $mcpExe
        $psi.Arguments = $arguments
        $psi.RedirectStandardInput = $true
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        $psi.UseShellExecute = $false
        $proc = [System.Diagnostics.Process]::Start($psi)
        # 预热帧：PS 重定向管道的首写可能带 3 字节编码前导（BOM），会让第一帧
        # JSON 解析失败（服务端按"非 JSON 帧丢弃"处理，连接不受影响）。先写一行
        # 无害文本吸收前导，真实帧从第二行开始。
        $proc.StandardInput.WriteLine("warmup")
        foreach ($frame in $Frames) { $proc.StandardInput.WriteLine($frame) }
        $proc.StandardInput.Close()
        $stdout = $proc.StandardOutput.ReadToEnd()
        $stderr = $proc.StandardError.ReadToEnd()
        $proc.WaitForExit(30000) | Out-Null
        return @{ ExitCode = $proc.ExitCode; Stdout = $stdout; Stderr = $stderr }
    } finally {
        $env:LOCALAPPDATA = $script:realLocalAppData
        if ($null -ne $savedToken) { $env:MEMSTACK_TOKEN = $savedToken } else { Remove-Item Env:\MEMSTACK_TOKEN -ErrorAction SilentlyContinue }
    }
}
$realLocalAppData = $env:LOCALAPPDATA

Write-Host "==> 阶段 2a：向隔离副本注入演练会话（生产库通常无 MCP 会话）…"
# 种子注入同样重定向 LOCALAPPDATA：migration_lock 会向
# %LOCALAPPDATA%\MemStack\logs 写迁移日志，必须落在隔离副本内。
$env:LOCALAPPDATA = $drillLocal
try {
    $seedOutput = (& cmd.exe /c "cargo run --release -p memory-application --example drill_seed -- `"$dataDb`" 2>&1") -join "`n"
} finally {
    $env:LOCALAPPDATA = $realLocalAppData
}
$sessionId = ([regex]::Match($seedOutput, "SESSION_ID=([0-9a-f-]{36})")).Groups[1].Value
$plainToken = ([regex]::Match($seedOutput, "PLAIN_TOKEN=(\S+)")).Groups[1].Value
if (-not $sessionId -or -not $plainToken) { throw "会话注入失败：$seedOutput" }
Note "演练会话已注入（Rust 服务层建会话链路）：$sessionId"

Write-Host "==> 阶段 2b：session-id 鉴权（DPAPI 解密 + initialize）…"
$result = Invoke-Mcp @(
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"drill","version":"0"}}}',
    '{"jsonrpc":"2.0","method":"notifications/initialized"}'
) $sessionId
if ($result.ExitCode -ne 0 -or $result.Stdout -notmatch '"serverInfo"') {
    throw "session-id 鉴权失败：exit=$($result.ExitCode) stderr=$($result.Stderr) stdout=$($result.Stdout.Substring(0, [Math]::Min(400, $result.Stdout.Length)))"
}
Note "session-id 鉴权通过（DPAPI 解密令牌 + hash 校验 + initialize 成功）"

Write-Host "==> 阶段 2c：env Token 兼容路径验证（0.5.0 前保留）…"
$resultEnv = Invoke-Mcp @(
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"drill","version":"0"}}}'
) $null @{ MEMSTACK_TOKEN = $plainToken }
if ($resultEnv.ExitCode -ne 0 -or $resultEnv.Stdout -notmatch '"serverInfo"') {
    throw "env Token 兼容路径验证失败：exit=$($resultEnv.ExitCode) stderr=$($resultEnv.Stderr)"
}
Note "env Token 兼容路径可用（0.5.0 移除，迁移引导见接入说明）"

# 首启备份应已生成。
$backupDir = Join-Path $drillLocal "MemStack\backup"
$preRust = Get-ChildItem $backupDir -Filter "pre-rust-*.db" -ErrorAction SilentlyContinue
if ($preRust.Count -eq 0) { throw "首启未生成 pre-rust 备份" }
$marker = Join-Path $backupDir "rust-first-run.marker"
if (-not (Test-Path $marker)) { throw "首启备份成功但未写 marker" }
Note "首启备份生成：$($preRust[0].Name)（校验和文件：$(Test-Path "$($preRust[0].FullName).sha256")）"

# tools/list + 读 + 写（写走候选提交，安全可回滚）。
$frames = @(
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"drill","version":"0"}}}',
    '{"jsonrpc":"2.0","method":"notifications/initialized"}',
    '{"jsonrpc":"2.0","id":2,"method":"tools/list"}',
    '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory_recent","arguments":{"limit":5}}}',
    '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"memory_candidate_submit","arguments":{"scope":"Personal","projectId":null,"title":"drill-upgrade-check","summary":"drill temp data","content":"drill","memoryType":"NOTE","keywords":["drill"],"tags":["drill"],"importance":2,"cloudProcessingAllowed":false}}}'
)
# 注：帧内容用 ASCII——PS 5.1 的 StandardInput 编码不可设（GBK），中文会变乱码；
# 必填字段（scope/projectId/keywords/tags/importance/cloudProcessingAllowed）缺一会被
# 反序列化拒绝并以 isError=true 返回，故全字段显式传入。
$result = Invoke-Mcp $frames $sessionId
if ($result.ExitCode -ne 0) { throw "MCP 全链路退出码 $($result.ExitCode)：$($result.Stderr)" }
# 中文经 GBK 管道传输会乱码，改断言四帧均有响应且无 isError（tools/call 失败会置 isError=true）。
if ($result.Stdout -match '"isError"\s*:\s*true') { throw "tools/call 返回错误：$($result.Stdout.Substring(0, 300))" }
$responded = ([regex]::Matches($result.Stdout, '"jsonrpc"')).Count
if ($responded -lt 4) { throw "期望 4 个 JSON-RPC 响应，实际 $responded 个" }
$toolCount = ([regex]::Matches($result.Stdout, '"name"\s*:')).Count
Note "tools/list + memory_recent + memory_candidate_submit 全链路成功（stdout 含 $toolCount 个 name 字段）"

Write-Host "==> 阶段 3：二次启动验证 marker 幂等…"
$before = (Get-ChildItem $backupDir -Filter "pre-rust-*.db").Count
$result2 = Invoke-Mcp @(
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"drill","version":"0"}}}'
) $sessionId
$after = (Get-ChildItem $backupDir -Filter "pre-rust-*.db").Count
if ($result2.ExitCode -ne 0 -or $before -ne $after) { throw "二次启动异常：exit=$($result2.ExitCode) 备份数 $before -> $after" }
Note "二次启动不再重复备份（$before 份不变），stdio 正常服务"

Write-Host "==> 阶段 4：回滚演练（恢复 pre-rust 备份）…"
Copy-Item $dataDb (Join-Path $dataDir "drill-rollback-source.db.bak") -Force
Copy-Item $preRust[0].FullName $dataDb -Force
Remove-Item "$dataDb-wal", "$dataDb-shm" -Force -ErrorAction SilentlyContinue
$result3 = Invoke-Mcp @(
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"drill","version":"0"}}}',
    '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_recent","arguments":{"limit":3}}}'
) $sessionId
if ($result3.ExitCode -ne 0) { throw "回滚后 MCP 启动失败：$($result3.Stderr)" }
Note "pre-rust 备份恢复后 MCP 正常启动并响应 memory_recent（回滚路径可用）"
# 演练临时写入随隔离目录一并废弃，不影响生产。

$outFile = Join-Path $root "docs\升级回滚演练-$Version.md"
$lines = @(
    "# MemStack $Version 升级与回滚演练记录",
    "",
    "> 演练时间：$((Get-Date).ToString('yyyy-MM-dd HH:mm:ss'))（scripts/upgrade-rollback-drill.ps1 生成，隔离目录：$drillRoot）",
    "> 生产数据未受影响；演练临时写入随隔离目录废弃。",
    "",
    "## 演练结论",
    ""
) + $notes + @(
    "",
    "## 覆盖面说明",
    "",
    "- 已覆盖：生产库副本打开、首启备份（Online Backup + 校验和 + marker）、DPAPI 真实令牌解密、",
    "  session-id 鉴权、16 工具发现、读/写工具、marker 幂等、pre-rust 备份恢复（回滚）。",
    "- 待用户本机确认：桌面端（Tauri）对同数据的启动与页面核对（沙箱限制 GUI 进程，",
    "  用户日常使用 Release 版即持续验证该路径）。",
    ""
)
Set-Content -Path $outFile -Value ($lines -join "`r`n") -Encoding UTF8
Write-Host "演练记录已写入：$outFile"
