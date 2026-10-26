const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const CELL_W = 92;
const CELL_H = 72;
const MARGIN = 14;
const CARD_GRID = 36;
const CARD_GRID_Y = 18;
const CARD_ANCHOR = 14;

const iconsEl = document.getElementById("icons");
const menuEl = document.getElementById("menu");
const sortMenuEl = document.getElementById("sort-menu");
const paperEl = document.getElementById("paper");
const notepadsEl = document.getElementById("notepads");
const widgetsEl = document.getElementById("widgets");
const iconsNextEl = document.getElementById("icons-next");
const wipeLineEl = document.getElementById("wipe-line");
const marqueeEl = document.getElementById("marquee");
const ghostEl = document.getElementById("ghost");

// ---- the spaces view (single payload = single truth) ----------------------

let view = null; // last hushtop:spaces payload
const cells = new Map(); // "space:name" -> { el, img, label, item, space }
let renderedActive = 0; // the space painted in the main layers (single mode)

function regionOf(space) {
  if (view && view.dual && view.monitors[space]) {
    return view.monitors[space];
  }
  // single display: the viewport IS the region (one monitor spans it all)
  if (!view || !view.monitors.length) {
    return { x: 0, y: 0, w: innerWidth, h: innerHeight };
  }
  let l = Infinity, t = Infinity, r = -Infinity, b = -Infinity;
  for (const m of view.monitors) {
    l = Math.min(l, m.x); t = Math.min(t, m.y);
    r = Math.max(r, m.x + m.w); b = Math.max(b, m.y + m.h);
  }
  return { x: l, y: t, w: r - l, h: b - t };
}

function spaceAtPoint(x, y) {
  if (view && view.dual) {
    for (let i = 0; i < Math.min(2, view.monitors.length); i++) {
      const m = view.monitors[i];
      if (x >= m.x && x < m.x + m.w && y >= m.y && y < m.y + m.h) return i;
    }
    return view.active || 0;
  }
  return (view && view.active) || 0;
}

function visibleSpaces() {
  if (view && view.dual) return [0, 1];
  return [renderedActive];
}

function placeByCell(el, c, r, space) {
  const reg = regionOf(space);
  el.style.left = reg.x + CARD_ANCHOR + c * CARD_GRID + "px";
  el.style.top = reg.y + CARD_ANCHOR + r * CARD_GRID_Y + "px";
}

function cardCell(x, y, space) {
  const reg = regionOf(space);
  return {
    c: Math.round((x - reg.x - CARD_ANCHOR) / CARD_GRID),
    r: Math.round((y - reg.y - CARD_ANCHOR) / CARD_GRID_Y),
  };
}

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

// ---------- selection -------------------------------------------------------

let selSet = new Set(); // selected icon names

function paintSelection() {
  for (const [, entry] of cells) {
    entry.el.classList.toggle("selected", selSet.has(entry.item.name));
  }
}

function setSelection(names) {
  selSet = new Set(names);
  paintSelection();
}

// ---------- rendering (diff, never rebuild) ---------------------------------

const cellKey = (space, name) => space + ":" + name;

function makeCell(item, space) {
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
  return { el, img, label, item, space };
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

function renderIcons() {
  if (!view) return;
  const want = new Set();
  for (const space of visibleSpaces()) {
    for (const item of view.items[space] || []) {
      want.add(cellKey(space, item.name));
      const key = cellKey(space, item.name);
      const entry = cells.get(key);
      if (entry) {
        paintCell(entry, item);
      } else {
        const e = makeCell(item, space);
        paintCell(e, item);
        cells.set(key, e);
      }
    }
  }
  for (const [key, entry] of [...cells]) {
    if (!want.has(key)) {
      entry.el.remove();
      cells.delete(key);
      selSet.delete(entry.item.name);
    }
  }
  paintSelection();
}

// ---------- icon drag (movable, config-persisted, desktop untouched) -------

function gridSize(space) {
  const reg = regionOf(space);
  const cols = Math.max(1, Math.floor((reg.w - MARGIN * 2) / CELL_W));
  const rows = Math.max(1, Math.floor((reg.h - MARGIN * 2) / CELL_H));
  return { cols, rows };
}

const ck = (c, r) => c + "," + r;

// Cells currently owned by OTHER icons OF THE SAME SPACE, in that space's
// region-relative coordinates.
function occupiedCells(space, exceptNames) {
  const taken = new Set();
  const reg = regionOf(space);
  const { cols, rows } = gridSize(space);
  for (const [, entry] of cells) {
    if (entry.space !== space || exceptNames.has(entry.item.name)) continue;
    taken.add(ck(
      Math.min(cols - 1, Math.max(0, Math.round((entry.item.x - reg.x - MARGIN) / CELL_W))),
      Math.min(rows - 1, Math.max(0, Math.round((entry.item.y - reg.y - MARGIN) / CELL_H)))
    ));
  }
  return taken;
}

// Nearest free cell — the JS twin of hushtop.rs nearest_free_cell
// (expanding ring, ties toward smaller column then row). Running it here,
// BEFORE the drop lands, means a drop onto an occupied cell rehomes
// INSTANTLY with no IPC roundtrip and no visible stacking; the native
// cmd_move re-checks and stays the enforcer for races.
function nearestFreeCell(taken, cols, rows, c0, r0) {
  const inGrid = (c, r) => c >= 0 && r >= 0 && c < cols && r < rows;
  if (inGrid(c0, r0) && !taken.has(ck(c0, r0))) return [c0, r0];
  const maxR = Math.max(rows, cols);
  for (let rad = 1; rad <= maxR; rad++) {
    let best = null;
    for (let dc = -rad; dc <= rad; dc++) {
      for (let dr = -rad; dr <= rad; dr++) {
        if (Math.abs(dc) !== rad && Math.abs(dr) !== rad) continue;
        const c = c0 + dc;
        const r = r0 + dr;
        if (!inGrid(c, r) || taken.has(ck(c, r))) continue;
        const dist = Math.abs(dc) + Math.abs(dr);
        if (!best || dist < best[0] || (dist === best[0] && (c < best[1] || (c === best[1] && r < best[2])))) {
          best = [dist, c, r];
        }
      }
    }
    if (best) return [best[1], best[2]];
  }
  return [c0, r0]; // grid full — native decides
}

let oleDrag = null; // { name, space } — the icon handed to the native OLE drag

function wireCell(el, img, label) {
  let drag = null;

  el.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    hideMenu();
    const name = el.dataset.name;
    const entry = [...cells.values()].find((c) => c.el === el);
    const space = entry ? entry.space : 0;
    if (e.ctrlKey) {
      // toggle membership, no drag starts from a ctrl-click
      if (selSet.has(name)) selSet.delete(name);
      else selSet.add(name);
      paintSelection();
      return;
    }
    if (!selSet.has(name)) setSelection([name]);
    const names = [...selSet];
    const multi = names.length > 1;
    const parts = names
      .map((n) => {
        for (const s of visibleSpaces()) {
          const c = cells.get(cellKey(s, n));
          if (c) return { name: n, el: c.el, space: c.space, ax: e.clientX - c.el.offsetLeft, ay: e.clientY - c.el.offsetTop };
        }
        return null;
      })
      .filter(Boolean);
    drag = {
      names,
      multi,
      space,
      parts,
      dx: e.clientX - el.offsetLeft,
      dy: e.clientY - el.offsetTop,
      moved: false,
      handed: false,
    };
    el.setPointerCapture(e.pointerId);
  });

  el.addEventListener("pointermove", (e) => {
    if (!drag) return;
    // The OLE modal loop owns the release and can swallow the pointerup —
    // a drag still alive with no held button is a ghost. Kill it on the
    // first buttonless move instead of hover-dragging the icon around.
    if (!(e.buttons & 1)) {
      for (const p of drag.parts) p.el.classList.remove("dragging");
      drag = null;
      return;
    }
    const lead = drag.parts.find((p) => p.el === el) || drag.parts[0];
    if (!drag.moved && (Math.abs(e.clientX - lead.ax - lead.el.offsetLeft) > 2 || Math.abs(e.clientY - lead.ay - lead.el.offsetTop) > 2)) {
      drag.moved = true;
      for (const p of drag.parts) p.el.classList.add("dragging");
      // ONE icon only hands the drag to the native OLE pipeline (drag to
      // Explorer windows etc.). A multi-selection moves inside the desktop.
      // The handoff does NOT stop the JS follow: the icon keeps tracking
      // the cursor right here — the native drag_move stream (when it
      // delivers) writes the exact same position (cursor minus grab
      // offset), and the drop is re-homed by the boomerang Drop either
      // way. Movement must never depend on the stream arriving.
      if (!drag.multi && !drag.handed) {
        drag.handed = true;
        oleDrag = { name: lead.name, space: drag.space };
        invoke("hushtop_dragout", { name: lead.name, dx: lead.ax, dy: lead.ay }).catch(() => {});
      }
    }
    if (!drag.moved) return;
    for (const p of drag.parts) {
      p.el.style.left = Math.round(e.clientX - p.ax) + "px";
      p.el.style.top = Math.round(e.clientY - p.ay) + "px";
    }
  });

  const finish = async (commit) => {
    if (!drag) return;
    const d = drag;
    drag = null;
    for (const p of d.parts) p.el.classList.remove("dragging");
    if (!d.moved) return; // plain click — nothing to save
    if (!commit) return;
    // A handed drag settles HERE too: the boomerang Drop re-homes the icon
    // through cmd_move with the same top-left cell, and a double commit
    // lands on that same cell — but when the native side never started
    // (drag_out refused, OLE dead), this is the only thing that saves the
    // position at all. Movement and persistence must not hinge on OLE.
    // Free placing: every icon lands EXACTLY on the cell it was dropped on
    // (collision -> nearest free, one motion, no re-sorting of anything else).
    const drops = d.parts.map((p) => {
      const px = p.el.offsetLeft;
      const py = p.el.offsetTop;
      const space = spaceAtPoint(px, py);
      const reg = regionOf(space);
      const { cols, rows } = gridSize(space);
      const c0 = Math.min(cols - 1, Math.max(0, Math.round((px - reg.x - MARGIN) / CELL_W)));
      const r0 = Math.min(rows - 1, Math.max(0, Math.round((py - reg.y - MARGIN) / CELL_H)));
      return { p, space, c0, r0 };
    });
    // resolve collisions among the moving set, per space, first-come wins
    const takenBySpace = new Map();
    const settled = new Map(); // name -> {space, c, r}
    for (const d0 of drops) {
      if (!takenBySpace.has(d0.space)) {
        takenBySpace.set(d0.space, occupiedCells(d0.space, new Set(d.names)));
      }
      const taken = takenBySpace.get(d0.space);
      const { cols, rows } = gridSize(d0.space);
      const [fc, fr] = nearestFreeCell(taken, cols, rows, d0.c0, d0.r0);
      taken.add(ck(fc, fr));
      settled.set(d0.p.name, { space: d0.space, c: fc, r: fr });
      const reg = regionOf(d0.space);
      d0.p.el.style.left = reg.x + MARGIN + fc * CELL_W + "px";
      d0.p.el.style.top = reg.y + MARGIN + fr * CELL_H + "px";
    }
    // commit sequentially — the native side re-checks occupancy per drop
    for (const [name, s] of settled) {
      const reg = regionOf(s.space);
      const gx = reg.x + MARGIN + s.c * CELL_W;
      const gy = reg.y + MARGIN + s.r * CELL_H;
      await invoke("hushtop_move", { name, x: gx, y: gy }).catch(() => {});
    }
  };

  el.addEventListener("pointerup", () => { finish(true); });
  el.addEventListener("pointercancel", () => { finish(false); });

  el.addEventListener("dblclick", () => {
    invoke("hushtop_open", { name: el.dataset.name }).catch(() => toast("Could not open"));
  });

  el.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    e.stopPropagation();
    const name = el.dataset.name;
    const many = selSet.size > 1 && selSet.has(name);
    if (!selSet.has(name)) setSelection([name]);
    const targets = many ? [...selSet] : [name];
    const base = [];
    if (many) {
      base.push({
        t: "Open",
        a: () => {
          for (const n of targets) invoke("hushtop_open", { name: n }).catch(() => {});
        },
      });
    } else {
      base.push({ t: "Open", a: () => invoke("hushtop_open", { name }).catch(() => toast("Could not open")) });
      base.push({ t: "Rename", a: () => invoke("hushtop_prompt_show", { mode: "rename", name }).catch(() => {}) });
    }
    // Icons are PER-DESKTOP: hand the selection to the other one (single
    // display only — with two monitors a drag across the boundary already
    // moves icons via the drop point).
    if (view && !view.dual) {
      const other = (view.active || 0) === 0 ? 1 : 0;
      base.push({
        t: "Move to desktop " + (other + 1),
        a: () =>
          invoke("hushtop_move_space", { names: targets, target: other }).catch(() =>
            toast("Could not move")
          ),
      });
    }
    const tail = [{ sep: true }];
    tail.push({
      t: many ? "Delete selected" : "Delete",
      a: () => invoke("hushtop_delete_many", { names: targets }).catch(() => toast("Could not delete")),
    });
    if (!many) {
      invoke("startmenu_is_pinned", { name })
        .then((pinned) => {
          const pinRow = pinned
            ? { t: "Unpin from start", a: () => invoke("startmenu_unpin", { target: name }).catch(() => {}) }
            : { t: "Pin to start", a: () => invoke("startmenu_pin", { target: name }).catch(() => {}) };
          showMenu(e.clientX, e.clientY, [...base, pinRow, ...tail]);
        })
        .catch(() => showMenu(e.clientX, e.clientY, [...base, ...tail]));
    } else {
      showMenu(e.clientX, e.clientY, [...base, ...tail]);
    }
  });
}

// ---------- context menu ----------------------------------------------------

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

// ---------- the sort menu (uiexample paper, per-space, explicit only) -------

function showSortMenu(x, y) {
  const space = spaceAtPoint(x, y);
  sortMenuEl.replaceChildren();
  const items = [
    { t: "By name", m: "name" },
    { t: "By date", m: "date" },
    { t: "By type", m: "type" },
  ];
  for (const it of items) {
    const mi = document.createElement("div");
    mi.className = "mi";
    mi.textContent = it.t;
    mi.addEventListener("click", () => {
      hideSortMenu();
      invoke("hushtop_sort", { space, mode: it.m }).catch(() => {});
    });
    sortMenuEl.appendChild(mi);
  }
  sortMenuEl.classList.remove("hidden");
  sortMenuEl.style.left = Math.max(2, Math.min(x, innerWidth - sortMenuEl.offsetWidth - 4)) + "px";
  sortMenuEl.style.top = Math.max(2, Math.min(y, innerHeight - sortMenuEl.offsetHeight - 4)) + "px";
}

function hideSortMenu() {
  sortMenuEl.classList.add("hidden");
}

// The sort menu answers only to its own two closers — an outside press or a
// choice. Everything else (menus closing, foreground moves, drags) leaves it
// strictly alone.
document.addEventListener("pointerdown", (e) => {
  if (sortMenuEl.classList.contains("hidden")) return;
  if (sortMenuEl.contains(e.target)) return;
  hideSortMenu();
});

paperEl.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  hideMenu();
  showMenu(e.clientX, e.clientY, [
    { t: "Refresh", a: () => invoke("hushtop_refresh").catch(() => {}) },
    { t: "Sort", a: () => showSortMenu(e.clientX, e.clientY) },
    { sep: true },
    { t: "New folder", a: () => invoke("hushtop_prompt_show", { mode: "newfolder", name: "" }).catch(() => {}) },
    { t: "New file", a: () => invoke("hushtop_prompt_show", { mode: "newfile", name: "" }).catch(() => {}) },
  ]);
});

// ---------- marquee selection (hold LMB on the paper, sweep, release) -------

let marquee = null;

paperEl.addEventListener("pointerdown", (e) => {
  if (e.button !== 0) return;
  if (e.target !== paperEl && e.target !== iconsEl) return;
  hideMenu();
  invoke("startmenu_hide").catch(() => {});
  marquee = { x0: e.clientX, y0: e.clientY, moved: false };
  marqueeEl.style.left = e.clientX + "px";
  marqueeEl.style.top = e.clientY + "px";
  marqueeEl.style.width = "0px";
  marqueeEl.style.height = "0px";
  paperEl.setPointerCapture(e.pointerId);
});

paperEl.addEventListener("pointermove", (e) => {
  if (!marquee) return;
  const x = Math.min(marquee.x0, e.clientX);
  const y = Math.min(marquee.y0, e.clientY);
  const w = Math.abs(e.clientX - marquee.x0);
  const h = Math.abs(e.clientY - marquee.y0);
  if (!marquee.moved && (w > 3 || h > 3)) {
    marquee.moved = true;
    marqueeEl.classList.add("on");
  }
  if (!marquee.moved) return;
  marqueeEl.style.left = x + "px";
  marqueeEl.style.top = y + "px";
  marqueeEl.style.width = w + "px";
  marqueeEl.style.height = h + "px";
  const rx = x + w, ry = y + h;
  const hit = new Set();
  for (const [, entry] of cells) {
    const el = entry.el;
    const l = el.offsetLeft, t = el.offsetTop;
    if (l < rx && l + el.offsetWidth > x && t < ry && t + el.offsetHeight > y) {
      hit.add(entry.item.name);
    }
  }
  setSelection(hit);
});

const marqueeEnd = (commit) => {
  if (!marquee) return;
  const m = marquee;
  marquee = null;
  marqueeEl.classList.remove("on");
  if (!commit) return;
  if (!m.moved) setSelection([]); // a plain paper click clears the selection
};

paperEl.addEventListener("pointerup", () => marqueeEnd(true));
paperEl.addEventListener("pointercancel", () => marqueeEnd(false));

// bulk delete — the Delete key clears every selected icon to the bin
document.addEventListener("keydown", (e) => {
  if (e.key === "Delete" && selSet.size > 0 && !e.target.closest("textarea, input")) {
    invoke("hushtop_delete_many", { names: [...selSet] }).catch(() => toast("Could not delete"));
  } else if (e.key === "Escape") {
    hideSortMenu();
    hideMenu();
    setSelection([]);
  }
});

// heartbeat-driven close (foreground moved to another app) — the regular
// menu only; the sort menu keeps its own counsel
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

// ---------- events from the backend -----------------------------------------

let menuOpenFor = null; // null = closed, "open" = a menu is up

// THE desktop payload: regions, both spaces' icons, both spaces' cards,
// the active space and the effect. Everything renders from this.
listen("hushtop:spaces", (e) => onSpaces(e.payload)).catch(console.error);

// Card-only updates (add/save/del of widgets + notepads).
listen("hushtop:cards", (e) => {
  if (!view) return;
  view.spaces = (e.payload && e.payload.spaces) || view.spaces;
  renderNotepads();
  renderWidgets();
}).catch(console.error);

listen("hushtop:effect", (e) => {
  // A pick from the effects widget — this IS the user's intent, so it is
  // remembered even while suspended: the unlock restore replays exactly
  // this, never a value a stray payload could have dressed the sheet with.
  fxUserKind = desktopFxKind(e.payload || "none");
  fxSet(fxUserKind);
}).catch(console.error);

listen("hushtop:drag_move", (e) => {
  const p = e.payload || {};
  const key = oleDrag ? cellKey(oleDrag.space, p.name) : null;
  const entry = (key && cells.get(key)) || [...cells.values()].find((c) => c.item.name === p.name);
  if (!entry) return;
  entry.el.classList.add("dragging");
  entry.el.style.left = p.x + "px";
  entry.el.style.top = p.y + "px";
});

listen("hushtop:drag_ended", (e) => {
  const name = typeof e.payload === "string" ? e.payload : e.payload && e.payload.name;
  for (const [, entry] of cells) {
    if (entry.item.name === name) entry.el.classList.remove("dragging");
  }
  oleDrag = null;
});

// startup sync in case the first emit beat us to it
(async () => {
  try {
    const st = await invoke("sync_state");
    if (st.spaces) onSpaces(st.spaces);
  } catch (e) {
    console.error(e);
  }
})();

// ---------- the spaces payload -> everything --------------------------------

const spacesUiHooks = new Set(); // widgets that mirror the spaces state

// one-liner into the shell log (proof when something misbehaves on a rig
// we cannot reproduce)
function ulog(msg) {
  invoke("hushtop_ui_log", { line: msg }).catch(() => {});
}

function onSpaces(dto) {
  if (!dto) return;
  const prevActive = view ? (view.dual ? renderedActive : view.active) : null;
  view = dto;
  const switchedSingle =
    !dto.dual &&
    prevActive !== null &&
    dto.active !== prevActive;
  renderedActive = dto.active;
  fxUserKind = desktopFxKind(dto.effect);
  fxSet(fxUserKind);
  if (switchedSingle) ulog("spaces: switch -> space " + (dto.active + 1) + " (wipe)");
  for (const hook of spacesUiHooks) {
    try { hook(dto); } catch (err) { /* a dead widget never blocks the rest */ }
  }
  if (switchedSingle) {
    playWipe(dto, prevActive);
  } else {
    renderAll();
  }
}

function renderAll() {
  renderIcons();
  renderNotepads();
  renderWidgets();
  playBoot(); // first real render = the desktop exists — start the boot reveal
}

// ---------- the space-switch wipe (a line sweeps right -> left) -------------

let wipeBusy = false;

function playWipe(dto, fromSpace) {
  if (wipeBusy) { renderAll(); return; }
  wipeBusy = true;
  const incoming = dto.active;
  // The incoming space paints into its own layer: icons + static previews of
  // its cards, positioned exactly like the real render will place them.
  iconsNextEl.replaceChildren();
  for (const item of dto.items[incoming] || []) {
    const el = document.createElement("div");
    el.className = "cell";
    const img = document.createElement("img");
    img.src = item.icon ? `data:image/png;base64,${item.icon}` : PLACEHOLDER;
    img.draggable = false;
    const label = document.createElement("div");
    label.className = "label";
    label.textContent = displayName(item.name, item.kind);
    el.appendChild(img);
    el.appendChild(label);
    el.style.left = item.x + "px";
    el.style.top = item.y + "px";
    iconsNextEl.appendChild(el);
  }
  const cards = (dto.spaces[incoming] || { widgets: [], notepads: [] });
  for (const w of cards.widgets) iconsNextEl.appendChild(staticCard(w, incoming, w.kind));
  for (const n of cards.notepads) iconsNextEl.appendChild(staticCard(n, incoming, "notes"));

  iconsNextEl.style.clipPath = "inset(0 0 0 100%)";
  wipeLineEl.style.left = "100%";
  iconsNextEl.classList.add("on");
  wipeLineEl.classList.add("on");

  const dur = 650;
  const ease = "cubic-bezier(0.33, 0.9, 0.25, 1)";
  const anim = iconsNextEl.animate(
    [
      { clipPath: "inset(0 0 0 100%)" },
      { clipPath: "inset(0 0 0 0%)" },
    ],
    { duration: dur, easing: ease }
  );
  wipeLineEl.animate(
    [
      { left: "100%" },
      { left: "0%" },
    ],
    { duration: dur, easing: ease }
  );
  // The OUTGOING desktop leaves WITH the bar: right of the line the incoming
  // space is already visible, left of it the old one lingers — the bar
  // itself erases the old icons as it passes. (Only the incoming layer used
  // to animate, so on a switch to a mostly-empty desktop the old icons sat
  // untouched until the final swap, then "just disappeared" — no sweep.)
  // fill:forwards holds the final clip; cancel() below drops it the same
  // tick renderAll repaints the new space — no last-frame flash.
  const outgoing = [iconsEl, notepadsEl, widgetsEl].map((el) =>
    el.animate(
      [
        { clipPath: "inset(0 0% 0 0)" },
        { clipPath: "inset(0 100% 0 0)" },
      ],
      { duration: dur, easing: ease, fill: "forwards" }
    )
  );
  anim.onfinish = () => {
    for (const a of outgoing) a.cancel();
    iconsNextEl.classList.remove("on");
    wipeLineEl.classList.remove("on");
    iconsNextEl.replaceChildren();
    wipeBusy = false;
    renderedActive = incoming;
    renderAll();
  };
}

// A preview box that LOOKS like the real card for the length of the wipe.
function staticCard(rec, space, kind) {
  const el = document.createElement("div");
  el.className = "notepad widget " + kind;
  // CARD cadence — placeByCell is the 92x72 icon grid and threw every wipe
  // preview up to ~600px away from where the real card will land.
  placeByCard(el, rec.col, rec.row, space);
  el.style.width = rec.w + "px";
  el.style.height = rec.h + "px";
  const head = document.createElement("div");
  head.className = "grip";
  const dots = document.createElement("span");
  dots.className = "grip-dots";
  for (let i = 0; i < 3; i++) dots.appendChild(document.createElement("i"));
  head.appendChild(dots);
  el.appendChild(head);
  // the REAL widget body — previews are the widget, not a drawing of it
  el.appendChild(hushWidgetPreview(kind));
  return el;
}

// ---------- notepads ---------------------------------------------------------

const npEl = new Map(); // id -> { el, ta, space }
const npSaveTimers = {};

// ---- card snapping: the ruled sheet (Grid 2.0) ----------------------------
// The landing math is the paper cadence: cards settle on a rhythm anchored
// at the 14px margin (the same one as the horizontal rules and the icon
// grid). Horizontal stays on the 36px rule; the vertical step is 18px.
function cardMagnet(v, axis, space) {
  const reg = regionOf(space);
  const grid = axis === "x" ? CARD_GRID : CARD_GRID_Y;
  const anchor = (axis === "x" ? reg.x : reg.y) + CARD_ANCHOR;
  return Math.round(anchor + Math.round((v - anchor) / grid) * grid);
}

function cellPx(c, r, space) {
  const reg = regionOf(space);
  return { x: reg.x + CARD_ANCHOR + c * CARD_GRID, y: reg.y + CARD_ANCHOR + r * CARD_GRID_Y };
}

// NO out-of-bounds protection — by design. The map is the single truth:
// a card renders EXACTLY where its cell says, even if that cell sits past
// the screen border.
function placeByCard(el, c, r, space) {
  const p = cellPx(c, r, space);
  el.style.left = p.x + "px";
  el.style.top = p.y + "px";
}

// the drag chrome: one landing frame + alignment guides
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

// Same frame from KNOWN dimensions — the ghost stream runs at 25 Hz and must
// never force a layout (offsetWidth) per event.
function showLandingSized(x, y, w, h) {
  gridLand.style.display = "block";
  gridLand.style.left = x + "px";
  gridLand.style.top = y + "px";
  gridLand.style.width = w + "px";
  gridLand.style.height = h + "px";
}

// alignment lines for the landing rect: sibling edges that coincide with
// the landing's left/right (vertical guides) or top/bottom (horizontal
// guides), plus the paper margins. Informational only — nothing pulls.
const GUIDE_TOL = 1;
function guideLines(land, el) {
  const L = land.x, T = land.y;
  const R = land.x + el.offsetWidth, B = land.y + el.offsetHeight;
  const xs = new Map();
  const ys = new Map();
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
  const reg0 = regionOf(0);
  if (near(L, reg0.x + MARGIN)) addX(L, T, B);
  if (near(R, reg0.x + reg0.w - MARGIN)) addX(R, T, B);
  if (near(T, reg0.y + MARGIN)) addY(T, L, R);
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
function wireCardDrag(el, space, onDrop) {
  let drag = null;
  el.addEventListener("pointerdown", (e) => {
    if (e.target.closest("textarea, button, input")) return;
    el.classList.remove("settle"); // a fresh grab interrupts any glide
    const r = el.getBoundingClientRect();
    drag = { dx: e.clientX - r.left, dy: e.clientY - r.top, moved: false, land: null, space };
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
    drag.land = { x: cardMagnet(x, "x", drag.space), y: cardMagnet(y, "y", drag.space) };
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
      const cell = cardCell(p.x, p.y, d.space);
      if (!d.land) {
        // Alt free-drop: the map keeps the nearest CELL, so settle the card
        // onto that cell's pixels too — what you see must be what the map
        // keeps, or the next boot moves the card by up to half a cell
        const q = cellPx(cell.c, cell.r, d.space);
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

function makeNotepad(n, space) {
  const el = document.createElement("div");
  el.className = "notepad";
  placeByCard(el, n.col, n.row, space);
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

  wireCardDrag(el, space, () => saveNotepad(n.id, el, ta, space));

  ta.addEventListener("input", () => {
    clearTimeout(npSaveTimers[n.id]);
    npSaveTimers[n.id] = setTimeout(() => saveNotepad(n.id, el, ta, space), 600);
  });

  return { el, ta, space };
}

function saveNotepad(id, el, ta, space) {
  const cell = cardCell(el.offsetLeft, el.offsetTop, space);
  const sw = Math.min(720, Math.max(252, Math.ceil(el.offsetWidth / CARD_GRID) * CARD_GRID));
  const sh = Math.min(540, Math.max(144, Math.ceil(el.offsetHeight / CARD_GRID_Y) * CARD_GRID_Y));
  if (sw !== el.offsetWidth) el.style.width = sw + "px";
  if (sh !== el.offsetHeight) el.style.height = sh + "px";
  invoke("hushtop_notepad_save", {
    id,
    col: cell.c,
    row: cell.r,
    w: sw,
    h: sh,
    text: ta.value,
  }).catch(() => {});
}

function renderNotepads() {
  if (!view) return;
  const seen = new Set();
  for (const space of visibleSpaces()) {
    for (const n of (view.spaces[space] || {}).notepads || []) {
      seen.add(n.id);
      const e = npEl.get(n.id);
      if (e) {
        e.space = space;
        placeByCard(e.el, n.col, n.row, space);
        e.el.style.width = n.w + "px";
        e.el.style.height = n.h + "px";
        if (document.activeElement !== e.ta && e.ta.value !== n.text) {
          e.ta.value = n.text;
        }
      } else {
        npEl.set(n.id, makeNotepad(n, space));
      }
    }
  }
  for (const [id, e] of [...npEl]) {
    if (!seen.has(id)) {
      e.el.remove();
      npEl.delete(id);
    }
  }
}

// ---------- widgets ----------------------------------------------------------

const widgets = new Map(); // id -> { el, kind, refs, space, rec }

let volOpenIds = new Set();  // ids with the session dropdown expanded
let volTouch = 0;            // last user slider touch (skip poll 1.6s)

function volLabel(v) {
  return Math.round(v * 100) + "%";
}

// Resize the widget element to exactly its content height. animate=false
// snaps (boot self-heal); animate=true glides (easeOutCubic, ~220ms).
function fitVolume(entry, animate, persist) {
  const el = entry.el;
  const from = el.offsetHeight;
  el.style.height = "auto";
  const target = Math.ceil(el.offsetHeight / CARD_GRID_Y) * CARD_GRID_Y;
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
  const sw = Math.ceil(entry.el.offsetWidth / CARD_GRID) * CARD_GRID;
  const sh = Math.ceil(entry.el.offsetHeight / CARD_GRID_Y) * CARD_GRID_Y;
  if (sw !== entry.el.offsetWidth) entry.el.style.width = sw + "px";
  if (sh !== entry.el.offsetHeight) entry.el.style.height = sh + "px";
  invoke("hushtop_widget_save", {
    id: entry.rec.id,
    col: entry.rec.col,
    row: entry.rec.row,
    w: sw,
    h: sh,
  }).catch(() => {});
}

function volumeBody(r) {
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

  const ruler = document.createElement("div");
  ruler.className = "vol-ruler";

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

  let sessions = document.createElement("div");
  sessions.className = "vol-sessions hidden";

  let lastGet = null;
  let muted = false; // the OS mute flag — the same truth the volume flyout uses

  function paint(v) {
    const n = Math.round(v * 100);
    slider.value = n;
    pct.textContent = volLabel(v);
    lastGet = n;
  }

  function paintMute() {
    mute.classList.toggle("is-muted", muted);
    state.textContent = muted ? "muted" : "master";
  }

  function apply(v, push) {
    paint(v);
    // pushing a positive volume while muted unmutes — same rule the
    // desktop volume bar follows, so both surfaces stay identical
    if (push) {
      volTouch = Date.now();
      invoke("audio_master_set", { vol: v }).catch(() => {});
      if (muted && Math.round(v * 100) > 0) {
        muted = false;
        paintMute();
        invoke("audio_mute_set", { muted: false }).catch(() => {});
      }
    }
  }

  slider.addEventListener("input", () => {
    apply(slider.value / 100, true);
  });

  mute.addEventListener("click", () => {
    muted = !muted;
    paintMute();
    invoke("audio_mute_set", { muted }).catch(() => {});
  });

  arrow.addEventListener("click", () => {
    const hidden = sessions.classList.toggle("hidden");
    arrow.classList.toggle("is-open", !hidden);
    if (!hidden) {
      volOpenIds.add(r.id);
      refreshSessions(r);
    } else {
      volOpenIds.delete(r.id);
      if (r.fit) r.fit();
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
        if (r.fit) r.fit();
      })
      .catch(() => {});
  }

  r.poll = setInterval(() => {
    if (Date.now() - volTouch < 1600) return;
    invoke("audio_state")
      .then((st) => {
        const n = Math.round(st.vol * 100);
        const m = !!st.muted;
        if (n !== lastGet || m !== muted) {
          muted = m;
          paint(st.vol);
          paintMute();
        }
      })
      .catch(() => {});
    if (volOpenIds.has(r.id)) refreshSessions(r);
  }, 2000);

  // the flyout on the bar, a media key or any other surface that moves the
  // master volume lands here instantly — no waiting for the poll
  listen("audio:master", (e) => {
    const p = e.payload || {};
    const m = !!p.muted;
    const v = typeof p.vol === "number" ? p.vol : null;
    const n = v == null ? null : Math.round(v * 100);
    if ((n != null && n !== lastGet) || m !== muted) {
      muted = m;
      if (n != null) paint(v);
      paintMute();
    }
  }).catch(() => {});

  invoke("audio_state")
    .then((st) => {
      muted = !!st.muted;
      paint(st.vol);
      paintMute();
    })
    .catch(() => {});

  return { readout, slider, ruler, foot, sessions, pct };
}

// ---- brightness (a cousin of the volume card: sun readout, no mixer) -------

let brTouch = 0;

function brightnessBody() {
  const readout = document.createElement("div");
  readout.className = "vol-readout";
  const sun = document.createElement("span");
  sun.className = "br-sun";
  sun.innerHTML =
    '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round">' +
    '<circle cx="12" cy="12" r="4.4"/><path d="M12 2.6v2.4M12 19v2.4M2.6 12h2.4M19 12h2.4M5 5l1.7 1.7M17.3 17.3 19 19M19 5l-1.7 1.7M6.7 17.3 5 19"/></svg>';
  const pct = document.createElement("span");
  pct.className = "vol-pct";
  const state = document.createElement("span");
  state.className = "vol-state";
  state.textContent = "display";
  readout.appendChild(sun);
  readout.appendChild(pct);
  readout.appendChild(state);

  const slider = document.createElement("input");
  slider.type = "range";
  slider.className = "vol-slider vol-main br-slider";
  slider.min = 0;
  slider.max = 100;
  slider.value = 60;
  slider.setAttribute("aria-label", "Display brightness");

  const ruler = document.createElement("div");
  ruler.className = "vol-ruler br-ruler";

  let lastGet = null;

  function apply(v) {
    pct.textContent = Math.round(v) + "%";
    slider.value = Math.round(v);
    lastGet = Math.round(v);
  }

  slider.addEventListener("input", () => {
    brTouch = Date.now();
    pct.textContent = slider.value + "%";
    invoke("brightness_set", { v: Math.round(slider.value) }).catch(() => {});
  });

  setInterval(() => {
    if (Date.now() - brTouch < 1600) return;
    invoke("brightness_get")
      .then((v) => {
        if (v == null) {
          slider.disabled = true;
          state.textContent = "n/a";
          return;
        }
        slider.disabled = false;
        state.textContent = "display";
        if (Math.round(v) !== lastGet) apply(v);
      })
      .catch(() => {});
  }, 3000);

  invoke("brightness_get")
    .then((v) => {
      if (v == null) {
        slider.disabled = true;
        state.textContent = "n/a";
      } else {
        apply(v);
      }
    })
    .catch(() => {});

  return { readout, slider, ruler, pct };
}

// ---- effects (snow / leaves / off — one layer over every desktop) ---------
// Two live picks plus OFF (the only way off). The retired star and the
// lock-screen farm have no buttons; stale stored values still render and
// the caption says where they went.

const FX_SVG = {
  snow:
    '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round">' +
    '<path d="M12 3v18M4.2 7.5l15.6 9M19.8 7.5l-15.6 9M12 3l-2 2M12 3l2 2M12 21l-2-2M12 21l2-2M4.2 7.5l.5 2.7M4.2 7.5l2.7-.6M19.8 16.5l-.5-2.7M19.8 16.5l-2.7.6M19.8 7.5l-2.7-.6M19.8 7.5l-.5 2.7M4.2 16.5l2.7.6M4.2 16.5l.5-2.7"/></svg>',
  leaves:
    // the leaf from the falling-leaves reference (viewBox 0 0 24 32), veins
    // as a class so page CSS can cut them against the button's live background
    // — tilted 45° so it reads as falling, not standing
    '<svg viewBox="0 0 24 32" width="12" height="16" fill="currentColor" stroke="none" style="transform:rotate(45deg)">' +
    '<path d="M12 0 C 24 8, 24 22, 12 32 C 0 22, 0 8, 12 0 Z"/>' +
    '<path class="lv" d="M12 3 L12 29" stroke-width="1" fill="none"/>' +
    '<path class="lv" d="M12 10 L7 14 M12 10 L17 14 M12 18 L6 22 M12 18 L18 22" stroke-width="0.8" fill="none"/>' +
    '</svg>',
  off:
    '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round">' +
    '<circle cx="12" cy="12" r="8.2"/><path d="M6.2 17.8 17.8 6.2"/></svg>',
};

function effectsBody() {
  const row = document.createElement("div");
  row.className = "fx-row";
  const cap = document.createElement("div");
  cap.className = "fx-cap";
  cap.textContent = "off";

  const btns = {};
  for (const kind of ["snow", "leaves", "off"]) {
    const b = document.createElement("button");
    b.className = "fx-btn";
    b.tabIndex = 0;
    b.setAttribute("aria-label", kind === "off" ? "effects off" : kind + " effect");
    b.innerHTML = FX_SVG[kind];
    // A click SETS the effect — no toggle; the OFF button is the only way
    // off (the old toggle made the snowflake look broken: click snow while
    // snow was already the stored effect and it silently went off).
    b.addEventListener("click", () => {
      const next = kind === "off" ? "none" : kind;
      ulog("effects: pick " + next);
      invoke("hushtop_effect_set", { kind: next }).catch(() => {});
    });
    btns[kind] = b;
    row.appendChild(b);
  }

  function sync(kind) {
    for (const k of Object.keys(btns)) {
      btns[k].classList.toggle("active", k === kind || (k === "off" && kind === "none"));
    }
    cap.textContent =
      kind === "none"
        ? "off"
        : kind === "star"
        ? "retired"
        : kind === "wind"
        ? "lock screen"
        : kind;
  }
  effectUiHooks.add(sync);
  sync(fxUserKind);

  return { row, cap };
}

// ---- second desktop (the spaces switcher) ----------------------------------

function spacesBody() {
  const seg = document.createElement("div");
  seg.className = "sp-seg";
  const btns = [];
  for (const i of [0, 1]) {
    const b = document.createElement("button");
    b.className = "sp-btn";
    b.textContent = String(i + 1);
    b.addEventListener("click", () => {
      if (view && view.dual) return;
      if ((view ? view.active : 0) === i) return;
      invoke("hushtop_space_set", { idx: i }).catch(() => {});
    });
    btns.push(b);
    seg.appendChild(b);
  }
  const lock = document.createElement("div");
  lock.className = "sp-lock hidden";
  lock.innerHTML =
    '<span class="lock-glyph"><i class="shackle"></i><i class="body"></i><i class="keyhole"></i></span>' +
    '<span class="sp-lock-text">This widget only works when one monitor is connected</span>';

  function sync(dto) {
    const dual = !!(dto && dto.dual);
    lock.classList.toggle("hidden", !dual);
    seg.classList.toggle("hidden", dual);
    if (!dual) {
      const a = dto ? dto.active : 0;
      for (let i = 0; i < 2; i++) btns[i].classList.toggle("active", i === a);
    }
  }
  spacesUiHooks.add(sync);
  sync(view);

  return { seg, lock };
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

// THE CD WIDGET: the artwork IS the disc. It spins while the song plays,
// freezes (dimmed) when it pauses, and falls back to a grooved nude disc
// when the player ships no art. Controls sit directly under the disc, the
// title/artist under them — and that is the whole card: the spectrum
// footer is gone, the disc owns the space it used to take.
function mediaBody(el) {
  const cd = document.createElement("div");
  cd.className = "media-cd";

  const disc = document.createElement("div");
  disc.className = "media-disc";
  const art = document.createElement("img");
  art.className = "media-disc-art";
  art.alt = "";
  art.draggable = false;
  art.hidden = true;
  const sheen = document.createElement("div");
  sheen.className = "media-disc-sheen";
  const hole = document.createElement("div");
  hole.className = "media-disc-hole";
  disc.appendChild(art);
  disc.appendChild(sheen);
  disc.appendChild(hole);
  cd.appendChild(disc);

  const ctrl = document.createElement("div");
  ctrl.className = "media-ctrl";

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

  ctrl.appendChild(prev);
  ctrl.appendChild(play);
  ctrl.appendChild(next);

  const info = document.createElement("div");
  info.className = "media-info";
  const title = document.createElement("div");
  title.className = "media-title";
  const artist = document.createElement("div");
  artist.className = "media-artist";
  info.appendChild(title);
  info.appendChild(artist);

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

  function paint(m) {
    const has = !!(m && (m.title || m.artist || m.cover));
    empty.style.display = has ? "none" : "";
    cd.style.display = has ? "" : "none";
    ctrl.style.display = has ? "" : "none";
    info.style.display = has ? "" : "none";
    el.classList.toggle("playing", !!(m && m.playing));
    if (!has) return;
    title.textContent = m.title || "";
    artist.textContent = m.artist || "";
    if (m.cover) {
      const src = "data:image/png;base64," + m.cover;
      if (!art.src.endsWith(m.cover)) art.src = src;
      art.hidden = false;
      disc.classList.remove("nude");
    } else {
      art.hidden = true;
      art.removeAttribute("src");
      disc.classList.add("nude");
    }
    play.innerHTML = m.playing ? PAUSE_P : PLAY_P;
  }

  return {
    cd, disc, art, ctrl, info, empty,
    title, artist, play, prev, next, paint,
  };
}

// ---- lyrics (for the currently playing song, may be wrong in special cases)

function lyricsBody(el) {
  const head = document.createElement("div");
  head.className = "ly-head";
  const headTitle = document.createElement("div");
  headTitle.className = "ly-head-title";
  const headArtist = document.createElement("div");
  headArtist.className = "ly-head-artist";
  head.appendChild(headTitle);
  head.appendChild(headArtist);

  const empty = document.createElement("div");
  empty.className = "ly-empty";
  empty.textContent = "NOTHING PLAYING";

  const body = document.createElement("div");
  body.className = "ly-body";
  const note = document.createElement("div");
  note.className = "ly-note";
  const list = document.createElement("div");
  list.className = "ly-list";
  body.appendChild(note);
  body.appendChild(list);

  // Manual scrolling pauses the autoscroll for a few seconds — the reader
  // is somewhere else, the machine does not fight them for the view.
  let manualUntil = 0;
  body.addEventListener("wheel", () => {
    manualUntil = performance.now() + 4000;
  }, { passive: true });

  let state = "empty"; // empty | loading | none | synced | words | plain
  let curKey = "";
  let lines = [];
  let words = []; // per-word karaoke lines (parallel to lines when present)
  let activeIdx = -1;
  // WHAT THE SEARCH SAW: the duration the current track's fetch ran with,
  // the track's own identity for the one late re-ask, and whether that
  // re-ask already happened. A track change fires the search the moment
  // the title arrives — players that publish their timeline a beat late
  // made that first search run duration-blind, and a duration-blind search
  // cannot tell the studio cut from a live one. The first hush:media_pos
  // anchor carrying a REAL duration re-asks once; the backend re-runs the
  // search (its cache refuses duration-blind entries once a real duration
  // exists) and the better-matched result replaces the old one in place.
  let fetchedDur = 0;
  let trackMeta = { title: "", artist: "", album: "" };
  let reaskAttempts = 0;

  // The result gate: a WORSE result never wipes what is on screen. The
  // late re-ask re-runs the whole search — a miss (or a plain-text
  // demotion) arriving for a track that already shows synced words is a
  // downgrade, not an answer.
  function acceptResult(dto) {
    if (!dto) return;
    const have = state === "synced" || state === "words" || state === "plain";
    if (!dto.found && have) return;
    if (!dto.synced && (state === "synced" || state === "words")) return;
    apply(dto);
  }

  // The playback anchor: hush:media_pos (1 Hz) re-stamps it, the highlighter
  // interpolates between stamps so the line tracking is smooth, not steppy.
  // Small anchor corrections are SLEWED (the clock keeps running and biases
  // toward the truth) — snapping every 1 Hz stamp made the clock stutter
  // backward and the active line flap next/previous; only a real jump
  // (seek, resume, track change) snaps.
  const anchor = { p: 0, at: 0, playing: false };
  const ANCHOR_SLEW = 0.9; // s of disagreement that still counts as noise
  function nowPos() {
    if (!anchor.playing) return anchor.p;
    return anchor.p + (performance.now() - anchor.at) / 1000;
  }

  function setNote(text) {
    note.textContent = text || "";
    note.style.display = text ? "" : "none";
  }

  function render() {
    el.classList.toggle("has", state !== "empty");
    el.classList.toggle("synced", state === "synced" || state === "words");
    empty.style.display = state === "empty" ? "" : "none";
    head.style.display = state === "empty" ? "none" : "";
    body.style.display = state === "empty" ? "none" : "";
    if (state === "empty") return;
    list.replaceChildren();
    if (state === "loading") {
      setNote("looking for lyrics\u2026");
    } else if (state === "none") {
      setNote("no lyrics found");
    } else if (state === "synced" || state === "words") {
      // THE NOTE FIX: the loading note used to survive into the lyric
      // states (render never silenced it) — the reader saw "looking for
      // lyrics\u2026" under fully rendered lyrics forever. The note row
      // only ever speaks for loading / no-lyrics-found.
      setNote("");
      for (let i = 0; i < lines.length; i++) {
        const d = document.createElement("div");
        d.className = "ly-line";
        const wl = words[i];
        if (state === "words" && wl && wl.words.length) {
          for (const w of wl.words) {
            const s = document.createElement("span");
            s.className = "w" + (w.sp ? " sp" : "");
            s.textContent = w.w;
            d.appendChild(s);
          }
        } else {
          d.textContent = lines[i].text;
        }
        list.appendChild(d);
      }
      activeIdx = -1;
    } else {
      setNote("");
      for (const text of lastMeta.plainLines) {
        const d = document.createElement("div");
        d.className = "ly-line";
        d.textContent = text;
        list.appendChild(d);
      }
    }
  }

  const lastMeta = { agree: 1, sources: [], plainLines: [] };

  function apply(dto) {
    if (!dto || !dto.found) {
      state = "none";
      render();
      return;
    }
    lastMeta.agree = dto.agreement || 1;
    lastMeta.sources = dto.sources || [];
    if (dto.synced && dto.lines && dto.lines.length) {
      lines = dto.lines;
      words = Array.isArray(dto.words) ? dto.words : [];
      if (words.length !== lines.length) words = []; // karaoke must be parallel
      state = words.length ? "words" : "synced";
    } else {
      words = [];
      // Plain text: keep interior blank lines (verse spacing), drop the
      // leading/trailing padding the sites like to ship.
      lastMeta.plainLines = (dto.plain || "").split("\n").map((s) => s.trimEnd());
      while (lastMeta.plainLines.length && !lastMeta.plainLines[0]) lastMeta.plainLines.shift();
      while (lastMeta.plainLines.length && !lastMeta.plainLines[lastMeta.plainLines.length - 1]) lastMeta.plainLines.pop();
      state = "plain";
    }
    body.scrollTop = 0;
    render();
  }

  // ---- track changes -------------------------------------------------------

  function onTrack(m) {
    const has = !!(m && (m.title || m.artist));
    if (!has) {
      curKey = "";
      state = "empty";
      render();
      return;
    }
    headTitle.textContent = m.title || "";
    headArtist.textContent = m.artist || "";
    const key = (m.title || "") + "\u{1}" + (m.artist || "");
    if (key === curKey) return; // same track — pause/play keeps the lyrics
    curKey = key;
    trackMeta = {
      title: m.title || "",
      artist: m.artist || "",
      album: m.album || "",
    };
    fetchedDur = m.duration || 0;
    reaskAttempts = 0;
    state = "loading";
    render();
    invoke("lyrics_fetch", {
      title: m.title || "",
      artist: m.artist || "",
      album: m.album || "",
      duration: m.duration || 0,
    })
      .then((dto) => {
        if (dto) acceptResult(dto); // cached — render now; else await hush:lyrics
      })
      .catch(() => {
        if (key === curKey) {
          state = "none";
          render();
        }
      });
  }

  function onResult(key, dto) {
    if (key !== curKey) return; // stale — the track moved on
    acceptResult(dto);
  }

  function onPos(p) {
    const now = performance.now();
    const cur = nowPos();
    const target = p.position || 0;
    const playing = !!p.playing;
    const diff = target - cur;
    if (!playing || Math.abs(diff) > ANCHOR_SLEW) {
      // pause boundary, seek, track change or the very first stamp: snap
      anchor.p = target;
      anchor.at = now;
      anchor.playing = playing;
    } else {
      // noise: keep the clock running, bias it toward the truth so the
      // 1 Hz stamps cannot make the highlight stutter backward
      anchor.p = cur + diff * 0.4;
      anchor.at = now;
      anchor.playing = true;
    }
    // THE LATE-DURATION RE-ASK: the search ran duration-blind (the player
    // had not published its timeline yet) and a real duration just showed
    // up — re-run it, quietly: the current lyric stays on screen until a
    // better-matched result lands (the gate above drops downgrades). A
    // None means a search is still in flight for this track — the next 1 Hz
    // anchor retries (bounded), so a slow player's timeline never costs
    // the match.
    const realDur = p.duration || 0;
    if (
      curKey &&
      reaskAttempts < 20 &&
      realDur >= 3 &&
      realDur - fetchedDur > 3
    ) {
      reaskAttempts += 1;
      invoke("lyrics_fetch", {
        title: trackMeta.title,
        artist: trackMeta.artist,
        album: trackMeta.album,
        duration: realDur,
      })
        .then((dto) => {
          if (dto) {
            fetchedDur = realDur; // the answer IS duration-aware — stop asking
            acceptResult(dto);
          }
        })
        .catch(() => {});
    }
  }

  // ---- the highlighter -----------------------------------------------------
  // The line switches when the SINGER moves to it, not when the previous
  // one runs out of words: a line becomes active at its own LRC timestamp
  // (a hair EARLY — LEAD — so it never reads late), and it only ever moves
  // BACKWARD for a real seek (the clock more than BACKGUARD before the
  // current line's start). Ordinary clock jitter can no longer make the
  // highlight flip forward/backward a couple of times.

  const LEAD = 0.18; // s of anticipation on every line start
  const BACKGUARD = 1.2; // s of genuine rewind the backward move must prove
  const SEEK_JUMP = 1.5; // s of backward motion a single tick may never make

  // THE SEEK-BACK WIPE: the karaoke lights only ever spread — a line the
  // singer passed keeps its words lit, so the moment the listener comes
  // back to an earlier point every line after that point still reads as
  // read. Any real rewind hands those lines their future back: every span
  // AFTER the point goes dark again. Lines BEFORE the point stay lit —
  // they were genuinely sung — and the active line re-sweeps from the
  // clock on the ticks that follow. Text-only synced lines carry no
  // spans, so the wipe is a no-op there by construction.
  function wipeAfter(idx) {
    const kids = list.children;
    for (let i = idx + 1; i < kids.length; i++) {
      const spans = kids[i].children;
      for (let j = 0; j < spans.length; j++) spans[j].classList.remove("lit");
    }
  }

  let prevPos = 0; // the jump detector's reference frame

  const poll = setInterval(() => {
    if ((state !== "synced" && state !== "words") || !lines.length) return;
    const pos = nowPos();
    // last line whose (start - lead) <= pos, lead halved when the previous
    // line starts too close (never highlight two lines in one breath)
    let idx = -1;
    for (let i = 0; i < lines.length; i++) {
      const start = lines[i].t;
      const lead = i > 0
        ? Math.min(LEAD, Math.max(0, start - lines[i - 1].t) / 2)
        : LEAD;
      if (start - lead <= pos) idx = i;
      else break;
    }
    if (idx < activeIdx) {
      // going backward: only a real seek may rewind the highlight — the
      // clock must be BEFORE the current line's start by more than the
      // backguard; anything less is jitter and the line holds
      const cur = lines[activeIdx];
      if (!(cur && pos < cur.t - BACKGUARD)) idx = activeIdx;
    }
    if (idx !== activeIdx) {
      const rewind = idx < activeIdx;
      activeIdx = idx;
      const kids = list.children;
      for (let i = 0; i < kids.length; i++) {
        kids[i].classList.toggle("on", i === idx);
      }
      if (rewind) wipeAfter(idx); // crossed lines backward — unmark what is ahead now
      if (idx >= 0 && performance.now() >= manualUntil) {
        const line = kids[idx];
        if (line) {
          const top = line.offsetTop - body.clientHeight * 0.36;
          body.scrollTo({ top: Math.max(0, top), behavior: "smooth" });
        }
      }
    }
    // Karaoke sweep INSIDE the active line: the current word lights as the
    // singer reaches it (word.t), earlier words stay lit, later ones wait.
    // Runs every tick — one line's spans, a few class toggles at most.
    if (state === "words" && idx >= 0 && words[idx]) {
      const line = list.children[idx];
      if (line) {
        const spans = line.children;
        const ws = words[idx].words;
        for (let i = 0; i < spans.length; i++) {
          spans[i].classList.toggle("lit", i < ws.length && ws[i].t <= pos);
        }
      }
    }
    // The same-line seek-back: the user came back to a point INSIDE the
    // line that is still active (no line crossing, so the rewind branch
    // above never fires) — the clock jumping backward more than a tick
    // can ever manufacture (the slew bias drifts at most ~0.04 s per
    // 120 ms tick) IS the seek, and everything after the point unmarks.
    // Catching it here also covers scrubs while PAUSED, where the line
    // index has no reason to move at all.
    if (pos < prevPos - SEEK_JUMP) wipeAfter(idx);
    prevPos = pos;
  }, 120);

  return { head, body, empty, onTrack, onResult, onPos, poll };
}

// ---- widget factory + diff render ---------------------------------------

function makeWidget(w, space) {
  const el = document.createElement("div");
  el.className = "notepad widget " + w.kind;
  placeByCard(el, w.col, w.row, space);
  el.style.width = w.w + "px";
  el.style.height = w.h + "px";

  const head = makeGrip(() => {
    invoke("hushtop_widget_del", { id: w.id }).catch(() => {});
  });
  el.appendChild(head);

  const refs = { };
  if (w.kind === "volume") {
    refs.id = w.id;
    Object.assign(refs, volumeBody(refs));
    el.appendChild(refs.readout);
    el.appendChild(refs.slider);
    el.appendChild(refs.ruler);
    el.appendChild(refs.foot);
    el.appendChild(refs.sessions);
  } else if (w.kind === "brightness") {
    Object.assign(refs, brightnessBody());
    el.appendChild(refs.readout);
    el.appendChild(refs.slider);
    el.appendChild(refs.ruler);
  } else if (w.kind === "effects") {
    Object.assign(refs, effectsBody());
    el.appendChild(refs.row);
    el.appendChild(refs.cap);
  } else if (w.kind === "spaces") {
    Object.assign(refs, spacesBody());
    el.appendChild(refs.seg);
    el.appendChild(refs.lock);
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
    el.appendChild(refs.cd);
    el.appendChild(refs.ctrl);
    el.appendChild(refs.info);
    el.appendChild(refs.empty);
    // hush:media only fires when title|artist|playing CHANGES — a widget
    // rebuilt by a desktop switch (every space owns its own media card)
    // would sit on the empty state until the user toggled playback. Feed
    // it the cached snapshot now, and let media_now confirm — the same
    // restore the lyrics widget already does.
    refs.paint(lastMedia);
    invoke("media_now")
      .then((m) => paintMedia(m))
      .catch(() => {});
  } else if (w.kind === "lyrics") {
    Object.assign(refs, lyricsBody(el));
    el.appendChild(refs.head);
    el.appendChild(refs.body);
    el.appendChild(refs.empty);
    refs.onTrack(lastMedia);
    invoke("media_now")
      .then((m) => refs.onTrack(m))
      .catch(() => {});
  }

  widgetsEl.appendChild(el);

  wireCardDrag(el, space, (c, r) => {
    w.col = c; // keep the record current — geom saves commit rec.col/row
    w.row = r;
    invoke("hushtop_widget_save", { id: w.id, col: c, row: r }).catch(() => {});
  });

  const entry = { el, kind: w.kind, refs, rec: w, space };
  if (w.kind === "volume") {
    refs.fit = () => fitVolume(entry, true);
    fitVolume(entry, false, false); // boot snap is display-only — never persist a boot measurement
  }
  return entry;
}

// The widget roster a render pass draws: every card of the VISIBLE spaces —
// nothing else. Each desktop owns its cards outright, the switcher included:
// a desktop without it shows none, and going back means adding one there
// (the start menu's widget tab is always reachable).
function widgetRoster() {
  const roster = [];
  const added = new Set();
  for (const space of visibleSpaces()) {
    for (const w of (view.spaces[space] || {}).widgets || []) {
      if (!added.has(w.id)) {
        added.add(w.id);
        roster.push([w, space]);
      }
    }
  }
  return roster;
}

function renderWidgets() {
  if (!view) return;
  const seen = new Set();
  for (const [w, space] of widgetRoster()) {
    if (w.kind === "media" && (w.w !== 252 || w.h !== 252)) {
      w.w = 252;
      w.h = 252; // whole grid rows again (18 × 14) — the 6px above the disc paid for it
    }
    if (w.kind === "lyrics" && (w.w !== 252 || w.h !== 288)) {
      w.w = 252;
      w.h = 288;
    }
    seen.add(w.id);
    const e = widgets.get(w.id);
    if (e) {
      e.rec = w; // keep the record fresh — geom saves commit rec.col/row
      e.space = space;
      placeByCard(e.el, w.col, w.row, space);
      e.el.style.width = w.w + "px";
      e.el.style.height = w.h + "px";
    } else {
      widgets.set(w.id, makeWidget(w, space));
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

let lastMedia = null;
function paintMedia(m) {
  lastMedia = m || lastMedia;
  for (const w of widgets.values()) {
    if (w.kind === "media" && w.refs && w.refs.paint) w.refs.paint(m);
  }
}
listen("hush:media", (e) => {
  paintMedia(e.payload);
  for (const w of widgets.values()) {
    if (w.kind === "lyrics" && w.refs && w.refs.onTrack) w.refs.onTrack(e.payload);
  }
}).catch(console.error);

// The lyrics widget's 1 Hz playback anchor (position interpolation).
listen("hush:media_pos", (e) => {
  for (const w of widgets.values()) {
    if (w.kind === "lyrics" && w.refs && w.refs.onPos) w.refs.onPos(e.payload || {});
  }
}).catch(console.error);

// The answer to a lyrics_fetch — keyed by title+artist, stale keys dropped.
listen("hush:lyrics", (e) => {
  const msg = e.payload || {};
  for (const w of widgets.values()) {
    if (w.kind === "lyrics" && w.refs && w.refs.onResult) {
      w.refs.onResult(msg.key, msg.lyrics);
    }
  }
}).catch(console.error);
invoke("media_now")
  .then((m) => paintMedia(m))
  .catch(() => {});

// ---------- the widget ghost (dragged out of the start menu) ----------------

// The ghost carries the widget's REAL body (hushWidgetPreview) at the
// widget's real size — what follows the cursor is exactly what will land.
// There is no separate "preview render method" left anywhere: ghost, wipe
// previews and the start-menu gallery all build the same DOM.

let ghostKind = "";

// Watchdog: the ghost is driven by a start-menu event stream. If that stream
// ever dies without an explicit off (capture yanked, window vanished, any
// missed path), the ghost would sit on the desktop forever. Every on-event
// re-arms this timer; 3.5 s of silence hides the ghost.
let ghostWatch = 0;

function hideGhost() {
  if (!ghostEl.classList.contains("hidden")) {
    ulog("ghost: watchdog hid the ghost (stream went silent)");
  }
  clearTimeout(ghostWatch);
  ghostWatch = 0;
  ghostEl.classList.add("hidden");
  ghostEl.className = "notepad widget ghost hidden";
  ghostEl.replaceChildren();
  ghostKind = "";
  hideGridChrome();
}

listen("hushtop:widget_ghost", (e) => {
  const p = e.payload || {};
  if (!p.on) {
    hideGhost();
    return;
  }
  clearTimeout(ghostWatch);
  ghostWatch = setTimeout(hideGhost, 3500);
  ghostEl.className = "notepad widget ghost " + (p.kind || "");
  if (ghostKind !== (p.kind || "")) {
    ghostKind = p.kind || "";
    ghostEl.replaceChildren(hushWidgetPreview(ghostKind));
  }
  ghostEl.style.width = p.w + "px";
  ghostEl.style.height = p.h + "px";
  ghostEl.style.left = p.x + "px";
  ghostEl.style.top = p.y + "px";
  ghostEl.classList.remove("hidden");
  const space = spaceAtPoint(p.x, p.y);
  showLandingSized(
    cardMagnet(p.x, "x", space),
    cardMagnet(p.y, "y", space),
    p.w,
    p.h
  );
}).catch(console.error);

// ---------- desktop effects (snow / leaves / star / wind — rain retired) ------

const fx = document.getElementById("fx");
const fxBk = document.getElementById("fxback");

// The renderer is SHARED (ui/fx.js): the lock screen mounts the same engine
// on its own canvas, so both stages draw the identical scene. This page
// only keeps the UI honest — the effects picker mirrors the live kind —
// and may suspend the desktop effect while the sheet is locked.
const effectUiHooks = new Set();

let fxSuspended = false; // true while locked — the choreography wants a bare sheet

// The boot gate: the particles are part of the REVEAL, not the skeleton —
// they wait for the boot choreography (the icons/widgets stage) and fade in
// with it. Every fxSet caller is coerced to "none" until then.
let fxBooting = true;

// The lock fade-out: the class drops first (0.6s CSS melt) and the ENGINE
// stops only after the fade — an instant set("none") would clear the canvas
// before a single faded frame shows.
let fxFadeTimer = 0;

// The desktop's OWN effect — the last user/config-driven pick. The lock
// choreography forces the ENGINE to "none" while locked, but this variable
// is what the unlock restore replays, so the user's pick survives the
// round trip by construction (suspension never writes here).
let fxUserKind = "none";

// The desktop never runs the wind farm — it is the LOCK SCREEN's scene
// (exclusive, always on, no off switch). Snow and leaves are the desktop's
// own picks; a stale stored "wind" (or the retired "rain"/"star" — star
// still renders, rain does not) or anything unknown lands as none/kept per
// the engine; the engine itself keeps the wind renderer because the
// lockscreen page mounts this file.
function desktopFxKind(kind) {
  return kind === "snow" || kind === "leaves" || kind === "star" ? kind : "none";
}

function fxSyncUi(k) {
  for (const hook of effectUiHooks) {
    try { hook(k); } catch (e) { /* nothing */ }
  }
}

function fxSet(kind) {
  const k = desktopFxKind(kind);
  if (fxSuspended || fxBooting) {
    // suspension/boot: the ENGINE is not touched at all — a caller while
    // locked (a stray spaces payload, a config replay) must never stop or
    // respawn the frozen scene; only the UI mirror + the canvas gate move.
    // The engine keeps its kind; the unlock restore reconciles.
    fx.classList.remove("on");
    fxSyncUi(k);
    return;
  }
  const eff = hushFx.set(k);
  // the canvas owns its own opacity: .on fades the particles in (boot,
  // unlock, a fresh pick) and out (lock) — the engine only ever starts/stops
  // under a finished fade
  fx.classList.toggle("on", eff !== "none");
  fxSyncUi(eff);
}

// The unlock restore. The engine holds the FROZEN lock-time scene (pause()
// kept kind, particles and the painted frame): the same pick as before the
// lock → resume() continues the EXACT same particles from their frozen
// positions — the sky is simply still there when the canvas fades back in,
// no respawn, no bare-canvas second. A pick that changed while locked takes
// the normal (re)start path instead.
function fxRestore() {
  const k = desktopFxKind(fxUserKind);
  if (k !== "none" && hushFx.kind() === k) {
    hushFx.resume();
    fx.classList.add("on");
    fxSyncUi(k);
  } else {
    fxSet(fxUserKind);
  }
}

hushFx.mount({ back: fxBk, front: fx });

fxUserKind = desktopFxKind((view && view.effect) || "none");
fxSet(fxUserKind);

// ---------- lock + boot choreography (sweep line, ruled lines, fades) ------

const rulesEl = document.getElementById("rules");

// The sweep — the SAME primitive as the space switch, nothing more: ONE ink
// line crosses the sheet and the desktop's OWN horizontal ruled lines are
// erased or revealed in lockstep with it (clip-path inset tracked to the
// line position). No curtain, no extra elements, no light ink anywhere:
// the veil is paper-dark from the first frame and the only moving ink is
// the single line.
//   dir -1 (lock):        line right -> left, rules erased behind it — the
//                         sheet goes bare exactly "with the bar".
//   dir +1 (unlock/boot): line left -> right, rules revealed behind it —
//                         they slide back in from the left edge.
const SWEEP_MS = 1500;
const SWEEP_EASE = "cubic-bezier(0.65, 0, 0.35, 1)";

let sweepAnims = [];

function sweepDesk(dir, holdLine) {
  // CONTINUITY: read the line's and the rules' CURRENT animated state
  // BEFORE canceling anything. A cancel snaps both back to their underlying
  // values, and animating from those snapped values is exactly the jump the
  // owner saw (a lock landing mid-boot-sweep teleported the line to the far
  // edge before the retrace). From = wherever the motion actually is right
  // now; the at-rest fallbacks reproduce the old endpoints when nothing is
  // in flight (the line is display:none -> computed left "auto").
  const lineNow = getComputedStyle(wipeLineEl).left;
  const clipNow = getComputedStyle(rulesEl).clipPath;
  for (const a of sweepAnims) a.cancel();
  sweepAnims = [];
  // the line may still carry a space-switch animation (a lock pressed
  // mid-wipe) — take the element over completely
  for (const a of wipeLineEl.getAnimations()) a.cancel();
  let fromClip = clipNow;
  if (fromClip === "none") {
    fromClip = dir < 0 ? "inset(0 0% 0 0)" : "inset(0 100% 0 0)";
  }
  const toClip = dir < 0 ? "inset(0 100% 0 0)" : "inset(0 0% 0 0)";
  const atRest = lineNow === "auto" || lineNow === "";
  const fromLeft = atRest ? (dir < 0 ? "100%" : "0%") : lineNow;
  const toLeft = dir < 0 ? "0%" : "100%";
  wipeLineEl.classList.add("on");
  const opts = { duration: SWEEP_MS, easing: SWEEP_EASE, fill: "forwards" };
  const lineAnim = wipeLineEl.animate(
    [{ left: fromLeft }, { left: toLeft }],
    opts
  );
  const clipAnim = rulesEl.animate(
    [{ clipPath: fromClip }, { clipPath: toClip }],
    opts
  );
  sweepAnims = [lineAnim, clipAnim];
  clipAnim.onfinish = () => {
    rulesEl.style.clipPath = toClip; // the resting state, held without fill
    clipAnim.cancel();
    sweepAnims = [];
    if (holdLine) {
      // The boot sweep HOLDS the line parked at its far edge on a live
      // fill: the lock's erase retrace takes it over from exactly where the
      // reveal stopped — one continuous line from the first frame of the
      // boot to the bare locked sheet. The line animation is deliberately
      // left running (canceling it would snap the line to its static
      // left-edge position while still visible); it stays registered so the
      // next sweepDesk finds and takes it over.
      sweepAnims = [lineAnim];
    } else {
      lineAnim.cancel();
      wipeLineEl.classList.remove("on");
    }
  };
  return clipAnim;
}

// Boot: the shell starts on an opaque paper-dark veil (the CSS default for
// #veil — never light ink), a bare sheet with no rules. The reveal stages:
// the veil lifts, the single line sweeps left -> right and the ruled lines
// slide back in behind it. There is no third stage on the schedule — the
// sweep IS the loading, and what follows it is THE LOCK: the shell launches
// straight to the lockscreen, so the reveal finishes first (the line holds
// its far edge) and the boot lock lands the moment the crossing is done.
// The icons'/widgets' first bloom belongs to the UNLOCK choreography, never
// to a flash that the lock would immediately erase.
let bootPlayed = false;
function playBoot() {
  if (bootPlayed) return;
  bootPlayed = true;
  setTimeout(() => {
    document.body.classList.add("booted");
    ulog("choreo: boot — background fades in");
  }, 400);
  setTimeout(() => {
    // holdLine: the line parks at the right edge instead of vanishing —
    // the lock's erase retrace starts exactly there. The signal fires on
    // the ANIMATION's finish (never a timer): the backend locks as the
    // sweep completes, so no stage of the boot is ever cut short.
    const reveal = sweepDesk(1, true);
    reveal.addEventListener("finish", () => {
      invoke("boot_reveal_done").catch(() => {});
      // The desktop bloom (icons/widgets/effect) is gated on the lock NOT
      // having landed: the show event beats this timer by more than a
      // second when the signal works (the sheet stays bare for the lock
      // choreography; body.unlocking replays the bloom after the
      // password). Only a lost signal — the backend's fallback owns the
      // lock from here — lets the desktop bloom while still open.
      setTimeout(() => {
        // the lock landed on schedule (the honest boot) OR an unlock is
        // already playing (a no-password keydown slipped in within the
        // grace beat) — either way the bloom is owned elsewhere
        if (
          document.body.classList.contains("locked") ||
          document.body.classList.contains("unlocking")
        ) {
          return;
        }
        document.body.classList.add("boot-icons");
        // the boot reveal is done enough for the sky: the gate lifts here and the
        // particles FADE IN with the icons/widgets — they are part of the reveal,
        // never the skeleton (they did not run during the veil/sweep stages)
        fxBooting = false;
        fxSet(fxUserKind);
        ulog("choreo: boot — icons and widgets fade in, desktop effect fades in with them");
      }, 1500);
    });
    ulog("choreo: boot — the line sweeps left -> right, ruled lines slide in");
  }, 1200);
}

// The body carries .boot from script start (before the first paint) so the
// paper-dark veil + hidden layers are the very first thing ever on screen.
document.body.classList.add("boot");

// Lock: icons/widgets fade out slowly, the veil calms the sheet, the line
// sweeps right -> left and erases the ruled lines with it, and the
// (transparent) lock window rises its ink stack once the sweep is done.
// The desktop's own effect FREEZES for all of it — the sheet the lock
// choreography draws on must be bare; the lock screen then runs THE WIND
// FARM (its own scene — always on, no option, no off switch) once the
// animation is done, and the desktop's own scene comes back exactly as it
// left at the unlock reveal.
// Unlock: the wind farm melts out FIRST (0.6s) — the comeback holds the
// locked wash until it is gone; then the veil lifts, the line sweeps back
// left -> right with the rules sliding in behind it, icons/widgets fade in
// after the sweep with the frozen desktop scene fading in alongside — the
// backend restores the minimized windows last of all.
// The comeback is sequenced BEHIND the lock screen's own fade-out: the
// wind farm melts over 0.6s (its engine clears at 700ms), so the desktop
// holds the locked wash until then and the reveal starts only once the
// overlay is truly empty — never underneath a still-fading turbine scene.
// unlockSeq invalidates a pending choreography when a re-lock lands
// mid-unlock (the show handler bumps it): every unlock timer re-checks its
// own seq, so a stale callback can never play over a fresh lock. fxSuspended
// is NOT a valid guard here — it holds TRUE through the whole unlock (it is
// the gate that keeps stray callers off the frozen engine) and only the
// finish step clears it.
const WIND_FADE_MS = 700;
let unlockSeq = 0;
// The lock choreography dedupe: the lockscreen_hello re-emit can land right
// behind a show this page did catch (the boot lock races webview boot) —
// replaying the lock fade/sweep twice reads as a glitch. 800ms covers every
// honest ordering; a re-lock minutes later passes freely.
let lastLockShowAt = 0;

const lockShowReady = listen("lockscreen:show", () => {
  const now = Date.now();
  if (now - lastLockShowAt < 800) return;
  lastLockShowAt = now;
  bootPlayed = true; // a lock supersedes an incomplete boot
  fxBooting = false; // the gate must never outlive the boot — unlock replays the pick
  unlockSeq++; // a re-lock kills any unlock choreography still in flight
  document.body.classList.add("locked", "booted");
  document.body.classList.remove("boot", "unlocking");
  // suspend (not clear) the desktop effect: every fxSet caller is gated to
  // the UI mirror while the flag is up — a stray spaces payload mid-lock
  // cannot sneak the scene back onto the bare sheet, and nothing stops or
  // respawns the engine either. The PARTICLES melt out first (the .on class
  // drops, 0.6s CSS), then the engine FREEZES — kind, particles and the
  // painted frame all survive for the unlock resume.
  fxSuspended = true;
  clearTimeout(fxFadeTimer);
  fx.classList.remove("on");
  fxFadeTimer = setTimeout(() => {
    if (fxSuspended) hushFx.pause(); // superseded by an unlock → the timer no-ops
  }, 700);
  ulog("choreo: lock — desktop effect fades out, then freezes in place");
  // the icons/widgets fade out first (0.9s); the line starts crossing once
  // they are half-gone — the erase reads as the second beat of one motion
  setTimeout(() => sweepDesk(-1), 550);
  ulog("choreo: lock — icons fade out, then the line erases the rules right -> left");
}).catch(() => {});

const lockHideReady = listen("lockscreen:hide", () => {
  const seq = ++unlockSeq;
  clearTimeout(fxFadeTimer); // a pending lock-time freeze loses to the unlock
  ulog("choreo: unlock — holding the wash until the wind farm has melted out");
  setTimeout(() => {
    if (seq !== unlockSeq) return; // re-locked mid-choreography
    document.body.classList.add("unlocking");
    document.body.classList.remove("locked");
    sweepDesk(1);
    ulog("choreo: unlock — the wind is gone; the line sweeps back left -> right, rules slide in");
    setTimeout(() => {
      if (seq !== unlockSeq) return;
      document.body.classList.remove("unlocking");
      ulog("choreo: unlock — icons and widgets fade in");
      // the desktop effect returns WITH the icon fade — the exact mirror
      // of the lock (the lock screen's wind farm started when the lock
      // animation was done). The frozen scene resumes seamlessly: the
      // particles are already mid-flight when the canvas fades in.
      fxSuspended = false;
      fxRestore();
      ulog("choreo: unlock — desktop effect restored (" + fxUserKind + ")");
    }, SWEEP_MS);
  }, WIND_FADE_MS);
}).catch(() => {});

// The overlay is TRULY gone (the backend hides the lock window at 1700ms —
// past the wind melt, mid-comeback). The comeback choreography may still be
// mid-flight at this point; while fxSuspended holds, the pending unlock
// timer owns the restore and this event stays out of its way. Once the
// choreography has finished (or a hide event was lost entirely), this is
// the safety net that re-asserts the desktop's own effect. No kick: the
// engine is no longer destroyed by the lock, and resume() re-arms the loop
// fresh at the restore — a loop cannot be left dead by the hand-off.
listen("lockscreen:gone", () => {
  if (fxSuspended) return;
  fxRestore();
  ulog("choreo: unlock — overlay gone, desktop effect re-asserted (" + fxUserKind + ")");
}).catch(() => {});

// Listener handshake (same shape as the lock page's): once the lock
// listeners are registered, tell the backend. If the shell booted straight
// into the lock while THIS webview was still loading, the broadcast show was
// missed and the backend re-emits it to both pages — the dedupe above eats
// the echo when the real show was caught. A normal alt+l lock gets a no-op
// hello: nothing to heal.
Promise.all([lockShowReady, lockHideReady])
  .catch(() => {})
  .then(() => invoke("lockscreen_hello", { page: "hushtop" }).catch(() => {}));
