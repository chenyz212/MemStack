
$ErrorActionPreference = 'Continue'
$p = Start-Process -FilePath 'E:\AICoding\mcp-ai-memory\desktop-client\target\release\memstack-desktop.exe' -PassThru -RedirectStandardOutput 'E:\AICoding\mcp-ai-memory\desktop-client\testdata\graph-acceptance\stdout.log' -RedirectStandardError 'E:\AICoding\mcp-ai-memory\desktop-client\testdata\graph-acceptance\stderr.log' -WorkingDirectory 'E:\AICoding\mcp-ai-memory\desktop-client'
Start-Sleep -Seconds 8
if (-not $p.HasExited) { Write-Output 'RUNNING_PID=' + $p.Id; Stop-Process -Id $p.Id -Force } else { Write-Output 'EXITED_CODE=' + $p.ExitCode }
