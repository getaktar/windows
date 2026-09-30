; File Explorer integration for the NSIS install (see src/shell.rs). The app
; itself adds "Upload with Aktar" to the right-click menu, in the language
; it runs in; the installer adds the "Send to" shortcut and the uninstaller
; removes both. The Microsoft Store package doesn't use this file.

!macro NSIS_HOOK_POSTINSTALL
  CreateShortCut "$SENDTO\Aktar.lnk" "$INSTDIR\${MAINBINARYNAME}.exe" "--upload" "$INSTDIR\${MAINBINARYNAME}.exe" 0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; An update reinstalls over the old copy with the uninstaller in update
  ; mode; keep the entries then, the new version adds them back anyway.
  ${If} $UpdateMode <> 1
    Delete "$SENDTO\Aktar.lnk"
    DeleteRegKey HKCU "Software\Classes\*\shell\Aktar.Upload"
  ${EndIf}
!macroend
