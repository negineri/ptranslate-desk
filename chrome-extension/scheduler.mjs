// background全体で共有する枠。応答の読み終わりまで保持し、待機中の中止にも対応する。
export class Scheduler {
  constructor() { this.groups = new Map(); }
  async run(key, limit, signal, call) {
    signal.throwIfAborted();
    let group = this.groups.get(key);
    if (!group) this.groups.set(key, group = { active: 0, limit, waiting: [] });
    group.limit = limit;
    const release = () => { group.active--; pump(); };
    const pump = () => {
      while (group.active < group.limit && group.waiting.length) {
        const waiter = group.waiting.shift();
        waiter.cleanup(); group.active++; waiter.resolve();
      }
    };
    await new Promise((resolve, reject) => {
      const waiter = { resolve, cleanup: () => signal.removeEventListener("abort", cancel) };
      const cancel = () => {
        const i = group.waiting.indexOf(waiter);
        if (i < 0) return;
        group.waiting.splice(i, 1); waiter.cleanup(); reject(signal.reason);
      };
      group.waiting.push(waiter); signal.addEventListener("abort", cancel, { once: true }); pump();
    });
    let res;
    try { signal.throwIfAborted(); res = await call(); } catch (e) { release(); throw e; }
    if (!res.body) { release(); return res; }
    const reader = res.body.getReader();
    let released = false;
    const finish = () => { if (!released) { released = true; signal.removeEventListener("abort", cancel); release(); } };
    const cancel = () => { reader.cancel(signal.reason).catch(() => {}); finish(); };
    signal.addEventListener("abort", cancel, { once: true });
    if (signal.aborted) cancel();
    return { status: res.status, body: new ReadableStream({
      async pull(controller) {
        try {
          signal.throwIfAborted(); const { value, done } = await reader.read(); signal.throwIfAborted();
          if (done) { finish(); controller.close(); } else controller.enqueue(value);
        } catch (error) { finish(); controller.error(error); }
      },
      async cancel(reason) { try { await reader.cancel(reason); } finally { finish(); } },
    }, { highWaterMark: 0 }) };
  }
}
