; NSIS 安装/卸载钩子：解决覆写/删除 MemStack-MCP.exe 报"无法打开要写入的文件"。
; 该 exe 由 AI 客户端（Codex/TraeWork 等）以 stdio 方式拉起且长驻；强杀后客户端
; 会在数百毫秒内重启子进程，文件再次被锁——单纯 taskkill 无法保证后续 File 成功。
; 关键手段：Windows 允许重命名正在运行的 exe（映像锁只禁止写/删，允许同卷改名），
; 先把旧 exe 改名为 .old 让路，NSIS 写入新文件畅通无阻；.old 由安装收尾/下次安装清理。
; taskkill 未命中进程时无副作用；nsExec 静默执行不弹黑窗口。

!macro KillMcpProcesses
  nsExec::Exec 'taskkill /F /T /IM MemStack-MCP.exe'
  Pop $0
  ; 客户端重启后锁的是改名前的旧文件（映像名 MemStack-MCP.exe.old），一并清理。
  nsExec::Exec 'taskkill /F /T /IM MemStack-MCP.exe.old'
  Pop $0
  Sleep 500
!macroend

; 改名让路：正在运行的 exe 无法写入但可改名；.old 先删（上次残留），失败则忽略。
!macro RenameMcpExeAside
  Delete "$INSTDIR\MemStack-MCP.exe.old"
  IfFileExists "$INSTDIR\MemStack-MCP.exe" 0 +2
    Rename "$INSTDIR\MemStack-MCP.exe" "$INSTDIR\MemStack-MCP.exe.old"
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro KillMcpProcesses
  !insertmacro RenameMcpExeAside
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; 收尾清理 .old：此刻若 AI 客户端重启了 MCP，其锁的是 .old 文件，杀掉后删除；
  ; 失败静默（残留到下次安装/卸载再清）。
  nsExec::Exec 'taskkill /F /T /IM MemStack-MCP.exe.old'
  Pop $0
  Sleep 300
  Delete "$INSTDIR\MemStack-MCP.exe.old"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro KillMcpProcesses
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  !macroend
