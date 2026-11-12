(async () => {
  let step = "ログイン";
  const $ = (id) => document.getElementById(id);
  const wait = async (check, timeoutMessage = () => "タイムアウト") => {
    const end = Date.now() + 15000;
    while (!await check()) {
      if (Date.now() > end) throw new Error(timeoutMessage());
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  };
  try {
    step = "初回ウィザード";
    await wait(() => $("setup-wizard") && $("wizard-next").disabled);
    const wizard = $("setup-wizard");
    const currentStep = () => wizard.querySelector("[data-step]:not([hidden])")?.dataset.step;
    if (!$("settings-page").inert || currentStep() !== "usage") throw new Error("初回画面");
    const localChoice = wizard.querySelector('[data-profile="both"]');
    if (!localChoice.disabled) {
      localChoice.click();
      $("wizard-next").click();
      await wait(() => currentStep() === "license");
      if (!$("wizard-next").disabled) throw new Error("ライセンス同意なしで進める");
      $("wizard-license-agree").click();
      $("wizard-next").click();
      await wait(() => currentStep() === "model");
      if (!$("wizard-next").disabled) throw new Error("モデル取得前に進める");
      if (!$("wizard-model-description").textContent || !$("local-download").getClientRects().length) throw new Error("モデル取得画面の案内");
      $("wizard-back").click();
      $("wizard-back").click();
    }
    wizard.querySelector('[data-profile="server"]').click();
    $("wizard-next").click();
    await wait(() => currentStep() === "license");
    if (!$("wizard-license-agree").checked) $("wizard-license-agree").click();
    $("wizard-next").click();
    await wait(() => currentStep() === "connection");
    $("server-url").value = "https://wizard.example.com";
    $("discovery-url").value = "https://auth.example.com/.well-known/openid-configuration";
    $("wizard-next").click();
    await wait(() => currentStep() === "login");
    $("wizard-login").click();
    await wait(() => !$("wizard-next").disabled);
    $("wizard-next").click();
    await wait(() => currentStep() === "shortcut");
    if (!wizard.contains($("shortcut")) || $("shortcut").value !== "double-copy") throw new Error("ウィザードのショートカット設定");
    $("wizard-next").click();
    await wait(() => currentStep() === "done");
    $("wizard-next").click();
    await wait(() => !$("setup-wizard") && $("desktop-settings") && !document.querySelector("#translation-page section").inert);
    if (await window.__TAURI__.core.invoke("setup_state") !== "done") throw new Error("初期設定完了の保存");
    $("settings-tab").click();
    $("desktop-settings").open = false;
    if (!$("start-setup").closest(".settings-heading") || !$("start-setup").getClientRects().length) throw new Error("設定見出しの初期設定ボタン");
    $("start-setup").click();
    await wait(() => $("setup-wizard") && !$("wizard-close").hidden);
    $("wizard-close").click();
    await wait(() => !$("setup-wizard") && !$("settings-page").inert);
    if (await window.__TAURI__.core.invoke("setup_state") !== "done") throw new Error("手動で開いたウィザードで完了状態が変わった");
    $("server-active").click();
    await wait(async () => !(await window.__TAURI__.core.invoke("session_status")).server_authenticated);
    step = "初回の設定";
    await wait(() => $("local-active") && $("connection-settings") && !$("settings-page").hidden);
    if ($("mode-welcome") || $("mode-local") || $("account-menu")) throw new Error("旧設定が表示されている");
    $("local-settings").open = false;
    $("connection-settings").open = false;
    if (!$("local-active").getClientRects().length || !$("server-active").getClientRects().length) throw new Error("折りたたみ時のスイッチ");
    $("connection-settings").open = true;
    $("local-settings").open = true;
    if ((await window.__TAURI__.core.invoke("local_status")).supported) {
      $("local-download").click();
      if (!$("local-download-confirm").matches(":modal")) throw new Error("モデル取得の確認");
      $("local-decline-download").click();
      if ($("local-download-confirm").open) throw new Error("モデル取得のキャンセル");
      let rejectedDownload = false;
      try {
        await window.__TAURI__.core.invoke("download_local_resources", { confirmed: false, progress: new window.__TAURI__.core.Channel() });
      } catch (e) { rejectedDownload = String(e).includes("確認"); }
      if (!rejectedDownload) throw new Error("確認なしで取得できてしまう");
      if (!$("local-setup").hidden) throw new Error("モデル取得前にデバイス・メモリ設定が表示される");
      if ($("local-auto-release").checked) throw new Error("自動解放の初期値");
    }
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
    if (connection.server_url !== "https://wizard.example.com" || connection.client_id !== "translator-desktop") throw new Error("ウィザードで設定した接続先");
    if ($("server-url").value !== connection.server_url) throw new Error("接続先の読み込み");
    let rejected = false;
    try {
      await window.__TAURI__.core.invoke("save_connection", { connection: { ...connection, server_url: "http://example.com" } });
    } catch { rejected = true; }
    if (!rejected) throw new Error("安全でない接続先を保存できてしまう");
    step = "接続先の適用とキャンセル";
    $("settings-tab").click();
    const editConnection = () => {
      $("server-url").value = "http://translate.example.com";
      $("discovery-url").value = "https://auth.example.com/.well-known/openid-configuration";
      $("server-url").dispatchEvent(new Event("input", { bubbles: true }));
    };
    editConnection();
    if ($("settings-apply").hidden || getComputedStyle($("settings-apply")).position !== "fixed") throw new Error("固定の適用ダイアログ");
    $("cancel-settings").click();
    if (!$("settings-apply").hidden || $("server-url").value !== connection.server_url ||
        $("discovery-url").value !== connection.discovery_url) throw new Error("キャンセルで復元されない");
    editConnection();
    $("apply-settings").click();
    await wait(() => $("settings-apply").textContent.includes("HTTPS") && !$("apply-settings").disabled);
    if ((await window.__TAURI__.core.invoke("load_connection")).server_url !== connection.server_url) throw new Error("不正な接続先が保存された");
    $("cancel-settings").click();
    step = "接続先を再起動せずに適用";
    $("connection-settings").open = true;
    $("src").value = "Keep this text";
    $("server-url").value = "https://translate.example.com";
    $("discovery-url").value = "https://auth.example.com/.well-known/openid-configuration";
    $("server-url").dispatchEvent(new Event("input", { bubbles: true }));
    $("apply-settings").click();
    await wait(async () => $("settings-apply").hidden &&
      (await window.__TAURI__.core.invoke("load_connection")).server_url === "https://translate.example.com");
    if ($("src").value !== "Keep this text" || $("settings-page").hidden) throw new Error("適用で画面の状態を失った");
    $("src").value = "";
    step = "ログイン";
    $("translation-tab").click();
    $("destination-server").click();
    if ($("settings-page").hidden || !$("app-header").contains($("destination-server")) ||
        $("app-header").contains($("server-active"))) throw new Error("無効なサーバーから設定への導線");
    await wait(() => $("server-active") && !$("server-active").disabled);
    $("connection-settings").open = false;
    $("server-active").click();
    step = "サーバースイッチによるログイン";
    await wait(() => $("destination-server").querySelector("small").textContent === "有効");
    await wait(() => !document.querySelector("section").inert);
    if ($("target").options.length < 53) throw new Error("言語一覧");
    step = "一時的な翻訳先";
    const server = $("destination-server"), local = $("destination-local");
    if (server.getAttribute("aria-pressed") !== "true" || server.querySelector("small").textContent !== "有効" ||
        local.querySelector("small").textContent !== "無効") throw new Error("利用できる翻訳先");
    const serverBounds = server.getBoundingClientRect(), localBounds = local.getBoundingClientRect();
    if (serverBounds.width !== localBounds.width || serverBounds.top !== localBounds.top ||
        getComputedStyle(server).borderColor === getComputedStyle(local).borderColor) throw new Error("翻訳先の表示");
    let unavailable = false;
    try {
      await window.__TAURI__.core.invoke("start_request", {
        id: "unavailable-local", route: "api/translate", body: null, localOverride: true,
        stream: new window.__TAURI__.core.Channel(),
      });
    } catch { unavailable = true; }
    if (!unavailable) throw new Error("ローカル翻訳が使えないときに要求を受け付けた");
    $("src").value = "Keep the source";
    $("translation-tab").click();
    local.click();
    if ($("settings-page").hidden || $("local-options").hidden || $("src").value !== "Keep the source" ||
        server.getAttribute("aria-pressed") !== "true" || $("connection-settings").hidden ||
        document.activeElement !== $("local-settings-title")) throw new Error("無効なローカルから設定への導線");
    $("src").value = "";
    if (!$("server-active").checked || !$("server-settings-panel").contains($("server-active"))) throw new Error("サーバースイッチ");
    $("translation-tab").click();
    step = "自動翻訳とストリーミング";
    const translationStatus = $("status");
    const translationTimeout = () => translationStatus.textContent || "状態表示なしでタイムアウト";
    $("src").value = "Hello Mac";
    $("src").dispatchEvent(new Event("input"));
    step = "自動翻訳の開始";
    await wait(() => !$("stop").hidden, translationTimeout);
    step = "最初の訳の断片";
    await wait(() => $("out").textContent.includes("Hello"), translationTimeout);
    step = "自動翻訳の完了";
    await wait(() => translationStatus.textContent === "訳し終わりました", translationTimeout);
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
    $("settings-tab").click();
    $("connection-settings").open = true;
    $("server-active").click();
    await wait(async () => !(await invoke("session_status")).logged_in && !$("server-active").checked);
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
