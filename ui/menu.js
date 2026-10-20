const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const menuEl = document.getElementById("menu");
let open = false;

function hide() {
  if (!open) return;
  open = false;
  menuEl.classList.add("hidden");
  // The native side hides the window and (when the focus never left it)
  // hands the foreground back to the app that was active before the open.
  invoke("task_menu_hide").catch(() => {});
}

listen("menu:show", (e) => {
  const items = e.payload.items || [];
  menuEl.replaceChildren();
  for (const it of items) {
    if (!it.t) {
      const s = document.createElement("div");
      s.className = "sep";
      menuEl.appendChild(s);
      continue;
    }
    const mi = document.createElement("div");
    mi.className = "mi";
    mi.textContent = it.t;
    mi.addEventListener("click", () => {
      open = false;
      menuEl.classList.add("hidden");
      invoke("task_menu_run", { action: it.a, ids: e.payload.ids || [] }).catch(() => {});
    });
    menuEl.appendChild(mi);
  }
  menuEl.classList.remove("hidden");
  open = true;
});

// NO blur close: focus drift (a background app waking up, the activation
// ladder losing a race) must never kill the menu. The contract is —
// disappear on a press outside (the native tick sees hardware presses;
// the margin handler below sees presses inside this window), on Escape,
// or on a menu item.
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") hide();
});

// Presses on the transparent margin around the sheet are outside presses.
document.body.addEventListener("pointerdown", (e) => {
  if (!menuEl.contains(e.target)) hide();
});
