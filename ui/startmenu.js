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
  hideMenu();
  resetToLanding();
  sheet.classList.remove("in");
  sheet.classList.add("out");
  setTimeout(() => {
    invoke("startmenu_close_finished").catch(() => {});
  }, 170);
}).catch(() => {});

// ---------- views / search / controls / pinned ----------
const views = {
  landing: document.getElementById("view-landing"),
  search: document.getElementById("view-search"),
  controls: document.getElementById("view-controls"),
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
    const sc = views[k].querySelector("#controls-scroll, #pinned-scroll, #results");
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
  } else if (name === "controls") {
    syncControls();
  }
}

document.getElementById("box-search").addEventListener("click", () => showView("search"));
document.getElementById("box-controls").addEventListener("click", () => showView("controls"));
document.getElementById("box-pinned").addEventListener("click", () => showView("pinned"));
for (const id of ["search-back", "controls-back", "pinned-back"]) {
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

// ---------- controls ----------

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

function syncControls() {
  syncLockPassword();
  syncLockHotkey();
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

for (const btn of document.querySelectorAll(".wbtn")) {
  btn.addEventListener("click", () => {
    invoke("startmenu_add_widget", { kind: btn.dataset.kind })
      .then(() => {
        btn.classList.add("ok");
        const old = btn.textContent;
        btn.textContent = "Added";
        setTimeout(() => {
          btn.classList.remove("ok");
          btn.textContent = old;
        }, 900);
      })
      .catch(() => {});
  });
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
