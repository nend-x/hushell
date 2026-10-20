const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const iconEl = document.getElementById("wg-icon");
const nameEl = document.getElementById("wg-name");
const listEl = document.getElementById("wg-list");

function render(p) {
  if (!p) return;
  nameEl.textContent = p.name || "Windows";
  if (p.icon) {
    iconEl.src = "data:image/png;base64," + p.icon;
    iconEl.style.display = "";
  } else {
    iconEl.removeAttribute("src");
    iconEl.style.display = "none";
  }

  const items = p.items || [];
  listEl.replaceChildren(
    ...items.map((it) => {
      const row = document.createElement("div");
      row.className = "wg-row";
      const title = document.createElement("span");
      title.className = "wg-row-title";
      title.textContent = it.title || "(untitled window)";
      row.appendChild(title);
      if (it.minimized) {
        const chip = document.createElement("span");
        chip.className = "wg-row-min";
        chip.textContent = "Minimized";
        row.appendChild(chip);
      }
      row.addEventListener("click", () => {
        invoke("wingroup_activate", { id: it.id }).catch(() => {});
      });
      // Same contract as the preview card: right-click on a row opens the
      // taskbar-style context menu for that ONE window (Close included).
      row.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        invoke("task_menu", { ids: [it.id] }).catch(() => {});
      });
      return row;
    })
  );
  if (items.length === 0) {
    const empty = document.createElement("div");
    empty.className = "wg-empty";
    empty.textContent = "No windows left in this group";
    listEl.appendChild(empty);
  }
  // every fresh open starts at the top of the list
  listEl.scrollTop = 0;
}

listen("wingroup:show", (e) => render(e.payload)).catch(() => {});
// boot pull — covers a missed push no matter when this script ran
invoke("wingroup_data")
  .then((p) => render(p))
  .catch(() => {});

document.getElementById("wg-close").addEventListener("click", () => {
  invoke("wingroup_hide").catch(() => {});
});

// NO blur close: focus drift alone never closes the sheet — a press
// outside (native tick), the X, a row, or Escape does.
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    invoke("wingroup_hide").catch(() => {});
  }
});

// no default webview menu anywhere on the sheet — right-click is always
// ours (row menus above) or nothing
document.addEventListener("contextmenu", (e) => e.preventDefault());
