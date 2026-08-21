
# 关闭已有进程
Get-Process memstack-desktop -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1
# 启动新实例（不关联终端）
$exePath = 'E:\AICoding\mcp-ai-memory\desktop-client\target\release\memstack-desktop.exe'
$pinfo = New-Object System.Diagnostics.ProcessStartInfo
$pinfo.FileName = $exePath
$pinfo.UseShellExecute = $true  # 从 Shell 上下文启动，不经终端沙箱
$pinfo.WindowStyle = 'Normal'
$proc = [System.Diagnostics.Process]::Start($pinfo)
Start-Sleep -Seconds 2
$check = Get-Process memstack-desktop -ErrorAction SilentlyContinue
if ($check) { Write-Output "OK PID=$($check.Id)" } else { Write-Output "FAIL" }
