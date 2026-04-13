let state = {
  baseUrl: "http://127.0.0.1:3000",
  sessions: new Map(),
  selectedId: "",
  es: null,
  events: []
};

const $ = (id) => document.getElementById(id);

function shortId(full) {
  return String(full).slice(0, 8);
}

function setConn(text, ok) {
  const el = $("conn");
  const dot = el.querySelector(".pill__dot");
  // reset
  el.className = "pill";
  if (ok === true) el.classList.add("pill--ok");
  if (ok === false) el.classList.add("pill--bad");
  el.lastChild.textContent = ` ${text}`;
  if (dot) {
    dot.style.background = ok === true ? "var(--color-status-working)" : ok === false ? "var(--color-status-error)" : "var(--color-text-tertiary)";
  }
}

function attentionLevel(session) {
  // Minimal mapping (ao-ts has richer PR-driven attention).
  const status = (session.status || "").toLowerCase();
  const activity = (session.activity || "").toLowerCase();
  if (status.includes("ci_failed") || status.includes("errored")) return "error";
  if (status.includes("changes_requested") || status.includes("needs_input")) return "respond";
  if (activity === "active" || status === "working") return "working";
  if (activity === "blocked" || activity === "exited") return "error";
  return "attention";
}

function renderSessions() {
  const box = $("sessions");
  box.innerHTML = "";
  const sessions = Array.from(state.sessions.values()).sort(
    (a, b) => (b.created_at ?? 0) - (a.created_at ?? 0),
  );

  for (const s of sessions) {
    const level = attentionLevel(s);
    const card = document.createElement("div");
    card.className = "session-card";
    card.dataset.level = level;
    card.dataset.selected = String(state.selectedId === s.id);
    card.onclick = () => {
      state.selectedId = s.id;
      $("selected").value = `${shortId(s.id)} (${s.id})`;
      renderSessions();
    };

    const title = (s.task ?? s.id ?? "").trim();
    const sub = [
      s.project_id ? `project: ${s.project_id}` : null,
      s.branch ? `branch: ${s.branch}` : null,
    ]
      .filter(Boolean)
      .join(" · ");

    card.innerHTML = `
      <div class="session-card__strip"></div>
      <div class="session-card__top">
        <div class="session-card__id">${escapeHtml(shortId(s.id))}</div>
        <div class="session-card__meta">${escapeHtml(s.status ?? "-")} / ${escapeHtml(s.activity ?? "-")}</div>
      </div>
      <div class="session-card__title">${escapeHtml(title.slice(0, 120))}</div>
      <div class="session-card__sub">${escapeHtml(sub)}</div>
      <div class="session-card__pills">
        <span class="mini-pill"><span class="mini-pill__dot" style="background: var(--color-accent)"></span>agent: ${escapeHtml(s.agent ?? "-")}</span>
      </div>
    `;
    box.appendChild(card);
  }
}

function appendEvent(evt) {
  const box = $("events");
  const div = document.createElement("div");
  div.className = "evt";
  const typ = evt?.type ?? "event";
  const id = evt?.id ? shortId(evt.id) : "-";
  div.innerHTML = `<div class="evt__type">${escapeHtml(typ)} <span class="hint">session=${escapeHtml(id)}</span></div><div class="evt__meta">${escapeHtml(JSON.stringify(evt))}</div>`;
  box.prepend(div);

  // bound memory
  while (box.childNodes.length > 200) {
    box.removeChild(box.lastChild);
  }
}

function escapeHtml(s) {
  return String(s)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

async function fetchSessions() {
  const url = `${state.baseUrl}/api/sessions`;
  const resp = await fetch(url);
  if (!resp.ok) throw new Error(`GET /api/sessions failed: ${resp.status}`);
  const arr = await resp.json();
  state.sessions.clear();
  for (const s of arr) {
    state.sessions.set(s.id, s);
  }
  renderSessions();
}

function connectEvents() {
  if (state.es) {
    state.es.close();
    state.es = null;
  }
  const url = `${state.baseUrl}/api/events`;
  const es = new EventSource(url);
  state.es = es;

  es.onopen = () => setConn("connected (SSE)", true);
  es.onerror = () => setConn("error (SSE)", false);
  es.onmessage = (msg) => {
    try {
      const evt = JSON.parse(msg.data);
      appendEvent(evt);
      // best-effort session updates: when event includes id, refetch that session lazily later
    } catch {
      // ignore
    }
  };
}

async function sendMessage() {
  const id = state.selectedId;
  if (!id) {
    $("sendStatus").textContent = "select a session first";
    return;
  }
  const message = $("message").value.trim();
  if (!message) {
    $("sendStatus").textContent = "message is empty";
    return;
  }
  $("sendStatus").textContent = "sending...";
  const url = `${state.baseUrl}/api/sessions/${encodeURIComponent(id)}/message`;
  const resp = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ message })
  });
  if (!resp.ok) {
    $("sendStatus").textContent = `failed (${resp.status})`;
    return;
  }
  $("sendStatus").textContent = "sent";
  $("message").value = "";
}

function wire() {
  $("baseUrl").addEventListener("change", (e) => {
    state.baseUrl = e.target.value.trim() || state.baseUrl;
  });
  $("connect").onclick = async () => {
    state.baseUrl = $("baseUrl").value.trim() || state.baseUrl;
    try {
      setConn("connecting...", null);
      await fetchSessions();
      connectEvents();
      setConn("connected", true);
    } catch (e) {
      setConn(`disconnected: ${e.message}`, false);
    }
  };
  $("refresh").onclick = async () => {
    try {
      await fetchSessions();
    } catch (e) {
      setConn(`disconnected: ${e.message}`, false);
    }
  };
  $("send").onclick = sendMessage;

  $("toggleTheme").onclick = () => {
    document.body.classList.toggle("dark");
  };
}

wire();

