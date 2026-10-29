; Loov Agent 安装程序钩子（tauri.conf.json bundle.windows.nsis.installerHooks）
; 自定义模板 installer.nsi 通过这里拿到素材目录：模板由 Tauri 渲染到 target 目录再编译，
; 只有钩子文件保留原始路径。

!define LOOV_ASSETS "${__FILEDIR__}\assets"

; 升级后桌面/开始菜单快捷方式仍显示旧图标：exe 路径没变，资源管理器沿用了图标缓存。
; 安装完通知外壳刷新关联与图标缓存。
!macro NSIS_HOOK_POSTINSTALL
  ; SHCNE_ASSOCCHANGED, SHCNF_IDLIST | SHCNF_FLUSH
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0x1000, p 0, p 0)'
  ${If} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
    ; SHCNE_UPDATEITEM, SHCNF_PATHW | SHCNF_FLUSH
    System::Call 'shell32::SHChangeNotify(i 0x2000, i 0x1005, w "$DESKTOP\${PRODUCTNAME}.lnk", p 0)'
  ${EndIf}
  ${If} ${FileExists} "$SYSDIR\ie4uinit.exe"
    nsExec::Exec '"$SYSDIR\ie4uinit.exe" -show'
    Pop $0
  ${EndIf}
!macroend
