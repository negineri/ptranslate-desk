// 接続先と同意の版を確認し、翻訳の各区切りに同じ値を渡す。
export function volunteerFields(config, { auto = false, onAgree } = {}) {
  const server = config.desktop_server;
  if (config.pool?.kind !== "volunteer") return server ? { desktop_server: server } : {};
  const version = config.pool.consent_version;
  if (!server || !Number.isSafeInteger(version) || version < 1) {
    throw new Error("ボランティア枠の同意の版を確認できませんでした");
  }
  const key = `ptranslate-volunteer-consent:${server}`;
  if (window.localStorage.getItem(key) === String(version)) {
    return { desktop_server: server, consent: version };
  }
  if (!auto) {
    const dialog = document.getElementById("volunteer-consent");
    if (!dialog.open) {
      const accept = document.getElementById("volunteer-accept");
      document.getElementById("volunteer-server").textContent = server;
      accept.onclick = () => {
        try {
          window.localStorage.setItem(key, String(version));
          document.getElementById("status").textContent = "同意しました。もう一度翻訳してください。";
          onAgree?.();
        } catch {
          document.getElementById("status").textContent = "同意を保存できませんでした。";
        }
      };
      dialog.showModal();
    }
  }
  throw new Error("ボランティア枠の利用には同意が必要です");
}
