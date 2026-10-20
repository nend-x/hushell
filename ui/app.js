const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const tasksEl = document.getElementById("tasks");
const timeEl = document.getElementById("time");
const dateEl = document.getElementById("date");

let firstRender = true;
let hoverTimer = null;
let hoverKey = null;

// CSS x of an element's center relative to the bar window (works for any
// positioning scheme — the centered task strip breaks offsetLeft math).
function anchorX(el) {
  const r = el.getBoundingClientRect();
  const bar = document.getElementById("bar").getBoundingClientRect();
  return r.left + r.width / 2 - bar.left;
}

// ---------- rendering ----------

function makeButton(t) {
  const b = document.createElement("button");
  b.className = "task pop";
  b.dataset.pid = t.pid;
  if (t.minimized) b.classList.add("is-min");
  if (t.active) b.classList.add("is-active");

  const img = document.createElement("img");
  img.className = "task-icon";
  img.alt = "";
  if (t.icon) img.src = `data:image/png;base64,${t.icon}`;
  else {
    // tiny neutral placeholder glyph
    img.src =
      "data:image/svg+xml;utf8," +
      encodeURIComponent(
        `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="4" y="4" width="16" height="16" fill="#c9c9c5"/></svg>`
      );
  }

  const dot = document.createElement("span");
  dot.className = "task-dot";

  b.appendChild(img);
  b.appendChild(dot);

  // staggered pop on the first paint only
  if (firstRender) {
    const i = tasksEl.children.length;
    b.style.animationDelay = `${140 + i * 30}ms`;
  } else {
    b.classList.remove("pop");
  }

  const winIds = () => t.wins.map((w) => w.id);

  b.addEventListener("mouseenter", () => {
    clearTimeout(hoverTimer);
    hoverKey = t.pid;
    hoverTimer = setTimeout(() => {
      if (hoverKey !== t.pid) return;
      invoke("preview_show", { ids: winIds(), anchorX: anchorX(b) }).catch(() => {});
    }, 170);
  });
  b.addEventListener("mouseleave", () => {
    if (hoverKey !== t.pid) return;
    clearTimeout(hoverTimer);
    hoverKey = null;
    // NO instant hide: the native side arms a 320ms grace window (the
    // "cooldown" — the cursor may travel to the card) and JS mouseenter on
    // the card cancels it entirely. Sweeping across icons never flaps.
    invoke("preview_hover", { enter: false }).catch(() => {});
  });
  b.addEventListener("click", () => {
    invoke("activate", { id: t.id });
    closeTrayfly();
    closeVolfly();
  });
  b.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    clearTimeout(hoverTimer);
    invoke("preview_hide").catch(() => {});
    invoke("task_menu", { ids: winIds() }).catch(() => {});
  });

  b.__key = `${t.pid}|${t.wins.map((w) => w.id).join(",")}`;
  b.__wins = t.wins;

  return b;
}

function render(list) {
  // group identity = pid + its window ids (order matters for previews)
  const key = (t) => `${t.pid}|${t.wins.map((w) => w.id).join(",")}`;

  // same groups & same order → in-place state update (no re-pop)
  const same =
    list.length === tasksEl.children.length &&
    list.every((t, i) => tasksEl.children[i].__key === key(t));

  if (same) {
    list.forEach((t, i) => {
      const el = tasksEl.children[i];
      el.classList.toggle("is-min", t.minimized);
      el.classList.toggle("is-active", t.active);
      const img = el.querySelector(".task-icon");
      if (img && t.icon && !img.src.endsWith(t.icon)) {
        img.src = `data:image/png;base64,${t.icon}`;
      }
      el.__wins = t.wins;
    });
    return;
  }

  tasksEl.replaceChildren(...list.map((t) => makeButton(t)));
  firstRender = false;
}

// ---------- clock ----------

function tickClock() {
  const d = new Date();
  timeEl.textContent = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  dateEl.textContent = d.toLocaleDateString([], { day: "numeric", month: "short" });
}
setInterval(tickClock, 1000);
tickClock();

// ---------- events from the backend ----------

listen("tasks", (e) => render(e.payload)).catch(console.error);

const langEl = document.getElementById("lang");
const langCodeEl = document.getElementById("lang-code");

function setLang(l) {
  if (!l || !l.code) {
    langEl.style.display = "none";
    return;
  }
  langEl.style.display = "";
  if (langCodeEl.textContent !== l.code) langCodeEl.textContent = l.code;
}

listen("lang", (e) => setLang(e.payload)).catch(console.error);

const trayBtn = document.getElementById("trayBtn");
let trayflyOpen = false;

function closeTrayfly() {
  if (!trayflyOpen) return;
  trayflyOpen = false;
  trayBtn.classList.remove("open");
  invoke("trayfly_hide").catch(() => {});
}

trayBtn.addEventListener("click", () => {
  closeVolfly();
  if (trayflyOpen) {
    closeTrayfly();
    return;
  }
  trayflyOpen = true;
  trayBtn.classList.add("open");
  invoke("trayfly_show", { anchorX: anchorX(trayBtn) }).catch(() => {});
});

const volBtn = document.getElementById("volBtn");
let volflyOpen = false;

function closeVolfly() {
  if (!volflyOpen) return;
  volflyOpen = false;
  invoke("volfly_hide").catch(() => {});
}

volBtn.addEventListener("click", () => {
  closeTrayfly();
  if (volflyOpen) {
    closeVolfly();
    return;
  }
  volflyOpen = true;
  invoke("volfly_show", { anchorX: anchorX(volBtn) }).catch(() => {});
});

volBtn.addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    const d = e.deltaY < 0 ? 0.02 : -0.02;
    invoke("audio_master_step", { delta: d }).catch(() => {});
  },
  { passive: false }
);

// Any close path that did not start here (the native poll, Escape in the
// drop-up) lands here so the button never reads as stuck-open.
listen("volfly:closed", () => {
  volflyOpen = false;
}).catch(console.error);

document.addEventListener("pointerdown", () => {
  invoke("task_menu_hide").catch(() => {});
});

// Any close path that did not start here (blur, Escape, icon click in the
// flyout) lands here so the chevron always rotates back.
listen("trayfly:closed", () => {
  trayflyOpen = false;
  trayBtn.classList.remove("open");
}).catch(console.error);

listen("bar:reveal", () => {
  document.getElementById("bar").classList.add("in");
});

// ---------- startup sync (safety net for missed events) ----------

(async () => {
  try {
    const st = await invoke("sync_state");
    if (st.tasks) render(st.tasks);
    if (st.lang) setLang(st.lang);
  } catch (e) {
    console.error(e);
  }
  setTimeout(() => document.getElementById("bar").classList.add("in"), 1600);
})();
