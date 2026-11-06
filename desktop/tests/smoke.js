(async () => {
  let step = "ログイン";
  const $ = (id) => document.getElementById(id);
  const wait = async (check) => {
    const end = Date.now() + 15000;
    while (!await check()) {
      if (Date.now() > end) throw new Error("タイムアウト");
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  };
  try {
    await wait(() => [...document.querySelectorAll("button")].some((b) => b.textContent === "モックにログイン"));
    [...document.querySelectorAll("button")].find((b) => b.textContent === "モックにログイン").click();
    await wait(() => !document.querySelector("section").inert);
    if ($("target").options.length < 53) throw new Error("言語一覧");
    step = "自動翻訳とストリーミング";
    $("src").value = "Hello Mac";
    $("src").dispatchEvent(new Event("input"));
    await wait(() => $("out").textContent.includes("Hello"));
    if ($("stop").hidden) throw new Error("ストリーミング");
    await wait(() => $("status").textContent === "訳し終わりました");
    if (!$("out").textContent.includes("[ja] Hello Mac")) throw new Error("訳文");
    step = "訳をコピー";
    $("copy").click();
    await wait(() => $("status").textContent === "訳をコピーしました");
    step = "中止";
    $("src").value = "Cancel " + "waiting ".repeat(20);
    $("go").click();
    await wait(() => $("out").textContent.includes("Cancel"));
    $("stop").click();
    await wait(() => $("status").textContent === "中止しました");
    $("clear").click();
    if ($("src").value || $("out").textContent) throw new Error("空にする");
    const { invoke } = window.__TAURI__.core;
    const snapshot = () => invoke("smoke_snapshot");
    const action = (action, text = null) => invoke("smoke_action", { action, text });
    const menu = (action) => invoke("desktop_action", { action });
    step = "ショートカットの変更と設定の保存";
    const saved = await invoke("load_settings");
    await invoke("save_settings", { settings: { ...saved, shortcut: "CommandOrControl+Shift+T" } });
    if ((await invoke("session_status")).shortcut !== "CommandOrControl+Shift+T") throw new Error(step);
    await invoke("save_settings", { settings: { ...saved, shortcut: "double-copy" } });
    step = "主窓を閉じても常駐";
    await action("close-main");
    await wait(async () => !(await snapshot()).main_visible);
    if (!(await snapshot()).tray) throw new Error(step);
    step = "ショートカットから小窓のストリーミング";
    await action("shortcut", "Popup " + "waiting ".repeat(20));
    await wait(async () => (await snapshot()).popup.output.includes("Popup"));
    if (!(await snapshot()).popup_visible || (await snapshot()).requests !== 1) throw new Error(step);
    step = "Esc で小窓を閉じて中止";
    await action("escape");
    await wait(async () => !(await snapshot()).popup_visible && (await snapshot()).requests === 0);
    if (await invoke("popup_content") !== null) throw new Error(step);
    step = "小窓で翻訳とコピー";
    await action("shortcut", "Hello popup");
    await wait(async () => (await snapshot()).popup.status === "訳し終わりました");
    if (!(await snapshot()).popup.output.includes("[ja] Hello popup")) throw new Error(step);
    await action("copy");
    await wait(async () => (await snapshot()).popup.status === "訳をコピーしました");
    step = "小窓から主窓への引き継ぎ";
    await action("open-main");
    await wait(() => $("src").value === "Hello popup");
    await wait(() => $("status").textContent.startsWith("訳し終わりました"));
    if (!(await snapshot()).main_visible || (await snapshot()).popup_visible) throw new Error(step);
    step = "メニューバーの設定と一時停止";
    await menu("settings");
    await wait(() => $("desktop-settings").open);
    await menu("pause");
    if (!(await invoke("session_status")).paused) throw new Error(step);
    await action("shortcut", "Paused");
    await new Promise((resolve) => setTimeout(resolve, 250));
    if ((await snapshot()).popup_visible) throw new Error(step);
    await menu("pause");
    step = "小窓の外にフォーカスを移して閉じる";
    await action("shortcut", "Focus " + "waiting ".repeat(20));
    await wait(async () => (await snapshot()).popup.output.includes("Focus"));
    await menu("main");
    await wait(async () => !(await snapshot()).popup_visible && (await snapshot()).requests === 0);
    step = "ログアウト";
    await menu("logout");
    let refused = false;
    try {
      await window.__TAURI__.core.invoke("start_request", {
        id: "logged-out", route: "api/config", body: null, popupRevision: null,
        stream: new window.__TAURI__.core.Channel(),
      });
    } catch { refused = true; }
    if (!refused) throw new Error("ログアウト");
    await window.__TAURI__.core.invoke("smoke_result", { passed: true, step });
  } catch (error) {
    await window.__TAURI__.core.invoke("smoke_result", { passed: false, step: `${step}: ${error.message || error}` });
  }
})();
