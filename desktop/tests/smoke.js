(async () => {
  let step = "ログイン";
  const $ = (id) => document.getElementById(id);
  const wait = async (check) => {
    const end = Date.now() + 15000;
    while (!check()) {
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
    step = "ログアウト";
    await window.__TAURI__.core.invoke("set_mock_login", { loggedIn: false });
    let refused = false;
    try {
      await window.__TAURI__.core.invoke("start_request", {
        id: 9999, route: "api/config", body: null,
        stream: new window.__TAURI__.core.Channel(),
      });
    } catch { refused = true; }
    if (!refused) throw new Error("ログアウト");
    await window.__TAURI__.core.invoke("smoke_result", { passed: true, step });
  } catch {
    await window.__TAURI__.core.invoke("smoke_result", { passed: false, step });
  }
})();
