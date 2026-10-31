/* nah GUI - vanilla JS, no frameworks */

const chat = document.getElementById("chat");
const input = document.getElementById("input");
const sendBtn = document.getElementById("send");

let busy = false;
let activeTool = null; // tool card currently streaming into

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
  const chip = el("span", "chip wait", "waiting");
  head.appendChild(chip);
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
  const cards = chat.querySelectorAll(".toolcard");
  for (const c of cards) {
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
      addMsg("info").textContent = "error: the server refused the request (" + resp.status + ")";
      return;
    }
    await parseSSE(resp, {
      delta(d) { bubble.textContent += d.text; scrollDown(); },
      tool(t) {
        activeTool = toolCard(t.verb, t.path);
      },
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
      info(d) { addMsg("info").textContent = d.text; },
      done(d) {
        if (bubble.textContent.trim() === "") bubble.textContent = "(no final text)";
        if (d.stats) document.getElementById("s-stats").textContent = d.stats;
      },
      error(d) {
        addMsg("info").textContent = "error: " + d.text;
      },
    });
  } catch (e) {
    addMsg("info").textContent = "connection error: " + e.message;
  } finally {
    setBusy(false);
    poll();
  }
}

function setBusy(v) {
  busy = v;
  sendBtn.disabled = v;
  document.getElementById("s-busy").textContent = v ? "working…" : "";
  if (v) sendBtn.textContent = "…";
  else sendBtn.textContent = "Send";
}

/* ---------- state polling ---------- */

async function poll() {
  try {
    const resp = await fetch("/api/state");
    const s = await resp.json();

    document.getElementById("m-model").textContent = s.model;
    document.getElementById("m-server").textContent = s.server;

    for (const b of document.querySelectorAll(".modes button")) {
      b.classList.toggle("active", b.dataset.mode === s.mode);
    }

    const used = s.usage_tokens, total = s.ctx_tokens;
    document.getElementById("s-ctx").textContent = used + " / " + total + " tokens";
    document.getElementById("s-ctx-fill").style.width =
      Math.min(100, total ? (used / total) * 100 : 0) + "%";
    if (s.stats) document.getElementById("s-stats").textContent = s.stats;

    // attach approval buttons to matching tool cards
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

    if (!busy && s.busy) setBusy(true); // a turn started elsewhere
  } catch (e) { /* server briefly unavailable - keep polling */ }
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

/* ---------- mode switcher ---------- */

document.getElementById("modes").addEventListener("click", async (e) => {
  const btn = e.target.closest("button");
  if (!btn || busy) return;
  for (const b of document.querySelectorAll(".modes button")) b.classList.remove("active");
  btn.classList.add("active");
  try {
    await fetch("/api/mode", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ mode: btn.dataset.mode }),
    });
  } catch (e) { /* ignore */ }
});

/* ---------- composer ---------- */

function autoGrow() {
  input.style.height = "auto";
  input.style.height = Math.min(180, input.scrollHeight) + "px";
}

input.addEventListener("input", autoGrow);
input.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    send();
  }
});
sendBtn.addEventListener("click", send);

setInterval(poll, 700);
poll();
input.focus();
