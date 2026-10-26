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
    // the 0.6s canvas fade brings the farm fully on — the LOCKING
    // choreography is now complete, so the lock's own AHK hooks take
    // over (the session hooks die with this call; the shell swaps them)
    setTimeout(() => invoke("hook_lock_on").catch(() => {}), 600);
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

// ---------- burn-in protection (config: lock_burn_mode) ----------
// A minute of lock-screen silence, then one of three things — the options
// dropdown owns the mode, this page reads it fresh at every show:
//   "oled"    — a full-black frame fades in (static bright pixels are what
//               burn an OLED panel). Any KEYBOARD input fades it back out
//               and restarts the idle clock; the mouse deliberately does
//               neither (moving the pointer changes nothing on screen, so
//               it must not wake the shield — the user asked for exactly
//               this).
//   "monitor" — the display itself is powered off (SC_MONITORPOWER via the
//               monitor_off command) and sleeps until input. Any input —
//               keys AND the mouse — wakes the screen physically, so here
//               the mouse re-arms the idle clock too: a mouse-wake with no
//               re-arm would leave the screen lit forever.
//   "none"    — protection off entirely.

const burn = document.getElementById("burn");
const pwWarn = document.getElementById("pw-warn");
const BURN_MS = 60000;
let burnTimer = 0;
let burnMode = "oled"; // optimistic default until the setting round-trips
// The page's own lock truth: true between the show bookkeeping and the hide
// bookkeeping. Input cannot reach a hidden page, so this flag is what keeps
// the burn machinery from ever running while the shell is unlocked.
let pageShown = false;

// the shield also takes the cursor with it: a lone pointer floating on the
// black frame is exactly the static artifact this shield exists to prevent
function setShield(on) {
  burn.classList.toggle("on", on);
  document.body.classList.toggle("burning", on);
}

function armBurn() {
  clearTimeout(burnTimer);
  // A stale arm attempt (the show's mode round-trip landing after the hide
  // already ran) must never leave a timer behind on a page nothing can calm.
  if (!pageShown) return;
  if (burnMode === "none") return;
  burnTimer = setTimeout(() => {
    if (burnMode === "monitor") {
      // ONE SHOT per arm: the display sleeps till input, and the wake input
      // re-arms the clock through calmBurn. The old self-re-arm here built
      // an immortal chain — on a page that stopped receiving input
      // (unlocked, hidden) nothing could ever calm it, and the monitor
      // went off every minute on the open desktop. Fire once; live input
      // earns the next cycle. (The backend also refuses monitor_off while
      // unlocked — belt and suspenders.)
      invoke("monitor_off").catch(() => {});
    } else {
      setShield(true);
    }
  }, BURN_MS);
}

function calmBurn() {
  if (burn.classList.contains("on")) {
    setShield(false); // base transition = the 0.6s fade-out
  }
  armBurn();
}

// keyboard always restarts the idle clock (the password field gets the
// same key — the shield just gets out of the way in parallel)
document.addEventListener("keydown", calmBurn, true);

// mouse input is noise for the OLED shield — no shield wake, no re-arm —
// but in monitor mode the display wakes on any mouse move, so the clock
// must follow or a mouse-wake would leave the screen lit forever
document.addEventListener(
  "mousemove",
  () => {
    if (burnMode === "monitor") calmBurn();
  },
  true
);

// ---------- show / hide ----------
// The desktop beneath this transparent window plays its own choreography
// (icons fade, the line curtain sweeps, the veil calms the sheet); this
// page only rises/fades its own stack in time with it.

// A duplicate show that lands right behind the real one (the
// lockscreen_hello re-emit racing a show the page did catch) must not
// restart the entrance — 800ms covers every honest ordering.
let lastShowAt = 0;

function runShow() {
  const now = Date.now();
  if (now - lastShowAt < 800) return;
  lastShowAt = now;
  pageShown = true;
  input.value = "";
  input.classList.remove("bad");
  pwWarn.hidden = true; // refreshed below — never a stale warning
  // fresh screen, fresh idle clock: shield off, timer re-armed per the
  // mode (the dropdown lives in the start menu's options view, which can
  // never be open while locked — reading it here is always fresh)
  clearTimeout(burnTimer);
  setShield(false);
  burnMode = "oled"; // optimistic default until the setting round-trips
  invoke("lock_burn_mode_get")
    .then((mode) => {
      burnMode = mode === "monitor" || mode === "none" ? mode : "oled";
      armBurn(); // arms only when a protection mode is set
    })
    .catch(() => armBurn());
  // the warning under the password line: shown only while no password
  // is configured (read fresh at every show — it can change in options
  // between locks)
  invoke("lock_password_get")
    .then((set) => {
      pwWarn.hidden = !!set;
    })
    .catch(() => {});
  // the desktop beneath suspended its own effect for the choreography —
  // this screen takes over: the options choice, after the animation
  startLockEffect();
  // restart the entrance choreography (classes are consumed on hide)
  lockEl.classList.remove("in", "out");
  void lockEl.offsetWidth;
  lockEl.classList.add("in");
  setTimeout(() => input.focus(), 0);
}

function runHide() {
  pageShown = false;
  input.value = "";
  form.classList.remove("shake");
  // the screen is going away: shield off NOW (cursor with it), timer dead,
  // and the lock screen's effect dies with it — the desktop brings its own
  // back once the sweep has crossed. Monitor mode: the defensive wake — a
  // no-op while the unlock keypress already lit the screen.
  clearTimeout(burnTimer);
  setShield(false);
  if (burnMode === "monitor") invoke("monitor_on").catch(() => {});
  stopLockEffect();
  // fade the stack out while the line sweeps back on the desktop; the
  // backend hides this window once the choreography has played out
  lockEl.classList.remove("in");
  lockEl.classList.add("out");
}

const showReady = listen("lockscreen:show", runShow);
const hideReady = listen("lockscreen:hide", runHide);

// Listener handshake: once BOTH listeners are registered, tell the backend
// (this page names itself — the boot-land show hold keys on THIS page's
// greeting, since the overlay must never be revealed before its entrance
// can play). If the shell booted straight into the lock while this webview
// was still loading, the show was HELD until now — this greeting releases
// it. A missed show mid-lock heals via the re-emit. A normal alt+l lock
// gets a no-op hello: nothing to heal.
Promise.all([showReady, hideReady])
  .catch(() => {})
  .then(() => invoke("lockscreen_hello", { page: "lockscreen" }).catch(() => {}));

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
