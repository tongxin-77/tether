; Tauri NSIS installer hooks
; Kill running GUI/daemon so exe files are not locked (this ends running sessions).
!macro TETHER_KILL
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM tether.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM tetherd.exe'
  Pop $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro TETHER_KILL
!macroend

; Register HKCU Run autostart + AppUserModelID for toasts
!macro NSIS_HOOK_POSTINSTALL
  nsExec::Exec '"$INSTDIR\tetherd.exe" --install'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro TETHER_KILL
  nsExec::Exec '"$INSTDIR\tetherd.exe" --uninstall'
  Pop $0
!macroend
