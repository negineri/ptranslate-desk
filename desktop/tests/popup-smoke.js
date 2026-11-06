(() => {
  window.addEventListener("error", (event) => window.__TAURI__.core.invoke("smoke_popup_report", { output: "", status: event.message }));
  window.addEventListener("unhandledrejection", (event) => window.__TAURI__.core.invoke("smoke_popup_report", { output: "", status: String(event.reason) }));
  const send = () => window.__TAURI__.core.invoke("smoke_popup_report", {
    output: document.getElementById("out").textContent,
    status: document.getElementById("status").textContent,
  });
  new MutationObserver(send).observe(document.querySelector("main"), { subtree: true, childList: true, characterData: true });
})();
