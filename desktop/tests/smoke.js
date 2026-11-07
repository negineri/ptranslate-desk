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
    step = "ログイン前のヘッダー";
    await wait(() => $("version-badge")?.textContent.startsWith("v"));
    if (!$("version-badge").textContent.includes("0.0.0-dev")) throw new Error("開発版のバージョン");
    if (!$("app-header").contains($("check-update"))) throw new Error("更新操作の配置");
    $("version-badge").click();
    if (!$("update-menu").open || !$("check-update").getClientRects().length) throw new Error("更新メニュー");
    $("version-badge").dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    if ($("update-menu").open) throw new Error("更新メニューを閉じる");
    step = "ログイン前の接続先設定";
    await wait(() => $("connection-form") && $("desktop-settings"));
    if ($("connection-settings").closest("section")) throw new Error("接続先設定がログイン前に操作できない");
    if ($("settings-page").hidden || !$("translation-page").hidden) throw new Error("未ログイン時の初期タブ");
    if ($("settings-page").hidden || !$("translation-page").hidden || $("settings-page").closest("[inert]")) throw new Error("ログイン前の設定タブ");
    if (!$("settings-page").contains($("connection-form"))) throw new Error("接続先の配置");
    $("translation-tab").click();
    const connection = await window.__TAURI__.core.invoke("load_connection");
    if (connection.server_url || connection.discovery_url || connection.client_id !== "translator-desktop") throw new Error("接続先の初期値");
    if ($("server-url").value !== connection.server_url) throw new Error("接続先の読み込み");
    let rejected = false;
    try {
      await window.__TAURI__.core.invoke("save_connection", { connection: { ...connection, server_url: "http://example.com" } });
    } catch { rejected = true; }
    if (!rejected) throw new Error("安全でない接続先を保存できてしまう");
    step = "接続先の保存ボタンと確認";
    $("settings-tab").click();
    $("server-url").value = "http://translate.example.com";
    $("discovery-url").value = "https://auth.example.com/.well-known/openid-configuration";
    $("save-connection").click();
    if (!$("connection-confirm").matches(":modal")) throw new Error("保存の確認がモーダルで表示されない");
    await wait(() => document.activeElement === $("cancel-connection"));
    $("cancel-connection").click();
    if ($("connection-confirm").open || $("connection-status").textContent) throw new Error("保存のキャンセル");
    if (document.activeElement !== $("save-connection")) throw new Error("キャンセル後のフォーカス");
    $("save-connection").click();
    $("confirm-connection").click();
    await wait(() => $("connection-status").classList.contains("err"));
    if (!$("connection-status").textContent.includes("HTTPS")) throw new Error("保存ボタンから検証処理に進めない");
    if ($("save-connection").disabled || $("connection-confirm").open) throw new Error("保存失敗後の再操作");
    if ((await window.__TAURI__.core.invoke("load_connection")).server_url) throw new Error("不正な接続先が保存された");
    $("server-url").value = connection.server_url;
    $("discovery-url").value = connection.discovery_url;
    step = "ログイン";
    await wait(() => [...document.querySelectorAll("button")].some((b) => b.textContent === "モックにログイン"));
    [...document.querySelectorAll("button")].find((b) => b.textContent === "モックにログイン").click();
    await wait(() => !document.querySelector("section").inert);
    if ($("target").options.length < 53) throw new Error("言語一覧");
    step = "ログイン後のユーザーメニュー";
    if ($("account-menu").hidden || !$("who").textContent || !$("app-header").contains($("who"))) throw new Error(step);
    $("account-menu").querySelector("summary").click();
    if (!$("login-button").getClientRects().length || $("login-button").textContent !== "ログアウト") throw new Error(step);
    $("translation-tab").click();
    if ($("account-menu").open) throw new Error("ユーザーメニューの外を押して閉じる");
    step = "自動翻訳とストリーミング";
    $("src").value = "Hello Mac";
    $("src").dispatchEvent(new Event("input"));
    await wait(() => $("out").textContent.includes("Hello"));
    if ($("stop").hidden) throw new Error("ストリーミング");
    await wait(() => $("status").textContent === "訳し終わりました");
    if (!$("out").textContent.includes("[ja] Hello Mac")) throw new Error("訳文");
    step = "設定タブへの切り替えと翻訳の保持";
    const output = $("out").textContent;
    $("settings-tab").click();
    if ($("settings-page").hidden || !$("translation-page").hidden || !$("settings-page").contains($("desktop-settings"))) throw new Error(step);
    $("settings-tab").dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }));
    if ($("translation-page").hidden || !$("settings-page").hidden || $("src").value !== "Hello Mac" || $("out").textContent !== output) throw new Error(step);
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
    step = "メインウィンドウを閉じても常駐";
    await action("close-main");
    await wait(async () => !(await snapshot()).main_visible);
    if (!(await snapshot()).tray) throw new Error(step);
    step = "ショートカットから翻訳ポップアップのストリーミング";
    await action("shortcut", "Popup " + "waiting ".repeat(20));
    await wait(async () => (await snapshot()).popup.output.includes("Popup"));
    if (!(await snapshot()).popup_visible || (await snapshot()).requests !== 1) throw new Error(step);
    step = "Esc で翻訳ポップアップを閉じて中止";
    await action("escape");
    await wait(async () => !(await snapshot()).popup_visible && (await snapshot()).requests === 0);
    if (await invoke("popup_content") !== null) throw new Error(step);
    step = "翻訳ポップアップで翻訳とコピー";
    await action("shortcut", "Hello popup");
    await wait(async () => (await snapshot()).popup.status === "訳し終わりました");
    if (!(await snapshot()).popup.output.includes("[ja] Hello popup")) throw new Error(step);
    await action("copy");
    await wait(async () => (await snapshot()).popup.status === "訳をコピーしました");
    step = "翻訳ポップアップからメインウィンドウへの引き継ぎ";
    await action("open-main");
    await wait(() => $("src").value === "Hello popup");
    await wait(() => $("status").textContent.startsWith("訳し終わりました"));
    if (!(await snapshot()).main_visible || (await snapshot()).popup_visible) throw new Error(step);
    step = "メニューバーの設定と一時停止";
    await menu("settings");
    await wait(() => $("desktop-settings").open && !$("settings-page").hidden);
    await menu("pause");
    if (!(await invoke("session_status")).paused) throw new Error(step);
    await action("shortcut", "Paused");
    await new Promise((resolve) => setTimeout(resolve, 250));
    if ((await snapshot()).popup_visible) throw new Error(step);
    await menu("pause");
    step = "翻訳ポップアップの外にフォーカスを移して閉じる";
    await action("shortcut", "Focus " + "waiting ".repeat(20));
    await wait(async () => (await snapshot()).popup.output.includes("Focus"));
    await menu("main");
    await wait(async () => !(await snapshot()).popup_visible && (await snapshot()).requests === 0);
    step = "ログアウト";
    await menu("logout");
    await wait(async () => !(await invoke("session_status")).logged_in);
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
