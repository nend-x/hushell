/* hushtop name dialog: rename | new folder | new file.
   Shown/positioned/activated natively; Enter = OK, Esc = Cancel. */

const { invoke } = window.__TAURI__.core;
const { getCurrentWindow } = window.__TAURI__.window;

const headEl = document.getElementById("pt-head");
const closeEl = document.getElementById("pt-close");
headEl.addEventListener("pointerdown", (e) => {
  if (e.target.closest("#pt-close")) return;
  getCurrentWindow().startDragging();
});
closeEl.addEventListener("click", () => {
  invoke("hushtop_prompt_hide").catch(() => {});
});
const { listen } = window.__TAURI__.event;

const titleEl = document.getElementById("pt-title");
const mainEl = document.getElementById("pt-main");
const extEl = document.getElementById("pt-ext");
const errEl = document.getElementById("pt-err");
const rowEl = document.getElementById("pt-row");
const msgEl = document.getElementById("pt-msg");
const okEl = document.getElementById("pt-ok");

let mode = "rename";
let currentName = "";
let busy = false;

const TITLES = {
  rename: "Rename",
  newfolder: "New folder",
  newfile: "New file",
  fresh: "Start fresh",
};

function applyState(st) {
  mode = st.mode || "rename";
  currentName = st.name || "";
  titleEl.textContent = TITLES[mode] || mode;
  // fresh = a plain confirm: no fields, the warning message instead
  const isFresh = mode === "fresh";
  extEl.classList.toggle("hidden", mode !== "newfile");
  rowEl.classList.toggle("hidden", isFresh);
  msgEl.classList.toggle("hidden", !isFresh);
  okEl.textContent = isFresh ? "Start fresh" : "OK";
  mainEl.value = mode === "rename" ? currentName : "";
  extEl.value = "";
  errEl.textContent = "";
  if (isFresh) return;
  setTimeout(() => mainEl.focus(), 40);
  // rename: select the stem (desktop convention — the extension stays)
  if (mode === "rename" && mainEl.value) {
    const dot = mainEl.value.lastIndexOf(".");
    const end = dot > 0 ? dot : mainEl.value.length;
    mainEl.setSelectionRange(0, end);
  }
}

listen("hushtop:prompt", (e) => applyState(e.payload)).catch(console.error);

// safety net for a missed event while this webview was still booting
(async () => {
  try {
    applyState(await invoke("hushtop_prompt_state"));
  } catch (e) {
    console.error(e);
  }
})();

function fail(msg) {
  errEl.textContent = typeof msg === "string" ? msg : "Something went wrong";
  busy = false;
}

async function submit() {
  if (busy) return;
  busy = true;
  const name = mainEl.value.trim();
  try {
    if (mode === "rename") {
      await invoke("hushtop_rename", { old: currentName, new: name });
    } else if (mode === "newfolder") {
      await invoke("hushtop_create", { kind: "folder", name, ext: "" });
    } else if (mode === "newfile") {
      await invoke("hushtop_create", { kind: "file", name, ext: extEl.value.trim() });
    } else if (mode === "fresh") {
      await invoke("shell_fresh_start");
    }
    await invoke("hushtop_prompt_hide");
    busy = false;
  } catch (e) {
    fail(String(e));
  }
}

function cancel() {
  invoke("hushtop_prompt_hide").catch(() => {});
  errEl.textContent = "";
}

document.getElementById("pt-ok").addEventListener("click", submit);
document.getElementById("pt-cancel").addEventListener("click", cancel);

document.addEventListener("keydown", (e) => {
  if (e.key === "Enter") submit();
  else if (e.key === "Escape") cancel();
});
