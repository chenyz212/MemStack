# 第一轮 T4 基线测量脚本：冷启动 P95 + 进程树内存采样。
# 用途：在用户本机 PowerShell 中直接运行（不要经由 AI 沙箱终端），结果写入 bench-startup-result.json。
# 前提：正式客户端已退出（端口 18461 空闲）。

$ErrorActionPreference = 'Stop'
$exe = "e:\AICoding\mcp-ai-memory\desktop-client\MemStack-Portable-0.4.0\MemStack.exe"
$outFile = "e:\AICoding\mcp-ai-memory\desktop-client\testdata\bench-startup-result.json"

# --- 冷启动 5 次 ---
$cold = @()
for ($i = 1; $i -le 5; $i++) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $proc = Start-Process $exe -PassThru
    $up = $false
    while ($sw.Elapsed.TotalSeconds -lt 90) {
        try {
            $resp = Invoke-WebRequest "http://127.0.0.1:18461/api/health" -UseBasicParsing -TimeoutSec 1
            if ($resp.StatusCode -eq 200 -and $resp.Content -match "UP") { $up = $true; break }
        } catch { Start-Sleep -Milliseconds 120 }
    }
    $sw.Stop()
    $cold += [ordered]@{ run = $i; up = $up; ms = $sw.ElapsedMilliseconds }
    Write-Host ("run {0}: up={1} {2}ms" -f $i, $up, $sw.ElapsedMilliseconds)
    taskkill /PID $proc.Id /T /F | Out-Null
    Start-Sleep -Seconds 2
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Date) -lt $deadline) {
        if (-not (Get-NetTCPConnection -LocalPort 18461 -State Listen -ErrorAction SilentlyContinue)) { break }
        Start-Sleep -Milliseconds 300
    }
}
$sorted = ($cold | ForEach-Object { $_.ms } | Sort-Object)
$p95 = $sorted[[Math]::Ceiling($sorted.Count * 0.95) - 1]

# --- 内存采样：启动后空闲 60 秒，统计进程树工作集 ---
$proc = Start-Process $exe -PassThru
$deadline = (Get-Date).AddSeconds(90)
do {
    Start-Sleep -Milliseconds 200
    $up = $false
    try {
        $resp = Invoke-WebRequest "http://127.0.0.1:18461/api/health" -UseBasicParsing -TimeoutSec 1
        if ($resp.StatusCode -eq 200 -and $resp.Content -match "UP") { $up = $true }
    } catch {}
} while (-not $up -and (Get-Date) -lt $deadline)
Start-Sleep -Seconds 60
$all = Get-CimInstance Win32_Process
$ids = New-Object System.Collections.Generic.List[int]
$ids.Add($proc.Id)
$changed = $true
while ($changed) {
    $changed = $false
    foreach ($p in $all) {
        if ($ids.Contains([int]$p.ParentProcessId) -and -not $ids.Contains([int]$p.ProcessId)) {
            $ids.Add([int]$p.ProcessId); $changed = $true
        }
    }
}
$workingSetMb = [math]::Round((Get-Process | Where-Object { $ids.Contains($_.Id) } | Measure-Object WorkingSet64 -Sum).Sum / 1MB, 1)
$processCount = $ids.Count
taskkill /PID $proc.Id /T /F | Out-Null

# --- 体积与哈希 ---
$exeItem = Get-Item $exe
$sizeMb = [math]::Round($exeItem.Length / 1MB, 2)
$sha256 = (Get-FileHash $exe -Algorithm SHA256).Hash

$result = [ordered]@{
    measuredAt  = (Get-Date).ToString("o")
    coldStarts  = $cold
    coldStartP95ms = $p95
    memoryIdleMb = $workingSetMb
    processTreeCount = $processCount
    portableExeSizeMb = $sizeMb
    portableExeSha256 = $sha256
}
$result | ConvertTo-Json | Out-File $outFile -Encoding utf8
Write-Host "`n结果已写入 $outFile"
Write-Host ("冷启动 P95 = {0}ms；空闲内存 = {1}MB（{2} 个进程）；体积 = {3}MB" -f $p95, $workingSetMb, $processCount, $sizeMb)
