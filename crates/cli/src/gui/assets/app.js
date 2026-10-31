/* nah GUI - vanilla JS, no frameworks */

const chat = document.getElementById("chat");
const input = document.getElementById("input");
const sendBtn = document.getElementById("send");
const palette = document.getElementById("palette");
const drawer = document.getElementById("drawer");
const backdrop = document.getElementById("drawer-backdrop");

let busy = false;
let activeTool = null;
let palSel = 0;

function el(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
}

function scrollDown() {
  chat.scrollTop = chat.scrollHeight;
}

function addMsg(kind) {
  const msg = el("div", "msg " + kind);
  const bubble = el("div", "bubble");
  msg.appendChild(bubble);
  chat.appendChild(msg);
  scrollDown();
  return bubble;
}

function toolCard(verb, path) {
  const card = el("div", "toolcard");
  const head = el("div", "head");
  head.appendChild(el("span", "verb", verb));
  head.appendChild(el("span", "path", path));
  head.appendChild(el("span", "chip wait", "waiting"));
  card.appendChild(head);
  card.appendChild(el("div", "body"));
  const approve = el("div", "approve");
  const ok = el("button", "btn primary", "Approve");
  const no = el("button", "btn danger", "Reject");
  approve.appendChild(ok);
  approve.appendChild(no);
  card.appendChild(approve);
  chat.appendChild(card);
  scrollDown();
  return card;
}

function findCard(verb, path) {
  for (const c of chat.querySelectorAll(".toolcard")) {
    const v = c.querySelector(".verb").textContent;
    const p = c.querySelector(".path").textContent;
    if (v === verb && p === path && !c.dataset.done) return c;
  }
  return null;
}

function setChip(card, cls, label) {
  const chip = card.querySelector(".chip");
  chip.className = "chip " + cls;
  chip.textContent = label;
}

function info(text) {
  addMsg("info").textContent = text;
}

/* ---------- slash command palette ---------- */

const COMMANDS = [
  { cmd: "/new", desc: "start a fresh conversation", run: () => newChat() },
  { cmd: "/compact", desc: "summarize older history now", run: () => compact() },
  { cmd: "/clear", desc: "clear the chat view", run: () => { chat.innerHTML = ""; } },
  { cmd: "/tools", desc: "open the tools panel", run: () => openDrawer("tools") },
  { cmd: "/files", desc: "open the files panel", run: () => openDrawer("files") },
  { cmd: "/settings", desc: "open settings", run: () => openDrawer("settings") },
  { cmd: "/model", desc: "open the model picker", run: () => openDrawer("settings") },
  { cmd: "/mode read-only", desc: "only reads are allowed", run: () => setMode("read-only") },
  { cmd: "/mode confirm", desc: "ask before every action", run: () => setMode("confirm") },
  { cmd: "/mode auto", desc: "run actions without asking", run: () => setMode("auto") },
];

function paletteItems() {
  const q = input.value.trim().toLowerCase();
  return COMMANDS.filter((c) => c.cmd.startsWith(q) || c.cmd.split(" ")[0] === q);
}

function renderPalette() {
  const items = paletteItems();
  if (!input.value.startsWith("/") || /\s/.test(input.value.trim().slice(1)) || items.length === 0) {
    // keep open for "/mode " with an argument
    const trimmed = input.value.trim();
    const isModeArg = /^\/mode\s+\S*$/.test(trimmed);
    if (!isModeArg) {
      palette.classList.remove("open");
      return;
    }
  }
  palSel = Math.min(palSel, Math.max(0, items.length - 1));
  palette.innerHTML = "";
  items.forEach((c, i) => {
    const item = el("div", "item" + (i === palSel ? " sel" : ""));
    item.appendChild(el("b", null, c.cmd));
    item.appendChild(el("span", null, c.desc));
    item.addEventListener("mousedown", (e) => {
      e.preventDefault();
      runCommand(c.cmd);
    });
    palette.appendChild(item);
  });
  palette.classList.add("open");
}

function runCommand(cmd) {
  palette.classList.remove("open");
  input.value = "";
  autoGrow();
  const entry = COMMANDS.find((c) => c.cmd === cmd);
  if (entry) entry.run();
  else info("unknown command: " + cmd + " — type / for the list");
}

/* ---------- streaming chat ---------- */

async function parseSSE(response, handlers) {
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    let idx;
    while ((idx = buf.indexOf("\n\n")) >= 0) {
      const chunk = buf.slice(0, idx);
      buf = buf.slice(idx + 2);
      let event = "message";
      let data = "";
      for (const line of chunk.split("\n")) {
        if (line.startsWith("event: ")) event = line.slice(7).trim();
        else if (line.startsWith("data: ")) data += line.slice(6);
      }
      if (data && handlers[event]) {
        try { handlers[event](JSON.parse(data)); } catch (e) { /* skip */ }
      }
    }
  }
}

async function send() {
  const text = input.value.trim();
  if (!text || busy) return;

  // slash commands never reach the model
  if (text.startsWith("/")) {
    const entry = COMMANDS.find((c) => c.cmd === text.toLowerCase())
      || COMMANDS.find((c) => c.cmd.split(" ")[0] === text.toLowerCase().split(" ")[0] && !c.cmd.includes(" "));
    if (entry) runCommand(entry.cmd);
    else info("unknown command: " + text + " — type / for the list");
    return;
  }

  input.value = "";
  autoGrow();
  setBusy(true);
  addMsg("user").textContent = text;
  const bubble = addMsg("assistant");
  activeTool = null;

  try {
    const resp = await fetch("/api/chat", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text }),
    });
    if (!resp.ok) {
      info("error: the server refused the request (" + resp.status + ")");
      return;
    }
    await parseSSE(resp, {
      delta(d) { bubble.textContent += d.text; scrollDown(); },
      tool(t) { activeTool = toolCard(t.verb, t.path); },
      result(r) {
        const card = (activeTool && !activeTool.dataset.done) ? activeTool : findCard(r.verb, r.path);
        if (card) {
          card.dataset.done = "1";
          card.classList.remove("pending");
          card.classList.add("has-body");
          card.querySelector(".body").textContent = r.body;
          setChip(card, r.is_error ? "err" : "ok", r.is_error ? "error" : "ok");
          activeTool = null;
          scrollDown();
        }
      },
      info(d) { info(d.text); },
      done(d) {
        if (bubble.textContent.trim() === "") {
          bubble.parentElement.remove();
          info("(the model produced no final text)");
        }
        if (d.stats) document.getElementById("s-stats").textContent = d.stats;
      },
      error(d) { info("error: " + d.text); },
    });
  } catch (e) {
    info("connection error: " + e.message);
  } finally {
    setBusy(false);
    poll();
  }
}

function setBusy(v) {
  busy = v;
  sendBtn.disabled = v;
  document.getElementById("s-busy").textContent = v ? "working…" : "";
  sendBtn.textContent = v ? "…" : "Send";
  for (const id of ["tb-new", "tb-compact"]) {
    document.getElementById(id).disabled = v;
  }
}

/* ---------- toolbar actions ---------- */

async function newChat() {
  if (busy) return;
  try {
    const r = await fetch("/api/reset", { method: "POST" });
    if (r.ok) {
      chat.innerHTML = "";
      info("new conversation started");
    } else info("could not reset (" + r.status + ")");
  } catch (e) { info("connection error: " + e.message); }
}

async function compact() {
  if (busy) return;
  setBusy(true);
  info("summarizing older history…");
  try {
    const r = await fetch("/api/compact", { method: "POST" });
    const d = await r.json();
    if (d.ok) {
      if (d.absorbed > 0) info("summarized " + d.absorbed + " older message(s)");
      else info("nothing to compact yet");
      poll();
    } else info("compaction failed" + (d.error ? ": " + d.error : ""));
  } catch (e) { info("connection error: " + e.message); }
  finally { setBusy(false); }
}

/* ---------- drawer ---------- */

function openDrawer(tab) {
  drawer.classList.add("open");
  backdrop.classList.add("open");
  selectTab(tab);
}

function closeDrawer() {
  drawer.classList.remove("open");
  backdrop.classList.remove("open");
}

function selectTab(tab) {
  for (const b of document.querySelectorAll(".drawer-tabs button")) {
    b.classList.toggle("active", b.dataset.tab === tab);
  }
  for (const p of document.querySelectorAll(".pane")) {
    p.hidden = p.dataset.pane !== tab;
  }
  if (tab === "tools") loadTools();
  if (tab === "files") loadFiles();
  if (tab === "settings") fillSettings();
}

document.querySelector(".drawer-tabs").addEventListener("click", (e) => {
  const b = e.target.closest("button");
  if (b) selectTab(b.dataset.tab);
});
document.getElementById("drawer-close").addEventListener("click", closeDrawer);
backdrop.addEventListener("click", closeDrawer);

/* ---------- settings ---------- */

async function fillSettings() {
  try {
    const s = await (await fetch("/api/state")).json();
    document.getElementById("cfg-provider").value = s.provider || "ollama";
    document.getElementById("cfg-url").value = s.server;
    document.getElementById("cfg-ctx").value = s.ctx_tokens;
    document.getElementById("cfg-history").value = s.history || 5;
    document.getElementById("cfg-root").value = s.root;
    document.getElementById("cfg-autocompact").checked = !!s.auto_compact;
  } catch (e) { /* ignore */ }
  await refreshModels(document.getElementById("cfg-model"));
}

async function refreshModels(select) {
  try {
    const r = await fetch("/api/models");
    const d = await r.json();
    const current = document.getElementById("m-model").value;
    select.innerHTML = "";
    for (const name of d.models || []) {
      const o = document.createElement("option");
      o.value = name;
      o.textContent = name;
      select.appendChild(o);
    }
    if (current && ![...select.options].some((o) => o.value === current)) {
      const o = document.createElement("option");
      o.value = current;
      o.textContent = current;
      select.appendChild(o);
    }
    select.value = current || select.value;
  } catch (e) {
    const hint = document.getElementById("cfg-hint");
    if (hint) hint.textContent = "could not list models (" + e.message + ")";
  }
}

document.getElementById("cfg-model-refresh").addEventListener("click", () => {
  refreshModels(document.getElementById("cfg-model"));
});

document.getElementById("cfg-apply").addEventListener("click", async () => {
  const hint = document.getElementById("cfg-hint");
  hint.textContent = "applying…";
  const body = {
    provider: document.getElementById("cfg-provider").value,
    url: document.getElementById("cfg-url").value.trim(),
    model: document.getElementById("cfg-model").value,
    ctx_tokens: String(document.getElementById("cfg-ctx").value),
    history: String(document.getElementById("cfg-history").value),
    root: document.getElementById("cfg-root").value.trim(),
    auto_compact: document.getElementById("cfg-autocompact").checked ? "true" : "false",
  };
  try {
    const r = await fetch("/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    const d = await r.json();
    if (d.ok) {
      hint.textContent = "saved";
      if (body.model !== document.getElementById("m-model").value) {
        chat.innerHTML = "";
        info("model switched — starting a fresh conversation");
      }
      await loadModels();
      poll();
    } else hint.textContent = d.error || ("failed (" + r.status + ")");
  } catch (e) { hint.textContent = e.message; }
});

/* ---------- tools panel ---------- */

async function loadTools() {
  const hint = document.getElementById("tools-hint");
  try {
    const r = await fetch("/api/tools");
    const d = await r.json();
    const list = document.getElementById("tools-list");
    list.innerHTML = "";
    for (const t of d.tools || []) {
      const row = el("div", "toolrow");
      row.appendChild(el("span", "tname", t.name));
      row.appendChild(el("span", "tkind", t.kind + (t.stateful ? " · writes" : "")));
      const wrap = el("span", "tstate switch");
      const cb = document.createElement("input");
      cb.type = "checkbox";
      cb.checked = t.enabled;
      const knob = document.createElement("i");
      wrap.appendChild(cb);
      wrap.appendChild(knob);
      cb.addEventListener("change", async () => {
        const resp = await fetch("/api/tools", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ name: t.name, on: cb.checked }),
        });
        if (!resp.ok) {
          cb.checked = !cb.checked;
          hint.textContent = "could not change " + t.name;
        } else hint.textContent = t.name + (cb.checked ? " enabled" : " disabled");
      });
      row.appendChild(wrap);
      list.appendChild(row);
    }
    hint.textContent = (d.tools || []).length + " tools";
  } catch (e) { hint.textContent = e.message; }
}

document.getElementById("tools-reload").addEventListener("click", async () => {
  const hint = document.getElementById("tools-hint");
  try {
    const r = await fetch("/api/tools/reload", { method: "POST" });
    const d = await r.json();
    hint.textContent = d.warnings && d.warnings.length
      ? "reloaded with warnings: " + d.warnings.join("; ")
      : "reloaded";
    loadTools();
  } catch (e) { hint.textContent = e.message; }
});

/* ---------- files panel ---------- */

async function loadFiles() {
  const list = document.getElementById("files-list");
  try {
    const r = await fetch("/api/files");
    const d = await r.json();
    list.innerHTML = "";
    if (!d.files || d.files.length === 0) {
      list.appendChild(el("div", "hint", "no files in the working set yet"));
      return;
    }
    for (const f of d.files) {
      const row = el("div", "filerow");
      const head = el("div", "fhead");
      head.appendChild(el("span", null, f.path));
      row.appendChild(head);
      const pre = document.createElement("pre");
      pre.textContent = f.content;
      row.appendChild(pre);
      head.addEventListener("click", () => row.classList.toggle("open"));
      list.appendChild(row);
    }
  } catch (e) {
    list.innerHTML = "";
    list.appendChild(el("div", "hint", "could not list files: " + e.message));
  }
}

/* ---------- model selector + modes ---------- */

async function loadModels() {
  await refreshModels(document.getElementById("m-model"));
}

document.getElementById("m-model").addEventListener("change", async (e) => {
  const model = e.target.value;
  if (!model || busy) return;
  try {
    const r = await fetch("/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model }),
    });
    const d = await r.json();
    if (d.ok) {
      chat.innerHTML = "";
      info("model switched to " + model + " — fresh conversation");
      poll();
    } else {
      info("could not switch model" + (d.error ? ": " + d.error : ""));
      poll();
    }
  } catch (err) { info("connection error: " + err.message); }
});

async function setMode(mode) {
  try {
    await fetch("/api/mode", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ mode }),
    });
    poll();
    info("mode set to " + mode);
  } catch (e) { /* ignore */ }
}

document.getElementById("modes").addEventListener("click", (e) => {
  const btn = e.target.closest("button");
  if (!btn || busy) return;
  setMode(btn.dataset.mode);
});

/* ---------- state polling ---------- */

async function poll() {
  try {
    const resp = await fetch("/api/state");
    const s = await resp.json();

    const sel = document.getElementById("m-model");
    if (sel.value !== s.model) {
      sel.innerHTML = "";
      const o = document.createElement("option");
      o.value = s.model;
      o.textContent = s.model;
      sel.appendChild(o);
      sel.value = s.model;
    }
    document.getElementById("m-server").textContent = s.server;
    document.getElementById("tb-root").textContent = "root: " + s.root;

    for (const b of document.querySelectorAll(".modes button")) {
      b.classList.toggle("active", b.dataset.mode === s.mode);
    }

    document.getElementById("s-ctx").textContent = s.usage_tokens + " / " + s.ctx_tokens + " tokens";
    document.getElementById("s-ctx-fill").style.width =
      Math.min(100, s.ctx_tokens ? (s.usage_tokens / s.ctx_tokens) * 100 : 0) + "%";
    if (s.stats) document.getElementById("s-stats").textContent = s.stats;

    for (const p of s.pending) {
      let card = findCard(p.verb, p.path);
      if (!card) card = toolCard(p.verb, p.path);
      if (!card.classList.contains("pending")) {
        card.classList.add("pending");
        if (p.preview) {
          card.classList.add("has-body");
          card.querySelector(".body").textContent = p.preview;
        }
        const btns = card.querySelectorAll(".approve button");
        btns[0].onclick = () => resolve(p.id, true, card);
        btns[1].onclick = () => resolve(p.id, false, card);
      }
    }

    if (!busy && s.busy) setBusy(true);
  } catch (e) { /* keep polling */ }
}

async function resolve(id, allow, card) {
  card.classList.remove("pending");
  setChip(card, "wait", allow ? "approved" : "rejected");
  try {
    await fetch("/api/confirm", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id, allow }),
    });
  } catch (e) { /* ignore */ }
}

/* ---------- composer + palette events ---------- */

function autoGrow() {
  input.style.height = "auto";
  input.style.height = Math.min(180, input.scrollHeight) + "px";
}

input.addEventListener("input", () => { autoGrow(); palSel = 0; renderPalette(); });
input.addEventListener("keydown", (e) => {
  const open = palette.classList.contains("open");
  const items = paletteItems();
  if (open && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
    e.preventDefault();
    palSel = (palSel + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length;
    renderPalette();
    return;
  }
  if (e.key === "Escape") { palette.classList.remove("open"); return; }
  if (e.key === "Enter") {
    e.preventDefault();
    if (open && items.length > 0) runCommand(items[palSel].cmd);
    else send();
  }
});
input.addEventListener("blur", () => setTimeout(() => palette.classList.remove("open"), 150));
sendBtn.addEventListener("click", send);

document.getElementById("tb-new").addEventListener("click", newChat);
document.getElementById("tb-compact").addEventListener("click", compact);
document.getElementById("tb-tools").addEventListener("click", () => openDrawer("tools"));
document.getElementById("tb-files").addEventListener("click", () => openDrawer("files"));
document.getElementById("tb-settings").addEventListener("click", () => openDrawer("settings"));
document.getElementById("tb-root").addEventListener("click", () => openDrawer("settings"));

setInterval(poll, 700);
loadModels();
poll();
input.focus();
