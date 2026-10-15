Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"

!ifndef PackageDir
  !define PackageDir "dist\AgentEgressController-0.5.2-portable"
!endif
!ifndef ProductVersion
  !define ProductVersion "0.5.2"
!endif
!ifndef InstallerOut
  !define InstallerOut "dist\AgentEgressController-${ProductVersion}-setup.exe"
!endif

Name "Agent Egress Controller ${ProductVersion}"
OutFile "${InstallerOut}"
InstallDir "$LOCALAPPDATA\Programs\Agent Egress Controller"
RequestExecutionLevel user
ShowInstDetails show
ShowUnInstDetails show

Function .onInit
  SetShellVarContext current
FunctionEnd

!define MUI_ABORTWARNING
!if /FileExists "${PackageDir}\app.ico"
  !define MUI_ICON "${PackageDir}\app.ico"
  !define MUI_UNICON "${PackageDir}\app.ico"
!endif
!define MUI_WELCOMEPAGE_TITLE "Agent Egress Controller 安装向导"
!define MUI_WELCOMEPAGE_TEXT "本程序安装在当前 Windows 用户目录，不需要管理员权限。安装后仍由 Agent Egress 托盘和看板管理出口、规则与本机 Agent 接口。设置与凭据会保留在当前用户数据目录和 Windows 凭据管理器。$\r$\n$\r$\n更新只替换程序文件；卸载不会删除用户数据或凭据。"
!define MUI_FINISHPAGE_RUN "$INSTDIR\aec-tray.exe"
!define MUI_FINISHPAGE_RUN_TEXT "安装完成后启动托盘"
!define MUI_FINISHPAGE_RUN_CHECKED

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${PackageDir}\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "SimpChinese"

Section "程序文件（必需）" SEC_PROGRAM
  SectionIn RO
  IfFileExists "$INSTDIR\portable.flag" portable_target install_files
  portable_target:
    MessageBox MB_OK|MB_ICONSTOP "所选目录是便携版目录。为避免切换或覆盖原有 data，请保留旧目录并安装到新的目录。"
    Abort
  install_files:
  SetOutPath "$INSTDIR"
  File /r "${PackageDir}\*"
  Delete "$INSTDIR\portable.flag"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AgentEgressController" "DisplayName" "Agent Egress Controller"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AgentEgressController" "DisplayVersion" "${ProductVersion}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AgentEgressController" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AgentEgressController" "InstallLocation" "$INSTDIR"
SectionEnd

Section "开始菜单入口" SEC_STARTMENU
  CreateDirectory "$SMPROGRAMS\Agent Egress"
  ExecWait '"$INSTDIR\aec-control.exe" --create-shortcuts start-menu' $0
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONEXCLAMATION "创建开始菜单入口失败：同名快捷方式可能属于另一份安装。请从安装目录启动托盘并检查入口。"
  ${EndIf}
  CreateShortcut "$SMPROGRAMS\Agent Egress\卸载 Agent Egress.lnk" "$INSTDIR\uninstall.exe"
SectionEnd

Section "桌面快捷方式" SEC_DESKTOP
  ExecWait '"$INSTDIR\aec-control.exe" --create-shortcuts desktop' $0
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONEXCLAMATION "创建桌面入口失败：同名快捷方式可能属于另一份安装。请从安装目录启动托盘并检查入口。"
  ${EndIf}
SectionEnd

Section /o "登录时自动启动" SEC_AUTOSTART
  ExecWait '"$INSTDIR\aec-tray.exe" --enable-autostart'
SectionEnd

Section "Uninstall"
  MessageBox MB_OKCANCEL|MB_ICONINFORMATION "卸载前请在托盘退出 Agent 模式并确认恢复完成，然后退出托盘。用户设置与凭据保留在本机。继续卸载？" IDOK +2
  Abort
  ExecWait '"$INSTDIR\aec-tray.exe" --disable-autostart'
  ExecWait '"$INSTDIR\aec-control.exe" --quit'
  Sleep 1000
  Delete "$INSTDIR\aec-tray.exe"
  IfErrors tray_still_running continue_uninstall
  tray_still_running:
    MessageBox MB_OK|MB_ICONSTOP "托盘仍在运行或程序文件被占用。请先从托盘正常退出并确认恢复完成，再重新卸载。"
    Abort
  continue_uninstall:
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\AgentEgressController"
  Delete "$DESKTOP\Agent Egress Controller.lnk"
  Delete "$DESKTOP\Agent Egress Dashboard.lnk"
  Delete "$SMPROGRAMS\Agent Egress\Agent Egress Controller.lnk"
  Delete "$SMPROGRAMS\Agent Egress\Agent Egress Dashboard.lnk"
  Delete "$SMPROGRAMS\Agent Egress\卸载 Agent Egress.lnk"
  RMDir "$SMPROGRAMS\Agent Egress"
  RMDir /r "$INSTDIR"
SectionEnd
