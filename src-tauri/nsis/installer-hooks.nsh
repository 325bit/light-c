; ============================================================================
; LightC NSIS hooks - normal installer
; ============================================================================

!macro NSIS_HOOK_PREINSTALL
  ; During updates or manual reinstalls, an old LightC.exe may still lock target files; close it first to avoid "cannot open file for writing".
  Call LightC_CloseRunningProcess
  ; The regular installer no longer downloads WebView2 automatically: Microsoft's bootstrapper can create extra Edge shortcuts in unusual environments.
  Call LightC_CheckWebView2Runtime
!macroend

Function LightC_CloseRunningProcess
  nsExec::ExecToStack 'taskkill /IM "LightC.exe" /F'
  Pop $0
  Pop $1
  Sleep 800
FunctionEnd

Function LightC_CheckWebView2Runtime
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  Call LightC_IsValidWebView2Version
  Pop $1
  StrCmp $1 "1" done

  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  Call LightC_IsValidWebView2Version
  Pop $1
  StrCmp $1 "1" done

  ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  Call LightC_IsValidWebView2Version
  Pop $1
  StrCmp $1 "1" done

  MessageBox MB_OK|MB_ICONSTOP "当前系统未检测到 Microsoft Edge WebView2 Runtime。$\r$\n$\r$\n常规安装包不会再联网自动安装 WebView2，以避免创建额外 Edge 快捷方式或触发异常安装。$\r$\n$\r$\n请从官方 Release 下载带版本号的 WebView2 离线安装包，或先安装 Microsoft Edge WebView2 Runtime 后再运行常规安装包。"
  Abort

  done:
FunctionEnd

Function LightC_IsValidWebView2Version
  StrCmp $0 "" invalid
  StrCmp $0 "0.0.0.0" invalid
  Push "1"
  Return

  invalid:
    Push "0"
FunctionEnd
