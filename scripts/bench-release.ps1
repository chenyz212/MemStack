# Release 性能测量脚本（第五轮 T8，对照总计划 §15）：
#   1) 双 exe 体积与 SHA-256
#   2) 二万条检索 P95（复用 cargo test --release search_perf）
#   3) 冷启动至窗口可交互 P95（MEMSTACK_DB_PATH 隔离库，不触发首启备份/不动生产数据）
#   4) 主进程 + WebView2 进程树空闲工作集（稳定 60s 后采样）
# 各阶段独立容错；任一阶段失败不影响其余结果落盘 docs/发布性能数据-<版本>.md。
param(
    [string]$Version = "0.4.0",
    [int]$ColdStartRuns = 20,
    [switch]$SkipCargoPerf,
    [switch]$SkipAppPhases
)
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$release = Join-Path $root "target\release"
$desktopExe = Join-Path $release "memstack-desktop.exe"
$mcpExe = Join-Path $release "MemStack-MCP.exe"
foreach ($file in @($desktopExe, $mcpExe)) {
    if (-not (Test-Path $file)) { throw "缺少构建产物：$file" }
}

$results = [ordered]@{}
$results["产物体积"] = @{
    "MemStack.exe (memstack-desktop.exe)" = "{0:N2} MB" -f ((Get-Item $desktopExe).Length / 1MB)
    "MemStack-MCP.exe" = "{0:N2} MB" -f ((Get-Item $mcpExe).Length / 1MB)
    "合计" = "{0:N2} MB（目标 ≤ 35 MB）" -f (((Get-Item $desktopExe).Length + (Get-Item $mcpExe).Length) / 1MB)
}
$results["SHA-256"] = @{
    "MemStack.exe" = (Get-FileHash $desktopExe -Algorithm SHA256).Hash.ToLower()
    "MemStack-MCP.exe" = (Get-FileHash $mcpExe -Algorithm SHA256).Hash.ToLower()
}

function Write-Report {
    param([string]$Reason = "")
    $lines = @(
        "# MemStack $Version 发布性能数据",
        "",
        "> 测量时间：$((Get-Date).ToString('yyyy-MM-dd HH:mm:ss'))（本机 Release 构建，scripts/bench-release.ps1 生成）",
        "> 对照总计划 §15 指标；超标项需形成偏差记录。$Reason",
        ""
    )
    foreach ($key in $results.Keys) {
        $lines += "## $key"
        $lines += ""
        $value = $results[$key]
        if ($value -is [string]) {
            $lines += "- $value"
        } else {
            foreach ($sub in $value.Keys) { $lines += "- ${sub}：$($value[$sub])" }
        }
        $lines += ""
    }
    $lines += "## 热启动与托盘恢复"
    $lines += ""
    $lines += "- 热启动（托盘恢复）保留 WebView 进程零销毁，恢复路径为纯 Win32 Show/SetFocus；"
    $lines += "  自动化脚本无法可靠模拟托盘交互，该指标以第四轮集成测试 + 用户日常使用确认（目标 < 400 ms）。"
    $lines += ""
    $lines += "## MCP stdio 单进程工作集"
    $lines += ""
    $lines += "- 由 mcp_stdio_e2e / mcp_concurrency 测试环境覆盖（5 进程并发常驻），单进程目标 ≤ 12 MB；"
    $lines += "- 手工复核命令：启动客户端连接后任务管理器查看 MemStack-MCP.exe 提交大小。"
    $lines += ""
    $outFile = Join-Path $root "docs\发布性能数据-$Version.md"
    Set-Content -Path $outFile -Value ($lines -join "`r`n") -Encoding UTF8
    Write-Host "性能数据已写入：$outFile"
}

function Stop-ProcessTree([int]$Id) {
    # /T 连同子进程（WebView2 渲染树）一起结束，避免孤儿进程继续写 webview 缓存。
    & taskkill /PID $Id /T /F 2>&1 | Out-Null
}

try {
    # ---- Rust 侧性能测试（先跑：不依赖 GUI 进程）----
    if (-not $SkipCargoPerf -and -not $results.Contains("检索性能")) {
        Write-Host "==> cargo test --release 性能用例（检索 20k）…"
        try {
            # 经 cmd.exe 合并 stderr：PS 5.1 会把 cargo 的编译进度（stderr）当错误流中断捕获。
            $perfOutput = (& cmd.exe /c "cargo test --release -p memory-application --test search_perf -- --nocapture 2>&1") -join "`n"
            $p95Lines = ($perfOutput -split "`n" | Where-Object { $_ -match "P95" }) -join "；"
            $results["检索性能"] = if ($p95Lines) { $p95Lines } else { "search_perf 已执行（未见 P95 行，详见 cargo 输出）" }
        } catch {
            $results["检索性能"] = "执行失败：$($_.Exception.Message)"
        }
    }

    if ($SkipAppPhases) { throw [System.Exception]::new("skip-app-phases") }

    if (Get-Process -Name "memstack-desktop" -ErrorAction SilentlyContinue) {
        throw "检测到 memstack-desktop 正在运行；性能测量需独占环境，请先退出 MemStack（托盘右键退出）。"
    }

    # App 阶段整体隔离 LOCALAPPDATA：webview 缓存/日志会写
    # %LOCALAPPDATA%\MemStack（沙箱禁止触生产目录），重定向到临时目录。
    $benchLocal = Join-Path $env:TEMP "memstack-bench-local-$Version"
    New-Item -ItemType Directory -Force $benchLocal | Out-Null
    $savedLocalAppData = $env:LOCALAPPDATA
    $env:LOCALAPPDATA = $benchLocal

    # ---- 冷启动至窗口可交互 P95 ----
    Write-Host "==> 冷启动测量（$ColdStartRuns 次）…"
    $isolatedDb = Join-Path $env:TEMP "memstack-bench-$Version.db"
    Remove-Item $isolatedDb -Force -ErrorAction SilentlyContinue
    Remove-Item "$isolatedDb-wal", "$isolatedDb-shm" -Force -ErrorAction SilentlyContinue
    # MEMSTACK_DB_PATH 隔离：既不动生产数据，也跳过首启备份（显式覆盖语义）。
    $env:MEMSTACK_DB_PATH = $isolatedDb
    $coldMs = @()
    for ($i = 0; $i -lt $ColdStartRuns; $i++) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $proc = Start-Process -FilePath $desktopExe -PassThru -WorkingDirectory $release
        try {
            # 轮询主窗口句柄非零 = 窗口已创建可交互（10s 上限）。
            $handle = [IntPtr]::Zero
            for ($tick = 0; $tick -lt 1000; $tick++) {
                $proc.Refresh()
                if ($proc.HasExited) { throw "第 $($i+1) 次启动进程提前退出" }
                if ($proc.MainWindowHandle -ne 0) { $handle = $proc.MainWindowHandle; break }
                Start-Sleep -Milliseconds 10
            }
            $sw.Stop()
            if ($handle -eq [IntPtr]::Zero) { throw "第 $($i+1) 次启动 10s 内未见主窗口" }
            $coldMs += $sw.ElapsedMilliseconds
        } finally {
            Stop-ProcessTree $proc.Id
            Start-Sleep -Milliseconds 500
        }
    }
    $sorted = $coldMs | Sort-Object
    $p95 = $sorted[[math]::Ceiling($sorted.Count * 0.95) - 1]
    $mean = ($coldMs | Measure-Object -Average).Average
    $results["冷启动"] = "P95 = $p95 ms（均值 $([math]::Round($mean,1)) ms，$ColdStartRuns 次；目标 < 800 ms）"

    # ---- 空闲工作集（进程树）----
    Write-Host "==> 空闲工作集采样（稳定 60s）…"
    $proc = Start-Process -FilePath $desktopExe -PassThru -WorkingDirectory $release
    try {
        for ($tick = 0; $tick -lt 1000; $tick++) {
            $proc.Refresh()
            if ($proc.MainWindowHandle -ne 0) { break }
            Start-Sleep -Milliseconds 10
        }
        $minStart = $proc.StartTime
        Start-Sleep -Seconds 60
        $proc.Refresh()
        $mainWs = $proc.WorkingSet64 / 1MB

        function Get-TreeWorkingSet([int]$ParentId, [datetime]$MinStart) {
            # StartTime 过滤：冷启动轮次中被 taskkill 强杀的 WebView2 孤儿进程
            # ParentProcessId 残留，PID 复用会被误计入进程树，必须按启动时间排除。
            $total = 0.0
            $children = Get-CimInstance Win32_Process -Filter "ParentProcessId = $ParentId" -ErrorAction SilentlyContinue
            foreach ($child in $children) {
                $childProc = Get-Process -Id $child.ProcessId -ErrorAction SilentlyContinue
                if ($childProc -and $childProc.StartTime -ge $MinStart) {
                    $total += $childProc.WorkingSet64 / 1MB
                    $total += Get-TreeWorkingSet $child.ProcessId $MinStart
                }
            }
            return $total
        }
        $treeWs = $mainWs + (Get-TreeWorkingSet $proc.Id $minStart)
        $results["空闲工作集"] = @{
            "桌面主进程" = "{0:N1} MB（目标 ≤ 15 MB）" -f $mainWs
            "完整 WebView2 进程树" = "{0:N1} MB（目标 ≤ 60 MB）" -f $treeWs
            "口径说明" = "沙箱内测量含 WebView2 全进程树（browser/gpu/renderer/utility）WorkingSet；超标项以用户本机任务管理器复核为准并形成偏差记录"
        }
    } finally {
        Stop-ProcessTree $proc.Id
    }
} catch {
    $message = $_.Exception.Message
    if ($message -ne "skip-app-phases") {
        Write-Warning "阶段失败（结果可能不完整）：$message"
        if (-not $results.Contains("执行说明")) {
            $results["执行说明"] = "部分阶段未完成：$message"
        }
    }
} finally {
    if ($null -ne $savedLocalAppData) { $env:LOCALAPPDATA = $savedLocalAppData }
    Remove-Item Env:\MEMSTACK_DB_PATH -ErrorAction SilentlyContinue
    Write-Report
}
