# 绿色版交付整理脚本（第五轮 T7）：
# 从 target/release 组装 MemStack-Portable-<版本>/ 发布目录：
#   MemStack.exe（重命名自 memstack-desktop.exe）+ MemStack-MCP.exe + version.txt（含 SHA-256）
# 并核对发布目录不含 WebView2 用户缓存（webview 数据固定写入 %LOCALAPPDATA%）。
param(
    [string]$Version = "0.4.0"
)
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$release = Join-Path $root "target\release"
$outDir = Join-Path $root "MemStack-Portable-$Version"

$desktopExe = Join-Path $release "memstack-desktop.exe"
$mcpExe = Join-Path $release "MemStack-MCP.exe"
foreach ($file in @($desktopExe, $mcpExe)) {
    if (-not (Test-Path $file)) {
        throw "缺少构建产物：$file（请先执行 cargo build --release）"
    }
}

if (Test-Path $outDir) {
    Remove-Item $outDir -Recurse -Force
}
New-Item -ItemType Directory -Path $outDir | Out-Null

$mainExe = Join-Path $outDir "MemStack.exe"
Copy-Item $desktopExe $mainExe
Copy-Item $mcpExe (Join-Path $outDir "MemStack-MCP.exe")

$desktopHash = (Get-FileHash $mainExe -Algorithm SHA256).Hash.ToLower()
$mcpHash = (Get-FileHash (Join-Path $outDir "MemStack-MCP.exe") -Algorithm SHA256).Hash.ToLower()
$versionText = @(
    "MemStack 绿色版 $Version",
    "构建时间：$((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))",
    "",
    "MemStack.exe      SHA-256 = $desktopHash",
    "MemStack-MCP.exe  SHA-256 = $mcpHash",
    "",
    "使用说明：解压到任意目录直接运行；移动目录后 AI 客户端接入配置会失效，",
    "在「连接你的 AI」页面会显示黄色警示，点击「一键重新注册」即可修复。"
) -join "`r`n"
Set-Content -Path (Join-Path $outDir "version.txt") -Value $versionText -Encoding UTF8

# 核对：发布目录不得包含 WebView2 用户缓存目录（数据固定在 %LOCALAPPDATA%\MemStack\webview）。
$webviewDir = Join-Path $outDir "webview"
if (Test-Path $webviewDir) {
    throw "发布目录包含 WebView2 缓存（webview\），禁止分发"
}

$totalMb = [math]::Round(((Get-ChildItem $outDir | Measure-Object Length -Sum).Sum / 1MB), 2)
Write-Host "绿色版组装完成：$outDir（共 $totalMb MB）"
Write-Host "  MemStack.exe       SHA-256 = $desktopHash"
Write-Host "  MemStack-MCP.exe SHA-256 = $mcpHash"
