; LaterScreen Windows 安装器（NSIS）。
; 由 scripts/package.sh 调用：把本脚本与 lscreen.exe 复制到同一临时目录后
; 在该目录内执行 makensis -DVERSION=x.y.z installer.nsi，
; 产物 lscreen-setup.exe 再由脚本移到 dist/ 并改名。
; （File/OutFile 一律用相对文件名：NSIS 在 Windows/POSIX 上对
;   绝对路径分隔符的解析规则不同，相对同目录是唯一双平台稳妥解）
Unicode true
Name "LaterScreen ${VERSION}"
OutFile "lscreen-setup.exe"
InstallDir "$PROGRAMFILES64\LaterScreen"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

Section "Install"
  SetOutPath "$INSTDIR"
  File "lscreen.exe"
  CreateShortcut "$SMPROGRAMS\LaterScreen.lnk" "$INSTDIR\lscreen.exe"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\LaterScreen" \
    "DisplayName" "LaterScreen"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\LaterScreen" \
    "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\LaterScreen" \
    "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
SectionEnd

Section "Uninstall"
  Delete "$SMPROGRAMS\LaterScreen.lnk"
  Delete "$INSTDIR\lscreen.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\LaterScreen"
SectionEnd
