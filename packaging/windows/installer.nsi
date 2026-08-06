; Stacio Windows 安装脚本（NSIS，全中文界面）
; 由 CI 调用：makensis installer.nsi
; 输入：../target/x86_64-pc-windows-msvc/release/stacio-app.exe（已 build）
;       ../../assets/icons/stacio-256.png（CI 先转 ico）

Unicode true
ManifestDPIAware true

!define APPNAME "Stacio"
!define APPVERSION "0.1.0"
!define COMPANY "Stacio"

Name "${APPNAME} ${APPVERSION}"
OutFile "StacioSetup-${APPVERSION}.exe"
InstallDir "$LOCALAPPDATA\${APPNAME}"
InstallDirRegKey HKCU "Software\${APPNAME}" "InstallDir"
RequestExecutionLevel user
ShowInstDetails hide
ShowUnInstDetails hide

; 现代中文 UI
!include "MUI2.nsh"
!define MUI_ABORTWARNING
!define MUI_ICON "stacio.ico"
!define MUI_UNICON "stacio.ico"
!define MUI_WELCOMEPAGE_TITLE "${APPNAME} 安装向导"
!define MUI_WELCOMEPAGE_TEXT "此向导将引导您完成 ${APPNAME} 的安装。$\r$\n$\r$\n点击「下一步」继续。"
!define MUI_TEXT_DIRECTORY_TITLE "选择安装位置"
!define MUI_TEXT_INSTALLING_TITLE "正在安装"
!define MUI_TEXT_FINISH_TITLE "安装完成"
!define MUI_TEXT_FINISH_SUBTITLE "安装已成功完成。"
!define MUI_BUTTONTEXT_FINISH "完成"
!define MUI_TEXT_ABORT_TITLE "安装已中止"
!define MUI_UNTEXT_WELCOME_TITLE "${APPNAME} 卸载向导"
!define MUI_UNTEXT_CONFIRM_TITLE "确认卸载"
!define MUI_UNTEXT_CONFIRM_SUBTITLE "确认从您的计算机移除 ${APPNAME}。"
!define MUI_UNTEXT_FINISH_TITLE "卸载完成"
!define MUI_UNTEXT_FINISH_SUBTITLE "卸载已成功完成。"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_WELCOME
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH

; 简体中文
!insertmacro MUI_LANGUAGE "SimpChinese"

Section "${APPNAME}（必需）" SecCore
  SectionIn RO
  SetOutPath "$INSTDIR"
  File "stacio-app.exe"
  File /nonfatal "stacio.ico"

  ; 注册表：卸载信息 + 安装目录。
  WriteRegStr HKCU "Software\${APPNAME}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "DisplayName" "${APPNAME}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "DisplayVersion" "${APPVERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "Publisher" "${COMPANY}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "NoRepair" 1

  ; 开始菜单快捷方式。
  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  CreateShortcut "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk" "$INSTDIR\stacio-app.exe" "" "$INSTDIR\stacio.ico"
  CreateShortcut "$SMPROGRAMS\${APPNAME}\卸载 ${APPNAME}.lnk" "$INSTDIR\Uninstall.exe"

  ; 桌面快捷方式。
  CreateShortcut "$DESKTOP\${APPNAME}.lnk" "$INSTDIR\stacio-app.exe" "" "$INSTDIR\stacio.ico"

  ; 单实例：注册命名 Mutex 名（应用内也检查，此处仅占位便于将来扩展）。
  WriteUninstaller "$INSTDIR\Uninstall.exe"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\stacio-app.exe"
  Delete "$INSTDIR\stacio.ico"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk"
  Delete "$SMPROGRAMS\${APPNAME}\卸载 ${APPNAME}.lnk"
  RMDir "$SMPROGRAMS\${APPNAME}"
  Delete "$DESKTOP\${APPNAME}.lnk"

  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}"
  DeleteRegKey HKCU "Software\${APPNAME}"
SectionEnd
