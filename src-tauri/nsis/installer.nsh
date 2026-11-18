; ============================================================================
; LightC Custom NSIS Installer Script
; ============================================================================
; Injected into the main installer script by Tauri's installerScript config
; Customizes the MUI2 UI, compression algorithm, language support, etc.

; ============================================================================
; 1. Compression - solid LZMA for the smallest installer
; ============================================================================
SetCompressor /SOLID lzma
SetCompressorDictSize 64

; ============================================================================
; 2. MUI2 visual customization
; ============================================================================

; Branding title
!define MUI_WELCOMEPAGE_TITLE "欢迎使用 LightC$\n轻量级 C 盘清理工具"
!define MUI_WELCOMEPAGE_TITLE_3LINES

; Welcome page text
!define MUI_WELCOMEPAGE_TEXT "LightC 是一款轻量级 Windows C 盘智能清理工具，帮助您快速释放磁盘空间、清理垃圾文件、优化系统性能。$\r$\n$\r$\n点击「下一步」继续安装。"

; Finish page setup
!define MUI_FINISHPAGE_TITLE "安装完成"
!define MUI_FINISHPAGE_TEXT "LightC 已成功安装到您的计算机。$\r$\n$\r$\n点击「完成」退出安装向导。"
!define MUI_FINISHPAGE_RUN "$INSTDIR\LightC.exe"
!define MUI_FINISHPAGE_RUN_TEXT "立即运行 LightC"
!define MUI_FINISHPAGE_RUN_CHECKED

; Desktop shortcut option
!define MUI_FINISHPAGE_SHOWREADME ""
!define MUI_FINISHPAGE_SHOWREADME_TEXT "创建桌面快捷方式"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION CreateDesktopShortcut
!define MUI_FINISHPAGE_SHOWREADME_CHECKED

; Welcome/finish page bitmap (164x314)
!define MUI_WELCOMEFINISHPAGE_BITMAP "nsis\wizard.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "nsis\wizard.bmp"

; Header banner bitmap (150x57)
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_BITMAP "nsis\header.bmp"
!define MUI_HEADERIMAGE_BITMAP_STRETCH FillHeight
!define MUI_HEADERIMAGE_UNBITMAP "nsis\header.bmp"

; Icons
!define MUI_ICON "icons\icon.ico"
!define MUI_UNICON "icons\icon.ico"

; UI behavior
!define MUI_ABORTWARNING
!define MUI_ABORTWARNING_TEXT "确定要取消 LightC 的安装吗？"

; ============================================================================
; 3. Language support
; ============================================================================
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

; Localized strings
LangString DESC_SecMain ${LANG_SIMPCHINESE} "LightC 主程序文件"
LangString DESC_SecMain ${LANG_ENGLISH} "LightC main application files"

LangString MSG_ALREADY_RUNNING ${LANG_SIMPCHINESE} "LightC 正在运行中，请先关闭程序后再继续安装。"
LangString MSG_ALREADY_RUNNING ${LANG_ENGLISH} "LightC is currently running. Please close it before continuing."

; ============================================================================
; 4. Custom functions
; ============================================================================

; Create the desktop shortcut; skipped in update mode, which is handled by the generated CreateOrUpdateDesktopShortcut
Function CreateDesktopShortcut
    ; Do not duplicate the shortcut during an update
    ${If} $UpdateMode = 1
        Return
    ${EndIf}
    CreateShortCut "$DESKTOP\LightC.lnk" "$INSTDIR\LightC.exe" "" "$INSTDIR\LightC.exe" 0
FunctionEnd

; Check whether the app is running
Function CheckAppRunning
    FindWindow $0 "" "LightC"
    StrCmp $0 0 notRunning
        MessageBox MB_OK|MB_ICONEXCLAMATION "$(MSG_ALREADY_RUNNING)"
        Abort
    notRunning:
FunctionEnd

; Pre-install callback
Function .onInit
    ; Check for a running instance
    Call CheckAppRunning
    
    ; Show the language selector
    !insertmacro MUI_LANGDLL_DISPLAY
FunctionEnd

; ============================================================================
; 5. Uninstaller appearance
; ============================================================================

; Uninstall confirmation page
!define MUI_UNCONFIRMPAGE_TEXT_TOP "即将从您的计算机中卸载 LightC。$\r$\n$\r$\n卸载前请确保 LightC 已关闭。"

; Uninstall finish page
!define MUI_UNFINISHPAGE_NOAUTOCLOSE

; Pre-uninstall check
Function un.onInit
    FindWindow $0 "" "LightC"
    StrCmp $0 0 notRunning
        MessageBox MB_OK|MB_ICONEXCLAMATION "$(MSG_ALREADY_RUNNING)"
        Abort
    notRunning:
FunctionEnd

; Remove the desktop shortcut on uninstall; skipped in update mode so it is not deleted without being recreated
Function un.onUninstSuccess
    ; Keep the shortcut during an update
    ${If} $UpdateMode = 1
        Return
    ${EndIf}
    Delete "$DESKTOP\LightC.lnk"
FunctionEnd
