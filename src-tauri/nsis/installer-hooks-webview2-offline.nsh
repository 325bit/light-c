; ============================================================================
; LightC NSIS hooks - WebView2 offline installer
; ============================================================================

!macro NSIS_HOOK_PREINSTALL
  ; The offline package ships its own WebView2 installer, so only handle stale process locks here; do not block on a missing WebView2 Runtime.
  Call LightC_CloseRunningProcess
!macroend

Function LightC_CloseRunningProcess
  nsExec::ExecToStack 'taskkill /IM "LightC.exe" /F'
  Pop $0
  Pop $1
  Sleep 800
FunctionEnd
