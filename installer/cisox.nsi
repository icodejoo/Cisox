; Cisox 安装包脚本（NSIS，当前用户安装，无需管理员权限）。
; 由 scripts/package-cisox.ps1 调用：
;   makensis /DVERSION=<版本> /DSTAGE=<暂存目录绝对路径> /DOUTFILE=<输出exe绝对路径> installer\cisox.nsi
; 所有文件放同一目录（宿主 cisox.exe 按自身目录找 cisox-ui.exe 与各 worker）。
Unicode true
!include "MUI2.nsh"

!ifndef VERSION
  !error "缺少 /DVERSION"
!endif
!ifndef STAGE
  !error "缺少 /DSTAGE"
!endif
!ifndef OUTFILE
  !error "缺少 /DOUTFILE"
!endif

!define APP_NAME "Cisox"
!define HOST_EXE "cisox.exe"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Cisox"

Name "${APP_NAME}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${APP_NAME}"
InstallDirRegKey HKCU "Software\${APP_NAME}" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP_NAME}"
VIAddVersionKey "FileDescription" "${APP_NAME} Setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\${HOST_EXE}"
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

; 让运行中的宿主自己退出（它会连带关掉 UI 与 worker），再覆盖文件。
!macro QuitRunningHost
  IfFileExists "$INSTDIR\${HOST_EXE}" 0 +3
    ExecWait '"$INSTDIR\${HOST_EXE}" --cmd quit'
    Sleep 1500
!macroend

Section "Install"
  !insertmacro QuitRunningHost
  SetOutPath "$INSTDIR"
  File /r "${STAGE}\*.*"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${HOST_EXE}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk" "$INSTDIR\Uninstall.exe"

  WriteRegStr HKCU "Software\${APP_NAME}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${HOST_EXE}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

; 卸载只删程序文件和快捷方式，不动用户数据（配置、历史、已下载模型都在数据目录里）。
Section "Uninstall"
  !insertmacro QuitRunningHost
  RMDir /r "$INSTDIR"
  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk"
  RMDir "$SMPROGRAMS\${APP_NAME}"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegKey HKCU "Software\${APP_NAME}"
SectionEnd
