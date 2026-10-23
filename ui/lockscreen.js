const T = window.__TAURI__ || {};
const invoke =
  T.core && T.core.invoke ? T.core.invoke.bind(T.core) : function () { return Promise.resolve(); };
const listen =
  T.event && T.event.listen ? T.event.listen.bind(T.event) : function () { return Promise.resolve(); };

const clockEl = document.getElementById("clock");
const form = document.getElementById("unlock-form");
const input = document.getElementById("password");
const lockEl = document.getElementById("lock");
const fxEl = document.getElementById("fx");

// ---------- the lock screen's own effect ----------
// The SAME engine the desktop runs (ui/fx.js) — one canvas here serves the
// whole scene. The lock screen's scene is THE WIND FARM, always: it is
// exclusive to the lock screen (the desktop picker does not offer it), it
// has no off switch and no config field — locking IS the switch. The scene
// starts only when the lock animation is DONE (the sweep ends ~2.05s after
// show) and melts out (0.6s) when the unlock choreography begins — the
// engine stops only AFTER the fade, so the turbines never vanish mid-air.
// The .on class fades the canvas so the scene never pops in over the sweep.

hushFx.mount({ back: fxEl, front: fxEl });

const FX_LATE_MS = 2050; // icons fade (0.9s) + the sweep (0.55s delay + 1.5s)
let fxTimer = 0;

function startLockEffect() {
  clearTimeout(fxTimer);
  fxTimer = setTimeout(() => {
    hushFx.set("wind");
    fxEl.classList.add("on");
  }, FX_LATE_MS);
}

function stopLockEffect() {
  clearTimeout(fxTimer);
  fxEl.classList.remove("on"); // 0.6s fade-out — the turbines melt into the sheet
  // the engine clears only after the fade; a re-lock inside the window
  // cancels this timer and the scene continues uninterrupted
  fxTimer = setTimeout(() => hushFx.set("none"), 700);
}

// ---------- clock ----------

function pad(n) {
  return String(n).padStart(2, "0");
}

function tickClock() {
  const d = new Date();
  clockEl.textContent = pad(d.getHours()) + ":" + pad(d.getMinutes());
}

tickClock();
setInterval(tickClock, 1000);

// ---------- the OLED burn shield ----------
// A minute of lock-screen silence fades in a full-black frame — static
// bright pixels are what burn an OLED panel. It covers everything. Any
// KEYBOARD input fades it back out and restarts the idle timer; the mouse
// deliberately does neither (moving the pointer changes nothing on screen,
// so it must not wake the shield — the user asked for exactly this).

const burn = document.getElementById("burn");
const BURN_MS = 60000;
let burnTimer = 0;
let burnEnabled = true; // the options switch — read from the config at show

// the shield also takes the cursor with it: a lone pointer floating on the
// black frame is exactly the static artifact this shield exists to prevent
function setShield(on) {
  burn.classList.toggle("on", on);
  document.body.classList.toggle("burning", on);
}

function armBurn() {
  clearTimeout(burnTimer);
  if (!burnEnabled) return;
  burnTimer = setTimeout(() => setShield(true), BURN_MS);
}

function calmBurn() {
  if (burn.classList.contains("on")) {
    setShield(false); // base transition = the 0.6s fade-out
  }
  armBurn();
}

// keyboard is the only wake: keydown anywhere (the password field gets the
// same key — the shield just gets out of the way in parallel)
document.addEventListener("keydown", calmBurn, true);

// mouse input is noise here: no listener at all, it can neither wake the
// shield nor restart its timer

// ---------- show / hide ----------
// The desktop beneath this transparent window plays its own choreography
// (icons fade, the line curtain sweeps, the veil calms the sheet); this
// page only rises/fades its own stack in time with it.

listen("lockscreen:show", () => {
  input.value = "";
  input.classList.remove("bad");
  // fresh screen, fresh idle clock: shield off, timer re-armed per the
  // settings (the switch lives in the start menu's options view, which can
  // never be open while locked — reading it here is always fresh)
  clearTimeout(burnTimer);
  setShield(false);
  burnEnabled = true; // optimistic default until the setting round-trips
  invoke("lock_burn_get")
    .then((on) => {
      burnEnabled = !!on;
      armBurn(); // arms only when the shield is enabled
    })
    .catch(() => armBurn());
  // the desktop beneath suspended its own effect for the choreography —
  // this screen takes over: the options choice, after the animation
  startLockEffect();
  // restart the entrance choreography (classes are consumed on hide)
  lockEl.classList.remove("in", "out");
  void lockEl.offsetWidth;
  lockEl.classList.add("in");
  setTimeout(() => input.focus(), 0);
}).catch(() => {});

listen("lockscreen:hide", () => {
  input.value = "";
  form.classList.remove("shake");
  // the screen is going away: shield off NOW (cursor with it), timer dead,
  // and the lock screen's effect dies with it — the desktop brings its own
  // back once the sweep has crossed
  clearTimeout(burnTimer);
  setShield(false);
  stopLockEffect();
  // fade the stack out while the line sweeps back on the desktop; the
  // backend hides this window once the choreography has played out
  lockEl.classList.remove("in");
  lockEl.classList.add("out");
}).catch(() => {});

// Safety net for a missed show event (the webview may still be booting).
setTimeout(() => input.focus(), 300);

// ---------- unlock ----------

let busy = false;

form.addEventListener("submit", (e) => {
  e.preventDefault();
  if (busy) return;
  busy = true;
  const pw = input.value;
  invoke("lockscreen_unlock", { password: pw })
    .then(() => {
      // verified: the backend emits lockscreen:hide (this page fades out)
      // and restores everything once the sweep has passed
      input.value = "";
      input.classList.remove("bad");
      busy = false;
    })
    .catch(() => {
      // wrong password: one flat shake, the hairline goes error-red,
      // clear the field, stay
      busy = false;
      input.value = "";
      input.classList.add("bad");
      form.classList.remove("shake");
      void form.offsetWidth;
      form.classList.add("shake");
      setTimeout(() => input.focus(), 0);
    });
});

input.focus();
