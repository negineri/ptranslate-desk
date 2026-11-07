// 試験の共通の道具
export const SERVER = "https://translate.example.com";

// chrome.runtime.Port の両端をまねる。メッセージは非同期に、複製して届ける
export function portPair() {
  const make = () => {
    const listeners = { message: new Set(), disconnect: new Set() };
    const event = (set) => ({ addListener: (f) => set.add(f), removeListener: (f) => set.delete(f) });
    return { listeners, onMessage: event(listeners.message), onDisconnect: event(listeners.disconnect) };
  };
  const a = make(), b = make();
  let open = true;
  const wire = (self, other) => {
    self.postMessage = (msg) => {
      if (!open) throw new Error("閉じた port");
      const copy = structuredClone(msg);
      setTimeout(() => { if (open) for (const f of [...other.listeners.message]) f(copy); });
    };
    self.disconnect = () => {
      if (!open) return;
      open = false;
      setTimeout(() => { for (const f of [...other.listeners.disconnect]) f(); });
    };
  };
  wire(a, b);
  wire(b, a);
  return [a, b];
}

export function sse(events, { status = 200 } = {}) {
  const body = events.map((e) => `data: ${JSON.stringify(e)}\n\n`).join("");
  return new Response(body, { status, headers: { "Content-Type": "text/event-stream" } });
}
