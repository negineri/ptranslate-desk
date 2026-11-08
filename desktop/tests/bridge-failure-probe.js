// Chrome連携のパイプを別プロセスが占有していても、アプリの初期化が完了することを確認する。
(async () => {
  const invoke = window.__TAURI__.core.invoke;
  try {
    const status = await invoke("local_status");
    if (!status.chrome_error?.includes("Chrome連携を起動できませんでした")) throw new Error("連携失敗の状態がありません");
    const snapshot = await invoke("smoke_snapshot");
    if (!snapshot.main_visible || !snapshot.tray) throw new Error("デスクトップ本体が起動していません");
    // 登録の失敗を案内し、動作中の別プロセスの登録を書き換えない。
    let rejected = false;
    try { await invoke("register_chrome", { extensionId: "a".repeat(32) }); }
    catch (error) { rejected = String(error).includes("Chrome連携を起動できませんでした"); }
    if (!rejected) throw new Error("連携できない状態で登録が成功しました");
    await invoke("smoke_result", { passed: true, step: "Chrome連携失敗時の本体起動・状態通知・登録の保護" });
  } catch (error) {
    await invoke("smoke_result", { passed: false, step: String(error) });
  }
})();
