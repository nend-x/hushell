const T = window.__TAURI__ || {};
const invoke =
  T.core && T.core.invoke ? T.core.invoke.bind(T.core) : function () { return Promise.resolve(); };
const listen =
  T.event && T.event.listen ? T.event.listen.bind(T.event) : function () { return Promise.resolve(); };

function reportUiError(what) {
  try { invoke("startmenu_ui_error", { msg: String(what) }); } catch (e) { /* nothing */ }
}
window.addEventListener("error", (e) => {
  reportUiError((e && (e.message || (e.error && e.error.message))) || "script error");
});

const sheet = document.getElementById("sheet");

// ---------- open / close lifecycle (registered FIRST, before anything
// that could throw — visibility may never depend on the code below) ----------

let lastOpenAt = 0;

// WebView2 carries the previous session across hide/show: whatever the
// cursor last touched keeps its :hover chain and a clicked box keeps DOM
// focus, so the reopened menu can light up a tile nobody points at. One
// synchronous display:none cycle wipes both — hiding the focused node
// blurs it, and with nothing under the cursor the hover chain clears.
// Real hover returns on the first genuine mouse move.
function purgeGhostHover() {
  try {
    const ae = document.activeElement;
    if (ae && ae !== document.body) ae.blur();
  } catch (e) { /* nothing */ }
  const b = document.body;
  b.style.display = "none";
  void b.offsetHeight;
  b.style.display = "";
}

function onOpenChoreo() {
  const now = Date.now();
  const fresh = now - lastOpenAt > 160;
  lastOpenAt = now;
  if (fresh) {
    sheet.classList.remove("out", "in", "pre");
  }
  purgeGhostHover();
  resetToLanding();
  invoke("startmenu_ui_shown").catch(() => {});
}
window.__hushMenuOpen = onOpenChoreo;

listen("startmenu:show", onOpenChoreo).catch(() => {});

document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") {
    try { purgeGhostHover(); } catch (e) { /* never block the reveal */ }
    try { resetToLanding(); } catch (e) { /* never block the reveal */ }
  }
});

listen("startmenu:hide", () => {
  // shed focus with the window — a focused box would ride into the next
  // open and paint its pressed/selected styling there
  try {
    const ae = document.activeElement;
    if (ae && ae !== document.body) ae.blur();
  } catch (e) { /* nothing */ }
  cancelWgDrag();
  hideMenu();
  resetToLanding();
  sheet.classList.remove("in");
  sheet.classList.add("out");
  setTimeout(() => {
    invoke("startmenu_close_finished").catch(() => {});
  }, 170);
}).catch(() => {});

// ---------- views / search / options / widgets ----------
const views = {
  landing: document.getElementById("view-landing"),
  search: document.getElementById("view-search"),
  options: document.getElementById("view-options"),
  widgets: document.getElementById("view-widgets"),
  pinned: document.getElementById("view-pinned"),
};

// ---------- view switching ----------

let current = "landing";

function resetToLanding() {
  for (const k of Object.keys(views)) {
    views[k].classList.toggle("active", k === "landing");
  }
  current = "landing";
  hideMenu();
  clearTimeout(searchTimer);
  searchSeq++;
  hits = [];
  sel = 0;
  input.value = "";
  paintResults();
  hintEl.classList.add("hidden");
  for (const k of Object.keys(views)) {
    const sc = views[k].querySelector("#options-scroll, #pinned-scroll, #results");
    if (sc) sc.scrollTop = 0;
  }
}

function showView(name) {
  if (!views[name] || current === name) return;
  for (const k of Object.keys(views)) {
    views[k].classList.toggle("active", k === name);
  }
  current = name;
  if (name === "search") {
    input.focus();
    input.select();
    runSearch(input.value);
  } else if (name === "options") {
    syncOptions();
  } else if (name === "widgets") {
    fitWgPreviews();
  }
}

document.getElementById("box-search").addEventListener("click", () => showView("search"));
document.getElementById("box-options").addEventListener("click", () => showView("options"));
document.getElementById("box-widgets").addEventListener("click", () => showView("widgets"));
for (const id of ["search-back", "options-back", "widgets-back", "pinned-back"]) {
  document.getElementById(id).addEventListener("click", () => showView("landing"));
}

// ---------- search ----------

const input = document.getElementById("search-input");
const resultsEl = document.getElementById("results");
const hintEl = document.getElementById("search-hint");

let hits = [];
let sel = 0;
let searchTimer = null;
let searchSeq = 0;

function paintResults() {
  resultsEl.replaceChildren();
  if (!hits.length) return;
  for (let i = 0; i < hits.length; i++) {
    const h = hits[i];
    const row = document.createElement("button");
    row.className = "hit" + (i === sel ? " sel" : "");
    const ic = document.createElement("span");
    ic.className = "h-icon";
    if (h.icon) {
      const img = document.createElement("img");
      img.src = "data:image/png;base64," + h.icon;
      img.draggable = false;
      ic.appendChild(img);
    } else {
      const ph = document.createElement("span");
      ph.className = "ph";
      ph.style.cssText = "display:block;width:20px;height:20px;border:1px solid var(--stroke);";
      ic.appendChild(ph);
    }
    row.appendChild(ic);
    const nm = document.createElement("span");
    nm.className = "h-name";
    nm.textContent = h.name;
    row.appendChild(nm);
    const sub = document.createElement("span");
    sub.className = "h-sub";
    sub.textContent = h.sub;
    row.appendChild(sub);
    row.addEventListener("click", () => openHit(h));
    row.addEventListener("mousemove", () => {
      if (sel !== i) {
        sel = i;
        for (let j = 0; j < resultsEl.children.length; j++) {
          resultsEl.children[j].classList.toggle("sel", j === sel);
        }
      }
    });
    resultsEl.appendChild(row);
  }
  const selEl = resultsEl.children[sel];
  if (selEl) selEl.scrollIntoView({ block: "nearest" });
}

function openHit(h) {
  invoke("startmenu_open", { target: h.target })
    .then(() => invoke("startmenu_hide"))
    .catch(() => {});
}

function runSearch(q) {
  const seq = ++searchSeq;
  invoke("startmenu_search", { q })
    .then((rows) => {
      if (seq !== searchSeq) return; // a newer keystroke owns the list
      hits = rows || [];
      sel = 0;
      paintResults();
      hintEl.classList.toggle("hidden", hits.length > 0 || !q.length);
    })
    .catch(() => {});
}

input.addEventListener("input", () => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => runSearch(input.value), 110);
});

input.addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    if (!hits.length) return;
    sel = (sel + (e.key === "ArrowDown" ? 1 : hits.length - 1)) % hits.length;
    paintResults();
  } else if (e.key === "Enter") {
    e.preventDefault();
    if (hits[sel]) openHit(hits[sel]);
  }
});

// ---------- options ----------

const lockpwInput = document.getElementById("lockpw-input");
const lockpwState = document.getElementById("lockpw-state");

function syncLockPassword() {
  invoke("lock_password_get")
    .then((set) => {
      lockpwState.textContent = set ? "set" : "not set";
    })
    .catch(() => {});
}

function saveLockPassword() {
  const pw = lockpwInput.value;
  invoke("lock_password_set", { password: pw })
    .then(() => {
      lockpwInput.value = "";
      lockpwState.textContent = pw ? "saved" : "cleared";
      setTimeout(syncLockPassword, 900);
    })
    .catch(() => {});
}

document.getElementById("lockpw-set").addEventListener("click", saveLockPassword);
lockpwInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    saveLockPassword();
  }
});

function syncOptions() {
  syncLockPassword();
  syncLockHotkey();
  syncLockBurn();
  syncStartup();
}

const swLockHotkey = document.getElementById("sw-lockhotkey");
const lockHotkeyState = document.getElementById("lockhotkey-state");
let lockHotkeyOn = true;

function syncLockHotkey() {
  invoke("lock_hotkey_get")
    .then((on) => {
      lockHotkeyOn = !!on;
      swLockHotkey.classList.toggle("on", lockHotkeyOn);
      lockHotkeyState.textContent = lockHotkeyOn ? "alt + l" : "off";
    })
    .catch(() => {});
}

swLockHotkey.addEventListener("click", () => {
  lockHotkeyOn = !lockHotkeyOn;
  // instant local flip — the page never waits for the round trip
  swLockHotkey.classList.toggle("on", lockHotkeyOn);
  lockHotkeyState.textContent = lockHotkeyOn ? "alt + l" : "off";
  invoke("lock_hotkey_set", { on: lockHotkeyOn }).catch(() => {});
});

// ---------- lock-screen burn shield switch (OLED protection) -------------

const swLockBurn = document.getElementById("sw-lockburn");
const lockBurnState = document.getElementById("lockburn-state");
let lockBurnOn = true;

function syncLockBurn() {
  invoke("lock_burn_get")
    .then((on) => {
      lockBurnOn = !!on;
      swLockBurn.classList.toggle("on", lockBurnOn);
      lockBurnState.textContent = lockBurnOn ? "lock screen · 1 min idle" : "off";
    })
    .catch(() => {});
}

swLockBurn.addEventListener("click", () => {
  lockBurnOn = !lockBurnOn;
  // instant local flip — the page never waits for the round trip
  swLockBurn.classList.toggle("on", lockBurnOn);
  lockBurnState.textContent = lockBurnOn ? "lock screen · 1 min idle" : "off";
  invoke("lock_burn_set", { on: lockBurnOn }).catch(() => {});
});

// ---------- startup switch (shell:startup self-registration) ----------

const swStartup = document.getElementById("sw-startup");
const startupState = document.getElementById("startup-state");
let startupOn = false;

function syncStartup() {
  invoke("startup_get")
    .then((on) => {
      startupOn = !!on;
      swStartup.classList.toggle("on", startupOn);
      startupState.textContent = startupOn ? "shell:startup" : "off";
    })
    .catch(() => {});
}

swStartup.addEventListener("click", () => {
  startupOn = !startupOn;
  // instant local flip — the page never waits for the round trip
  swStartup.classList.toggle("on", startupOn);
  startupState.textContent = startupOn ? "shell:startup" : "off";
  invoke("startup_set", { on: startupOn }).catch(() => {});
});

// ---------- start fresh (confirm lives in the shell prompt dialog) ----------

document.getElementById("row-fresh").addEventListener("click", () => {
  // the themed prompt dialog owns the decision — close the menu first so
  // the two never fight for the foreground
  invoke("startmenu_hide").catch(() => {});
  invoke("hushtop_prompt_show", { mode: "fresh", name: "" }).catch(() => {});
});

// ---------- the widget gallery (third tab) — press, drag out, drop ----------

const WIDGETS = [
  { kind: "notes", name: "Notes", desc: "a paper card for quick thoughts" },
  { kind: "volume", name: "Volume", desc: "master + per-program mixer" },
  { kind: "clipboard", name: "Clipboard", desc: "everything you copied, ready to paste" },
  { kind: "stats", name: "System stats", desc: "cpu, memory and battery at a glance" },
  { kind: "lock", name: "Lock", desc: "one press locks the shell" },
  { kind: "media", name: "Media", desc: "control your currently playing media" },
  { kind: "lyrics", name: "Lyrics", desc: "lyrics for the currently playing song, might be wrong in special cases" },
  { kind: "spaces", name: "Second desktop", desc: "swap between two desktop spaces" },
  { kind: "brightness", name: "Brightness", desc: "talks to the monitor's backlight" },
  { kind: "effects", name: "Effects", desc: "some effects for your desktop" },
];

// The gallery previews are the REAL widget bodies (hushWidgetPreview — the
// same DOM the desktop renders), scaled down to the card. No hand-drawn
// mocks exist anywhere anymore.
function wgPreviewBox(kind) {
  const fit = document.createElement("span");
  fit.className = "wprev-fit";
  const card = document.createElement("span");
  card.className = "wprev wprev-" + kind;
  card.dataset.kind = kind;
  // the widget's REAL box — the body lays out inside it exactly as it does
  // on the desktop (the lock button, for one, only fills a sized card)
  const [nw, nh] = hushWidgetDims(kind);
  card.style.width = nw + "px";
  card.style.height = nh + "px";
  card.appendChild(hushWidgetPreview(kind));
  fit.appendChild(card);
  return fit;
}

// Scale every preview to its card's content width. Measured lazily on the
// first show of the widgets tab — the view is display:none until then and a
// hidden node measures 0. Retries on every tab switch until all measured.
let wgPrevFitted = false;
function fitWgPreviews() {
  if (wgPrevFitted) return;
  const fits = [...wgGrid.querySelectorAll(".wprev-fit")];
  let all = fits.length > 0;
  for (const fit of fits) {
    const card = fit.firstChild;
    if (!card) {
      all = false;
      continue;
    }
    const [nw, nh] = hushWidgetDims(card.dataset.kind);
    const w = fit.clientWidth;
    if (!w) {
      all = false;
      continue;
    }
    // Miniatures, not 1:1 cards: every preview lands at most ~78% of its
    // column (media 288px used to render full size and clip at the menu
    // edge). The absolute card centers in the fit box at the scaled width.
    const s = Math.min(1, (w * 0.78) / nw);
    card.style.transform = "scale(" + s + ")";
    card.style.left = Math.round((w - nw * s) / 2) + "px";
    fit.style.height = Math.round(nh * s) + "px";
  }
  if (all) wgPrevFitted = true;
}

const wgGrid = document.getElementById("wg-grid");
for (const w of WIDGETS) {
  const card = document.createElement("button");
  card.className = "wgcard";
  card.dataset.kind = w.kind;
  const prev = wgPreviewBox(w.kind);
  prev.dataset.kind = w.kind;
  card.appendChild(prev);
  const nm = document.createElement("span");
  nm.className = "wg-name";
  nm.textContent = w.name;
  const ds = document.createElement("span");
  ds.className = "wg-desc";
  ds.textContent = w.desc;
  card.appendChild(nm);
  card.appendChild(ds);
  wgGrid.appendChild(card);
}

// The drag-out: pointer capture rides the card; every move streams to the
// native side (which translates start-menu client px -> desktop px and
// feeds the hushtop ghost); release lands the widget (or cancels off-paper).
let wgDrag = null;

wgGrid.addEventListener("pointerdown", (e) => {
  const card = e.target.closest(".wgcard");
  if (!card || e.button !== 0) return;
  card.setPointerCapture(e.pointerId);
  wgDrag = { kind: card.dataset.kind, started: false, sx: e.clientX, sy: e.clientY, last: 0 };
});

wgGrid.addEventListener("pointermove", (e) => {
  if (!wgDrag) return;
  if (!wgDrag.started) {
    if (Math.abs(e.clientX - wgDrag.sx) < 5 && Math.abs(e.clientY - wgDrag.sy) < 5) return;
    wgDrag.started = true;
  }
  const now = performance.now();
  if (now - wgDrag.last < 40) return;
  wgDrag.last = now;
  // remember the newest release point — a lost capture (window hidden,
  // capture yanked) has no pointerup, and the drop still needs coordinates
  wgDrag.lx = e.clientX;
  wgDrag.ly = e.clientY;
  invoke("widget_drag_move", { kind: wgDrag.kind, cx: e.clientX, cy: e.clientY }).catch(() => {});
});

const wgDragEnd = (ok) => {
  if (!wgDrag) return;
  const d = wgDrag;
  wgDrag = null;
  if (!ok || !d.started) {
    if (d.started) invoke("widget_drag_cancel").catch(() => {});
    return;
  }
  // the release point lands (or cancels) — the native side owns the verdict
  invoke("widget_drag_drop", { kind: d.kind, cx: d.lx ?? d.sx, cy: d.ly ?? d.sy })
    .catch(() => invoke("widget_drag_cancel").catch(() => {}));
};

wgGrid.addEventListener("pointerup", (e) => {
  if (wgDrag) {
    wgDrag.lx = e.clientX;
    wgDrag.ly = e.clientY;
  }
  wgDragEnd(true);
});
wgGrid.addEventListener("pointercancel", () => wgDragEnd(false));
// Capture lost WITHOUT pointerup/cancel (the window hid mid-drag, WebView2
// yanked the capture): end the drag here or the ghost would ride forever.
// After a normal release this fires with wgDrag already null — a no-op.
for (const c of wgGrid.querySelectorAll(".wgcard")) {
  c.addEventListener("lostpointercapture", () => wgDragEnd(true));
}
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && wgDrag && wgDrag.started) {
    wgDragEnd(false);
  }
});

// The menu is going away while a drag is live: no pointer event will ever
// arrive — cancel the ghost explicitly (a stuck ghost would sit on the
// desktop forever).
function cancelWgDrag() {
  if (!wgDrag) return;
  const d = wgDrag;
  wgDrag = null;
  if (d.started) invoke("widget_drag_cancel").catch(() => {});
}

document.getElementById("row-shutdown").addEventListener("click", () => {
  invoke("shell_shutdown").catch(() => {});
});

const powerBtn = document.getElementById("power-btn");
let menuOwner = null; // "power" | "tile" | null

powerBtn.addEventListener("click", () => {
  if (menuOwner === "power") {
    hideMenu();
    return;
  }
  const r = powerBtn.getBoundingClientRect();
  showMenu(
    r.left,
    r.bottom + 8,
    [
      { t: "Sleep", a: () => invoke("system_sleep").catch(() => {}) },
      { t: "Restart", a: () => invoke("system_restart").catch(() => {}) },
      { t: "Shut down", a: () => invoke("system_shutdown").catch(() => {}) },
    ],
    "power"
  );
  menuEl.style.top = r.bottom + 8 + "px";
  menuEl.style.left = Math.max(6, r.right - menuEl.offsetWidth) + "px";
});

// ---------- in-page menu ----------

const menuEl = document.getElementById("menu");

function showMenu(x, y, items, owner) {
  menuEl.replaceChildren();
  for (const it of items) {
    if (it.sep) {
      const s = document.createElement("div");
      s.className = "msep";
      menuEl.appendChild(s);
      continue;
    }
    const b = document.createElement("button");
    b.className = "mi";
    b.textContent = it.t;
    b.addEventListener("click", () => {
      hideMenu();
      it.a();
    });
    menuEl.appendChild(b);
  }
  menuEl.classList.remove("hidden");
  menuOwner = owner || "tile";
  const mw = menuEl.offsetWidth;
  const mh = menuEl.offsetHeight;
  const maxX = sheet.clientWidth - mw - 6;
  const maxY = sheet.clientHeight - mh - 6;
  menuEl.style.left = Math.min(Math.max(6, x), Math.max(6, maxX)) + "px";
  menuEl.style.top = Math.min(Math.max(6, y), Math.max(6, maxY)) + "px";
}

function hideMenu() {
  menuEl.classList.add("hidden");
  menuEl.replaceChildren();
  menuOwner = null;
}

document.addEventListener("mousedown", (e) => {
  if (menuEl.classList.contains("hidden")) return;
  if (menuEl.contains(e.target)) return;
  if (menuOwner === "power" && powerBtn.contains(e.target)) return;
  hideMenu();
});

// ---------- open / close lifecycle ----------

// Presses on the transparent margin (outside the sheet) close the menu.
document.body.addEventListener("mousedown", (e) => {
  if (e.target === document.body) {
    invoke("startmenu_hide").catch(() => {});
  }
});

// Escape closes the shell menu from any view — no landing bounce. An open
// in-page sheet (power/tile) closes first.
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (!menuEl.classList.contains("hidden")) {
      hideMenu();
    } else {
      invoke("startmenu_hide").catch(() => {});
    }
  }
});
