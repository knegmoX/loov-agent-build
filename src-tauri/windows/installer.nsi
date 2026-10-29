; Loov Agent 安装程序模板：基于 tauri-bundler 2.8.1 的 installer.nsi（与 @tauri-apps/cli 2.10.1 配套）。
; 只换界面：欢迎/许可/重装/目录四步合成一个白底安装页，进度页去掉日志列表，完成页自绘。
; 安装、升级、卸载逻辑保持上游原样；改动处都标了「Loov:」。升级 Tauri CLI 时需对照上游模板合并。
Unicode true
ManifestDPIAware true
; Add in `dpiAwareness` `PerMonitorV2` to manifest for Windows 10 1607+ (note this should not affect lower versions since they should be able to ignore this and pick up `dpiAware` `true` set by `ManifestDPIAware true`)
; Currently undocumented on NSIS's website but is in the Docs folder of source tree, see
; https://github.com/kichik/nsis/blob/5fc0b87b819a9eec006df4967d08e522ddd651c9/Docs/src/attributes.but#L286-L300
; https://github.com/tauri-apps/tauri/pull/10106
ManifestDPIAwareness PerMonitorV2

!if "{{compression}}" == "none"
  SetCompress off
!else
  ; Set the compression algorithm. We default to LZMA.
  SetCompressor /SOLID "{{compression}}"
!endif

!include MUI2.nsh
!include FileFunc.nsh
!include x64.nsh
!include WordFunc.nsh
!include "utils.nsh"
!include "FileAssociation.nsh"
!include "Win\COM.nsh"
!include "Win\Propkey.nsh"
!include "StrFunc.nsh"
${StrCase}
${StrLoc}

{{#if installer_hooks}}
!include "{{installer_hooks}}"
{{/if}}

!define WEBVIEW2APPGUID "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"

!define MANUFACTURER "{{manufacturer}}"
!define PRODUCTNAME "{{product_name}}"
!define VERSION "{{version}}"
!define VERSIONWITHBUILD "{{version_with_build}}"
!define HOMEPAGE "{{homepage}}"
!define INSTALLMODE "{{install_mode}}"
!define LICENSE "{{license}}"
!define INSTALLERICON "{{installer_icon}}"
!define SIDEBARIMAGE "{{sidebar_image}}"
!define HEADERIMAGE "{{header_image}}"
!define MAINBINARYNAME "{{main_binary_name}}"
!define MAINBINARYSRCPATH "{{main_binary_path}}"
!define BUNDLEID "{{bundle_id}}"
!define COPYRIGHT "{{copyright}}"
!define OUTFILE "{{out_file}}"
!define ARCH "{{arch}}"
!define ADDITIONALPLUGINSPATH "{{additional_plugins_path}}"
!define ALLOWDOWNGRADES "{{allow_downgrades}}"
!define DISPLAYLANGUAGESELECTOR "{{display_language_selector}}"
!define INSTALLWEBVIEW2MODE "{{install_webview2_mode}}"
!define WEBVIEW2INSTALLERARGS "{{webview2_installer_args}}"
!define WEBVIEW2BOOTSTRAPPERPATH "{{webview2_bootstrapper_path}}"
!define WEBVIEW2INSTALLERPATH "{{webview2_installer_path}}"
!define MINIMUMWEBVIEW2VERSION "{{minimum_webview2_version}}"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCTNAME}"
!define MANUKEY "Software\${MANUFACTURER}"
!define MANUPRODUCTKEY "${MANUKEY}\${PRODUCTNAME}"
!define UNINSTALLERSIGNCOMMAND "{{uninstaller_sign_cmd}}"
!define ESTIMATEDSIZE "{{estimated_size}}"
!define STARTMENUFOLDER "{{start_menu_folder}}"

Var PassiveMode
Var UpdateMode
Var NoShortcutMode
Var WixMode
Var OldMainBinaryName

; Loov: 自绘界面用到的状态
Var LoovExistingVersion   ; 已安装版本，空 = 全新安装
Var LoovVersionCmp        ; 与已安装版本比较：1 升级 / 0 同版本 / -1 降级，空 = 全新安装
Var LoovDesktopCheckbox
Var LoovDesktopShortcut   ; 1 = 创建桌面快捷方式
Var LoovRunCheckbox
Var LoovDirLabel
Var LoovTitleFont
Var LoovBodyFont
Var LoovSmallFont
Var LoovLogo

Name "${PRODUCTNAME}"
; Loov: 底部不显示版权文字
BrandingText " "
OutFile "${OUTFILE}"

; We don't actually use this value as default install path,
; it's just for nsis to append the product name folder in the directory selector
; https://nsis.sourceforge.io/Reference/InstallDir
!define PLACEHOLDER_INSTALL_DIR "placeholder\${PRODUCTNAME}"
InstallDir "${PLACEHOLDER_INSTALL_DIR}"

VIProductVersion "${VERSIONWITHBUILD}"
VIAddVersionKey "ProductName" "${PRODUCTNAME}"
VIAddVersionKey "FileDescription" "${PRODUCTNAME}"
VIAddVersionKey "LegalCopyright" "${COPYRIGHT}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

# additional plugins
!addplugindir "${ADDITIONALPLUGINSPATH}"

; Uninstaller signing command
!if "${UNINSTALLERSIGNCOMMAND}" != ""
  !uninstfinalize '${UNINSTALLERSIGNCOMMAND}'
!endif

; Handle install mode, `perUser`, `perMachine` or `both`
!if "${INSTALLMODE}" == "perMachine"
  RequestExecutionLevel admin
!endif

!if "${INSTALLMODE}" == "currentUser"
  RequestExecutionLevel user
!endif

!if "${INSTALLMODE}" == "both"
  !define MULTIUSER_MUI
  !define MULTIUSER_INSTALLMODE_INSTDIR "${PRODUCTNAME}"
  !define MULTIUSER_INSTALLMODE_COMMANDLINE
  !if "${ARCH}" == "x64"
    !define MULTIUSER_USE_PROGRAMFILES64
  !else if "${ARCH}" == "arm64"
    !define MULTIUSER_USE_PROGRAMFILES64
  !endif
  !define MULTIUSER_INSTALLMODE_DEFAULT_REGISTRY_KEY "${UNINSTKEY}"
  !define MULTIUSER_INSTALLMODE_DEFAULT_REGISTRY_VALUENAME "CurrentUser"
  !define MULTIUSER_INSTALLMODEPAGE_SHOWUSERNAME
  !define MULTIUSER_INSTALLMODE_FUNCTION RestorePreviousInstallLocation
  !define MULTIUSER_EXECUTIONLEVEL Highest
  !include MultiUser.nsh
!endif

; Installer icon
!if "${INSTALLERICON}" != ""
  !define MUI_ICON "${INSTALLERICON}"
  ; Loov: 卸载程序也用应用图标
  !define MUI_UNICON "${INSTALLERICON}"
!endif

; Loov: 整个窗口白底、隐藏标题栏下的页眉与分隔线
!define MUI_CUSTOMFUNCTION_GUIINIT LoovGuiInit
!define MUI_BGCOLOR "FFFFFF"

; Installer sidebar image
!if "${SIDEBARIMAGE}" != ""
  !define MUI_WELCOMEFINISHPAGE_BITMAP "${SIDEBARIMAGE}"
!endif

; Installer header image
!if "${HEADERIMAGE}" != ""
  !define MUI_HEADERIMAGE
  !define MUI_HEADERIMAGE_BITMAP  "${HEADERIMAGE}"
!endif

; Define registry key to store installer language
!define MUI_LANGDLL_REGISTRY_ROOT "HKCU"
!define MUI_LANGDLL_REGISTRY_KEY "${MANUPRODUCTKEY}"
!define MUI_LANGDLL_REGISTRY_VALUENAME "Installer Language"

; Installer pages, must be ordered as they appear
; Loov: 1~5 合并为一个安装页（不配置许可文件；安装模式固定 currentUser）
!if "${LICENSE}" != ""
  !error "Loov 模板未实现许可页"
!endif
!if "${INSTALLMODE}" == "both"
  !error "Loov 模板未实现安装模式选择页"
!endif
Page custom LoovWelcomeCreate LoovWelcomeLeave

; Loov: 原「重装/卸载」选择页改为自动处理：WiX 旧版先卸载，其余直接覆盖安装
;（与 Tauri 更新器的 /UPDATE 行为一致），检测逻辑保持上游原样。
Function LoovDetectExisting
  StrCpy $LoovExistingVersion ""
  StrCpy $LoovVersionCmp ""
  StrCpy $WixMode 0

  ; Uninstall previous WiX installation if exists.
  ;
  ; A WiX installer stores the installation info in registry
  ; using a UUID and so we have to loop through all keys under
  ; `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall`
  ; and check if `DisplayName` and `Publisher` keys match ${PRODUCTNAME} and ${MANUFACTURER}
  StrCpy $0 0
  wix_loop:
    EnumRegKey $1 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall" $0
    StrCmp $1 "" wix_loop_done ; Exit loop if there is no more keys to loop on
    IntOp $0 $0 + 1
    ReadRegStr $R0 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$1" "DisplayName"
    ReadRegStr $R1 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$1" "Publisher"
    StrCmp "$R0$R1" "${PRODUCTNAME}${MANUFACTURER}" 0 wix_loop
    ReadRegStr $R0 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$1" "UninstallString"
    ${StrCase} $R1 $R0 "L"
    ${StrLoc} $R0 $R1 "msiexec" ">"
    StrCmp $R0 0 0 wix_loop_done
    StrCpy $WixMode 1
    StrCpy $R6 "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$1"
    Goto compare_version
  wix_loop_done:

  ; Check if there is an existing installation
  ReadRegStr $R0 SHCTX "${UNINSTKEY}" ""
  ReadRegStr $R1 SHCTX "${UNINSTKEY}" "UninstallString"
  ${IfThen} "$R0$R1" == "" ${|} Return ${|}

  compare_version:
  ${If} $WixMode = 1
    ReadRegStr $R0 HKLM "$R6" "DisplayVersion"
  ${Else}
    ReadRegStr $R0 SHCTX "${UNINSTKEY}" "DisplayVersion"
  ${EndIf}
  StrCpy $LoovExistingVersion $R0
  ${If} $R0 == ""
    StrCpy $LoovVersionCmp 1
  ${Else}
    nsis_tauri_utils::SemverCompare "${VERSION}" $R0
    Pop $LoovVersionCmp
  ${EndIf}
  ; 上游 EarlyChecks 用 $R0 判断静默降级
  StrCpy $R0 $LoovVersionCmp
FunctionEnd

; WiX 旧版必须先卸载（上游 reinst_uninstall 的 WiX 分支）
Function LoovUninstallWix
  ${If} $WixMode <> 1
    Return
  ${EndIf}
  HideWindow
  ClearErrors
  ReadRegStr $R1 HKLM "$R6" "UninstallString"
  ExecWait '$R1' $0
  BringToFront
  ${IfThen} ${Errors} ${|} StrCpy $0 2 ${|}
  ${If} $0 <> 0
    ${If} $0 = 1602 ; 用户取消
      Abort
    ${EndIf}
    MessageBox MB_ICONEXCLAMATION "$(unableToUninstall)"
    Abort
  ${EndIf}
FunctionEnd

; ---------- Loov: 自绘界面 ----------

; 页面四周的系统控件：页眉(1034 背景/1037 标题/1038 副标题/1039 图标)、
; 页眉下分隔线(1035)、全窗口分隔线(1045)、底部品牌文字(1028/1256)
Function LoovGuiInit
  SetCtlColors $HWNDPARENT "" "FFFFFF"
  ${ForEach} $0 1034 1039 + 1
    GetDlgItem $1 $HWNDPARENT $0
    ShowWindow $1 ${SW_HIDE}
  ${Next}
  ${ForEach} $0 1045 1045 + 1
    GetDlgItem $1 $HWNDPARENT $0
    ShowWindow $1 ${SW_HIDE}
  ${Next}
  GetDlgItem $1 $HWNDPARENT 1028
  ShowWindow $1 ${SW_HIDE}
  GetDlgItem $1 $HWNDPARENT 1256
  ShowWindow $1 ${SW_HIDE}

  CreateFont $LoovTitleFont "$(^Font)" "20" "600"
  CreateFont $LoovBodyFont "$(^Font)" "10" "400"
  CreateFont $LoovSmallFont "$(^Font)" "9" "400"

  ; 按窗口 DPI 选 logo（72px 为 100% 缩放）
  System::Call "user32::GetDpiForWindow(p $HWNDPARENT) i .r0"
  ${If} $0 >= 168
    StrCpy $LoovLogo "$PLUGINSDIR\logo-144.bmp"
  ${ElseIf} $0 >= 120
    StrCpy $LoovLogo "$PLUGINSDIR\logo-108.bmp"
  ${Else}
    StrCpy $LoovLogo "$PLUGINSDIR\logo-72.bmp"
  ${EndIf}
FunctionEnd

; 居中 logo + 标题 + 副标题，$0 标题，$1 副标题，$2 第三行（小字，可空）
!macro LoovHero
  ${NSD_CreateBitmap} 0 14u 100% 46u ""
  Pop $R9
  ${NSD_AddStyle} $R9 ${SS_CENTERIMAGE}
  ${NSD_SetImage} $R9 $LoovLogo $R8
  SetCtlColors $R9 "" "FFFFFF"

  ${NSD_CreateLabel} 0 66u 100% 22u $0
  Pop $R9
  ${NSD_AddStyle} $R9 ${SS_CENTER}
  SendMessage $R9 ${WM_SETFONT} $LoovTitleFont 1
  SetCtlColors $R9 "111827" "FFFFFF"

  ${NSD_CreateLabel} 0 90u 100% 12u $1
  Pop $R9
  ${NSD_AddStyle} $R9 ${SS_CENTER}
  SendMessage $R9 ${WM_SETFONT} $LoovBodyFont 1
  SetCtlColors $R9 "4B5563" "FFFFFF"

  ${NSD_CreateLabel} 0 104u 100% 10u $2
  Pop $R9
  ${NSD_AddStyle} $R9 ${SS_CENTER}
  SendMessage $R9 ${WM_SETFONT} $LoovSmallFont 1
  SetCtlColors $R9 "9CA3AF" "FFFFFF"
!macroend

Function LoovWelcomeCreate
  Call LoovDetectExisting
  ${If} $PassiveMode = 1
    Call LoovUninstallWix
    Abort
  ${EndIf}

  nsDialogs::Create 1044
  Pop $R7
  ${IfThen} $(^RTL) = 1 ${|} nsDialogs::SetRTL $(^RTL) ${|}
  SetCtlColors $R7 "" "FFFFFF"

  StrCpy $0 "${PRODUCTNAME}"
  StrCpy $1 "$(loovTagline)"
  ${If} $LoovVersionCmp == ""
    StrCpy $2 "$(loovVersion) ${VERSION}"
  ${ElseIf} $LoovVersionCmp = 1
    StrCpy $2 "$(loovUpgradeFrom) $LoovExistingVersion $(loovUpgradeTo) ${VERSION}"
  ${ElseIf} $LoovVersionCmp = 0
    StrCpy $2 "$(loovSameVersion) ${VERSION}"
  ${Else}
    StrCpy $2 "$(loovDowngradeFrom) $LoovExistingVersion $(loovDowngradeTo) ${VERSION}"
  ${EndIf}
  !insertmacro LoovHero

  ; 安装位置：一行路径 + 「更改」
  ${NSD_CreateLabel} 40u 136u 52u 10u "$(loovInstallTo)"
  Pop $R9
  SendMessage $R9 ${WM_SETFONT} $LoovSmallFont 1
  SetCtlColors $R9 "6B7280" "FFFFFF"
  ${NSD_CreateLabel} 92u 136u 168u 10u "$INSTDIR"
  Pop $LoovDirLabel
  ${NSD_AddStyle} $LoovDirLabel 0x8000 ; SS_PATHELLIPSIS
  SendMessage $LoovDirLabel ${WM_SETFONT} $LoovSmallFont 1
  SetCtlColors $LoovDirLabel "374151" "FFFFFF"
  ${NSD_CreateLink} 264u 136u 30u 10u "$(loovChange)"
  Pop $R9
  SendMessage $R9 ${WM_SETFONT} $LoovSmallFont 1
  SetCtlColors $R9 "059669" "FFFFFF"
  ${NSD_OnClick} $R9 LoovBrowseDir
  ; 已安装时沿用原目录，不允许改（避免两份）
  ${If} $LoovVersionCmp != ""
    ShowWindow $R9 ${SW_HIDE}
  ${EndIf}

  ${NSD_CreateCheckbox} 40u 152u 200u 11u "$(createDesktop)"
  Pop $LoovDesktopCheckbox
  SendMessage $LoovDesktopCheckbox ${WM_SETFONT} $LoovSmallFont 1
  SetCtlColors $LoovDesktopCheckbox "374151" "FFFFFF"
  ; 全新安装默认勾选；升级时沿用原状态（原来有桌面快捷方式才勾选，勾选会重建它以刷新图标）
  ${If} $LoovDesktopShortcut == ""
    StrCpy $LoovDesktopShortcut 1
    ${If} $LoovVersionCmp != ""
    ${AndIfNot} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
      StrCpy $LoovDesktopShortcut 0
    ${EndIf}
  ${EndIf}
  ${If} $LoovDesktopShortcut = 1
    ${NSD_Check} $LoovDesktopCheckbox
  ${EndIf}

  GetDlgItem $R9 $HWNDPARENT 3
  ShowWindow $R9 ${SW_HIDE}
  GetDlgItem $R9 $HWNDPARENT 1
  ${If} $LoovVersionCmp == ""
    SendMessage $R9 ${WM_SETTEXT} 0 "STR:$(loovInstall)"
  ${Else}
    SendMessage $R9 ${WM_SETTEXT} 0 "STR:$(loovUpdate)"
  ${EndIf}
  ${NSD_SetFocus} $R9

  nsDialogs::Show
  ${NSD_FreeImage} $R8
FunctionEnd

Function LoovBrowseDir
  nsDialogs::SelectFolderDialog "$(loovChooseDir)" "$INSTDIR"
  Pop $0
  ${If} $0 != "error"
    ; 与上游目录页一致：选的是父目录时补上产品名
    ${GetFileName} $0 $1
    ${If} $1 != "${PRODUCTNAME}"
      StrCpy $0 "$0\${PRODUCTNAME}"
    ${EndIf}
    StrCpy $INSTDIR $0
    ${NSD_SetText} $LoovDirLabel $INSTDIR
  ${EndIf}
FunctionEnd

Function LoovWelcomeLeave
  ${NSD_GetState} $LoovDesktopCheckbox $0
  ${If} $0 = ${BST_CHECKED}
    StrCpy $LoovDesktopShortcut 1
  ${Else}
    StrCpy $LoovDesktopShortcut 0
  ${EndIf}
  Call LoovUninstallWix
FunctionEnd

; 6. Start menu shortcut page
Var AppStartMenuFolder
!if "${STARTMENUFOLDER}" != ""
  !define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
  !define MUI_STARTMENUPAGE_DEFAULTFOLDER "${STARTMENUFOLDER}"
!else
  !define MUI_PAGE_CUSTOMFUNCTION_PRE Skip
!endif
!insertmacro MUI_PAGE_STARTMENU Application $AppStartMenuFolder

; 7. Installation page
; Loov: 去掉日志列表，只留标题、进度条和当前步骤；装完自动进入完成页
;（上游靠 MUI 完成页的 NOAUTOCLOSE 控制，这里不用 MUI 完成页，需显式开启）
AutoCloseWindow true
!define MUI_PAGE_CUSTOMFUNCTION_SHOW LoovInstFilesShow
!insertmacro MUI_PAGE_INSTFILES

; 把进度页的内层对话框挪到全窗口区域(1044)，重新摆放控件（像素坐标按实际尺寸算，适配 DPI）
Function LoovInstFilesShow
  FindWindow $R7 "#32770" "" $HWNDPARENT
  GetDlgItem $0 $HWNDPARENT 1044
  System::Call "*(i, i, i, i) p .R6"
  System::Call "user32::GetWindowRect(p r0, p R6)"
  System::Call "user32::MapWindowPoints(p 0, p $HWNDPARENT, p R6, i 2)"
  System::Call "*$R6(i .r1, i .r2, i .r3, i .r4)"
  System::Free $R6
  IntOp $3 $3 - $1 ; 宽
  IntOp $4 $4 - $2 ; 高
  System::Call "user32::SetWindowPos(p R7, p 0, i r1, i r2, i r3, i r4, i 0x0014)" ; NOZORDER|NOACTIVATE
  SetCtlColors $R7 "" "FFFFFF"

  GetDlgItem $0 $R7 1016 ; 日志列表
  ShowWindow $0 ${SW_HIDE}
  GetDlgItem $0 $R7 1027 ; 「显示细节」
  ShowWindow $0 ${SW_HIDE}
  GetDlgItem $0 $HWNDPARENT 3
  ShowWindow $0 ${SW_HIDE}

  ; 进度条：宽 70%，居中，位于 55% 高度；高度按 DPI 取 6px
  System::Call "user32::GetDpiForWindow(p $HWNDPARENT) i .r9"
  IntOp $5 $3 * 70
  IntOp $5 $5 / 100
  IntOp $6 $3 - $5
  IntOp $6 $6 / 2
  IntOp $7 $4 * 55
  IntOp $7 $7 / 100
  IntOp $8 6 * $9
  IntOp $8 $8 / 96
  GetDlgItem $0 $R7 1004
  System::Call "user32::SetWindowPos(p r0, p 0, i r6, i r7, i r5, i r8, i 0x0014)"

  ; 当前步骤：进度条下方
  IntOp $7 $7 + $8
  IntOp $8 12 * $9
  IntOp $8 $8 / 96
  IntOp $7 $7 + $8
  IntOp $8 20 * $9
  IntOp $8 $8 / 96
  GetDlgItem $0 $R7 1006
  System::Call "user32::SetWindowPos(p r0, p 0, i r6, i r7, i r5, i r8, i 0x0014)"
  ${NSD_AddStyle} $0 ${SS_CENTER}
  SendMessage $0 ${WM_SETFONT} $LoovSmallFont 1
  SetCtlColors $0 "6B7280" "FFFFFF"

  ; 标题：进度条上方
  IntOp $7 $4 * 38
  IntOp $7 $7 / 100
  IntOp $8 36 * $9
  IntOp $8 $8 / 96
  ${If} $LoovVersionCmp == ""
    StrCpy $1 "$(loovInstalling)"
  ${Else}
    StrCpy $1 "$(loovUpdating)"
  ${EndIf}
  System::Call 'user32::CreateWindowEx(i 0, w "STATIC", w "$1", i 0x50000001, i 0, i r7, i r3, i r8, p R7, i 0, i 0, i 0) p .r0'
  SendMessage $0 ${WM_SETFONT} $LoovTitleFont 1
  SetCtlColors $0 "111827" "FFFFFF"
FunctionEnd

; 8. Finish page
Page custom LoovFinishCreate LoovFinishLeave

Function LoovFinishCreate
  ${IfThen} $PassiveMode = 1 ${|} Abort ${|}
  nsDialogs::Create 1044
  Pop $R7
  ${IfThen} $(^RTL) = 1 ${|} nsDialogs::SetRTL $(^RTL) ${|}
  SetCtlColors $R7 "" "FFFFFF"

  ${If} $LoovVersionCmp == ""
    StrCpy $0 "$(loovInstalled)"
  ${Else}
    StrCpy $0 "$(loovUpdated)"
  ${EndIf}
  StrCpy $1 "$(loovReady)"
  StrCpy $2 "$(loovVersion) ${VERSION}"
  !insertmacro LoovHero

  ${NSD_CreateCheckbox} 112u 140u 110u 11u "$(loovLaunch)"
  Pop $LoovRunCheckbox
  SendMessage $LoovRunCheckbox ${WM_SETFONT} $LoovBodyFont 1
  SetCtlColors $LoovRunCheckbox "374151" "FFFFFF"
  ${NSD_Check} $LoovRunCheckbox

  ; 只留一个「完成」按钮，放在最右侧（原「取消」的位置）
  GetDlgItem $R9 $HWNDPARENT 3
  ShowWindow $R9 ${SW_HIDE}
  GetDlgItem $R9 $HWNDPARENT 2
  ShowWindow $R9 ${SW_HIDE}
  System::Call "*(i, i, i, i) p .R6"
  System::Call "user32::GetWindowRect(p R9, p R6)"
  System::Call "user32::MapWindowPoints(p 0, p $HWNDPARENT, p R6, i 2)"
  System::Call "*$R6(i .r3, i .r4, i .r5, i .r6)"
  System::Free $R6
  IntOp $5 $5 - $3
  IntOp $6 $6 - $4
  GetDlgItem $R9 $HWNDPARENT 1
  System::Call "user32::SetWindowPos(p R9, p 0, i r3, i r4, i r5, i r6, i 0x0014)"
  SendMessage $R9 ${WM_SETTEXT} 0 "STR:$(loovDone)"

  nsDialogs::Show
  ${NSD_FreeImage} $R8
FunctionEnd

Function LoovFinishLeave
  ${NSD_GetState} $LoovRunCheckbox $0
  ${If} $0 = ${BST_CHECKED}
    Call RunMainBinary
  ${EndIf}
FunctionEnd

Function RunMainBinary
  nsis_tauri_utils::RunAsUser "$INSTDIR\${MAINBINARYNAME}.exe" ""
FunctionEnd
; Uninstaller Pages
; 1. Confirm uninstall page
Var DeleteAppDataCheckbox
Var DeleteAppDataCheckboxState
!define /ifndef WS_EX_LAYOUTRTL         0x00400000
!define MUI_PAGE_CUSTOMFUNCTION_SHOW un.ConfirmShow
Function un.ConfirmShow ; Add add a `Delete app data` check box
  ; $1 inner dialog HWND
  ; $2 window DPI
  ; $3 style
  ; $4 x
  ; $5 y
  ; $6 width
  ; $7 height
  FindWindow $1 "#32770" "" $HWNDPARENT ; Find inner dialog
  System::Call "user32::GetDpiForWindow(p r1) i .r2"
  ${If} $(^RTL) = 1
    StrCpy $3 "${__NSD_CheckBox_EXSTYLE} | ${WS_EX_LAYOUTRTL}"
    IntOp $4 50 * $2
  ${Else}
    StrCpy $3 "${__NSD_CheckBox_EXSTYLE}"
    IntOp $4 0 * $2
  ${EndIf}
  IntOp $5 100 * $2
  IntOp $6 400 * $2
  IntOp $7 25 * $2
  IntOp $4 $4 / 96
  IntOp $5 $5 / 96
  IntOp $6 $6 / 96
  IntOp $7 $7 / 96
  System::Call 'user32::CreateWindowEx(i r3, w "${__NSD_CheckBox_CLASS}", w "$(deleteAppData)", i ${__NSD_CheckBox_STYLE}, i r4, i r5, i r6, i r7, p r1, i0, i0, i0) i .s'
  Pop $DeleteAppDataCheckbox
  SendMessage $HWNDPARENT ${WM_GETFONT} 0 0 $1
  SendMessage $DeleteAppDataCheckbox ${WM_SETFONT} $1 1
FunctionEnd
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE un.ConfirmLeave
Function un.ConfirmLeave
  SendMessage $DeleteAppDataCheckbox ${BM_GETCHECK} 0 0 $DeleteAppDataCheckboxState
FunctionEnd
!define MUI_PAGE_CUSTOMFUNCTION_PRE un.SkipIfPassive
!insertmacro MUI_UNPAGE_CONFIRM

; 2. Uninstalling Page
!insertmacro MUI_UNPAGE_INSTFILES

;Languages
{{#each languages}}
!insertmacro MUI_LANGUAGE "{{this}}"
{{/each}}
!insertmacro MUI_RESERVEFILE_LANGDLL
{{#each language_files}}
  !include "{{this}}"
{{/each}}

; Loov: 中文界面用微软雅黑（NSIS 默认是宋体）；自绘界面的文案
!ifdef LANG_SIMPCHINESE
  SetFont /LANG=${LANG_SIMPCHINESE} "Microsoft YaHei UI" 9
  LangString loovTagline ${LANG_SIMPCHINESE} "言出法随，所愿即现"
  LangString loovVersion ${LANG_SIMPCHINESE} "版本"
  LangString loovUpgradeFrom ${LANG_SIMPCHINESE} "从"
  LangString loovUpgradeTo ${LANG_SIMPCHINESE} "更新到"
  LangString loovDowngradeFrom ${LANG_SIMPCHINESE} "从"
  LangString loovDowngradeTo ${LANG_SIMPCHINESE} "回退到"
  LangString loovSameVersion ${LANG_SIMPCHINESE} "重新安装"
  LangString loovInstallTo ${LANG_SIMPCHINESE} "安装位置"
  LangString loovChange ${LANG_SIMPCHINESE} "更改"
  LangString loovChooseDir ${LANG_SIMPCHINESE} "选择安装位置"
  LangString loovInstall ${LANG_SIMPCHINESE} "安装"
  LangString loovUpdate ${LANG_SIMPCHINESE} "更新"
  LangString loovInstalling ${LANG_SIMPCHINESE} "正在安装…"
  LangString loovUpdating ${LANG_SIMPCHINESE} "正在更新…"
  LangString loovInstalled ${LANG_SIMPCHINESE} "安装完成"
  LangString loovUpdated ${LANG_SIMPCHINESE} "更新完成"
  LangString loovReady ${LANG_SIMPCHINESE} "Loov Agent 已准备就绪"
  LangString loovLaunch ${LANG_SIMPCHINESE} "立即启动 Loov Agent"
  LangString loovDone ${LANG_SIMPCHINESE} "完成"
!endif
!ifdef LANG_ENGLISH
  SetFont /LANG=${LANG_ENGLISH} "Segoe UI" 9
  LangString loovTagline ${LANG_ENGLISH} "Say it, See it."
  LangString loovVersion ${LANG_ENGLISH} "Version"
  LangString loovUpgradeFrom ${LANG_ENGLISH} "Update from"
  LangString loovUpgradeTo ${LANG_ENGLISH} "to"
  LangString loovDowngradeFrom ${LANG_ENGLISH} "Roll back from"
  LangString loovDowngradeTo ${LANG_ENGLISH} "to"
  LangString loovSameVersion ${LANG_ENGLISH} "Reinstall"
  LangString loovInstallTo ${LANG_ENGLISH} "Install to"
  LangString loovChange ${LANG_ENGLISH} "Change"
  LangString loovChooseDir ${LANG_ENGLISH} "Choose install location"
  LangString loovInstall ${LANG_ENGLISH} "Install"
  LangString loovUpdate ${LANG_ENGLISH} "Update"
  LangString loovInstalling ${LANG_ENGLISH} "Installing…"
  LangString loovUpdating ${LANG_ENGLISH} "Updating…"
  LangString loovInstalled ${LANG_ENGLISH} "All set"
  LangString loovUpdated ${LANG_ENGLISH} "Updated"
  LangString loovReady ${LANG_ENGLISH} "Loov Agent is ready to use"
  LangString loovLaunch ${LANG_ENGLISH} "Launch Loov Agent"
  LangString loovDone ${LANG_ENGLISH} "Finish"
!endif

Function .onInit
  ${GetOptions} $CMDLINE "/P" $PassiveMode
  ${IfNot} ${Errors}
    StrCpy $PassiveMode 1
  ${EndIf}

  ${GetOptions} $CMDLINE "/NS" $NoShortcutMode
  ${IfNot} ${Errors}
    StrCpy $NoShortcutMode 1
  ${EndIf}

  ${GetOptions} $CMDLINE "/UPDATE" $UpdateMode
  ${IfNot} ${Errors}
    StrCpy $UpdateMode 1
  ${EndIf}

  !if "${DISPLAYLANGUAGESELECTOR}" == "true"
    !insertmacro MUI_LANGDLL_DISPLAY
  !endif

  ; Loov: 自绘界面的 logo
  InitPluginsDir
  File "/oname=$PLUGINSDIR\logo-72.bmp" "${LOOV_ASSETS}\logo-72.bmp"
  File "/oname=$PLUGINSDIR\logo-108.bmp" "${LOOV_ASSETS}\logo-108.bmp"
  File "/oname=$PLUGINSDIR\logo-144.bmp" "${LOOV_ASSETS}\logo-144.bmp"

  !insertmacro SetContext

  ${If} $INSTDIR == "${PLACEHOLDER_INSTALL_DIR}"
    ; Set default install location
    !if "${INSTALLMODE}" == "perMachine"
      ${If} ${RunningX64}
        !if "${ARCH}" == "x64"
          StrCpy $INSTDIR "$PROGRAMFILES64\${PRODUCTNAME}"
        !else if "${ARCH}" == "arm64"
          StrCpy $INSTDIR "$PROGRAMFILES64\${PRODUCTNAME}"
        !else
          StrCpy $INSTDIR "$PROGRAMFILES\${PRODUCTNAME}"
        !endif
      ${Else}
        StrCpy $INSTDIR "$PROGRAMFILES\${PRODUCTNAME}"
      ${EndIf}
    !else if "${INSTALLMODE}" == "currentUser"
      StrCpy $INSTDIR "$LOCALAPPDATA\${PRODUCTNAME}"
    !endif

    Call RestorePreviousInstallLocation
  ${EndIf}


  !if "${INSTALLMODE}" == "both"
    !insertmacro MULTIUSER_INIT
  !endif
FunctionEnd


Section EarlyChecks
  ; Abort silent installer if downgrades is disabled
  !if "${ALLOWDOWNGRADES}" == "false"
  ${If} ${Silent}
    ; If downgrading
    ${If} $R0 = -1
      System::Call 'kernel32::AttachConsole(i -1)i.r0'
      ${If} $0 <> 0
        System::Call 'kernel32::GetStdHandle(i -11)i.r0'
        System::call 'kernel32::SetConsoleTextAttribute(i r0, i 0x0004)' ; set red color
        FileWrite $0 "$(silentDowngrades)"
      ${EndIf}
      Abort
    ${EndIf}
  ${EndIf}
  !endif

SectionEnd

Section WebView2
  ; Check if Webview2 is already installed and skip this section
  ${If} ${RunningX64}
    ReadRegStr $4 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${Else}
    ReadRegStr $4 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${EndIf}
  ${If} $4 == ""
    ReadRegStr $4 HKCU "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${EndIf}

  ${If} $4 == ""
    ; Webview2 installation
    ;
    ; Skip if updating
    ${If} $UpdateMode <> 1
      !if "${INSTALLWEBVIEW2MODE}" == "downloadBootstrapper"
        Delete "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        DetailPrint "$(webview2Downloading)"
        NSISdl::download "https://go.microsoft.com/fwlink/p/?LinkId=2124703" "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        Pop $0
        ${If} $0 == "success"
          DetailPrint "$(webview2DownloadSuccess)"
        ${Else}
          DetailPrint "$(webview2DownloadError)"
          Abort "$(webview2AbortError)"
        ${EndIf}
        StrCpy $6 "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        Goto install_webview2
      !endif

      !if "${INSTALLWEBVIEW2MODE}" == "embedBootstrapper"
        Delete "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        File "/oname=$TEMP\MicrosoftEdgeWebview2Setup.exe" "${WEBVIEW2BOOTSTRAPPERPATH}"
        DetailPrint "$(installingWebview2)"
        StrCpy $6 "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        Goto install_webview2
      !endif

      !if "${INSTALLWEBVIEW2MODE}" == "offlineInstaller"
        Delete "$TEMP\MicrosoftEdgeWebView2RuntimeInstaller.exe"
        File "/oname=$TEMP\MicrosoftEdgeWebView2RuntimeInstaller.exe" "${WEBVIEW2INSTALLERPATH}"
        DetailPrint "$(installingWebview2)"
        StrCpy $6 "$TEMP\MicrosoftEdgeWebView2RuntimeInstaller.exe"
        Goto install_webview2
      !endif

      Goto webview2_done

      install_webview2:
        DetailPrint "$(installingWebview2)"
        ; $6 holds the path to the webview2 installer
        ExecWait "$6 ${WEBVIEW2INSTALLERARGS} /install" $1
        ${If} $1 = 0
          DetailPrint "$(webview2InstallSuccess)"
        ${Else}
          DetailPrint "$(webview2InstallError)"
          Abort "$(webview2AbortError)"
        ${EndIf}
      webview2_done:
    ${EndIf}
  ${Else}
    !if "${MINIMUMWEBVIEW2VERSION}" != ""
      ${VersionCompare} "${MINIMUMWEBVIEW2VERSION}" "$4" $R0
      ${If} $R0 = 1
        update_webview:
          DetailPrint "$(installingWebview2)"
          ${If} ${RunningX64}
            ReadRegStr $R1 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate" "path"
          ${Else}
            ReadRegStr $R1 HKLM "SOFTWARE\Microsoft\EdgeUpdate" "path"
          ${EndIf}
          ${If} $R1 == ""
            ReadRegStr $R1 HKCU "SOFTWARE\Microsoft\EdgeUpdate" "path"
          ${EndIf}
          ${If} $R1 != ""
            ; Chromium updater docs: https://source.chromium.org/chromium/chromium/src/+/main:docs/updater/user_manual.md
            ; Modified from "HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Microsoft EdgeWebView\ModifyPath"
            ExecWait `"$R1" /install appguid=${WEBVIEW2APPGUID}&needsadmin=true` $1
            ${If} $1 = 0
              DetailPrint "$(webview2InstallSuccess)"
            ${Else}
              MessageBox MB_ICONEXCLAMATION|MB_ABORTRETRYIGNORE "$(webview2InstallError)" IDIGNORE ignore IDRETRY update_webview
              Quit
              ignore:
            ${EndIf}
          ${EndIf}
      ${EndIf}
    !endif
  ${EndIf}
SectionEnd

Section Install
  SetOutPath $INSTDIR

  !ifmacrodef NSIS_HOOK_PREINSTALL
    !insertmacro NSIS_HOOK_PREINSTALL
  !endif

  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"

  ; Copy main executable
  File "${MAINBINARYSRCPATH}"

  ; Copy resources
  {{#each resources_dirs}}
    CreateDirectory "$INSTDIR\\{{this}}"
  {{/each}}
  {{#each resources}}
    File /a "/oname={{this.[1]}}" "{{no-escape @key}}"
  {{/each}}

  ; Copy external binaries
  {{#each binaries}}
    File /a "/oname={{this}}" "{{no-escape @key}}"
  {{/each}}

  ; Create file associations
  {{#each file_associations as |association| ~}}
    {{#each association.ext as |ext| ~}}
       !insertmacro APP_ASSOCIATE "{{ext}}" "{{or association.name ext}}" "{{association-description association.description ext}}" "$INSTDIR\${MAINBINARYNAME}.exe,0" "Open with ${PRODUCTNAME}" "$INSTDIR\${MAINBINARYNAME}.exe $\"%1$\""
    {{/each}}
  {{/each}}

  ; Register deep links
  {{#each deep_link_protocols as |protocol| ~}}
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}" "URL Protocol" ""
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}" "" "URL:${BUNDLEID} protocol"
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}\DefaultIcon" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0"
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}\shell\open\command" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""
  {{/each}}

  ; Create uninstaller
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; Save $INSTDIR in registry for future installations
  WriteRegStr SHCTX "${MANUPRODUCTKEY}" "" $INSTDIR

  !if "${INSTALLMODE}" == "both"
    ; Save install mode to be selected by default for the next installation such as updating
    ; or when uninstalling
    WriteRegStr SHCTX "${UNINSTKEY}" $MultiUser.InstallMode 1
  !endif

  ; Remove old main binary if it doesn't match new main binary name
  ReadRegStr $OldMainBinaryName SHCTX "${UNINSTKEY}" "MainBinaryName"
  ${If} $OldMainBinaryName != ""
  ${AndIf} $OldMainBinaryName != "${MAINBINARYNAME}.exe"
    Delete "$INSTDIR\$OldMainBinaryName"
  ${EndIf}

  ; Save current MAINBINARYNAME for future updates
  WriteRegStr SHCTX "${UNINSTKEY}" "MainBinaryName" "${MAINBINARYNAME}.exe"

  ; Registry information for add/remove programs
  WriteRegStr SHCTX "${UNINSTKEY}" "DisplayName" "${PRODUCTNAME}"
  WriteRegStr SHCTX "${UNINSTKEY}" "DisplayIcon" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\""
  WriteRegStr SHCTX "${UNINSTKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr SHCTX "${UNINSTKEY}" "Publisher" "${MANUFACTURER}"
  WriteRegStr SHCTX "${UNINSTKEY}" "InstallLocation" "$\"$INSTDIR$\""
  WriteRegStr SHCTX "${UNINSTKEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegDWORD SHCTX "${UNINSTKEY}" "NoModify" "1"
  WriteRegDWORD SHCTX "${UNINSTKEY}" "NoRepair" "1"

  ${GetSize} "$INSTDIR" "/M=uninstall.exe /S=0K /G=0" $0 $1 $2
  IntOp $0 $0 + ${ESTIMATEDSIZE}
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD SHCTX "${UNINSTKEY}" "EstimatedSize" "$0"

  !if "${HOMEPAGE}" != ""
    WriteRegStr SHCTX "${UNINSTKEY}" "URLInfoAbout" "${HOMEPAGE}"
    WriteRegStr SHCTX "${UNINSTKEY}" "URLUpdateInfo" "${HOMEPAGE}"
    WriteRegStr SHCTX "${UNINSTKEY}" "HelpLink" "${HOMEPAGE}"
  !endif

  ; Create start menu shortcut
  !insertmacro MUI_STARTMENU_WRITE_BEGIN Application
    Call CreateOrUpdateStartMenuShortcut
  !insertmacro MUI_STARTMENU_WRITE_END

  ; Create desktop shortcut for silent and passive installers
  ; because finish page will be skipped
  ; Loov: 界面安装时由安装页的复选框决定（上游在完成页）
  ${If} $PassiveMode = 1
  ${OrIf} ${Silent}
  ${OrIf} $LoovDesktopShortcut = 1
    Call CreateOrUpdateDesktopShortcut
  ${EndIf}

  !ifmacrodef NSIS_HOOK_POSTINSTALL
    !insertmacro NSIS_HOOK_POSTINSTALL
  !endif

  ; Auto close this page for passive mode
  ${If} $PassiveMode = 1
    SetAutoClose true
  ${EndIf}
SectionEnd

Function .onInstSuccess
  ; Check for `/R` flag only in silent and passive installers because
  ; GUI installer has a toggle for the user to (re)start the app
  ${If} $PassiveMode = 1
  ${OrIf} ${Silent}
    ${GetOptions} $CMDLINE "/R" $R0
    ${IfNot} ${Errors}
      ${GetOptions} $CMDLINE "/ARGS" $R0
      nsis_tauri_utils::RunAsUser "$INSTDIR\${MAINBINARYNAME}.exe" "$R0"
    ${EndIf}
  ${EndIf}
FunctionEnd

Function un.onInit
  !insertmacro SetContext

  !if "${INSTALLMODE}" == "both"
    !insertmacro MULTIUSER_UNINIT
  !endif

  !insertmacro MUI_UNGETLANGUAGE

  ${GetOptions} $CMDLINE "/P" $PassiveMode
  ${IfNot} ${Errors}
    StrCpy $PassiveMode 1
  ${EndIf}

  ${GetOptions} $CMDLINE "/UPDATE" $UpdateMode
  ${IfNot} ${Errors}
    StrCpy $UpdateMode 1
  ${EndIf}
FunctionEnd

Section Uninstall

  !ifmacrodef NSIS_HOOK_PREUNINSTALL
    !insertmacro NSIS_HOOK_PREUNINSTALL
  !endif

  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"

  ; Delete the app directory and its content from disk
  ; Copy main executable
  Delete "$INSTDIR\${MAINBINARYNAME}.exe"

  ; Delete resources
  {{#each resources}}
    Delete "$INSTDIR\\{{this.[1]}}"
  {{/each}}

  ; Delete external binaries
  {{#each binaries}}
    Delete "$INSTDIR\\{{this}}"
  {{/each}}

  ; Delete app associations
  {{#each file_associations as |association| ~}}
    {{#each association.ext as |ext| ~}}
      !insertmacro APP_UNASSOCIATE "{{ext}}" "{{or association.name ext}}"
    {{/each}}
  {{/each}}

  ; Delete deep links
  {{#each deep_link_protocols as |protocol| ~}}
    ReadRegStr $R7 SHCTX "Software\Classes\\{{protocol}}\shell\open\command" ""
    ${If} $R7 == "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""
      DeleteRegKey SHCTX "Software\Classes\\{{protocol}}"
    ${EndIf}
  {{/each}}


  ; Delete uninstaller
  Delete "$INSTDIR\uninstall.exe"

  {{#each resources_ancestors}}
  RMDir /REBOOTOK "$INSTDIR\\{{this}}"
  {{/each}}
  RMDir "$INSTDIR"

  ; Remove shortcuts if not updating
  ${If} $UpdateMode <> 1
    !insertmacro DeleteAppUserModelId

    ; Remove start menu shortcut
    !insertmacro MUI_STARTMENU_GETFOLDER Application $AppStartMenuFolder
    !insertmacro IsShortcutTarget "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $0
    ${If} $0 = 1
      !insertmacro UnpinShortcut "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
      Delete "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
      RMDir "$SMPROGRAMS\$AppStartMenuFolder"
    ${EndIf}
    !insertmacro IsShortcutTarget "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $0
    ${If} $0 = 1
      !insertmacro UnpinShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk"
      Delete "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    ${EndIf}

    ; Remove desktop shortcuts
    !insertmacro IsShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $0
    ${If} $0 = 1
      !insertmacro UnpinShortcut "$DESKTOP\${PRODUCTNAME}.lnk"
      Delete "$DESKTOP\${PRODUCTNAME}.lnk"
    ${EndIf}
  ${EndIf}

  ; Remove registry information for add/remove programs
  !if "${INSTALLMODE}" == "both"
    DeleteRegKey SHCTX "${UNINSTKEY}"
  !else if "${INSTALLMODE}" == "perMachine"
    DeleteRegKey HKLM "${UNINSTKEY}"
  !else
    DeleteRegKey HKCU "${UNINSTKEY}"
  !endif

  ; Removes the Autostart entry for ${PRODUCTNAME} from the HKCU Run key if it exists.
  ; This ensures the program does not launch automatically after uninstallation if it exists.
  ; If it doesn't exist, it does nothing.
  ; We do this when not updating (to preserve the registry value on updates)
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}"
  ${EndIf}

  ; Delete app data if the checkbox is selected
  ; and if not updating
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    ; Clear the install location $INSTDIR from registry
    DeleteRegKey SHCTX "${MANUPRODUCTKEY}"
    DeleteRegKey /ifempty SHCTX "${MANUKEY}"

    ; Clear the install language from registry
    DeleteRegValue HKCU "${MANUPRODUCTKEY}" "Installer Language"
    DeleteRegKey /ifempty HKCU "${MANUPRODUCTKEY}"
    DeleteRegKey /ifempty HKCU "${MANUKEY}"

    SetShellVarContext current
    RmDir /r "$APPDATA\${BUNDLEID}"
    RmDir /r "$LOCALAPPDATA\${BUNDLEID}"
  ${EndIf}

  !ifmacrodef NSIS_HOOK_POSTUNINSTALL
    !insertmacro NSIS_HOOK_POSTUNINSTALL
  !endif

  ; Auto close if passive mode or updating
  ${If} $PassiveMode = 1
  ${OrIf} $UpdateMode = 1
    SetAutoClose true
  ${EndIf}
SectionEnd

Function RestorePreviousInstallLocation
  ReadRegStr $4 SHCTX "${MANUPRODUCTKEY}" ""
  StrCmp $4 "" +2 0
    StrCpy $INSTDIR $4
FunctionEnd

Function Skip
  Abort
FunctionEnd

Function SkipIfPassive
  ${IfThen} $PassiveMode = 1  ${|} Abort ${|}
FunctionEnd
Function un.SkipIfPassive
  ${IfThen} $PassiveMode = 1  ${|} Abort ${|}
FunctionEnd

Function CreateOrUpdateStartMenuShortcut
  ; We used to use product name as MAINBINARYNAME
  ; migrate old shortcuts to target the new MAINBINARYNAME
  StrCpy $R0 0

  !insertmacro IsShortcutTarget "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\$OldMainBinaryName"
  Pop $0
  ${If} $0 = 1
    !insertmacro SetShortcutTarget "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    StrCpy $R0 1
  ${EndIf}

  !insertmacro IsShortcutTarget "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\$OldMainBinaryName"
  Pop $0
  ${If} $0 = 1
    !insertmacro SetShortcutTarget "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    StrCpy $R0 1
  ${EndIf}

  ${If} $R0 = 1
    Return
  ${EndIf}

  ; Skip creating shortcut if in update mode or no shortcut mode
  ; but always create if migrating from wix
  ${If} $WixMode = 0
    ${If} $UpdateMode = 1
    ${OrIf} $NoShortcutMode = 1
      Return
    ${EndIf}
  ${EndIf}

  !if "${STARTMENUFOLDER}" != ""
    CreateDirectory "$SMPROGRAMS\$AppStartMenuFolder"
    CreateShortcut "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
  !else
    CreateShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\${PRODUCTNAME}.lnk"
  !endif
FunctionEnd

Function CreateOrUpdateDesktopShortcut
  ; We used to use product name as MAINBINARYNAME
  ; migrate old shortcuts to target the new MAINBINARYNAME
  !insertmacro IsShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\$OldMainBinaryName"
  Pop $0
  ${If} $0 = 1
    !insertmacro SetShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Return
  ${EndIf}

  ; Skip creating shortcut if in update mode or no shortcut mode
  ; but always create if migrating from wix
  ${If} $WixMode = 0
    ${If} $UpdateMode = 1
    ${OrIf} $NoShortcutMode = 1
      Return
    ${EndIf}
  ${EndIf}

  CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
  !insertmacro SetLnkAppUserModelId "$DESKTOP\${PRODUCTNAME}.lnk"
FunctionEnd
