// State Management
const state = {
  workspaces: [],
  currentWorkspace: null,
  memories: [],
  stateItems: [],
  tags: [],
  selectedTag: null,
  activeTab: "memories",
  currentMemoryDetail: null,
  searchDebounceTimer: null,
  agents: []
};

// DOM Elements
const elements = {
  workspaceList: document.getElementById("workspaceList"),
  refreshWorkspacesBtn: document.getElementById("refreshWorkspacesBtn"),
  currentWsName: document.getElementById("currentWsName"),
  activeStoreCount: document.getElementById("activeStoreCount"),
  activeStoreSize: document.getElementById("activeStoreSize"),
  chunkBadgeCount: document.getElementById("chunkBadgeCount"),
  stateBadgeCount: document.getElementById("stateBadgeCount"),

  // Tabs
  tabBtns: document.querySelectorAll(".tab-btn"),
  tabContents: document.querySelectorAll(".tab-content"),

  // Filters & Search
  searchInput: document.getElementById("searchInput"),
  clearSearchBtn: document.getElementById("clearSearchBtn"),
  agentFilter: document.getElementById("agentFilter"),
  importanceFilter: document.getElementById("importanceFilter"),
  sortOrder: document.getElementById("sortOrder"),
  includeHistory: document.getElementById("includeHistory"),
  activeTagsContainer: document.getElementById("activeTagsContainer"),
  tagsList: document.getElementById("tagsList"),
  resetTagBtn: document.getElementById("resetTagBtn"),

  // Containers
  memoriesGrid: document.getElementById("memoriesGrid"),
  emptyState: document.getElementById("emptyState"),
  stateTableBody: document.getElementById("stateTableBody"),

  // Dashboard
  agentStatsList: document.getElementById("agentStatsList"),
  importanceStatsList: document.getElementById("importanceStatsList"),
  recentActivityList: document.getElementById("recentActivityList"),

  // Export
  exportJsonBtn: document.getElementById("exportJsonBtn"),
  exportMdBtn: document.getElementById("exportMdBtn"),

  // Modal
  detailModal: document.getElementById("detailModal"),
  closeModalBtn: document.getElementById("closeModalBtn"),
  modalAgentBadge: document.getElementById("modalAgentBadge"),
  modalImportanceBadge: document.getElementById("modalImportanceBadge"),
  modalId: document.getElementById("modalId"),
  modalTopic: document.getElementById("modalTopic"),
  modalTags: document.getElementById("modalTags"),
  modalCreatedAt: document.getElementById("modalCreatedAt"),
  modalUpdatedAt: document.getElementById("modalUpdatedAt"),
  modalExpiresRow: document.getElementById("modalExpiresRow"),
  modalExpiresAt: document.getElementById("modalExpiresAt"),
  modalContentMd: document.getElementById("modalContentMd"),
  modalContentRaw: document.getElementById("modalContentRaw"),
  viewMdBtn: document.getElementById("viewMdBtn"),
  viewRawBtn: document.getElementById("viewRawBtn"),
  copyContentBtn: document.getElementById("copyContentBtn"),
  modalHistory: document.getElementById("modalHistory"),
  modalStatusBadge: document.getElementById("modalStatusBadge"),

  // Toast
  toast: document.getElementById("toast")
};

// Initialize
document.addEventListener("DOMContentLoaded", () => {
  setupEventListeners();
  loadWorkspaces();
});

function setupEventListeners() {
  // Refresh Workspaces
  elements.refreshWorkspacesBtn.addEventListener("click", () => loadWorkspaces());

  // Tab switching
  elements.tabBtns.forEach(btn => {
    btn.addEventListener("click", () => {
      const tab = btn.dataset.tab;
      switchTab(tab);
    });
  });

  // Search Input with Debounce
  elements.searchInput.addEventListener("input", (e) => {
    elements.clearSearchBtn.hidden = !e.target.value;
    clearTimeout(state.searchDebounceTimer);
    state.searchDebounceTimer = setTimeout(() => {
      loadMemories();
    }, 250);
  });

  elements.clearSearchBtn.addEventListener("click", () => {
    elements.searchInput.value = "";
    elements.clearSearchBtn.hidden = true;
    loadMemories();
  });

  // Filter dropdowns
  elements.agentFilter.addEventListener("change", () => loadMemories());
  elements.importanceFilter.addEventListener("change", () => loadMemories());
  elements.sortOrder.addEventListener("change", () => loadMemories());
  elements.includeHistory.addEventListener("change", () => loadMemories());
  elements.resetTagBtn.addEventListener("click", () => {
    state.selectedTag = null;
    renderTags();
    loadMemories();
  });

  // Export buttons
  elements.exportJsonBtn.addEventListener("click", () => exportData("json"));
  elements.exportMdBtn.addEventListener("click", () => exportData("markdown"));

  // Modal controls
  elements.closeModalBtn.addEventListener("click", closeModal);
  elements.detailModal.addEventListener("click", (e) => {
    if (e.target === elements.detailModal) closeModal();
  });

  elements.viewMdBtn.addEventListener("click", () => {
    elements.viewMdBtn.classList.add("active");
    elements.viewRawBtn.classList.remove("active");
    elements.modalContentMd.classList.remove("hidden");
    elements.modalContentRaw.classList.add("hidden");
  });

  elements.viewRawBtn.addEventListener("click", () => {
    elements.viewRawBtn.classList.add("active");
    elements.viewMdBtn.classList.remove("active");
    elements.modalContentRaw.classList.remove("hidden");
    elements.modalContentMd.classList.add("hidden");
  });

  elements.copyContentBtn.addEventListener("click", () => {
    if (state.currentMemoryDetail) {
      navigator.clipboard.writeText(state.currentMemoryDetail.content);
      showToast("本文をクリップボードにコピーしました！");
    }
  });

  elements.modalId.addEventListener("click", () => {
    if (state.currentMemoryDetail) {
      navigator.clipboard.writeText(state.currentMemoryDetail.id);
      showToast(`ID (${state.currentMemoryDetail.id}) をコピーしました`);
    }
  });

  // Escape key for modal
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeModal();
  });
}

// 1. Fetch & Render Workspaces
async function loadWorkspaces() {
  try {
    elements.workspaceList.innerHTML = `
      <div class="skeleton-item"></div>
      <div class="skeleton-item"></div>
      <div class="skeleton-item"></div>
    `;

    const res = await fetch("/api/workspaces");
    const data = await res.json();
    state.workspaces = data.workspaces || [];

    renderWorkspaces();

    // Auto select first workspace if none selected
    if (state.workspaces.length > 0) {
      const target = state.workspaces.find(w => w.name === "異世界シミュレータ") || state.workspaces[0];
      selectWorkspace(target.name);
    } else {
      elements.workspaceList.innerHTML = `<div class="empty-state">ワークスペースがありません</div>`;
    }
  } catch (err) {
    showToast("ワークスペース一覧の取得に失敗しました", true);
    console.error(err);
  }
}

function renderWorkspaces() {
  elements.workspaceList.innerHTML = "";
  state.workspaces.forEach(ws => {
    const item = document.createElement("div");
    item.className = `ws-item ${ws.name === state.currentWorkspace ? "active" : ""}`;
    item.dataset.name = ws.name;

    // audit: DOM API で安全に構築（データ由来文字列は textContent へ）
    const name = document.createElement("span");
    name.className = "ws-name";
    name.textContent = ws.name;
    name.title = ws.name;
    const badge = document.createElement("span");
    badge.className = "ws-badge";
    badge.textContent = ws.chunkCount;
    item.append(name, badge);

    item.addEventListener("click", () => selectWorkspace(ws.name));
    elements.workspaceList.appendChild(item);
  });
}

// Select Workspace
function selectWorkspace(name) {
  state.currentWorkspace = name;
  state.selectedTag = null;

  renderWorkspaces();

  const ws = state.workspaces.find(w => w.name === name);
  if (ws) {
    elements.currentWsName.textContent = ws.name;
    elements.activeStoreCount.textContent = `${ws.chunkCount} メモリ`;
    elements.activeStoreSize.textContent = `${(ws.sizeBytes / 1024).toFixed(1)} KB`;
    elements.chunkBadgeCount.textContent = ws.chunkCount;
    elements.stateBadgeCount.textContent = ws.stateCount;
  }

  // Load active tab data
  loadAgents();
  loadTags();
  loadMemories();
  loadStateData();
  loadStats();
}

// audit: エージェント一覧をDBから動的取得
async function loadAgents() {
  if (!state.currentWorkspace) return;
  try {
    const res = await fetch(`/api/agents?workspace=${encodeURIComponent(state.currentWorkspace)}`);
    const data = await res.json();
    state.agents = data.agents || [];
    renderAgentFilter();
  } catch (err) {
    console.error("Failed to load agents:", err);
  }
}

function renderAgentFilter() {
  const current = elements.agentFilter.value;
  elements.agentFilter.innerHTML = "";
  const all = document.createElement("option");
  all.value = "";
  all.textContent = "全エージェント";
  elements.agentFilter.appendChild(all);
  for (const a of state.agents) {
    const opt = document.createElement("option");
    opt.value = a.agent;
    opt.textContent = `${a.agent} (${a.count})`;
    elements.agentFilter.appendChild(opt);
  }
  // 既存選択がまだ存在すれば復元
  if ([...elements.agentFilter.options].some(o => o.value === current)) {
    elements.agentFilter.value = current;
  }
}

// 2. Load Tags
async function loadTags() {
  if (!state.currentWorkspace) return;
  try {
    const res = await fetch(`/api/tags?workspace=${encodeURIComponent(state.currentWorkspace)}`);
    const data = await res.json();
    state.tags = data.tags || [];
    renderTags();
  } catch (err) {
    console.error("Failed to load tags:", err);
  }
}

function renderTags() {
  if (!state.tags || state.tags.length === 0) {
    elements.activeTagsContainer.classList.add("hidden");
    return;
  }

  elements.activeTagsContainer.classList.remove("hidden");
  elements.tagsList.innerHTML = "";

  state.tags.slice(0, 15).forEach(t => {
    const chip = document.createElement("button");
    chip.className = `tag-chip ${t.name === state.selectedTag ? "active" : ""}`;
    chip.textContent = `#${t.name} (${t.count})`;
    chip.addEventListener("click", () => {
      if (state.selectedTag === t.name) {
        state.selectedTag = null;
      } else {
        state.selectedTag = t.name;
      }
      renderTags();
      loadMemories();
    });
    elements.tagsList.appendChild(chip);
  });
}

// 3. Load & Render Memories
async function loadMemories() {
  if (!state.currentWorkspace) return;

  elements.memoriesGrid.innerHTML = `
    <div class="skeleton-item" style="height: 140px;"></div>
    <div class="skeleton-item" style="height: 140px;"></div>
    <div class="skeleton-item" style="height: 140px;"></div>
  `;
  elements.emptyState.classList.add("hidden");

  const q = elements.searchInput.value.trim();
  const agent = elements.agentFilter.value;
  const importance = elements.importanceFilter.value;
  const sort = elements.sortOrder.value;
  const includeHistory = elements.includeHistory.checked;

  let url = `/api/memories?workspace=${encodeURIComponent(state.currentWorkspace)}&sort=${sort}`;
  if (q) url += `&q=${encodeURIComponent(q)}`;
  if (agent) url += `&agent=${encodeURIComponent(agent)}`;
  if (importance) url += `&importance=${encodeURIComponent(importance)}`;
  if (state.selectedTag) url += `&tag=${encodeURIComponent(state.selectedTag)}`;
  if (includeHistory) url += `&include_history=true`;

  try {
    const res = await fetch(url);
    const data = await res.json();
    state.memories = data.memories || [];
    renderMemories();
  } catch (err) {
    showToast("記憶の取得に失敗しました", true);
    console.error(err);
  }
}

function renderMemories() {
  elements.memoriesGrid.innerHTML = "";

  if (state.memories.length === 0) {
    elements.emptyState.classList.remove("hidden");
    return;
  }

  elements.emptyState.classList.add("hidden");

  state.memories.forEach(mem => {
    const card = document.createElement("div");
    card.className = "card";

    const agentClass = getAgentClass(mem.agent);
    const impClass = mem.importance || "medium";
    const dateStr = formatDate(mem.updated_at || mem.created_at);

    // audit: すべてDOM APIで構築し、データ由来文字列を innerHTML に入れない
    const header = document.createElement("div");
    header.className = "card-header";
    const badgeGroup = document.createElement("div");
    badgeGroup.className = "badge-group";
    const agentBadge = document.createElement("span");
    agentBadge.className = `agent-badge ${agentClass}`;
    agentBadge.textContent = mem.agent;
    const impBadge = document.createElement("span");
    impBadge.className = `importance-badge ${impClass}`;
    impBadge.textContent = mem.importance;
    badgeGroup.append(agentBadge, impBadge);
    const dateEl = document.createElement("span");
    dateEl.className = "card-date";
    dateEl.textContent = dateStr;
    header.append(badgeGroup, dateEl);

    const topic = document.createElement("h3");
    topic.className = "card-topic";
    topic.textContent = mem.topic;

    const preview = document.createElement("div");
    preview.className = "card-preview";
    preview.textContent = mem.content;

    const footer = document.createElement("div");
    footer.className = "card-footer";
    const tagsDiv = document.createElement("div");
    tagsDiv.className = "card-tags";
    (mem.tags || []).slice(0, 3).forEach(t => {
      const tag = document.createElement("span");
      tag.className = "micro-tag";
      tag.textContent = `#${t}`;
      tagsDiv.appendChild(tag);
    });
    footer.appendChild(tagsDiv);

    card.append(header, topic, preview, footer);
    card.addEventListener("click", () => openDetailModal(mem));
    elements.memoriesGrid.appendChild(card);
  });
}

// 4. Load & Render State Data
async function loadStateData() {
  if (!state.currentWorkspace) return;
  try {
    const res = await fetch(`/api/state?workspace=${encodeURIComponent(state.currentWorkspace)}`);
    const data = await res.json();
    state.stateItems = data.state || [];
    renderStateTable();
  } catch (err) {
    console.error("Failed to load state table:", err);
  }
}

function renderStateTable() {
  elements.stateTableBody.innerHTML = "";
  if (state.stateItems.length === 0) {
    const tr = document.createElement("tr");
    const td = document.createElement("td");
    td.colSpan = 4;
    td.style.cssText = "text-align: center; color: var(--text-muted);";
    td.textContent = "State データはありません";
    tr.appendChild(td);
    elements.stateTableBody.appendChild(tr);
    return;
  }

  state.stateItems.forEach(item => {
    const tr = document.createElement("tr");

    const tdKey = document.createElement("td");
    const code = document.createElement("code");
    code.textContent = item.key;
    tdKey.appendChild(code);

    const tdVal = document.createElement("td");
    tdVal.style.cssText = "max-width: 400px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;";
    tdVal.textContent = item.value;
    tdVal.title = item.value;

    const tdDate = document.createElement("td");
    tdDate.textContent = formatDate(item.updated_at);

    const tdBtn = document.createElement("td");
    const btn = document.createElement("button");
    btn.className = "btn btn-secondary btn-small";
    btn.textContent = "コピー";
    btn.addEventListener("click", () => {
      navigator.clipboard.writeText(item.value);
      showToast("クリップボードにコピーしました");
    });
    tdBtn.appendChild(btn);

    tr.append(tdKey, tdVal, tdDate, tdBtn);
    elements.stateTableBody.appendChild(tr);
  });
}

// 5. Load & Render Stats Dashboard
async function loadStats() {
  if (!state.currentWorkspace) return;
  try {
    const res = await fetch(`/api/stats?workspace=${encodeURIComponent(state.currentWorkspace)}`);
    const data = await res.json();
    const stats = data.stats;

    renderDashboard(stats);
  } catch (err) {
    console.error("Failed to load stats:", err);
  }
}

function renderDashboard(stats) {
  // Agent stats
  elements.agentStatsList.innerHTML = "";
  const total = stats.totalChunks || 1;

  stats.agents.forEach(a => {
    const pct = ((a.count / total) * 100).toFixed(1);
    const agentClass = getAgentClass(a.agent);
    const div = document.createElement("div");
    div.className = "stat-bar-item";

    const header = document.createElement("div");
    header.className = "stat-bar-header";
    const badge = document.createElement("span");
    badge.className = `agent-badge ${agentClass}`;
    badge.textContent = a.agent;
    const label = document.createElement("span");
    label.textContent = `${a.count} 件 (${pct}%)`;
    header.append(badge, label);

    const bar = document.createElement("div");
    bar.className = "stat-progress";
    const fill = document.createElement("div");
    fill.className = "stat-progress-fill";
    fill.style.width = `${pct}%`;
    fill.style.background = `var(--accent-${agentClass === "antigravity" ? "antigravity" : agentClass === "claude-code" ? "claude" : agentClass === "codex" ? "codex" : agentClass === "hermes" ? "hermes" : "global"}, #6366f1)`;
    bar.appendChild(fill);

    div.append(header, bar);
    elements.agentStatsList.appendChild(div);
  });

  // Importance stats
  elements.importanceStatsList.innerHTML = "";
  stats.importance.forEach(imp => {
    const pct = ((imp.count / total) * 100).toFixed(1);
    const div = document.createElement("div");
    div.className = "stat-bar-item";

    const header = document.createElement("div");
    header.className = "stat-bar-header";
    const badge = document.createElement("span");
    badge.className = `importance-badge ${imp.importance}`;
    badge.textContent = imp.importance;
    const label = document.createElement("span");
    label.textContent = `${imp.count} 件 (${pct}%)`;
    header.append(badge, label);

    const bar = document.createElement("div");
    bar.className = "stat-progress";
    const fill = document.createElement("div");
    fill.className = "stat-progress-fill";
    fill.style.width = `${pct}%`;
    fill.style.background = `var(--${imp.importance}-color, #6366f1)`;
    bar.appendChild(fill);

    div.append(header, bar);
    elements.importanceStatsList.appendChild(div);
  });

  // Recent Activity
  elements.recentActivityList.innerHTML = "";
  stats.recentChunks.forEach(r => {
    const div = document.createElement("div");
    div.style.padding = "8px 0";
    div.style.borderBottom = "1px solid var(--border-color)";

    const row = document.createElement("div");
    row.style.cssText = "display: flex; justify-content: space-between; font-size: 13px;";
    const strong = document.createElement("strong");
    strong.textContent = r.topic;
    const date = document.createElement("span");
    date.style.cssText = "color: var(--text-dim); font-size: 11px;";
    date.textContent = formatDate(r.updated_at);
    row.append(strong, date);

    const badge = document.createElement("span");
    badge.className = `agent-badge ${getAgentClass(r.agent)}`;
    badge.style.cssText = "font-size: 9px; margin-top: 4px; display: inline-block;";
    badge.textContent = r.agent;

    div.append(row, badge);
    elements.recentActivityList.appendChild(div);
  });
}

// audit/recall: 履歴タイムラインの取得と描画
async function loadHistory(id) {
  elements.modalHistory.innerHTML = "";
  try {
    const res = await fetch(`/api/history?workspace=${encodeURIComponent(state.currentWorkspace)}&id=${encodeURIComponent(id)}`);
    const data = await res.json();

    // 状態バッジ（status + visibility）
    elements.modalStatusBadge.innerHTML = "";
    const statusBadge = document.createElement("span");
    statusBadge.className = `status-badge status-${data.status}`;
    statusBadge.textContent = data.status;
    elements.modalStatusBadge.appendChild(statusBadge);
    if (data.visibility) {
      const visBadge = document.createElement("span");
      visBadge.className = `vis-badge vis-${data.visibility}`;
      visBadge.textContent = `visibility: ${data.visibility}`;
      elements.modalStatusBadge.appendChild(visBadge);
    }
    if (data.usage && data.usage.access_count > 0) {
      const usageBadge = document.createElement("span");
      usageBadge.className = "usage-badge";
      usageBadge.textContent = `参照 ${data.usage.access_count}回`;
      usageBadge.title = data.usage.last_accessed_at ? `最終参照: ${data.usage.last_accessed_at}` : "";
      elements.modalStatusBadge.appendChild(usageBadge);
    }

    if ((!data.events || !data.events.length) && (!data.relations || !data.relations.length)
        && (!data.recalls || !data.recalls.length)) {
      const empty = document.createElement("p");
      empty.className = "history-empty";
      empty.textContent = "変更履歴はまだありません。";
      elements.modalHistory.appendChild(empty);
      return;
    }

    // 想起理由ログ（recall log）
    if (data.recalls && data.recalls.length) {
      const recallTitle = document.createElement("h4");
      recallTitle.textContent = "🧠 なぜ思い出したか (Recall reasons)";
      elements.modalHistory.appendChild(recallTitle);
      const recallList = document.createElement("ul");
      recallList.className = "history-recall";
      for (const r of data.recalls.slice(0, 8)) {
        const li = document.createElement("li");
        li.textContent =
          `${(r.created_at || "").replace("T", " ").slice(0, 16)} ` +
          `query「${r.query ?? "-"}」→ 理由: ${(r.reasons || []).join(", ")}`;
        li.title = r.query ?? "";
        recallList.appendChild(li);
      }
      elements.modalHistory.appendChild(recallList);
    }

    const timeline = document.createElement("div");
    timeline.className = "history-timeline";
    for (const e of data.events) {
      const item = document.createElement("div");
      item.className = "history-item";

      const head = document.createElement("div");
      head.className = "history-head";
      const type = document.createElement("span");
      type.className = `history-type history-${e.event_type}`;
      type.textContent = e.event_type;
      const actor = document.createElement("span");
      actor.className = "history-actor";
      actor.textContent = `by ${e.actor}` + (e.model ? ` (${e.model})` : "");
      const date = document.createElement("span");
      date.className = "history-date";
      date.textContent = formatDate(e.created_at);
      head.append(type, actor, date);

      item.appendChild(head);
      if (e.reason) {
        const reason = document.createElement("div");
        reason.className = "history-reason";
        reason.textContent = `理由: ${e.reason}`;
        item.appendChild(reason);
      }
      if (e.old_value || e.new_value) {
        const vals = document.createElement("div");
        vals.className = "history-values";
        vals.textContent = `${e.old_value ?? "-"} → ${e.new_value ?? "-"}`;
        item.appendChild(vals);
      }
      timeline.appendChild(item);
    }
    elements.modalHistory.appendChild(timeline);

    if (data.relations && data.relations.length) {
      const relTitle = document.createElement("h4");
      relTitle.textContent = "関係";
      elements.modalHistory.appendChild(relTitle);
      const relList = document.createElement("ul");
      relList.className = "history-relations";
      for (const r of data.relations) {
        const li = document.createElement("li");
        li.textContent = `${r.direction === "out" ? "この記録が" : "この記録を"} ${r.relation_type}: ${r.other_id}` + (r.reason ? ` — ${r.reason}` : "");
        relList.appendChild(li);
      }
      elements.modalHistory.appendChild(relList);
    }
  } catch (err) {
    console.error("Failed to load history:", err);
  }
}

// Modal Detail View
function openDetailModal(mem) {
  state.currentMemoryDetail = mem;

  elements.modalTopic.textContent = mem.topic;
  elements.modalId.textContent = `ID: ${mem.id}`;

  const agentClass = getAgentClass(mem.agent);
  elements.modalAgentBadge.className = `agent-badge ${agentClass}`;
  elements.modalAgentBadge.textContent = mem.agent;

  const impClass = mem.importance || "medium";
  elements.modalImportanceBadge.className = `importance-badge ${impClass}`;
  elements.modalImportanceBadge.textContent = mem.importance;

  elements.modalCreatedAt.textContent = formatDate(mem.created_at);
  elements.modalUpdatedAt.textContent = formatDate(mem.updated_at);

  if (mem.expires_at) {
    elements.modalExpiresRow.classList.remove("hidden");
    elements.modalExpiresAt.textContent = formatDate(mem.expires_at);
  } else {
    elements.modalExpiresRow.classList.add("hidden");
  }

  // Render Tags（textContent で安全に）
  elements.modalTags.innerHTML = "";
  (mem.tags || []).forEach(t => {
    const chip = document.createElement("span");
    chip.className = "tag-chip";
    chip.textContent = `#${t}`;
    elements.modalTags.appendChild(chip);
  });

  // Content rendering（audit: marked は使わず安全な表示のみ。
  // 生テキストをそのまま表示するためスクリプトは実行されない）
  elements.modalContentRaw.textContent = mem.content;
  elements.modalContentMd.textContent = mem.content;
  elements.modalContentMd.classList.remove("markdown-body");

  // 履歴を取得して表示
  loadHistory(mem.id);

  // Reset toggle buttons
  elements.viewMdBtn.click();

  elements.detailModal.classList.remove("hidden");
}

function closeModal() {
  elements.detailModal.classList.add("hidden");
  state.currentMemoryDetail = null;
}

// Data Export
function exportData(format) {
  if (!state.currentWorkspace) return;
  const url = `/api/export?workspace=${encodeURIComponent(state.currentWorkspace)}&format=${format}`;
  window.open(url, "_blank");
  showToast(`${format.toUpperCase()} 形式でエクスポートを開始しました`);
}

// Helpers
function switchTab(tabName) {
  state.activeTab = tabName;

  elements.tabBtns.forEach(btn => {
    btn.classList.toggle("active", btn.dataset.tab === tabName);
  });

  elements.tabContents.forEach(content => {
    content.classList.toggle("active", content.id === `${tabName}Tab`);
  });
}

function getAgentClass(agent) {
  if (!agent) return "global";
  const a = agent.toLowerCase();
  if (a.includes("antigravity")) return "antigravity";
  if (a.includes("claude")) return "claude-code";
  if (a.includes("codex")) return "codex";
  if (a.includes("hermes")) return "hermes";
  return "global";
}

function formatDate(isoStr) {
  if (!isoStr) return "-";
  try {
    const d = new Date(isoStr);
    return `${d.getFullYear()}/${(d.getMonth()+1).toString().padStart(2, '0')}/${d.getDate().toString().padStart(2, '0')} ${d.getHours().toString().padStart(2, '0')}:${d.getMinutes().toString().padStart(2, '0')}`;
  } catch (e) {
    return isoStr;
  }
}

function showToast(msg, isError = false) {
  elements.toast.textContent = msg;
  elements.toast.style.background = isError ? "#ef4444" : "#6366f1";
  elements.toast.classList.remove("hidden");
  setTimeout(() => {
    elements.toast.classList.add("hidden");
  }, 3000);
}
