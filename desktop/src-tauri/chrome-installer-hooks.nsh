; 更新ではChrome連携を維持し、通常の削除でだけ登録を解除する。
!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    ExecWait '$"$INSTDIR\${MAINBINARYNAME}.exe$" --unregister-chrome'
  ${EndIf}
!macroend
