Get-Process memstack-desktop -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1
$exePath = 'E:\AICoding\mcp-ai-memory\desktop-client\target\release\memstack-desktop.exe'
$pinfo = New-Object System.Diagnostics.ProcessStartInfo
$pinfo.FileName = $exePath
$pinfo.UseShellExecute = $true
$pinfo.WindowStyle = 'Normal'
$proc = [System.Diagnostics.Process]::Start($pinfo)
Start-Sleep -Seconds 4
$check = Get-Process memstack-desktop -ErrorAction SilentlyContinue
if ($check) { Write-Output 'LAUNCH_OK PID=' + $check.Id } else { Write-Output 'LAUNCH_FAIL' }
