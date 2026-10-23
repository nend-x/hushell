const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const slider = document.getElementById("slider");
const pct = document.getElementById("pct");
const mute = document.getElementById("mute");

let muted = false;
let touch = 0;          // last user gesture — the poll never fights the finger
let lastGet = -1;       // last applied master percent (dedupe)

window.addEventListener("error", (e) =>
  invoke("trayfly_diag", { msg: `volfly js error: ${e.message}` }).catch(() => {})
);

function label(v) {
  return Math.round(v * 100) + "%";
}

function applyMaster(v, push) {
  const n = Math.round(v * 100);
  slider.value = n;
  pct.textContent = label(v);
  lastGet = n;
  if (push) {
    touch = Date.now();
    invoke("audio_master_set", { vol: v }).catch(() => {});
    if (muted && n > 0) {
      muted = false;
      paintMute();
      invoke("audio_mute_set", { muted: false }).catch(() => {});
    }
  }
}

function paintMute() {
  mute.classList.toggle("is-muted", muted);
  mute.textContent = muted ? "Muted" : "Mute";
}

slider.addEventListener("input", () => {
  applyMaster(slider.value / 100, true);
});

mute.addEventListener("click", () => {
  muted = !muted;
  paintMute();
  touch = Date.now();
  invoke("audio_mute_set", { muted }).catch(() => {});
});

function applyState(st) {
  muted = !!st.muted;
  paintMute();
  const n = Math.round(st.vol * 100);
  if (n !== lastGet || pct.textContent === "--") applyMaster(st.vol, false);
}

// fast path: the native open rides a fresh snapshot
listen("volfly:state", (e) => applyState(e.payload)).catch(() => {});

// the widget on the desktop, a media key or any other surface that moves
// the master volume lands here instantly — no waiting for the poll
listen("audio:master", (e) => applyState(e.payload || {})).catch(() => {});

setInterval(() => {
  if (document.visibilityState !== "visible") return;
  if (Date.now() - touch < 1200) return;
  invoke("audio_state")
    .then((st) => applyState(st))
    .catch(() => {});
}, 600);

// boot + Escape
invoke("audio_state").then((st) => applyState(st)).catch(() => {});

window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") invoke("volfly_hide").catch(() => {});
});
