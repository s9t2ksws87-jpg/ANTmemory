"use strict";
const $ = (s) => document.querySelector(s);
const labels = {
  memories: "記憶",
  goals: "目標と作業",
  unknowns: "未解決の疑問",
  states: "共有された現在地",
};
const statusLabel = {
  active: "有効",
  superseded: "置換済み",
  retracted: "撤回済み",
  expired: "期限切れ",
  posted: "進行中",
  done: "完了",
  dropped: "取り下げ",
  merged: "統合済み",
};
const eventLabel = {
  created: "記録を保存",
  superseded: "新しい記録に置換",
  retracted: "記録を撤回",
  restored: "記録を復帰",
  importance_changed: "重要度を変更",
  visibility_changed: "表示範囲を変更",
  expired: "期限切れ",
  source_observed: "根拠を記録",
  goal_posted: "札を掲示",
  goal_edited: "札を編集",
  goal_closed: "札を終了",
  unknown_resolved: "疑問の解決を記録",
  ttl_cleared: "過去の期限を解除",
};
const state = {
  workspaces: [],
  workspace: "",
  view: "memories",
  items: [],
  visibleItems: [],
  goals: [],
  proposals: [],
  selected: null,
  page: 0,
  total: 0,
  limit: 30,
  epoch: 0,
  detailEpoch: 0,
  tab: "body",
  detail: null,
};
const esc = (v) =>
  String(v ?? "").replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        c
      ],
  );
const date = (v, full = false) => {
  if (!v) return "—";
  const d = new Date(v);
  return Number.isNaN(+d)
    ? v
    : d.toLocaleString(
        "ja-JP",
        full
          ? {
              year: "numeric",
              month: "2-digit",
              day: "2-digit",
              hour: "2-digit",
              minute: "2-digit",
            }
          : { month: "2-digit", day: "2-digit" },
      );
};
const agentClass = (v) =>
  /codex/i.test(v)
    ? "codex"
    : /claude/i.test(v)
      ? "claude"
      : /antigravity/i.test(v)
        ? "antigravity"
        : "";
const agent = (v) =>
  `<span class="agent ${agentClass(v || "")}">${esc(v || "不明")}</span>`;
const chip = (v) =>
  `<span class="chip ${Object.hasOwn(statusLabel, v) ? v : ""}">${esc(statusLabel[v] || v)}</span>`;
function toast(message) {
  $("#toast").textContent = message;
  $("#toast").hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => ($("#toast").hidden = true), 3500);
}
async function api(path, params = {}) {
  const q = new URLSearchParams({ workspace: state.workspace, ...params });
  const r = await fetch("/api/" + path + "?" + q);
  const value = await r.json();
  if (!r.ok) throw Error(value.error || "読み込めませんでした");
  return value;
}
function errorView(node, e) {
  node.innerHTML = `<div class="empty-list error"><strong>読み込めませんでした</strong>${esc(e.message)}<br>更新ボタンでもう一度確認できます。</div>`;
  $("#statusMessage").textContent = "読み込みエラー";
}
function resetDetail() {
  state.selected = null;
  state.detail = null;
  state.detailEpoch++;
  $("#selectionCrumb").textContent = "";
  $(".split-view").classList.remove("detail-open");
  $("#detail").innerHTML =
    '<div class="empty-detail"><div class="empty-symbol">▤</div><h2>記録を選択してください</h2><p>一覧から記録を選ぶと、本文と根拠、<br>これまでの変更を確認できます。</p><div class="shortcuts"><kbd>Ctrl K</kbd> 検索へ移動 <span>·</span> <kbd>↑ ↓</kbd> 一覧を移動</div></div>';
}
function workspaceList() {
  const term = $("#workspaceSearch").value.toLowerCase();
  $("#workspaces").innerHTML =
    state.workspaces
      .filter(
        (w) =>
          w.name.toLowerCase().includes(term) &&
          (w.count > 0 ||
            w.error ||
            w.name === state.workspace ||
            $("#showEmptyWorkspaces").checked),
      )
      .map(
        (w) =>
          `<button class="workspace-item ${w.name === state.workspace ? "active" : ""}" data-workspace="${esc(w.name)}" title="${esc(w.root || w.error || w.name)}" ${w.error ? "disabled" : ""}><span>▱</span><span class="ws-name">${esc(w.name)}</span><span class="ws-count">${w.error ? "!" : Number(w.count).toLocaleString()}</span></button>`,
      )
      .join("") || '<div class="empty-list">作業空間がありません</div>';
}
async function loadWorkspaces() {
  try {
    const data = await api("workspaces");
    state.workspaces = data.items;
    workspaceList();
    if (!state.workspace) {
      const saved = localStorage.getItem("antmemory.workspace");
      const first =
        state.workspaces.find((w) => !w.error && w.name === saved) ||
        state.workspaces.find((w) => !w.error && w.name === "ANTmemory") ||
        state.workspaces.find((w) => !w.error);
      if (first) await selectWorkspace(first.name);
      else {
        $("#resultCount").textContent = "作業空間なし";
        $("#results").innerHTML =
          '<div class="empty-list">V4の記憶がまだありません。<br>Agentが記録した作業空間がここに並びます。</div>';
      }
    }
  } catch (e) {
    errorView($("#workspaces"), e);
  }
}
function options(node, items, previous = "") {
  node.innerHTML =
    '<option value="">すべて</option>' +
    items
      .map(
        (x) =>
          `<option value="${esc(x.name)}">${esc(x.name)} (${x.count})</option>`,
      )
      .join("");
  node.value = items.some((x) => x.name === previous) ? previous : "";
}
async function selectWorkspace(name, preserve = false) {
  state.workspace = name;
  if (!preserve) state.page = 0;
  state.epoch++;
  resetDetail();
  localStorage.setItem("antmemory.workspace", name);
  workspaceList();
  $("#workspaceName").textContent = name;
  $("#windowTitle").textContent = name + " — Memory Explorer";
  $("#statusWorkspace").textContent = name;
  document.title = name + " — ANTmemory";
  if (!preserve) {
    if (!preserve) {
      resetFilters(false);
      $("#query").value = "";
    }
  }
  const epoch = state.epoch;
  try {
    const s = await api("summary");
    if (epoch !== state.epoch) return;
    $("#workspacePath").textContent = s.root || "作業フォルダの登録なし";
    $("#workspacePath").title = s.root || "";
    $("#overview").innerHTML =
      `<div class="overview-row"><span>記憶</span><strong>${s.memories.toLocaleString()}</strong></div><div class="overview-row"><span>掲示中の目標</span><strong>${s.goals.toLocaleString()}</strong></div><div class="overview-row"><span>未解決の疑問</span><strong>${s.unknowns.toLocaleString()}</strong></div>`;
    options($("#agent"), s.agents, $("#agent").value);
    options($("#tag"), s.tags, $("#tag").value);
    await loadView();
  } catch (e) {
    errorView($("#results"), e);
  }
}
function resetFilters(reload = true) {
  for (const k of ["agent", "visibility", "importance", "tag"])
    $("#" + k).value = "";
  $("#status").value = "active";
  updateFilterCount();
  if (reload) {
    state.page = 0;
    loadView();
  }
}
function updateFilterCount() {
  const n =
    ["agent", "visibility", "importance", "tag"].filter((k) => $("#" + k).value)
      .length + Number($("#status").value !== "active");
  $("#filterCount").textContent = n ? "(" + n + ")" : "";
}
async function changeView(view) {
  if (!Object.hasOwn(labels, view)) return;
  state.view = view;
  state.page = 0;
  $("#query").value = "";
  resetDetail();
  document
    .querySelectorAll("[data-view]")
    .forEach((b) => b.classList.toggle("active", b.dataset.view === view));
  $("#viewName").textContent = labels[view];
  $("#heading").textContent = {
    memories: "記憶を探す",
    goals: "目標と作業を見渡す",
    unknowns: "残っている疑問を確かめる",
    states: "共有された現在地を読む",
  }[view];
  $("#subtitle").textContent = {
    memories: "残された判断と、その経緯をたどる。",
    goals: "全体の目的から、今進んでいる作業まで。",
    unknowns: "分からないことを残し、次の観測につなげる。",
    states: "エージェントが共有している、現在の値。",
  }[view];
  $("#sort").hidden = ["goals", "states"].includes(view);
  $("#closedToggle").hidden = view !== "goals";
  $("#filterToggle").hidden = ["goals", "states"].includes(view);
  $("#filters").hidden = true;
  $("#filterToggle").setAttribute("aria-expanded", "false");
  $("#query").placeholder =
    view === "goals"
      ? "目標の名前・担当から絞り込む"
      : view === "states"
        ? "キー・現在の値から絞り込む"
        : "本文・タイトル・タグ・IDから探す";
  await loadView();
}
async function loadView() {
  if (!state.workspace) return;
  const epoch = ++state.epoch;
  $("#statusMessage").textContent = "読み込み中…";
  $("#resultCount").textContent = "読み込み中…";
  try {
    let d;
    if (state.view === "goals") {
      d = await api("goals");
      if (epoch !== state.epoch) return;
      state.goals = d.items;
      state.proposals = d.proposals;
      renderGoals();
    } else if (state.view === "states") {
      d = await api("states", {
        q: $("#query").value,
        page: state.page,
        limit: state.limit,
      });
      if (epoch !== state.epoch) return;
      state.items = d.items;
      state.total = d.total;
      state.page = d.page;
      renderStates();
    } else {
      const p = {
        q: $("#query").value,
        page: state.page,
        sort: $("#sort").value,
        limit: state.limit,
      };
      for (const k of ["agent", "status", "visibility", "importance", "tag"])
        p[k] = $("#" + k).value;
      if (state.view === "unknowns") p.kind = "unknown";
      d = await api("memories", p);
      if (epoch !== state.epoch) return;
      state.items = d.items;
      state.total = d.total;
      state.page = d.page;
      renderMemories();
    }
    $("#statusMessage").textContent =
      "最終取得 " +
      new Date().toLocaleTimeString("ja-JP", {
        hour: "2-digit",
        minute: "2-digit",
      }) +
      " · 読み取り専用";
  } catch (e) {
    if (epoch === state.epoch) errorView($("#results"), e);
  }
}
function record(v) {
  return `<button class="record ${state.selected === v.id ? "selected" : ""}" data-memory="${esc(v.id)}"><div class="record-head"><span class="record-id">${esc(v.id)}</span><time class="record-date">${esc(date(v.updated))}</time></div><h2>${esc(v.topic)}</h2><p>${esc(v.content)}</p><div class="record-foot">${agent(v.agent)}${v.status !== "active" ? chip(v.status) : ""}${v.tags
    .slice(0, 2)
    .map((t) => `<span class="chip">${esc(t)}</span>`)
    .join("")}</div></button>`;
}
function renderMemories() {
  state.visibleItems = state.items;
  const n = state.total;
  $("#resultCount").textContent =
    n.toLocaleString() +
    " 件 · " +
    (state.view === "unknowns"
      ? "未解決"
      : $("#status").value === "all"
        ? "全状態"
        : statusLabel[$("#status").value] || "有効");
  $("#results").innerHTML =
    state.items.map(record).join("") ||
    '<div class="empty-list"><strong>該当する記憶はありません</strong>語句を変えるか、絞り込みを解除して<br>もう一度探してみてください。</div>';
  $("#pagination").hidden = false;
  $("#pageInfo").textContent = n
    ? `${state.page * state.limit + 1}–${Math.min((state.page + 1) * state.limit, n)} / ${n.toLocaleString()}`
    : "0 件";
  $("#previous").disabled = state.page === 0;
  $("#next").disabled = (state.page + 1) * state.limit >= n;
}
function renderGoals() {
  const showClosed = $("#showClosed").checked,
    term = $("#query").value.toLowerCase();
  const byId = new Map(state.goals.map((g) => [g.id, g]));
  const keep = new Set();
  for (const g of state.goals) {
    if (
      (showClosed || g.state === "posted") &&
      (!term ||
        (g.title + " " + g.agent + " " + g.id).toLowerCase().includes(term))
    ) {
      let cur = g;
      for (let i = 0; cur && i < 60; i++) {
        keep.add(cur.id);
        cur = byId.get(cur.parent);
      }
    }
  }
  const out = [],
    seen = new Set();
  function visit(g, depth) {
    if (seen.has(g.id) || !keep.has(g.id)) return;
    seen.add(g.id);
    out.push(
      `<button class="record tree-row depth-${Math.min(depth, 4)} ${state.selected === g.id ? "selected" : ""}" data-goal="${esc(g.id)}"><div class="record-head"><span>${depth ? "└" : "⌄"}</span><span class="record-id">${esc(g.id)}</span><span class="goal-kind">${g.kind === "overall" ? "全体目標" : "作業目標"}</span></div><h2>${esc(g.title)}</h2><div class="record-foot">${chip(g.state)}${agent(g.agent)}</div></button>`,
    );
    state.goals
      .filter((c) => c.parent === g.id)
      .forEach((c) => visit(c, depth + 1));
  }
  state.goals
    .filter((g) => !g.parent || !byId.has(g.parent))
    .forEach((g) => visit(g, 0));
  for (const g of state.goals) visit(g, 0);
  state.visibleItems = state.goals.filter((g) => seen.has(g.id));
  state.total = state.goals.length;
  if (state.proposals.length)
    out.push(
      '<div class="subheading">未確認の候補</div>' +
        state.proposals
          .map(
            (g) =>
              `<div class="record"><h2>${esc(g.title || g.goal_id || g.id)}</h2><p>${esc(g.quote || "引用なし")}</p><span class="chip">候補・未確認</span></div>`,
          )
          .join(""),
    );
  $("#results").innerHTML =
    out.join("") ||
    '<div class="empty-list"><strong>掲示中の目標はありません</strong>Agentが掲げた作業目標が、<br>親子関係とともにここに並びます。</div>';
  $("#resultCount").textContent =
    state.goals.filter((g) => g.state === "posted").length +
    " 件の掲示中の目標";
  $("#pagination").hidden = true;
}
function renderStates() {
  const term = $("#query").value.toLowerCase();
  const items = state.items.filter((v) =>
    (v.key + " " + JSON.stringify(v.value)).toLowerCase().includes(term),
  );
  state.visibleItems = items;
  $("#resultCount").textContent = state.total.toLocaleString() + " 件の現在地";
  $("#results").innerHTML =
    items
      .map(
        (v) =>
          `<button class="record ${state.selected === v.key ? "selected" : ""}" data-state="${esc(v.key)}"><div class="record-head"><span class="purple">{ }</span><time class="record-date">${esc(date(v.updated))}</time></div><h2>${esc(v.key)}</h2><p class="state-preview">${esc(JSON.stringify(v.value))}</p></button>`,
      )
      .join("") ||
    '<div class="empty-list">共有された現在地はありません。</div>';
  $("#pagination").hidden = false;
  $("#pageInfo").textContent = state.total
    ? `${state.page * state.limit + 1}–${Math.min((state.page + 1) * state.limit, state.total)} / ${state.total}`
    : "0 件";
  $("#previous").disabled = state.page === 0;
  $("#next").disabled = (state.page + 1) * state.limit >= state.total;
}
function selectRow(id) {
  state.selected = id;
  document
    .querySelectorAll(".record")
    .forEach((r) =>
      r.classList.toggle(
        "selected",
        [r.dataset.memory, r.dataset.goal, r.dataset.state].includes(id),
      ),
    );
  $(".split-view").classList.add("detail-open");
}
async function openMemory(id) {
  selectRow(id);
  const epoch = ++state.detailEpoch;
  $("#detail").innerHTML =
    '<div class="empty-detail"><p>本文を読み込んでいます…</p></div>';
  try {
    const d = await api("memory", { id });
    if (epoch !== state.detailEpoch) return;
    state.detail = d;
    state.tab = "body";
    $("#selectionCrumb").textContent = "› " + d.id;
    renderDetail();
  } catch (e) {
    if (epoch === state.detailEpoch) errorView($("#detail"), e);
  }
}
function detailHeader(d) {
  return `<header class="document-header"><div class="document-topline">${chip(d.status)}<span class="chip">${esc(d.visibility)}</span><span class="record-id">${esc(d.id)}</span></div><h2>${esc(d.topic)}</h2><div class="document-meta">${agent(d.agent)}<span>記録 ${esc(date(d.created, true))}</span><span>更新 ${esc(date(d.updated, true))}</span></div><div class="document-actions"><button class="quiet-button" data-copy="id">IDをコピー</button><button class="quiet-button" data-copy="body">本文をコピー</button><button class="quiet-button" data-download="memory">↓ 保存</button><button class="text-button" data-back>一覧へ</button></div></header>`;
}
function renderDetail() {
  const d = state.detail;
  if (!d) return;
  const tabs = [
    ["body", "本文"],
    ["sources", "根拠と記録時の環境"],
    ["history", "変更履歴"],
  ];
  $("#detail").innerHTML =
    detailHeader(d) +
    `<nav class="detail-tabs" aria-label="記録の詳細表示">${tabs.map(([k, v]) => `<button class="${state.tab === k ? "active" : ""}" data-detail-tab="${k}">${v}</button>`).join("")}</nav>` +
    (state.tab === "body"
      ? `${d.current ? "" : '<div class="document-section"><div class="notice">この本文は履歴の記録です。現在有効な記憶とは区別して参照してください。</div></div>'}<div class="document-body">${esc(d.content)}</div>`
      : state.tab === "sources"
        ? sources(d)
        : historyView(d.history));
}
function sources(d) {
  let out =
    '<section class="document-section"><h3 class="section-label">この記憶の根拠</h3>';
  if (d.source_ref)
    out += `<div class="source-card"><h3>保存された参照先</h3><p>${esc(d.source_ref)}</p><button class="quiet-button" data-copy="source">参照をコピー</button></div>`;
  if (!d.source_ref && !d.anchors.length)
    out += '<p class="muted">ファイル等への参照は記録されていません。</p>';
  for (const a of d.anchors)
    out += `<div class="source-card"><h3>${esc(a.label)}</h3><p>${esc(a.segment)}</p><details><summary>保存時の指紋と今回の観測</summary><pre class="code-block">${esc(JSON.stringify({ 保存時: a.capture, 今回: a.observed }, null, 2))}</pre></details></div>`;
  out += '<h3 class="section-label">記録したときの環境</h3>';
  if (d.env) {
    const g = d.env.git || {};
    out += `<dl class="key-value"><dt>作業空間</dt><dd>${esc(d.env.root || d.env.workspace)}</dd><dt>エージェント</dt><dd>${esc(d.env.client || d.agent)}</dd><dt>ブランチ</dt><dd>${esc(g.branch || "記録なし")}</dd><dt>コミット</dt><dd>${esc(g.head || "記録なし")}</dd><dt>未コミット</dt><dd>${g.dirty ? "変更あり（" + esc(g.changed) + "件）" : "変更なし／記録なし"}</dd></dl>`;
  } else out += '<p class="muted">旧記録のため環境情報はありません。</p>';
  if (d.tags.length)
    out +=
      '<h3 class="section-label">保存時のタグ</h3>' +
      d.tags.map(chip).join(" ");
  if (d.recall.length)
    out +=
      '<h3 class="section-label">検索から覚えた語</h3>' +
      d.recall.map(chip).join(" ");
  return out + "</section>";
}
function historyView(h) {
  return (
    '<section class="document-section"><h3 class="section-label">この記録につながる記憶</h3>' +
    (h.relations.length
      ? h.relations
          .map(
            (r) =>
              `<button class="relation" data-memory="${esc(r.id)}">${esc({ supersedes: r.out ? "置き換え前の記録" : "この記録を置き換えた記憶", resolves: r.out ? "解決した疑問" : "この疑問の解決記録", derives_from: r.out ? "派生元の記録" : "派生した記録" }[r.kind] || r.kind)} → <span class="record-id">${esc(r.id)}</span>${r.reason ? "<br>" + esc(r.reason) : ""}</button>`,
          )
          .join("")
      : '<p class="muted">関連づけられた記憶はありません。</p>') +
    '<h3 class="section-label">変更の経緯</h3><div class="timeline">' +
    h.events
      .map(
        (e) =>
          `<article class="event"><h3>${esc(eventLabel[e.kind] || e.kind)}</h3><time>${esc(date(e.at, true))} · ${esc(e.agent)}</time>${e.reason ? "<p>" + esc(e.reason) + "</p>" : ""}${e.old || e.new ? "<p>" + esc(e.old || "—") + " → " + esc(e.new || "—") + "</p>" : ""}</article>`,
      )
      .join("") +
    "</div></section>"
  );
}
async function openGoal(id) {
  state.detail = null;
  selectRow(id);
  const epoch = ++state.detailEpoch,
    g = state.goals.find((g) => g.id === id);
  if (!g) return;
  $("#selectionCrumb").textContent = "› " + id;
  $("#detail").innerHTML =
    '<div class="empty-detail"><p>作業の記録を読み込んでいます…</p></div>';
  try {
    const data = await api("goal", { id });
    if (epoch !== state.detailEpoch) return;
    const parent = state.goals.find((p) => p.id === g.parent);
    $("#detail").innerHTML =
      `<header class="document-header"><div class="document-topline"><span class="goal-kind">${g.kind === "overall" ? "全体目標" : "作業目標"}</span>${chip(g.state)}<span class="record-id">${esc(g.id)}</span></div><h2>${esc(g.title)}</h2><div class="document-meta">${agent(g.agent)}<span>掲示 ${esc(date(g.created, true))}</span></div><div class="document-actions"><button class="text-button" data-back>一覧へ</button></div></header><section class="document-section">${parent ? '<h3 class="section-label">親の目標</h3><button class="relation" data-goal="' + esc(parent.id) + '">' + esc(parent.title) + "</button>" : ""}<h3 class="section-label">この目標の根拠</h3>${g.basis.map((b) => `<div class="source-card"><h3>${esc({ utterance: "照合済みのユーザー発言", unverified_quote: "未照合の引用", initiative: "エージェントの判断", parent: "親の目標" }[b.type] || b.type)}</h3><p>${esc(b.quote || b.note || b.goal_id || "")}</p>${b.source ? "<p>" + esc(b.source.client) + " · " + esc(b.source.file) + ":" + esc(b.source.line) + "</p>" : b.utterance_id ? "<p>発言参照: " + esc(b.utterance_id) + "</p>" : ""}</div>`).join("")}${g.reason ? '<div class="notice">終了の理由: ' + esc(g.reason) + "</div>" : ""}<h3 class="section-label">この作業中に残された記憶 · ${data.total}件${data.total > 100 ? "（新しい100件）" : ""}</h3>${data.reports.map((r) => `<button class="relation" data-memory="${esc(r.id)}">${esc(r.topic)}<br><span class="muted">${esc(r.agent)} · ${esc(date(r.updated, true))}</span></button>`).join("") || '<p class="muted">この札に結びついた記録はまだありません。</p>'}</section>` +
      historyView(data.history);
  } catch (e) {
    if (epoch === state.detailEpoch) errorView($("#detail"), e);
  }
}
function openState(key) {
  state.detail = null;
  selectRow(key);
  state.detailEpoch++;
  const v = state.items.find((v) => v.key === key);
  if (!v) return;
  $("#selectionCrumb").textContent = "› " + key;
  $("#detail").innerHTML =
    `<header class="document-header"><div class="document-topline"><span class="purple">{ } 現在の共有値</span></div><h2>${esc(key)}</h2><div class="document-meta">更新 ${esc(date(v.updated, true))}</div><div class="document-actions"><button class="quiet-button" data-copy-state="${esc(key)}">値をコピー</button><button class="text-button" data-back>一覧へ</button></div></header><section class="document-section"><pre class="code-block">${esc(JSON.stringify(v.value, null, 2))}</pre><p class="muted">判断や変更の理由は、関連する記憶を参照してください。</p></section>`;
  const section = $("#detail .document-section");
  if (key === "goal.current") {
    const notice = document.createElement("p");
    notice.className = "notice";
    notice.textContent =
      "これは共有stateの値です。正式な目標の札は「目標と作業」で確認できます。";
    section.append(notice);
  }
  if (v.value && typeof v.value === "object") {
    const ids = [
      v.value.purpose_memory_id,
      v.value.task_memory_id,
      ...(Array.isArray(v.value.blocking_unknown_ids)
        ? v.value.blocking_unknown_ids
        : []),
    ];
    for (const id of new Set(
      ids.filter((id) => typeof id === "string" && id.length),
    )) {
      const button = document.createElement("button");
      button.className = "relation";
      button.dataset.memory = id;
      button.textContent = "参照している記憶を開く → " + id;
      section.append(button);
    }
  }
}
async function copy(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast("コピーしました");
  } catch {
    toast("コピーできませんでした。本文を選択してコピーしてください。");
  }
}
function download(name, value) {
  const blob = new Blob([JSON.stringify(value, null, 2)], {
    type: "application/json;charset=utf-8",
  });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name.replace(/[<>:"/\\|?*]/g, "_");
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
  toast("表示している内容を保存しました");
}
document.addEventListener("click", (e) => {
  const b = e.target.closest("button");
  if (!b) return;
  if (b.dataset.workspace) selectWorkspace(b.dataset.workspace);
  else if (b.dataset.view) changeView(b.dataset.view);
  else if (b.dataset.memory) openMemory(b.dataset.memory);
  else if (b.dataset.goal) openGoal(b.dataset.goal);
  else if (b.dataset.state) openState(b.dataset.state);
  else if (b.dataset.detailTab) {
    state.tab = b.dataset.detailTab;
    renderDetail();
  } else if (b.hasAttribute("data-back")) {
    resetDetail();
    $("#results").focus();
  } else if (b.dataset.copy) {
    const d = state.detail;
    copy(
      b.dataset.copy === "id"
        ? d.id
        : b.dataset.copy === "source"
          ? d.source_ref
          : d.content,
    );
  } else if (b.dataset.copyState)
    copy(
      JSON.stringify(
        state.items.find((v) => v.key === b.dataset.copyState)?.value,
        null,
        2,
      ),
    );
  else if (b.dataset.download)
    download("ANTmemory-" + state.detail.id + ".json", state.detail);
});
$("#searchForm").addEventListener("submit", (e) => {
  e.preventDefault();
  state.page = 0;
  loadView();
});
$("#workspaceSearch").addEventListener("input", workspaceList);
$("#showEmptyWorkspaces").addEventListener("change", workspaceList);
$("#filterToggle").addEventListener("click", () => {
  const hidden = !$("#filters").hidden;
  $("#filters").hidden = hidden;
  $("#filterToggle").setAttribute("aria-expanded", String(!hidden));
});
for (const k of ["agent", "status", "visibility", "importance", "tag", "sort"])
  $("#" + k).addEventListener("change", () => {
    state.page = 0;
    updateFilterCount();
    loadView();
  });
$("#resetFilters").addEventListener("click", () => resetFilters());
$("#showClosed").addEventListener("change", renderGoals);
$("#previous").addEventListener("click", () => {
  if (state.page) {
    state.page--;
    loadView();
  }
});
$("#next").addEventListener("click", () => {
  state.page++;
  loadView();
});
$("#refresh").addEventListener("click", async () => {
  const workspace = state.workspace;
  const view = state.view;
  const selected = state.selected;
  const memoryId = state.detail?.id;
  const tab = state.tab;
  await loadWorkspaces();
  if (workspace !== state.workspace || view !== state.view) return;
  if (state.workspace) await selectWorkspace(state.workspace, true);
  if (workspace !== state.workspace || view !== state.view || !selected) return;
  if (memoryId) {
    await openMemory(memoryId);
    if (state.detail?.id === memoryId) {
      state.tab = tab;
      renderDetail();
    }
  } else if (view === "goals" && state.goals.some((g) => g.id === selected)) {
    await openGoal(selected);
  } else if (view === "states" && state.items.some((s) => s.key === selected)) {
    openState(selected);
  }
});
$("#exportList").addEventListener("click", () =>
  download("ANTmemory-" + state.workspace + "-" + state.view + ".json", {
    workspace: state.workspace,
    view: state.view,
    total: state.total,
    scope: "現在表示している一覧。記憶本文は抜粋。",
    items: state.visibleItems,
  }),
);
document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
    e.preventDefault();
    $("#query").focus();
    $("#query").select();
  }
  if (e.key === "Escape") $(".split-view").classList.remove("detail-open");
  if (
    ["ArrowDown", "ArrowUp"].includes(e.key) &&
    !["INPUT", "SELECT", "TEXTAREA"].includes(document.activeElement.tagName) &&
    (document.activeElement === document.body ||
      $("#results").contains(document.activeElement))
  ) {
    const rows = [...$("#results").querySelectorAll("button.record")];
    const at = rows.findIndex((r) => r.classList.contains("selected"));
    const i = Math.max(
      0,
      Math.min(rows.length - 1, at + (e.key === "ArrowDown" ? 1 : -1)),
    );
    if (rows[i]) {
      e.preventDefault();
      rows[i].click();
      rows[i].scrollIntoView({ block: "nearest" });
    }
  }
});
window.addEventListener("unhandledrejection", (e) => {
  toast(e.reason?.message || "処理に失敗しました");
});
loadWorkspaces();
