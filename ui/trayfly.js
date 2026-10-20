const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const grid = document.getElementById("grid");
const empty = document.getElementById("empty");
const card = document.getElementById("card");

let sig = "";
let epoch = 0;
let closeScheduled = false;

function diag(msg) {
  invoke("trayfly_diag", { msg }).catch(() => {});
}

window.addEventListener("error", (e) =>
  diag(`js error: ${e.message} @ ${e.filename}:${e.lineno}`)
);
window.addEventListener("unhandledrejection", (e) =>
  diag(`js rejection: ${e.reason}`)
);

function close() {
  invoke("trayfly_hide").catch(() => {});
}

function sendClick(key, button) {
  invoke("tray_click", { key, button }).catch((e) =>
    diag(`tray_click ${button} failed: ${e}`)
  );
  // Single close timer per open (no stacking, no restart-sticky state).
  if (!closeScheduled) {
    closeScheduled = true;
    setTimeout(close, 250);
  }
}

const cellMap = new Map(); // key -> {el, img, span, vis, key}
let entranceArmed = false;

function visualSig(t) {
  return `${t.tip || ""}\u0000${t.process || ""}\u0000${t.icon || ""}`;
}

function paintCell(cell, t) {
  cell.key = t.key;
  const vis = visualSig(t);
  if (vis === cell.vis) return; // byte-identical: touch nothing
  cell.vis = vis;
  if (t.icon) {
    const src = `data:image/png;base64,${t.icon}`;
    if (cell.img.getAttribute("src") !== src) cell.img.src = src;
    cell.img.style.display = "";
    cell.span.style.display = "none";
  } else {
    cell.img.removeAttribute("src");
    cell.img.style.display = "none";
    const txt = t.process || t.tip || "app";
    if (cell.span.textContent !== txt) cell.span.textContent = txt;
    cell.span.style.display = "";
  }
}

function buildCell(t, i, animate) {
  const b = document.createElement("button");
  b.className = "tray-cell";
  const img = document.createElement("img");
  img.alt = "";
  img.draggable = false;
  const span = document.createElement("span");
  span.className = "tray-fallback";
  b.appendChild(img);
  b.appendChild(span);
  if (animate) {
    b.style.animation = "fade-in 240ms var(--ease) backwards";
    b.style.animationDelay = `${i * 20}ms`;
  } else {
    // Refresh-born cells appear INSTANTLY — no entrance, no blink.
    b.style.animation = "none";
  }
  const cell = { el: b, img, span, vis: "", key: t.key };
  b.addEventListener("click", () => sendClick(cell.key, "left"));
  b.addEventListener("contextmenu", (ev) => {
    ev.preventDefault();
    ev.stopPropagation();
    sendClick(cell.key, "right");
  });
  b.addEventListener("auxclick", (ev) => {
    if (ev.button === 1) sendClick(cell.key, "middle");
  });
  b.addEventListener("dblclick", () => sendClick(cell.key, "dbl"));
  return cell;
}

function render(list) {
  empty.style.display = list.length ? "none" : "grid";
  const n = list.length;
  const cols = Math.min(3, Math.max(1, n));
  grid.style.gridTemplateColumns = `repeat(${cols}, 54px)`;
  const rows = n === 0 ? 1 : Math.min(5, Math.ceil(n / 3));
  grid.style.maxHeight = `${2 * 12 + rows * 54 + (rows - 1) * 6}px`;

  const want = new Set(list.map((t) => t.key));
  // Adoption pool: cells whose key vanished (identity flip) can be re-keyed
  // onto a new entry with IDENTICAL visuals instead of rebuilt+refetched.
  const adoptable = new Map();
  for (const [k, c] of cellMap) {
    if (!want.has(k)) {
      adoptable.set(c.vis, c);
      cellMap.delete(k);
    }
  }

  const animate = entranceArmed;
  if (entranceArmed) {
    entranceArmed = false;
    clearTimeout(render._strip);
    render._strip = setTimeout(() => {
      for (const c of cellMap.values()) c.el.style.animation = "none";
    }, 700);
  }
  for (const [i, t] of list.entries()) {
    let cell = cellMap.get(t.key);
    if (!cell) {
      cell = adoptable.get(visualSig(t));
      if (cell) adoptable.delete(cell.vis);
      else cell = buildCell(t, i, animate);
      cellMap.set(t.key, cell);
    }
    paintCell(cell, t);
  }
  grid.replaceChildren(...list.map((t) => cellMap.get(t.key).el));
}

function renderDebounced(list) {
  if (document.visibilityState === "hidden") {
    const s = JSON.stringify(list);
    if (s === sig) return;
    sig = s;
    render(list);
    return;
  }
  clearTimeout(renderDebounced._t);
  renderDebounced._t = setTimeout(() => {
    const s = JSON.stringify(list);
    if (s === sig) return;
    sig = s;
    render(list);
  }, 120);
}

listen("tray_icons", (e) => renderDebounced(e.payload)).catch((err) =>
  diag(`listen tray_icons failed: ${err}`)
);

listen("trayfly:show", (e) => {
  if (e && e.payload === "probe") {
    invoke("trayfly_pong").catch(() => {});
    diag(`probe re-show answered (vis=${document.visibilityState})`);
    return;
  }
  epoch++;
  const my = epoch;
  closeScheduled = false; // fresh open — the 250ms close timer may run again
  entranceArmed = true;   // the next render animates its fresh cells once
  invoke("trayfly_pong").catch(() => {});
  // Jump to the hidden .pre state without transition, then release it — the
  // base transition plays the entrance. Synchronous, never inside rAF.
  card.classList.remove("out");
  card.classList.add("pre");
  void card.offsetWidth; // apply .pre instantly
  card.classList.remove("pre");
  const cs = getComputedStyle(card);
  diag(
    `show vis=${document.visibilityState} foc=${document.hasFocus()} ` +
      `cells=${grid.childElementCount} op=${cs.opacity} ` +
      `vp=${innerWidth}x${innerHeight} dpr=${devicePixelRatio} my=${my}`
  );
}).catch((err) => diag(`listen trayfly:show failed: ${err}`));

listen("trayfly:hide", () => {
  epoch++;
  const my = epoch;
  card.classList.add("out");
  setTimeout(() => {
    if (epoch === my) card.classList.remove("out");
  }, 240);
}).catch((err) => diag(`listen trayfly:hide failed: ${err}`));

window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") close();
});

// Startup sync (icons may have arrived while this window was still hidden).
(async () => {
  try {
    const st = await invoke("sync_state");
    if (st.tray) {
      sig = JSON.stringify(st.tray);
      render(st.tray);
    }
    diag(`boot ok cells=${grid.childElementCount} tauri=${!!window.__TAURI__}`);
    invoke("trayfly_pong").catch(() => {});
  } catch (e) {
    diag(`boot sync_state failed: ${e}`);
  }
})();
