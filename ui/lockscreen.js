const T = window.__TAURI__ || {};
const invoke =
  T.core && T.core.invoke ? T.core.invoke.bind(T.core) : function () { return Promise.resolve(); };
const listen =
  T.event && T.event.listen ? T.event.listen.bind(T.event) : function () { return Promise.resolve(); };

const clockEl = document.getElementById("clock");
const dateEl = document.getElementById("date");
const form = document.getElementById("unlock-form");
const input = document.getElementById("password");

// ---------- clock ----------

function pad(n) {
  return String(n).padStart(2, "0");
}

function tickClock() {
  const d = new Date();
  clockEl.textContent = pad(d.getHours()) + ":" + pad(d.getMinutes());
  dateEl.textContent = d.toLocaleDateString(undefined, {
    weekday: "long",
    day: "numeric",
    month: "long",
  });
}

tickClock();
setInterval(tickClock, 1000);

// ---------- show / hide ----------

listen("lockscreen:show", () => {
  input.value = "";
  input.classList.remove("bad");
  setTimeout(() => input.focus(), 0);
}).catch(() => {});

listen("lockscreen:hide", () => {
  input.value = "";
  form.classList.remove("shake");
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
      // unlocked: the backend hid the window; park the form until next time
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
