// 実モデルを使う明示的な手動確認。設定は一時ディレクトリに分離する。
(async () => {
  const { invoke } = window.__TAURI__.core;
  const $ = (id) => document.getElementById(id);
  const wait = async (check) => {
    const end = Date.now() + 180000;
    while (!await check()) {
      if (Date.now() > end) throw new Error("タイムアウト");
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  };
  let step = "起動時のモデル読み込み";
  try {
    await wait(async () => (await invoke("local_status")).ready);
    const initial = await invoke("local_status");
    await wait(() => $("target")?.options.length >= 53);
    const expected = (await invoke("local_devices")).length + 1;
    step = "画面からデバイスの計測";
    $("settings-tab").click();
    if ($("local-setup").hidden) throw new Error("モデル取得後のデバイス・メモリ設定が表示されない");
    await wait(() => !$("local-measure").disabled);
    const start = performance.now();
    $("local-measure").click();
    await wait(() => $("local-candidates").children.length === expected && $("local-cancel-measure").hidden);
    if ($("local-candidates").textContent.split("おすすめ").length !== 2) throw new Error("おすすめの候補");
    const recommended = $("local-device").value;
    step = "手動選択の保持";
    $("local-device").value = "none";
    $("local-device").dispatchEvent(new Event("change"));
    await new Promise((resolve) => setTimeout(resolve, 1200));
    if ($("local-device").value !== "none") throw new Error("手動選択が上書きされた");
    if ((await invoke("local_status")).device !== initial.device) throw new Error("候補が勝手に保存された");
    step = "計測後にモデルを再読み込み";
    await wait(async () => (await invoke("local_status")).ready);
    await invoke("smoke_result", { passed: true, step: JSON.stringify({ candidates: expected, recommended, manual: "none", totalMs: Math.round(performance.now() - start) }) });
  } catch (error) {
    await invoke("smoke_result", { passed: false, step: `${step}: ${error}` });
  }
})();
