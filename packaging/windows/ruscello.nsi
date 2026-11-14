Unicode true
RequestExecutionLevel user
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "MUI2.nsh"

Name "Ruscello"
OutFile "${OUT}"
InstallDir "$LOCALAPPDATA\Programs\Ruscello"
InstallDirRegKey HKCU "Software\Ruscello" "InstallDir"
SetCompressor /SOLID lzma

!insertmacro MUI_PAGE_LICENSE "..\..\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "Italian"
LangString InstallFailed ${LANG_ENGLISH} "Ruscello could not be installed. Close the application and try again."
LangString InstallFailed ${LANG_ITALIAN} "Impossibile installare Ruscello. Chiudi l’applicazione e riprova."

Var UpdateMode

Function .onInit
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} $0 "/UPDATE" $1
  ${IfNot} ${Errors}
    StrCpy $UpdateMode "yes"
  ${EndIf}
FunctionEnd

Section "Ruscello"
  SetShellVarContext current
  SetOutPath "$INSTDIR"

  ; Stage both files on the destination volume before changing the installation.
  SetOverwrite try
  ClearErrors
  File /oname=ruscello.exe.new "${BIN}\ruscello.exe"
  IfErrors failed
  File /oname=ruscello-webview.exe.new "${BIN}\ruscello-webview.exe"
  IfErrors failed

  IfFileExists "$INSTDIR\ruscello.exe.old" failed
  IfFileExists "$INSTDIR\ruscello-webview.exe.old" failed
  StrCpy $0 0
  IfFileExists "$INSTDIR\ruscello.exe" waitDesktop moveHelper
waitDesktop:
  ClearErrors
  Rename "$INSTDIR\ruscello.exe" "$INSTDIR\ruscello.exe.old"
  IfErrors 0 moveHelper
  IntOp $0 $0 + 1
  IntCmp $0 120 failed 0 failed
  Sleep 250
  Goto waitDesktop

moveHelper:
  IfFileExists "$INSTDIR\ruscello-webview.exe" 0 replace
  ClearErrors
  Rename "$INSTDIR\ruscello-webview.exe" "$INSTDIR\ruscello-webview.exe.old"
  IfErrors restoreDesktop
replace:
  ClearErrors
  Rename "$INSTDIR\ruscello-webview.exe.new" "$INSTDIR\ruscello-webview.exe"
  IfErrors restoreHelper
  Rename "$INSTDIR\ruscello.exe.new" "$INSTDIR\ruscello.exe"
  IfErrors restoreHelper
  Delete "$INSTDIR\ruscello.exe.old"
  Delete "$INSTDIR\ruscello-webview.exe.old"

  File "..\..\LICENSE"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Ruscello" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Ruscello" "DisplayName" "Ruscello"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Ruscello" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Ruscello" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  CreateShortcut "$SMPROGRAMS\Ruscello.lnk" "$INSTDIR\ruscello.exe"
  ${If} $UpdateMode == "yes"
    Exec '"$INSTDIR\ruscello.exe"'
  ${EndIf}
  Goto done

restoreHelper:
  IfFileExists "$INSTDIR\ruscello-webview.exe.old" 0 restoreDesktop
  Delete "$INSTDIR\ruscello-webview.exe"
  Rename "$INSTDIR\ruscello-webview.exe.old" "$INSTDIR\ruscello-webview.exe"
restoreDesktop:
  IfFileExists "$INSTDIR\ruscello.exe.old" 0 failed
  Rename "$INSTDIR\ruscello.exe.old" "$INSTDIR\ruscello.exe"
failed:
  Delete "$INSTDIR\ruscello.exe.new"
  Delete "$INSTDIR\ruscello-webview.exe.new"
  ${If} $UpdateMode == "yes"
    Exec '"$INSTDIR\ruscello.exe" --update-failed'
  ${Else}
    MessageBox MB_OK|MB_ICONSTOP "$(InstallFailed)" /SD IDOK
  ${EndIf}
  SetErrorLevel 1
  Quit
done:
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  Delete "$INSTDIR\ruscello.exe"
  Delete "$INSTDIR\ruscello-webview.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\Uninstall.exe"
  Delete "$SMPROGRAMS\Ruscello.lnk"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Ruscello"
  DeleteRegKey HKCU "Software\Ruscello"
  RMDir "$INSTDIR"
SectionEnd
