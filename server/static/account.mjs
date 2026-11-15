const $ = (id) => document.getElementById(id);
const number = new Intl.NumberFormat("ja-JP");
const states = { ready: "待機中", probation: "試用中", quarantine: "検疫中", revoked: "失効" };

async function request(path, body) {
  const response = await fetch(path, body === undefined ? {} : {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (response.status === 401) throw new Error("アカウントへのログインが必要です");
  const data = await response.json();
  if (!response.ok) throw new Error(data.error || "読み込めませんでした");
  return data;
}

function row(...values) {
  const tr = document.createElement("tr");
  for (const value of values) {
    const td = document.createElement("td");
    td.textContent = value;
    tr.append(td);
  }
  return tr;
}

function contributionRow(period, data) {
  return row(period, ...[data.input_tokens + data.output_tokens, data.jobs, data.days].map((value) => number.format(value)));
}

async function account() {
  const data = await request("/api/account");
  $("contribution").replaceChildren(contributionRow("累計", data.total),
    ...data.monthly.map((month) => contributionRow(month.month, month)));
  for (const worker of data.workers) {
    const entry = document.createElement("li");
    const expires = new Date(worker.expires_at * 1000).toLocaleDateString("ja-JP");
    entry.textContent = `${worker.worker_id} · ${states[worker.state]} · 有効期限 ${expires}`;
    $("workers").append(entry);
  }
  if (!data.workers.length) $("workers").hidden = true;
  const agreement = data.terms ? `規約の同意 · 第${data.terms.version}版 · ${data.terms.agreed}` : "規約への同意の記録はありません。";
  $("terms").textContent = data.workers.length ? agreement : `登録したワーカーはありません。 ${agreement}`;

  const mode = $("listing-mode");
  const nickname = $("nickname");
  const form = $("listing");
  mode.value = data.listing.mode;
  if (mode.value === "nickname") nickname.value = data.listing.name;
  if (data.username) mode.options[1].textContent = `ユーザー名（${data.username}）で掲載する`;
  function updateMode() {
    nickname.required = mode.value === "nickname";
    $("nickname-label").hidden = !nickname.required;
  }
  updateMode();
  mode.addEventListener("change", updateMode);
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const submit = form.querySelector("button");
    submit.disabled = mode.disabled = nickname.disabled = true;
    try {
      await request("/api/account", { mode: mode.value, nickname: nickname.value });
      $("listing-status").textContent = "掲載設定を保存しました";
    } catch (error) {
      $("listing-status").textContent = error.message;
    } finally {
      submit.disabled = mode.disabled = nickname.disabled = false;
    }
  });

  for (const [label, url] of Object.entries(data.links)) {
    const link = document.createElement("a");
    link.href = url;
    link.textContent = label;
    $("links").append(link);
  }
  $("account-actions").hidden = !$("links").childElementCount;
  $("account-content").hidden = false;
  $("status").textContent = "";
}

function ranking() {
  const form = $("ranking");
  const period = $("period");
  const month = $("month");
  month.value = new Date().toISOString().slice(0, 7);
  period.addEventListener("change", () => {
    $("month-label").hidden = month.disabled = period.value === "all";
  });
  async function load() {
    form.querySelector("button").disabled = period.disabled = true;
    $("ranking-table").hidden = true;
    $("status").textContent = "読み込み中…";
    try {
      const value = period.value === "all" ? "all" : month.value;
      const data = await request(`/api/account/ranking?period=${encodeURIComponent(value)}`);
      $("ranking-entries").replaceChildren(...data.entries.map((entry, index) =>
        row(index + 1, entry.name, number.format(entry.tokens), number.format(entry.days))));
      $("ranking-table").hidden = !data.entries.length;
      $("status").textContent = data.entries.length ? "" : "この期間に掲載されている参加者はいません。";
    } catch (error) {
      $("status").textContent = error.message;
    } finally {
      form.querySelector("button").disabled = period.disabled = false;
    }
  }
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    void load();
  });
  void load();
}

if ($("account")) account().catch((error) => { $("status").textContent = error.message; });
if ($("ranking")) ranking();
