const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const CELL_W = 92;
const CELL_H = 72;
const MARGIN = 14;

const iconsEl = document.getElementById("icons");
const menuEl = document.getElementById("menu");
const paperEl = document.getElementById("paper");

const cells = new Map(); // name -> { el, img, label, item }
let selected = null;
let menuOpenFor = null; // null = closed, "desktop" = paper menu

const PLACEHOLDER =
  "data:image/svg+xml;utf8," +
  encodeURIComponent(
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="4" y="4" width="16" height="16" rx="3" fill="none" stroke="#111" stroke-width="1.6"/><path d="M8 12h8M8 16h6" stroke="#111" stroke-width="1.4"/></svg>`
  );

function displayName(name, kind) {
  let n = name;
  if (n.toLowerCase().endsWith(".lnk")) n = n.slice(0, -4);
  if (kind === "file") {
    const dot = n.lastIndexOf(".");
    if (dot > 0) n = n.slice(0, dot);
  }
  return n;
}

// ---------- rendering (diff, never rebuild) ----------

function makeCell(item) {
  const el = document.createElement("div");
  el.className = "cell";
  const img = document.createElement("img");
  img.alt = "";
  img.draggable = false;
  const label = document.createElement("div");
  label.className = "label";
  el.appendChild(img);
  el.appendChild(label);
  iconsEl.appendChild(el);
  wireCell(el, img, label);
  return { el, img, label, item };
}

function paintCell(entry, item) {
  entry.item = item;
  entry.el.dataset.name = item.name;
  entry.label.textContent = displayName(item.name, item.kind);
  const src = item.icon ? `data:image/png;base64,${item.icon}` : PLACEHOLDER;
  if (entry.img.src !== src) entry.img.src = src; // swap in place, no flash
  entry.el.style.left = item.x + "px";
  entry.el.style.top = item.y + "px";
}

function render(list) {
  const seen = new Set();
  for (const item of list) {
    seen.add(item.name);
    const entry = cells.get(item.name);
    if (entry) {
      paintCell(entry, item);
    } else {
      const e = makeCell(item);
      paintCell(e, item);
      cells.set(item.name, e);
    }
  }
  for (const [name, entry] of [...cells]) {
    if (!seen.has(name)) {
      entry.el.remove();
      cells.delete(name);
      if (selected === name) selected = null;
    }
  }
}

// ---------- drag (movable, config-persisted, desktop untouched) ----------

function gridSize() {
  const cols = Math.max(1, Math.floor((innerWidth - MARGIN * 2) / CELL_W));
  const rows = Math.max(1, Math.floor((innerHeight - MARGIN * 2) / CELL_H));
  return { cols, rows };
}

const cellKey = (c, r) => c + "," + r;

// Cells currently owned by OTHER icons. Their item.x/y come from the
// native occupancy resolver, so they are always on-grid and unique.
function occupiedCells(exceptName) {
  const taken = new Set();
  const { cols, rows } = gridSize();
  for (const [name, entry] of cells) {
    if (name === exceptName) continue;
    const c = Math.min(cols - 1, Math.max(0, Math.round((entry.item.x - MARGIN) / CELL_W)));
    const r = Math.min(rows - 1, Math.max(0, Math.round((entry.item.y - MARGIN) / CELL_H)));
    taken.add(cellKey(c, r));
  }
  return taken;
}

// Nearest free cell — the JS twin of hushtop.rs nearest_free_cell
// (expanding ring, ties toward smaller column then row). Running it here,
// BEFORE the drop lands, means a drop onto an occupied cell rehomes
// INSTANTLY with no IPC roundtrip and no visible stacking; the native
// cmd_move re-checks and stays the enforcer for races (new file between
// drag start and drop).
function nearestFreeCell(taken, cols, rows, c0, r0) {
  const inGrid = (c, r) => c >= 0 && r >= 0 && c < cols && r < rows;
  if (inGrid(c0, r0) && !taken.has(cellKey(c0, r0))) return [c0, r0];
  const maxR = Math.max(rows, cols);
  for (let rad = 1; rad <= maxR; rad++) {
    let best = null;
    for (let dc = -rad; dc <= rad; dc++) {
      for (let dr = -rad; dr <= rad; dr++) {
        if (Math.abs(dc) !== rad && Math.abs(dr) !== rad) continue;
        const c = c0 + dc;
        const r = r0 + dr;
        if (!inGrid(c, r) || taken.has(cellKey(c, r))) continue;
        const dist = Math.abs(dc) + Math.abs(dr);
        if (
          !best ||
          dist < best[0] ||
          (dist === best[0] && (c < best[1] || (c === best[1] && r < best[2])))
        ) {
          best = [dist, c, r];
        }
      }
    }
    if (best) return [best[1], best[2]];
  }
  return [c0, r0]; // grid full — native decides
}

function wireCell(el, img, label) {
  let drag = null;

  el.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    hideMenu();
    const name = el.dataset.name;
    if (selected && selected !== name) {
      const prev = cells.get(selected);
      if (prev) prev.el.classList.remove("selected");
    }
    selected = name;
    el.classList.add("selected");
    const r = el.getBoundingClientRect();
    drag = {
      name,
      dx: e.clientX - r.left,
      dy: e.clientY - r.top,
      moved: false,
      handed: false,
    };
    el.setPointerCapture(e.pointerId);
  });

  el.addEventListener("pointermove", (e) => {
    if (!drag) return;
    const x = Math.round(e.clientX - drag.dx);
    const y = Math.round(e.clientY - drag.dy);
    if (!drag.moved && (Math.abs(x - el.offsetLeft) > 2 || Math.abs(y - el.offsetTop) > 2)) {
      drag.moved = true;
      el.classList.add("dragging");
    }
    if (drag.moved) {
      if (!drag.handed) {
        drag.handed = true;
        invoke("hushtop_dragout", { name: el.dataset.name, dx: drag.dx, dy: drag.dy }).catch(
          () => {}
        );
      }
      el.style.left = x + "px";
      el.style.top = y + "px";
    }
  });

  el.addEventListener("pointerup", (e) => {
    if (!drag) return;
    el.classList.remove("dragging");
    const d = drag;
    drag = null;
    if (!d.moved) return; // plain click — nothing to save
    const { cols, rows } = gridSize();
    const c = Math.min(cols - 1, Math.max(0, Math.round((e.clientX - d.dx - MARGIN) / CELL_W)));
    const r = Math.min(rows - 1, Math.max(0, Math.round((e.clientY - d.dy - MARGIN) / CELL_H)));
    const [fc, fr] = nearestFreeCell(occupiedCells(d.name), cols, rows, c, r);
    const gx = MARGIN + fc * CELL_W;
    const gy = MARGIN + fr * CELL_H;
    el.style.left = gx + "px";
    el.style.top = gy + "px";
    invoke("hushtop_move", { name: d.name, x: gx, y: gy }).catch(() => {});
  });

  el.addEventListener("dblclick", () => {
    invoke("hushtop_open", { name: el.dataset.name }).catch(() => toast("Could not open"));
  });

  el.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    e.stopPropagation();
    const name = el.dataset.name;
    const base = [
      { t: "Open", a: () => invoke("hushtop_open", { name }).catch(() => toast("Could not open")) },
      { t: "Rename", a: () => invoke("hushtop_prompt_show", { mode: "rename", name }).catch(() => {}) },
    ];
    const tail = [
      { sep: true },
      { t: "Delete", a: () => invoke("hushtop_delete", { name }).catch(() => toast("Could not delete")) },
    ];
    invoke("startmenu_is_pinned", { name })
      .then((pinned) => {
        const pinRow = pinned
          ? { t: "Unpin from start", a: () => invoke("startmenu_unpin", { target: name }).catch(() => {}) }
          : { t: "Pin to start", a: () => invoke("startmenu_pin", { target: name }).catch(() => {}) };
        showMenu(e.clientX, e.clientY, [...base, pinRow, ...tail]);
      })
      .catch(() => showMenu(e.clientX, e.clientY, [...base, ...tail]));
  });
}

// ---------- context menu ----------

function showMenu(x, y, items) {
  menuEl.replaceChildren();
  for (const it of items) {
    if (it.sep) {
      const s = document.createElement("div");
      s.className = "sep";
      menuEl.appendChild(s);
      continue;
    }
    const mi = document.createElement("div");
    mi.className = "mi";
    mi.textContent = it.t;
    mi.addEventListener("click", () => {
      hideMenu();
      it.a();
    });
    menuEl.appendChild(mi);
  }
  menuEl.classList.remove("hidden");
  const mw = menuEl.offsetWidth;
  const mh = menuEl.offsetHeight;
  menuEl.style.left = Math.max(2, Math.min(x, innerWidth - mw - 4)) + "px";
  menuEl.style.top = Math.max(2, Math.min(y, innerHeight - mh - 4)) + "px";
  menuOpenFor = "open";
  invoke("hushtop_menu", { open: true }).catch(() => {});
}

function hideMenu() {
  if (!menuOpenFor) return;
  menuOpenFor = null;
  menuEl.classList.add("hidden");
  invoke("hushtop_menu", { open: false }).catch(() => {});
}

paperEl.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  hideMenu();
  showMenu(e.clientX, e.clientY, [
    { t: "Refresh", a: () => invoke("hushtop_refresh").catch(() => {}) },
    { sep: true },
    { t: "New folder", a: () => invoke("hushtop_prompt_show", { mode: "newfolder", name: "" }).catch(() => {}) },
    { t: "New file", a: () => invoke("hushtop_prompt_show", { mode: "newfile", name: "" }).catch(() => {}) },
  ]);
});

paperEl.addEventListener("pointerdown", (e) => {
  if (e.target === paperEl || e.target === iconsEl) {
    hideMenu();
    invoke("startmenu_hide").catch(() => {});
    if (selected) {
      const prev = cells.get(selected);
      if (prev) prev.el.classList.remove("selected");
      selected = null;
    }
  }
});

// heartbeat-driven close (foreground moved to another app)
listen("hushtop:menu_close", () => {
  menuOpenFor = null;
  menuEl.classList.add("hidden");
}).catch(() => {});

function toast(msg) {
  let t = document.getElementById("toast");
  if (!t) {
    t = document.createElement("div");
    t.id = "toast";
    paperEl.appendChild(t);
  }
  t.textContent = msg;
  t.classList.add("show");
  clearTimeout(toast._h);
  toast._h = setTimeout(() => t.classList.remove("show"), 2200);
}

// ---------- events from the backend ----------

listen("hushtop:icons", (e) => render(e.payload)).catch(console.error);

listen("hushtop:drag_move", (e) => {
  const p = e.payload || {};
  const entry = cells.get(p.name);
  if (!entry) return;
  entry.el.classList.add("dragging");
  entry.el.style.left = p.x + "px";
  entry.el.style.top = p.y + "px";
});

listen("hushtop:drag_ended", (e) => {
  const name = typeof e.payload === "string" ? e.payload : e.payload && e.payload.name;
  const entry = name && cells.get(name);
  if (entry) entry.el.classList.remove("dragging");
});

// startup sync in case the first emit beat us to it
(async () => {
  try {
    const st = await invoke("sync_state");
    if (st.hushtop) render(st.hushtop);
    if (st.notepads) renderNotepads(st.notepads);
    if (st.widgets) renderWidgets(st.widgets);
  } catch (e) {
    console.error(e);
  }
})();

const notepadsEl = document.getElementById("notepads");
const npEl = new Map(); // id -> { el, ta }
const npSaveTimers = {};

// ---- card snapping: the ruled sheet (Grid 2.0) ----------------------------
// The landing math is the paper cadence: cards settle on a rhythm anchored
// at the 14px margin (the same one as the horizontal rules and the icon
// grid). Horizontal stays on the 36px rule; the vertical step is 18px —
// twice the horizontal resolution, twice as many landing spots (every
// ruled line AND every midpoint between two lines). Snap is pure grid:
// cards never pull toward each other, and nothing reserves cells, so free
// arrangement (side by side included) stays free.
//
// What Grid 2.0 adds is the part the cadence alone could never show:
//   * the card follows the cursor 1:1 while a LANDING FRAME outlines the
//     exact cell it will occupy — no more guessing where the snap lands;
//   * hairline ALIGNMENT GUIDES flash when that cell lines up with a
//     sibling edge or the paper margin — zero pull, pure information;
//   * on release the card SETTLES onto the frame (110ms glide) instead of
//     teleporting;
//   * Alt holds the grid off — fine, snap-free placement for the rare
//     pixel-perfect moment.
const CARD_GRID = 36;
const CARD_GRID_Y = 18;
const CARD_ANCHOR = 14;

function cardMagnet(v, axis) {
  const grid = axis === "x" ? CARD_GRID : CARD_GRID_Y;
  return Math.round(CARD_ANCHOR + Math.round((v - CARD_ANCHOR) / grid) * grid);
}

// ---- the layout map -------------------------------------------------------
// The paper remembers CELLS, not pixels: a card's spot is its grid address
// (col, row) — col steps 36px, row steps 18px, both anchored at 14px. Pixels
// are derived at render time, so a save→reload round trip lands EXACTLY on
// the arranged cell no matter the DPI, monitor size, or when the webview
// boots. Rust stores the map verbatim; nothing on the native side touches
// geometry, so nothing can silently rewrite an arrangement.
function cardCell(x, y) {
  return {
    c: Math.round((x - CARD_ANCHOR) / CARD_GRID),
    r: Math.round((y - CARD_ANCHOR) / CARD_GRID_Y),
  };
}

function cellPx(c, r) {
  return { x: CARD_ANCHOR + c * CARD_GRID, y: CARD_ANCHOR + r * CARD_GRID_Y };
}

// NO out-of-bounds protection — by design. The map is the single truth:
// a card renders EXACTLY where its cell says, even if that cell sits past
// the screen border. The old render-time clamp (hold the right/bottom edge
// inside the viewport) was removed at the owner's directive — it nudged a
// border-hugging card one cell inward on every restart. A user who wants a
// widget half-off-screen gets exactly that, on every boot, unchanged.
function placeByCell(el, c, r) {
  const p = cellPx(c, r);
  el.style.left = p.x + "px";
  el.style.top = p.y + "px";
}

// the drag chrome: one landing frame + two vertical + two horizontal guides
const gridLand = document.createElement("div");
gridLand.id = "grid-land";
const gridGuides = [0, 1].flatMap(() => [
  Object.assign(document.createElement("i"), { className: "grid-guide v" }),
  Object.assign(document.createElement("i"), { className: "grid-guide h" }),
]);
paperEl.appendChild(gridLand);
for (const g of gridGuides) paperEl.appendChild(g);

function hideGridChrome() {
  gridLand.style.display = "none";
  for (const g of gridGuides) g.style.display = "none";
}

function showLanding(land, el) {
  gridLand.style.display = "block";
  gridLand.style.left = land.x + "px";
  gridLand.style.top = land.y + "px";
  gridLand.style.width = el.offsetWidth + "px";
  gridLand.style.height = el.offsetHeight + "px";
}

// alignment lines for the landing rect: sibling edges that coincide with
// the landing's left/right (vertical guides) or top/bottom (horizontal
// guides), plus the paper margins. Informational only — nothing pulls.
const GUIDE_TOL = 1;
function guideLines(land, el) {
  const L = land.x, T = land.y;
  const R = land.x + el.offsetWidth, B = land.y + el.offsetHeight;
  const xs = new Map(); // x -> [top, bottom]
  const ys = new Map(); // y -> [left, right]
  const addX = (x, t, b) => {
    const s = xs.get(x);
    if (s) { s[0] = Math.min(s[0], t); s[1] = Math.max(s[1], b); } else xs.set(x, [t, b]);
  };
  const addY = (y, l, r) => {
    const s = ys.get(y);
    if (s) { s[0] = Math.min(s[0], l); s[1] = Math.max(s[1], r); } else ys.set(y, [l, r]);
  };
  const near = (a, b) => Math.abs(a - b) <= GUIDE_TOL;
  const cards = [];
  for (const e of widgets.values()) if (e.el !== el) cards.push(e.el);
  for (const e of npEl.values()) if (e.el !== el) cards.push(e.el);
  for (const c of cards) {
    const sl = c.offsetLeft, st = c.offsetTop;
    const sr = sl + c.offsetWidth, sb = st + c.offsetHeight;
    if (near(L, sl) || near(L, sr)) addX(L, st, sb);
    else if (near(R, sl) || near(R, sr)) addX(R, st, sb);
    if (near(T, st) || near(T, sb)) addY(T, sl, sr);
    else if (near(B, st) || near(B, sb)) addY(B, sl, sr);
  }
  // the paper anchor — a landing on the cadence anchor sits on the icon margin
  if (near(L, MARGIN)) addX(L, T, B);
  if (near(R, innerWidth - MARGIN)) addX(R, T, B);
  if (near(T, MARGIN)) addY(T, L, R);
  return { xs: [...xs.entries()].slice(0, 2), ys: [...ys.entries()].slice(0, 2) };
}

function showGuides(land, el) {
  const { xs, ys } = guideLines(land, el);
  for (let i = 0; i < 2; i++) {
    const gv = gridGuides[i * 2], gh = gridGuides[i * 2 + 1];
    const x = xs[i], y = ys[i];
    if (x) {
      gv.style.display = "block";
      gv.style.left = x[0] + "px";
      gv.style.top = x[1][0] - 6 + "px";
      gv.style.height = x[1][1] - x[1][0] + 12 + "px";
    } else gv.style.display = "none";
    if (y) {
      gh.style.display = "block";
      gh.style.top = y[0] + "px";
      gh.style.left = y[1][0] - 6 + "px";
      gh.style.width = y[1][1] - y[1][0] + 12 + "px";
    } else gh.style.display = "none";
  }
}

// the shared card drag: raw follow + landing frame + guides + settle.
// onDrop(col, row) fires once per completed drag with the FINAL GRID CELL —
// the landing cell, or the nearest cell when Alt held the grid off (the map
// is cell-based, so even a free drop commits to the closest address).
function wireCardDrag(el, onDrop) {
  let drag = null;
  el.addEventListener("pointerdown", (e) => {
    if (e.target.closest("textarea, button, input")) return;
    el.classList.remove("settle"); // a fresh grab interrupts any glide
    const r = el.getBoundingClientRect();
    drag = { dx: e.clientX - r.left, dy: e.clientY - r.top, moved: false, land: null };
    el.setPointerCapture(e.pointerId);
  });
  el.addEventListener("pointermove", (e) => {
    if (!drag) return;
    const x = e.clientX - drag.dx;
    const y = e.clientY - drag.dy;
    if (!drag.moved && Math.abs(x - el.offsetLeft) <= 2 && Math.abs(y - el.offsetTop) <= 2) {
      return; // a resting finger is not a drag — keeps the chrome off plain clicks
    }
    drag.moved = true;
    if (!drag.grid) {
      drag.grid = true;
      paperEl.classList.add("grid-on");
    }
    el.style.left = Math.round(x) + "px";
    el.style.top = Math.round(y) + "px";
    if (e.altKey) {
      drag.land = null;
      hideGridChrome();
      return;
    }
    drag.land = { x: cardMagnet(x, "x"), y: cardMagnet(y, "y") };
    showLanding(drag.land, el);
    showGuides(drag.land, el);
  });
  const finish = (commit) => {
    if (!drag) return;
    const d = drag;
    drag = null;
    paperEl.classList.remove("grid-on");
    hideGridChrome();
    if (!d.moved) return;
    if (d.land && (d.land.x !== el.offsetLeft || d.land.y !== el.offsetTop)) {
      el.classList.add("settle");
      el.style.left = d.land.x + "px";
      el.style.top = d.land.y + "px";
      setTimeout(() => el.classList.remove("settle"), 150);
    }
    if (commit) {
      // the drop commits the CELL, not pixels — the layout map is the truth
      const p = d.land || { x: el.offsetLeft, y: el.offsetTop };
      const cell = cardCell(p.x, p.y);
      if (!d.land) {
        // Alt free-drop: the map keeps the nearest CELL, so settle the card
        // onto that cell's pixels too — what you see must be what the map
        // keeps, or the next boot moves the card by up to half a cell
        const q = cellPx(cell.c, cell.r);
        if (q.x !== el.offsetLeft || q.y !== el.offsetTop) {
          el.classList.add("settle");
          el.style.left = q.x + "px";
          el.style.top = q.y + "px";
          setTimeout(() => el.classList.remove("settle"), 150);
        }
      }
      onDrop(cell.c, cell.r);
    }
  };
  el.addEventListener("pointerup", () => finish(true));
  el.addEventListener("pointercancel", () => finish(false));
}

function makeGrip(onRemove) {
  const head = document.createElement("div");
  head.className = "grip";
  const dots = document.createElement("span");
  dots.className = "grip-dots";
  for (let i = 0; i < 3; i++) dots.appendChild(document.createElement("i"));
  const del = document.createElement("button");
  del.className = "notepad-del";
  del.setAttribute("aria-label", "Remove");
  del.addEventListener("click", onRemove);
  head.appendChild(dots);
  head.appendChild(del);
  return head;
}

function makeNotepad(n) {
  const el = document.createElement("div");
  el.className = "notepad";
  placeByCell(el, n.col, n.row);
  el.style.width = n.w + "px";
  el.style.height = n.h + "px";

  const head = makeGrip(() => {
    invoke("hushtop_notepad_del", { id: n.id }).catch(() => {});
  });

  const ta = document.createElement("textarea");
  ta.value = n.text;
  ta.placeholder = "\u2026";
  el.appendChild(head);
  el.appendChild(ta);
  notepadsEl.appendChild(el);

  // drag by the dark header; geometry persists on drop — Grid 2.0 chrome
  // (raw follow, landing frame, guides, settle) comes from wireCardDrag
  wireCardDrag(el, () => saveNotepad(n.id, el, ta));

  // text edits persist, debounce 600 ms
  ta.addEventListener("input", () => {
    clearTimeout(npSaveTimers[n.id]);
    npSaveTimers[n.id] = setTimeout(() => saveNotepad(n.id, el, ta), 600);
  });

  return { el, ta };
}

// the drop/typing persists the CELL + size — pixels are never stored
function saveNotepad(id, el, ta) {
  const cell = cardCell(el.offsetLeft, el.offsetTop);
  invoke("hushtop_notepad_save", {
    id,
    col: cell.c,
    row: cell.r,
    w: el.offsetWidth,
    h: el.offsetHeight,
    text: ta.value,
  }).catch(() => {});
}

function renderNotepads(list) {
  const seen = new Set();
  for (const n of list) {
    seen.add(n.id);
    const e = npEl.get(n.id);
    if (e) {
      // in-place: never clobber a textarea being edited
      placeByCell(e.el, n.col, n.row);
      e.el.style.width = n.w + "px";
      e.el.style.height = n.h + "px";
      if (document.activeElement !== e.ta && e.ta.value !== n.text) {
        e.ta.value = n.text;
      }
    } else {
      npEl.set(n.id, makeNotepad(n));
    }
  }
  for (const [id, e] of [...npEl]) {
    if (!seen.has(id)) {
      e.el.remove();
      npEl.delete(id);
    }
  }
}

listen("hushtop:notepads", (e) => renderNotepads(e.payload)).catch(console.error);

const widgetsEl = document.getElementById("widgets");
const widgets = new Map(); // id -> { el, kind, refs }

let volOpenIds = new Set();  // ids with the session dropdown expanded
let volTouch = 0;            // last user slider touch (skip poll 1.6s)

function volLabel(v) {
  return Math.round(v * 100) + "%";
}

// Resize the widget element to exactly its content height. animate=false
// snaps (boot self-heal); animate=true glides (easeOutCubic, ~220ms).
// persist=false keeps the snap display-only: the boot measurement runs
// before webfonts and content settle, and persisting it would rewrite the
// volume card's stored size on EVERY restart — the one-widget drift. Only
// user-visible fits (mixer toggle, session list changes) commit geometry.
function fitVolume(entry, animate, persist) {
  const el = entry.el;
  const from = el.offsetHeight;
  el.style.height = "auto";
  const target = el.offsetHeight;
  el.style.height = from + "px";
  if (!animate || from === target) {
    el.style.height = target + "px";
    if (persist !== false) saveWidgetGeom(entry);
    return;
  }
  const start = performance.now();
  const dur = 220;
  function step(t) {
    const p = Math.min(1, (t - start) / dur);
    const e2 = 1 - Math.pow(1 - p, 3);
    el.style.height = Math.round(from + (target - from) * e2) + "px";
    if (p < 1) {
      requestAnimationFrame(step);
    } else if (persist !== false) {
      saveWidgetGeom(entry);
    }
  }
  requestAnimationFrame(step);
}

function saveWidgetGeom(entry) {
  // the layout map is the truth: commit the RECORDED cell (never a
  // rendered position — keeps the commit unambiguous) + the content size
  invoke("hushtop_widget_save", {
    id: entry.rec.id,
    col: entry.rec.col,
    row: entry.rec.row,
    w: entry.el.offsetWidth,
    h: entry.el.offsetHeight,
  }).catch(() => {});
}

function volumeBody(r) {
  // readout row: big % + mute-state caption
  const readout = document.createElement("div");
  readout.className = "vol-readout";
  const pct = document.createElement("span");
  pct.className = "vol-pct";
  const state = document.createElement("span");
  state.className = "vol-state";
  state.textContent = "master";
  readout.appendChild(pct);
  readout.appendChild(state);

  const slider = document.createElement("input");
  slider.type = "range";
  slider.className = "vol-slider vol-main";
  slider.min = 0;
  slider.max = 100;
  slider.value = 0;
  slider.setAttribute("aria-label", "Master volume");

  // tick ruler under the slider (decorative, pure CSS)
  const ruler = document.createElement("div");
  ruler.className = "vol-ruler";

  // footer: MUTE left, MIXER expander right
  const foot = document.createElement("div");
  foot.className = "vol-foot";
  const mute = document.createElement("button");
  mute.className = "vol-btn vol-mute";
  mute.textContent = "Mute";
  const spacer = document.createElement("span");
  spacer.className = "vol-flex";
  const arrow = document.createElement("button");
  arrow.className = "vol-btn vol-arrow";
  arrow.innerHTML = 'Mixer<span class="chev">\u25be</span>';
  foot.appendChild(mute);
  foot.appendChild(spacer);
  foot.appendChild(arrow);

  const sessions = document.createElement("div");
  sessions.className = "vol-sessions hidden";

  let lastVol = null; // for mute/restore
  let lastGet = 0;    // last applied master value

  function apply(v, push) {
    const n = Math.round(v * 100);
    slider.value = n;
    pct.textContent = volLabel(v);
    lastGet = n;
    const muted = n === 0;
    mute.classList.toggle("is-muted", muted);
    state.textContent = muted ? "muted" : "master";
    if (push) {
      volTouch = Date.now();
      invoke("audio_master_set", { vol: v }).catch(() => {});
    }
  }

  slider.addEventListener("input", () => {
    apply(slider.value / 100, true);
  });

  mute.addEventListener("click", () => {
    const cur = slider.value / 100;
    if (cur > 0) {
      lastVol = cur;
      apply(0, true);
    } else {
      apply(lastVol == null ? 0.5 : lastVol, true);
    }
  });

  arrow.addEventListener("click", () => {
    const hidden = sessions.classList.toggle("hidden");
    arrow.classList.toggle("is-open", !hidden);
    if (!hidden) {
      volOpenIds.add(r.id);
      refreshSessions(r);
    } else {
      volOpenIds.delete(r.id);
      if (r.fit) r.fit(); // collapse back to the compact strip
    }
  });

  function refreshSessions(r) {
    invoke("audio_sessions")
      .then((rows) => {
        sessions.replaceChildren();
        if (!rows.length) {
          const empty = document.createElement("div");
          empty.className = "s-empty";
          empty.textContent = "no active programs";
          sessions.appendChild(empty);
        }
        for (const s of rows) {
          const row = document.createElement("div");
          row.className = "s-row";
          const name = document.createElement("span");
          name.className = "s-name";
          name.textContent = s.name;
          const sl = document.createElement("input");
          sl.type = "range";
          sl.className = "vol-slider s-slider";
          sl.min = 0;
          sl.max = 100;
          sl.value = Math.round(s.vol * 100);
          const sp = document.createElement("span");
          sp.className = "s-pct";
          sp.textContent = volLabel(s.vol);
          sl.addEventListener("input", () => {
            volTouch = Date.now();
            sp.textContent = volLabel(sl.value / 100);
            invoke("audio_session_set", { id: s.id, vol: sl.value / 100 }).catch(() => {});
          });
          row.appendChild(name);
          row.appendChild(sl);
          row.appendChild(sp);
          sessions.appendChild(row);
        }
        // rows changed -> the open height must follow
        if (r.fit) r.fit();
      })
      .catch(() => {});
  }

  r.poll = setInterval(() => {
    if (Date.now() - volTouch < 1600) return;
    invoke("audio_master_get")
      .then((v) => {
        const n = Math.round(v * 100);
        if (n !== lastGet) apply(v, false);
      })
      .catch(() => {});
    if (volOpenIds.has(r.id)) refreshSessions(r);
  }, 2000);

  invoke("audio_master_get").then((v) => apply(v, false)).catch(() => {});

  return { readout, slider, ruler, foot, sessions, pct };
}

let lastStats = { cpu: 0, ram: 0, battery: null, charging: false };

function statsBody() {
  const rows = document.createElement("div");
  rows.className = "st-rows";

  function mkRow(label, extra) {
    const row = document.createElement("div");
    row.className = "st-row";
    const head = document.createElement("div");
    head.className = "st-head";
    const name = document.createElement("span");
    name.className = "st-label";
    name.textContent = label;
    const pct = document.createElement("span");
    pct.className = "st-pct";
    head.appendChild(name);
    if (extra) {
      const st = document.createElement("span");
      st.className = "st-state";
      head.appendChild(st);
    }
    head.appendChild(pct);
    const track = document.createElement("div");
    track.className = "st-track";
    const fill = document.createElement("div");
    fill.className = "st-fill";
    track.appendChild(fill);
    row.appendChild(head);
    row.appendChild(track);
    return { row, pct, fill, state: extra ? head.querySelector(".st-state") : null };
  }

  const cpu = mkRow("CPU", false);
  const ram = mkRow("RAM", false);
  const bat = mkRow("Battery", true);
  rows.appendChild(cpu.row);
  rows.appendChild(ram.row);
  rows.appendChild(bat.row);

  function paint() {
    cpu.pct.textContent = Math.round(lastStats.cpu) + "%";
    cpu.fill.style.width = Math.min(100, Math.max(0, lastStats.cpu)) + "%";
    ram.pct.textContent = Math.round(lastStats.ram) + "%";
    ram.fill.style.width = Math.min(100, Math.max(0, lastStats.ram)) + "%";
    if (lastStats.battery == null) {
      bat.row.classList.add("hidden");
    } else {
      bat.row.classList.remove("hidden");
      bat.fill.style.width = Math.min(100, Math.max(0, lastStats.battery)) + "%";
      if (lastStats.charging) {
        bat.pct.textContent = "";
        if (bat.state) bat.state.textContent = "charging";
      } else {
        bat.pct.textContent = Math.round(lastStats.battery) + "%";
        if (bat.state) bat.state.textContent = "";
      }
    }
  }
  paint();

  return { rows, paint };
}

function paintStats() {
  for (const w of widgets.values()) {
    if (w.kind === "stats" && w.refs && w.refs.paint) w.refs.paint();
  }
}

listen("hushtop:stats", (e) => {
  lastStats = e.payload || lastStats;
  paintStats();
}).catch(console.error);

invoke("stats_now")
  .then((s) => {
    if (s) {
      lastStats = s;
      paintStats();
    }
  })
  .catch(() => {});

// ---- clipboard ----------------------------------------------------------

let clipList = []; // most recent first

function clipboardBody(r) {
  const list = document.createElement("div");
  list.className = "cb-list";

  function render() {
    list.replaceChildren();
    if (!clipList.length) {
      const empty = document.createElement("div");
      empty.className = "s-empty";
      empty.textContent = "nothing copied yet";
      list.appendChild(empty);
      return;
    }
    for (const t of clipList) {
      const tile = document.createElement("button");
      tile.className = "cb-tile";
      const prev = t.replace(/\s+/g, " ").trim().slice(0, 400);
      tile.textContent = prev;
      tile.addEventListener("click", () => {
        invoke("clipboard_copy", { text: t })
          .then(() => {
            tile.classList.add("copied");
            setTimeout(() => tile.classList.remove("copied"), 700);
          })
          .catch(() => {});
      });
      list.appendChild(tile);
    }
  }
  render();
  return { list, render };
}

function lockBody() {
  const btn = document.createElement("button");
  btn.className = "lock-btn";
  btn.setAttribute("aria-label", "Lock the desktop");
  const glyph = document.createElement("span");
  glyph.className = "lock-glyph";
  glyph.innerHTML = '<i class="shackle"></i><i class="body"></i><i class="keyhole"></i>';
  btn.appendChild(glyph);
  btn.addEventListener("click", () => {
    invoke("lockscreen_lock").catch(() => {});
  });
  return { lockBtn: btn };
}

const MEDIA_VIZ_BARS = 18;
const MEDIA_VIZ_MAX = 45; // px — band inner height minus its 8px top pad
// deterministic per-bar spread (0.8..1.0): 18 bars sample 12 native bands,
// so ~2 bars share a band — this spreads twins without faking energy.
const MEDIA_VIZ_SPREAD = Array.from(
  { length: MEDIA_VIZ_BARS },
  (_, i) => 0.8 + 0.2 * (((i * 7) % 5) / 4)
);

function mediaBody(el) {
  // ---- top zone: full-bleed cover + meta column ---------------------------
  const top = document.createElement("div");
  top.className = "media-top";

  const coverWrap = document.createElement("div");
  coverWrap.className = "media-cover-wrap";
  const cover = document.createElement("img");
  cover.className = "media-cover";
  cover.alt = "";
  cover.draggable = false;
  cover.hidden = true;
  coverWrap.appendChild(cover);

  const meta = document.createElement("div");
  meta.className = "media-meta";
  const title = document.createElement("div");
  title.className = "media-title";
  const artist = document.createElement("div");
  artist.className = "media-artist";
  const status = document.createElement("div");
  status.className = "media-status";
  const statusText = document.createElement("span");
  status.appendChild(statusText);
  meta.appendChild(title);
  meta.appendChild(artist);
  meta.appendChild(status);

  top.appendChild(coverWrap);
  top.appendChild(meta);

  // ---- band: transport + divider + the visualizer --------------------------
  const band = document.createElement("div");
  band.className = "media-band";

  const transport = document.createElement("div");
  transport.className = "media-transport";

  const prev = document.createElement("button");
  prev.className = "media-btn";
  prev.setAttribute("aria-label", "Previous track");
  prev.innerHTML =
    '<svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor" stroke="none"><path d="M6 5h2v14H6zM20 5v14L9 12z"/></svg>';

  const play = document.createElement("button");
  play.className = "media-btn play";
  play.setAttribute("aria-label", "Play or pause");
  const PLAY_P =
    '<svg viewBox="0 0 24 24" width="17" height="17" fill="currentColor" stroke="none"><path d="M8 5.2v13.6L19 12z"/></svg>';
  const PAUSE_P =
    '<svg viewBox="0 0 24 24" width="17" height="17" fill="currentColor" stroke="none"><path d="M7 5h3.4v14H7zM13.6 5H17v14h-3.4z"/></svg>';
  play.innerHTML = PLAY_P;

  const next = document.createElement("button");
  next.className = "media-btn";
  next.setAttribute("aria-label", "Next track");
  next.innerHTML =
    '<svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor" stroke="none"><path d="M16 5h2v14h-2zM4 5l11 7L4 19z"/></svg>';

  transport.appendChild(prev);
  transport.appendChild(play);
  transport.appendChild(next);

  const sep = document.createElement("div");
  sep.className = "media-viz-sep";

  const viz = document.createElement("div");
  viz.className = "media-viz";
  const vizBars = [];
  for (let i = 0; i < MEDIA_VIZ_BARS; i++) {
    const b = document.createElement("i");
    viz.appendChild(b);
    vizBars.push(b);
  }

  band.appendChild(transport);
  band.appendChild(sep);
  band.appendChild(viz);

  // ---- idle: the designed empty card ---------------------------------------
  const empty = document.createElement("div");
  empty.className = "media-empty";
  const emptyInner = document.createElement("div");
  emptyInner.className = "media-empty-inner";
  const emptyDisc = document.createElement("div");
  emptyDisc.className = "media-empty-disc";
  emptyDisc.innerHTML =
    '<svg viewBox="0 0 24 24" width="24" height="24" fill="currentColor" stroke="none">' +
    '<path d="M9 18.5a2.6 2.6 0 1 1-1.7-2.45V6.2a1 1 0 0 1 .76-.97l8-2a1 1 0 0 1 1.24.97v10.8a2.6 2.6 0 1 1-1.7-2.45V7.28l-6 1.5z"/></svg>';
  const emptyTitle = document.createElement("div");
  emptyTitle.className = "media-empty-title";
  emptyTitle.textContent = "NOTHING PLAYING";
  emptyInner.appendChild(emptyDisc);
  emptyInner.appendChild(emptyTitle);
  empty.appendChild(emptyInner);

  const act = (a) => invoke("media_ctl", { action: a }).catch(() => {});
  prev.addEventListener("click", () => act("prev"));
  play.addEventListener("click", () => act("toggle"));
  next.addEventListener("click", () => act("next"));

  const lastH = new Array(MEDIA_VIZ_BARS).fill(3);
  function paintViz(levels) {
    for (let i = 0; i < MEDIA_VIZ_BARS; i++) {
      const src = Math.min(11, Math.floor((i * 12) / MEDIA_VIZ_BARS));
      const lvl = Math.max(0, Math.min(1, (levels && levels[src]) || 0));
      const h = Math.max(3, Math.round(lvl * MEDIA_VIZ_SPREAD[i] * MEDIA_VIZ_MAX));
      if (h !== lastH[i]) {
        lastH[i] = h;
        vizBars[i].style.height = h + "px";
      }
    }
  }

  function paint(m) {
    const has = !!(m && (m.title || m.artist || m.cover));
    empty.style.display = has ? "none" : "";
    top.style.display = has ? "" : "none";
    band.style.display = has ? "" : "none";
    el.classList.toggle("playing", !!(m && m.playing));
    if (!has) return;
    title.textContent = m.title || "";
    artist.textContent = m.artist || "";
    statusText.textContent = "PAUSED";
    status.style.display = m.playing ? "none" : "";
    if (m.cover) {
      cover.src = "data:image/png;base64," + m.cover;
      cover.hidden = false;
    } else {
      cover.hidden = true;
      cover.removeAttribute("src");
    }
    play.innerHTML = m.playing ? PAUSE_P : PLAY_P;
  }

  return {
    top, band, empty, coverWrap, cover, title, artist, status,
    play, prev, next, viz, vizBars, paint, paintViz,
  };
}

// ---- widget factory + diff render ---------------------------------------

function makeWidget(w) {
  const el = document.createElement("div");
  el.className = "notepad widget " + w.kind;
  placeByCell(el, w.col, w.row);
  el.style.width = w.w + "px";
  el.style.height = w.h + "px";

  const head = makeGrip(() => {
    invoke("hushtop_widget_del", { id: w.id }).catch(() => {});
  });
  el.appendChild(head);

  const refs = { };
  if (w.kind === "volume") {
    // refs itself is the `r` volumeBody closes over: r.id, r.fit and r.poll
    // all live on the SAME object that lands in the widgets map — the fit
    // hook works from the widget's own events and the poll is cleanable.
    refs.id = w.id;
    Object.assign(refs, volumeBody(refs));
    el.appendChild(refs.readout);
    el.appendChild(refs.slider);
    el.appendChild(refs.ruler);
    el.appendChild(refs.foot);
    el.appendChild(refs.sessions);
  } else if (w.kind === "clipboard") {
    Object.assign(refs, clipboardBody(w));
    el.appendChild(refs.list);
    const clearBtn = document.createElement("button");
    clearBtn.className = "notepad-clear";
    clearBtn.setAttribute("aria-label", "Clear clipboard history");
    clearBtn.addEventListener("click", () => {
      invoke("clipboard_clear").catch(() => {});
    });
    head.insertBefore(clearBtn, head.lastChild);
  } else if (w.kind === "stats") {
    Object.assign(refs, statsBody());
    el.appendChild(refs.rows);
  } else if (w.kind === "lock") {
    Object.assign(refs, lockBody());
    el.appendChild(refs.lockBtn);
  } else if (w.kind === "media") {
    Object.assign(refs, mediaBody(el));
    el.appendChild(refs.top);
    el.appendChild(refs.band);
    el.appendChild(refs.empty);
  }

  // drag by the head (buttons/textareas excluded inside the drag guard)
  widgetsEl.appendChild(el);

  // drag by the head — buttons/inputs opt out of the capture (the same
  // guard that keeps the X button clickable). Grid 2.0: the card follows
  // the cursor raw while the landing frame + guides show the cadence cell;
  // the drop saves the CELL the card settled on (the layout map).
  wireCardDrag(el, (c, r) => {
    w.col = c; // keep the record current — geom saves commit rec.col/row
    w.row = r;
    invoke("hushtop_widget_save", { id: w.id, col: c, row: r }).catch(() => {});
  });

  const entry = { el, kind: w.kind, refs, rec: w };
  if (w.kind === "volume") {
    // the adaptive fit is wired through refs so volumeBody's own events
    // (mixer toggle, poll re-render) can re-run it once the entry exists
    refs.fit = () => fitVolume(entry, true);
    fitVolume(entry, false, false); // boot snap is display-only — never persist a boot measurement
  }
  return entry;
}

function renderWidgets(list) {
  const seen = new Set();
  for (const w of list) {
    if (w.kind === "media" && (w.w !== 280 || w.h !== 156)) {
      w.w = 280;
      w.h = 156;
    }
    seen.add(w.id);
    const e = widgets.get(w.id);
    if (e) {
      e.rec = w; // keep the record fresh — geom saves commit rec.col/row
      placeByCell(e.el, w.col, w.row);
      e.el.style.width = w.w + "px";
      e.el.style.height = w.h + "px";
    } else {
      widgets.set(w.id, makeWidget(w));
    }
  }
  for (const [id, e] of [...widgets]) {
    if (!seen.has(id)) {
      if (e.refs && e.refs.poll) clearInterval(e.refs.poll);
      e.el.remove();
      widgets.delete(id);
      volOpenIds.delete(id);
    }
  }
}

listen("hushtop:widgets", (e) => renderWidgets(e.payload)).catch(console.error);

// No resize handler, no viewport fetch: placement is a pure map→pixel
// function (cell × cadence, anchored at 14px), so it is identical at any
// window size — boot, monitor change, resolution shrink. Nothing to
// re-apply, nothing to clamp; the map always renders as stored.

listen("hushtop:clipboard", (e) => {
  clipList = e.payload || [];
  for (const [id, w] of widgets) {
    if (w.kind === "clipboard") w.refs.render();
  }
}).catch(console.error);

invoke("clipboard_history")
  .then((h) => {
    clipList = h || [];
    for (const [id, w] of widgets) {
      if (w.kind === "clipboard") w.refs.render();
    }
  })
  .catch(() => {});

// One hush:media broadcast repaints every media widget on the desktop
// (cover icon, title/artist, transport state, accent). The last payload is
// kept so a hush:levels tick can tint the visualizer with the SAME accent
// even between media events.
let lastMedia = null;
function paintMedia(m) {
  lastMedia = m || lastMedia;
  for (const w of widgets.values()) {
    if (w.kind === "media" && w.refs && w.refs.paint) w.refs.paint(m);
  }
}
listen("hush:media", (e) => paintMedia(e.payload)).catch(console.error);
invoke("media_now")
  .then((m) => paintMedia(m))
  .catch(() => {});

listen("hush:levels", (e) => {
  const v = e.payload || [];
  for (const w of widgets.values()) {
    if (w.kind === "media" && w.refs && w.refs.paintViz) {
      w.refs.paintViz(v);
    }
  }
}).catch(console.error);
