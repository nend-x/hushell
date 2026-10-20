/* The shell is dark-only: every window paints the dark palette from the
   first frame. Kept as a file so every HTML sheet keeps its one script tag;
   the light theme and the switch live only in history. */
(function () {
  document.documentElement.dataset.theme = "dark";
})();
