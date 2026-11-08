(async () => {
  const invoke = window.__TAURI__.core.invoke;
  const wait = async (check) => {
    const end = Date.now() + 15000;
    while (!await check()) {
      if (Date.now() > end) throw new Error("登録確認のタイムアウト");
      await new Promise((resolve) => setTimeout(resolve, 30));
    }
  };
  try {
    const $ = (id) => document.getElementById(id);
    await wait(() => $("chrome-registration-confirm")?.open);
    const pending = await invoke("chrome_registration_request");
    if (!/^[ab]{32}$/.test(pending.extension_id) || $("chrome-registration-id").textContent !== pending.extension_id) throw new Error("受信したIDと確認画面が違います");
    const state = await invoke("local_status");
    if (state.enabled || state.loading) throw new Error("連携要求で翻訳モードが変更されました");
    $("chrome-registration-cancel").click();
    await wait(async () => !$("chrome-registration-confirm").open && !await invoke("chrome_registration_request"));
    let staleRejected = false;
    try { await invoke("finish_chrome_registration", { token: pending.token, approve: true }); }
    catch { staleRejected = true; }
    if (!staleRejected) throw new Error("キャンセル後に登録できました");
    await invoke("smoke_result", { passed: true, step: "URL受信・ID表示・キャンセル・承認の失効" });
  } catch (error) { await invoke("smoke_result", { passed: false, step: String(error) }); }
})();
