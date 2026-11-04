; ScreenConnection NSIS 安装钩子
; 1) PREINSTALL：静默卸载旧版 Screenguard（屏幕守护）并清理残留
; 2) POSTINSTALL：自动创建桌面快捷方式（GUI 安装默认不创建，此钩子补齐）

!macro NSIS_HOOK_PREINSTALL
  ; ---------- 检测并静默卸载旧版 Screenguard ----------
  ReadRegStr $R0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Screenguard" "UninstallString"
  ReadRegStr $R1 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Screenguard" "InstallLocation"
  ${If} $R0 == ""
    ReadRegStr $R0 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Screenguard" "UninstallString"
    ReadRegStr $R1 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Screenguard" "InstallLocation"
  ${EndIf}
  ${If} $R0 == ""
    ReadRegStr $R0 HKLM "Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Screenguard" "UninstallString"
    ReadRegStr $R1 HKLM "Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Screenguard" "InstallLocation"
  ${EndIf}

  ${If} $R0 != ""
    DetailPrint "检测到旧版 Screenguard，正在静默卸载..."
    ; 结束旧版进程，避免文件占用导致卸载失败
    nsExec::Exec 'taskkill /IM Screenguard.exe /F'
    Pop $R2
    ; InstallLocation 缺失时回退到默认安装目录
    ${If} $R1 == ""
      StrCpy $R1 "$LOCALAPPDATA\Screenguard"
    ${EndIf}
    ; _?=dir 让卸载器原地同步运行，ExecWait 可等待其完成
    ExecWait '"$R0" /S _?=$R1'
    Delete "$R0"
    RMDir /r "$R1"
    Delete "$DESKTOP\Screenguard.lnk"
    Delete "$DESKTOP\屏幕守护.lnk"
    Delete "$SMPROGRAMS\Screenguard.lnk"
    Delete "$SMPROGRAMS\屏幕守护.lnk"
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Screenguard"
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Screenguard"
    DeleteRegKey HKLM "Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Screenguard"
    DetailPrint "旧版卸载完成"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; ---------- 自动创建桌面快捷方式（/NS 无快捷方式模式除外） ----------
  ${If} $NoShortcutMode = 0
    CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    DetailPrint "已创建桌面快捷方式"
  ${EndIf}
!macroend
