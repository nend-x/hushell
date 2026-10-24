/* The shell is dark-only: every window paints the dark palette from the
   first frame. Kept as a file so every HTML sheet keeps its one script tag;
   the light theme and the switch live only in history. */
(function () {
  document.documentElement.dataset.theme = "dark";
})();

/* No native webview context menu anywhere in the shell. Every page loads
   this file first, and the capture phase cannot be outrun by any handler
   registered later. The shell's own in-page menus (desktop, tasks, tray,
   widget chrome) run their own contextmenu handlers AFTER this one and keep
   working — preventDefault only suppresses the browser menu, the event
   still propagates. Text fields lose right-click paste; keyboard shortcuts
   remain the way in, by design. */
document.addEventListener(
  "contextmenu",
  (e) => e.preventDefault(),
  { capture: true }
);
