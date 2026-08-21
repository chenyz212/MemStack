<#
.SYNOPSIS
    忆栈 (MemStack) 一键构建脚本
.DESCRIPTION
    依次执行：前端打包 -> MCP 服务端编译 -> Tauri 应用打包 -> NSIS 安装程序
.PARAMETER SkipFrontend
    跳过前端构建（已手动 build 过时使用）
.PARAMETER SkipMcp
    跳过 MCP 服务端编译
.EXAMPLE
    .\build.ps1
    .\build.ps1 -SkipFrontend
#>
param(
    [switch]$SkipFrontend,
    [switch]$SkipMcp
)

$root = $PSScriptRoot
$rustHome = Join-Path $root ".rust-home"

# ---- Rust 环境变量 ----
$env:CARGO_HOME  = Join-Path $rustHome "cargo"
$env:RUSTUP_HOME = Join-Path $rustHome "rustup"
$env:Path = "$(Join-Path $env:CARGO_HOME 'bin');$env:Path"

function Invoke-Step {
    param([string]$Name, [scriptblock]$Action)
    Write-Host ""
    Write-Host "========== $Name ==========" -ForegroundColor Cyan
    & $Action
    if ($LASTEXITCODE -ne 0) {
        Write-Host "`n✗ $Name 失败 (exit $LASTEXITCODE)" -ForegroundColor Red
        exit 1
    }
    Write-Host "✓ $Name 完成" -ForegroundColor Green
}

# ---- 1. 前端构建 ----
if (-not $SkipFrontend) {
    Invoke-Step "前端构建 (vue-tsc + vite)" {
        Push-Location (Join-Path $root "memory-desktop-web")
        npm run build
        Pop-Location
    }
}

# ---- 2. MCP 服务端编译 ----
if (-not $SkipMcp) {
    Invoke-Step "MCP 服务端编译 (memstack-mcp-stdio)" {
        cargo build --release -p memstack-mcp-stdio
    }
}

# ---- 3. Tauri 应用打包 ----
Invoke-Step "Tauri 应用打包 (NSIS)" {
    cargo tauri build
}

# ---- 结果 ----
$nsisDir = Join-Path $root "target\release\bundle\nsis"
$installer = Get-ChildItem $nsisDir -Filter "*-setup.exe" -ErrorAction SilentlyContinue | Select-Object -First 1

Write-Host ""
Write-Host "========== 构建成功 ==========" -ForegroundColor Green
if ($installer) {
    $sizeMB = [math]::Round($installer.Length / 1MB, 2)
    Write-Host "安装包: $($installer.Name)"
    Write-Host "路径:   $($installer.FullName)"
    Write-Host "大小:   $sizeMB MB"
    Write-Host "时间:   $($installer.LastWriteTime)"
} else {
    Write-Host "警告: 未在 $nsisDir 找到安装包" -ForegroundColor Yellow
}
