; LaterScreen Windows 安装器（NSIS）。
; 由 scripts/package.sh 调用：
;   makensis -DVERSION=x.y.z -DBINDIR=<exe 所在目录> -DOUTFILE=<输出路径> packaging/installer.nsi
Unicode true
Name "LaterScreen ${VERSION}"
OutFile "${OUTFILE}"
InstallDir "$PROGRAMFILES64\LaterScreen"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

Section "Install"
  SetOutPath "$INSTDIR"
  File "${BINDIR}/lscreen.exe"
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
