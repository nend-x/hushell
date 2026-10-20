const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const cardEl = document.getElementById("preview-card");
const gridEl = document.getElementById("preview-grid");

// The card itself is hoverable: entering it cancels the pending hide (the
// native side applies a grace period while the cursor crosses the gap),
// leaving it starts the hide, clicking a slot activates that window.
cardEl.addEventListener("mouseenter", () => {
  invoke("preview_hover", { enter: true }).catch(() => {});
});
cardEl.addEventListener("mouseleave", () => {
  invoke("preview_hover", { enter: false }).catch(() => {});
});

listen("preview:show", (e) => {
  const slots = e.payload.slots || [];
  const overflow = e.payload.overflow || 0;
  const badge = e.payload.badge || null;

  const cells = slots.map((s) => {
    const cell = document.createElement("div");
    cell.className = "slot";
    cell.style.left = `${s.thumb.x}px`;
    cell.style.top = `${s.thumb.y}px`;
    cell.style.width = `${s.thumb.w}px`;
    cell.style.height = `${s.thumb.h}px`;
    cell.dataset.id = String(s.id);

    const row = document.createElement("div");
    row.className = "slot-label";
    const title = document.createElement("span");
    title.className = "slot-title";
    title.textContent = s.title;
    row.appendChild(title);
    if (s.minimized) {
      const chip = document.createElement("span");
      chip.className = "slot-min";
      chip.textContent = "Minimized";
      row.appendChild(chip);
    }
    cell.appendChild(row);

    // the live DWM thumbnail composites natively over this area
    const area = document.createElement("div");
    area.className = "slot-thumb";
    cell.appendChild(area);

    cell.addEventListener("mouseenter", () => cell.classList.add("hot"));
    cell.addEventListener("mouseleave", () => cell.classList.remove("hot"));
    cell.addEventListener("click", () => {
      invoke("preview_activate", { id: s.id }).catch(() => {});
    });
    // Right-click opens the SAME context menu as a taskbar icon (screen
    // centre, identical sheet) acting on this one window: ids carries the
    // single hwnd, so Close closes just it and End task ends just its pid.
    cell.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      invoke("task_menu", { ids: [s.id] }).catch(() => {});
    });
    return cell;
  });

  if (overflow > 0 && badge) {
    const cell = document.createElement("div");
    cell.className = "slot slot-badge";
    cell.style.left = `${badge.x}px`;
    cell.style.top = `${badge.y}px`;
    cell.style.width = `${badge.w}px`;
    cell.style.height = `${badge.h}px`;
    const num = document.createElement("div");
    num.className = "badge-num";
    num.textContent = `+${overflow}`;
    const lbl = document.createElement("div");
    lbl.className = "badge-label";
    lbl.textContent = overflow === 1 ? "window" : "windows";
    cell.appendChild(num);
    cell.appendChild(lbl);
    cell.addEventListener("mouseenter", () => cell.classList.add("hot"));
    cell.addEventListener("mouseleave", () => cell.classList.remove("hot"));
    cell.addEventListener("click", () => {
      invoke("preview_overflow").catch(() => {});
    });
    cells.push(cell);
  }

  gridEl.replaceChildren(...cells);
  cardEl.classList.add("in");
});

listen("preview:hide", () => {
  gridEl.replaceChildren();
});

// no default webview menu anywhere on the card — right-click is always ours
// (slot menus above) or nothing
document.addEventListener("contextmenu", (e) => e.preventDefault());
