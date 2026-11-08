// 実エンジンを使う明示的な手動試験。通常のCIでは実行しない。
(async () => {
  const invoke = window.__TAURI__.core.invoke;
  const wait = async (check) => {
    const end = Date.now() + 180000;
    while (!await check()) {
      if (Date.now() > end) throw new Error("タイムアウト");
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  };
  let step = "ローカルモードの開始";
  try {
    await wait(() => document.getElementById("target")?.options.length >= 53);
    const state = await invoke("local_status");
    if (!state.enabled || !state.available) throw new Error("モデルが利用できません");
    const devices = await invoke("local_devices");
    if (state.device !== "none" && !devices.some((d) => d.id === state.device)) throw new Error("GPUが見つかりません");
    step = "モデルの読み込み";
    const begin = performance.now();
    if (state.auto_release) {
      if (state.ready) throw new Error("自動解放時に起動だけでモデルを読み込んでいる");
      await invoke("start_local_engine");
    } else {
      await wait(async () => (await invoke("local_status")).ready);
    }
    const loadMs = Math.round(performance.now() - begin);
    const results = [];
    for (let i = 0; i < 2; i++) {
      step = `翻訳${i + 1}`;
      const messages = [];
      let ended = false;
      let firstMs = null;
      const start = performance.now();
      const channel = new window.__TAURI__.core.Channel();
      channel.onmessage = (message) => {
        messages.push(message);
        if (message.kind === "bytes" && firstMs === null) firstMs = Math.round(performance.now() - start);
        if (message.kind === "end" || message.kind === "error") ended = true;
      };
      await invoke("start_request", { id: `local-probe-${i}`, route: "api/translate", body: { text: "Hello. This application translates text on your computer.", target_lang: "ja" }, stream: channel });
      await wait(() => ended);
      const raw = new TextDecoder().decode(new Uint8Array(messages.filter((m) => m.kind === "bytes").flatMap((m) => m.bytes)));
      const events = raw.split("\n").filter((s) => s.startsWith("data: ")).map((s) => JSON.parse(s.slice(6)));
      const text = events.map((e) => e.t || "").join("");
      if (!messages.some((m) => m.kind === "start" && m.status === 200) || !events.some((e) => e.done) || !/[ぁ-んァ-ン一-龯]/.test(text) || events.some((e) => e.error)) throw new Error("翻訳が完了しませんでした");
      results.push({ firstMs, totalMs: Math.round(performance.now() - start), chars: text.length });
    }
    step = "画面からの翻訳";
    const $ = (id) => document.getElementById(id);
    $("translation-tab").click();
    $("auto").checked = false;
    $("target").value = "ja";
    $("src").value = "Hello. This application translates text on your computer.";
    $("go").click();
    await wait(() => $("status").textContent.includes("訳し終わりました") || $("status").classList.contains("err"));
    if ($("status").classList.contains("err")) throw new Error("翻訳画面にエラーが出ています");
    if (!/[ぁ-んァ-ン一-龯]/.test($("out").textContent)) throw new Error("訳が表示されませんでした");
    step = "翻訳の中止";
    let canceled = false;
    const channel = new window.__TAURI__.core.Channel();
    channel.onmessage = (message) => { if (message.kind === "canceled") canceled = true; };
    await invoke("start_request", { id: "local-probe-cancel", route: "api/translate", body: { text: "This application translates text on your computer. ".repeat(60), target_lang: "ja" }, stream: channel });
    await invoke("stop_request", { id: "local-probe-cancel" });
    await wait(() => canceled);
    if (state.auto_release) {
      step = "モデルの自動解放";
      await wait(async () => !(await invoke("local_status")).ready);
      if ((await invoke("local_status")).ready) throw new Error("モデルが解放されませんでした");
    }
    // 自動解放を無効にした場合は、以下のsmoke_resultによるアプリ終了で解放する。
    await invoke("smoke_result", { passed: true, step: JSON.stringify({ device: state.device, loadMs, results }) });
  } catch (error) {
    await invoke("smoke_result", { passed: false, step: `${step}: ${error}` });
  }
})();
