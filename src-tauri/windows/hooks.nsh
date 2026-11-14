; Tauri NSIS installer hooks
;
; 安装 / 升级只结束 GUI（tether.exe）。后台 tetherd.exe 持有全部会话，不结束：Windows 允许重命名正在运行的 exe，
; 先把它改名为 tetherd.old.exe 让出文件名，新文件随后写入；旧 daemon 继续服务，直到用户在 GUI 的提示条中
; “重启后台”或重新登录，新 daemon 启动时删除 tetherd.old*.exe。
; 上次更新的 tetherd.old.exe 仍在运行（删不掉）时改用带时间后缀的名字；改名仍失败才退回结束 daemon（会话据 sessions.json 恢复）。
; 注意 taskkill /IM 按进程启动时的映像名匹配，改过名的旧 daemon 仍叫 tetherd.exe。
!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM tether.exe'
  Pop $0
  ${If} ${FileExists} "$INSTDIR\tetherd.exe"
    Delete "$INSTDIR\tetherd.old.exe"
    ClearErrors
    Rename "$INSTDIR\tetherd.exe" "$INSTDIR\tetherd.old.exe"
    ${If} ${Errors}
      System::Call 'kernel32::GetTickCount() i .r0'
      ClearErrors
      Rename "$INSTDIR\tetherd.exe" "$INSTDIR\tetherd.old-$0.exe"
    ${EndIf}
    ${If} ${Errors}
      nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM tetherd.exe'
      Pop $0
    ${EndIf}
  ${EndIf}
!macroend

; Register HKCU Run autostart + AppUserModelID for toasts（只写注册表，不影响运行中的旧 daemon）
!macro NSIS_HOOK_POSTINSTALL
  nsExec::Exec '"$INSTDIR\tetherd.exe" --install'
  Pop $0
!macroend

; 卸载：结束 GUI 与 daemon（含改过名的旧 daemon，映像名仍为 tetherd.exe），删除注册表项与更新遗留文件
!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM tether.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM tetherd.exe'
  Pop $0
  nsExec::Exec '"$INSTDIR\tetherd.exe" --uninstall'
  Pop $0
  Delete "$INSTDIR\tetherd.old*.exe"
!macroend
