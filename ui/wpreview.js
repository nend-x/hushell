/* wpreview.js — THE widget preview renderer (shared by hushtop + startmenu).
 *
 * The old previews were hand-drawn abstract mocks (`wg-prev` ink bars). They
 * read as blank boxes no matter how much ink was poured in, and they could
 * never look like the widget they stood for. This renderer is the full
 * remake: it builds the widget's REAL body — the exact DOM the desktop
 * cards use (vol-readout, st-rows, media-cd, fx-row, sp-seg ...) — with
 * static stub data. A preview is the widget, not a drawing of it.
 *
 *   hushWidgetPreview(kind) -> DocumentFragment of the body children
 *   hushWidgetDims(kind)    -> [w, h] natural px (the card's real size)
 *
 * The fragment is INERT by construction: no invoke(), no listeners, no
 * timers, no polling. Call sites drop it into a real .notepad card (ghost,
 * wipe previews) or a .wprev mini card (start-menu gallery); containers own
 * the pointer-events discipline (ghost + wipe layers are pointer-events:
 * none, the gallery sets it on .wprev).
 */
(function () {
  "use strict";

  // The real card sizes (widget_add defaults / ghost_size).
  var DIMS = {
    notes: [252, 180],
    volume: [252, 108],
    clipboard: [252, 252],
    stats: [216, 144],
    lock: [72, 72],
    media: [252, 252],
    lyrics: [252, 288],
    spaces: [216, 72],
    brightness: [252, 108],
    effects: [216, 72],
  };

  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }

  function frag() {
    return document.createDocumentFragment();
  }

  // ---- volume (also the base of brightness) ----------------------------

  function volumeFrag(pct) {
    var f = frag();
    var readout = el("div", "vol-readout");
    readout.appendChild(el("span", "vol-pct", pct + "%"));
    readout.appendChild(el("span", "vol-state", "master"));
    var slider = el("input", "vol-slider vol-main");
    slider.type = "range";
    slider.min = 0;
    slider.max = 100;
    slider.value = pct;
    slider.disabled = true;
    slider.tabIndex = -1;
    f.appendChild(readout);
    f.appendChild(slider);
    f.appendChild(el("div", "vol-ruler"));
    var foot = el("div", "vol-foot");
    var mute = el("button", "vol-btn vol-mute", "Mute");
    mute.type = "button";
    mute.disabled = true;
    if (pct === 0) mute.classList.add("is-muted");
    var spacer = el("span", "vol-flex");
    var mixer = el("button", "vol-btn vol-arrow");
    mixer.type = "button";
    mixer.disabled = true;
    mixer.innerHTML = 'Mixer<span class="chev">\u25be</span>';
    foot.appendChild(mute);
    foot.appendChild(spacer);
    foot.appendChild(mixer);
    f.appendChild(foot);
    return f;
  }

  function brightnessFrag() {
    var f = frag();
    var readout = el("div", "vol-readout");
    var sun = el("span", "br-sun");
    sun.innerHTML =
      '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round">' +
      '<circle cx="12" cy="12" r="4.4"/><path d="M12 2.6v2.4M12 19v2.4M2.6 12h2.4M19 12h2.4M5 5l1.7 1.7M17.3 17.3 19 19M19 5l-1.7 1.7M6.7 17.3 5 19"/></svg>';
    readout.appendChild(sun);
    readout.appendChild(el("span", "vol-pct", "72%"));
    readout.appendChild(el("span", "vol-state", "display"));
    var slider = el("input", "vol-slider vol-main br-slider");
    slider.type = "range";
    slider.min = 0;
    slider.max = 100;
    slider.value = 72;
    slider.disabled = true;
    slider.tabIndex = -1;
    f.appendChild(readout);
    f.appendChild(slider);
    f.appendChild(el("div", "vol-ruler br-ruler"));
    return f;
  }

  // ---- clipboard (the stub shows one example tile) -----------------------

  var CLIP_STUBS = ["https://google.com"];

  function clipboardFrag() {
    var f = frag();
    var list = el("div", "cb-list");
    for (var i = 0; i < CLIP_STUBS.length; i++) {
      var tile = el("button", "cb-tile", CLIP_STUBS[i]);
      tile.type = "button";
      tile.disabled = true;
      tile.tabIndex = -1;
      list.appendChild(tile);
    }
    f.appendChild(list);
    return f;
  }

  // ---- stats -------------------------------------------------------------

  function statsFrag() {
    var f = frag();
    var rows = el("div", "st-rows");
    var data = [
      ["CPU", "34%", 34],
      ["RAM", "52%", 52],
      ["Battery", "78%", 78],
    ];
    for (var i = 0; i < data.length; i++) {
      var row = el("div", "st-row");
      var head = el("div", "st-head");
      head.appendChild(el("span", "st-label", data[i][0]));
      var pct = el("span", "st-pct", data[i][1]);
      head.appendChild(pct);
      var track = el("div", "st-track");
      var fill = el("div", "st-fill");
      fill.style.width = data[i][2] + "%";
      track.appendChild(fill);
      row.appendChild(head);
      row.appendChild(track);
      rows.appendChild(row);
    }
    f.appendChild(rows);
    return f;
  }

  // ---- lock --------------------------------------------------------------

  function lockFrag() {
    var f = frag();
    var btn = el("button", "lock-btn");
    btn.type = "button";
    btn.disabled = true;
    btn.tabIndex = -1;
    var glyph = el("span", "lock-glyph");
    glyph.innerHTML = '<i class="shackle"></i><i class="body"></i><i class="keyhole"></i>';
    btn.appendChild(glyph);
    f.appendChild(btn);
    return f;
  }

  // ---- media (the CD look — grooved disc, controls, title/artist) -------

  function mediaFrag() {
    var f = frag();

    var cd = el("div", "media-cd");
    // the blank pressing: no art img in the preview — the grooved nude
    // disc is the whole face (a srcless <img> used to sit over it)
    var disc = el("div", "media-disc nude");
    var sheen = el("div", "media-disc-sheen");
    var hole = el("div", "media-disc-hole");
    disc.appendChild(sheen);
    disc.appendChild(hole);
    cd.appendChild(disc);
    f.appendChild(cd);

    var ctrl = el("div", "media-ctrl");
    var prev = el("button", "media-btn");
    prev.type = "button";
    prev.disabled = true;
    prev.innerHTML =
      '<svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor" stroke="none"><path d="M6 5h2v14H6zM20 5v14L9 12z"/></svg>';
    var play = el("button", "media-btn play");
    play.type = "button";
    play.disabled = true;
    play.innerHTML =
      '<svg viewBox="0 0 24 24" width="17" height="17" fill="currentColor" stroke="none"><path d="M7 5h3.4v14H7zM13.6 5H17v14h-3.4z"/></svg>';
    var next = el("button", "media-btn");
    next.type = "button";
    next.disabled = true;
    next.innerHTML =
      '<svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor" stroke="none"><path d="M16 5h2v14h-2zM4 5l11 7L4 19z"/></svg>';
    ctrl.appendChild(prev);
    ctrl.appendChild(play);
    ctrl.appendChild(next);
    f.appendChild(ctrl);

    var info = el("div", "media-info");
    info.appendChild(el("div", "media-title", "Midnight City"));
    info.appendChild(el("div", "media-artist", "M83 \u2014 Hurry Up, We're Dreaming"));
    f.appendChild(info);
    return f;
  }

  // ---- spaces (single-display look: the 1 | 2 pair) ------------------------

  function spacesFrag() {
    var f = frag();
    var seg = el("div", "sp-seg");
    for (var i = 0; i < 2; i++) {
      var b = el("button", "sp-btn", String(i + 1));
      b.type = "button";
      b.disabled = true;
      b.tabIndex = -1;
      if (i === 0) b.classList.add("active");
      seg.appendChild(b);
    }
    f.appendChild(seg);
    return f;
  }

  // ---- effects (snow active; leaves sits in the middle slot) --------------

  var FX_SVG = {
    snow:
      '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round">' +
      '<path d="M12 3v18M4.2 7.5l15.6 9M19.8 7.5l-15.6 9M12 3l-2 2M12 3l2 2M12 21l-2-2M12 21l2-2M4.2 7.5l.5 2.7M4.2 7.5l2.7-.6M19.8 16.5l-.5-2.7M19.8 16.5l-2.7.6M19.8 7.5l-2.7-.6M19.8 7.5l-.5 2.7M4.2 16.5l2.7.6M4.2 16.5l.5-2.7"/></svg>',
    leaves:
      // the leaf from the falling-leaves reference — same glyph as the widget
      // (tilted 45°)
      '<svg viewBox="0 0 24 32" width="12" height="16" fill="currentColor" stroke="none" style="transform:rotate(45deg)">' +
      '<path d="M12 0 C 24 8, 24 22, 12 32 C 0 22, 0 8, 12 0 Z"/>' +
      '<path class="lv" d="M12 3 L12 29" stroke-width="1" fill="none"/>' +
      '<path class="lv" d="M12 10 L7 14 M12 10 L17 14 M12 18 L6 22 M12 18 L18 22" stroke-width="0.8" fill="none"/>' +
      '</svg>',
    off:
      '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round">' +
      '<circle cx="12" cy="12" r="8.2"/><path d="M6.2 17.8 17.8 6.2"/></svg>',
  };

  function effectsFrag() {
    var f = frag();
    var row = el("div", "fx-row");
    // wind is NOT in the preview — the farm is exclusive to the lock screen
    var kinds = ["snow", "leaves", "off"];
    for (var i = 0; i < kinds.length; i++) {
      var b = el("button", "fx-btn");
      b.type = "button";
      b.disabled = true;
      b.tabIndex = -1;
      b.innerHTML = FX_SVG[kinds[i]];
      if (kinds[i] === "snow") b.classList.add("active");
      row.appendChild(b);
    }
    f.appendChild(row);
    f.appendChild(el("div", "fx-cap", "snow"));
    return f;
  }

  // ---- lyrics (the synced look — stub track, one line lifted to ink) -----

  function lyricsFrag() {
    var f = frag();
    var head = el("div", "ly-head");
    head.appendChild(el("div", "ly-head-title", "Midnight City"));
    head.appendChild(el("div", "ly-head-artist", "M83 \u2014 Hurry Up, We're Dreaming"));
    f.appendChild(head);
    f.appendChild(el("div", "ly-note", "matched on 2 sites \u00b7 synced"));
    var body = el("div", "ly-body");
    var list = el("div", "ly-list");
    var lines = [
      "Midnight, the city holds its breath",
      "Neon spills across the empty street",
      "I was walking through the midnight rain",
      "Streetlights blink along the avenue",
      "Waiting for a ride in the dark",
      "The clock strikes midnight once again",
      "City of light, city of night",
      "Midnight keeps the whole town still",
      "Waiting for the morning light",
      "Around the corner of the street",
    ];
    for (var i = 0; i < lines.length; i++) {
      var d = el("div", "ly-line", lines[i]);
      if (i === 2) d.classList.add("on");
      list.appendChild(d);
    }
    body.appendChild(list);
    f.appendChild(body);
    return f;
  }

  // ---- notes (the paper itself) -------------------------------------------

  function notesFrag() {
    var f = frag();
    var ta = document.createElement("textarea");
    ta.readOnly = true;
    ta.tabIndex = -1;
    ta.value = "drag me onto the desktop\n\nanything typed here stays on the paper.";
    f.appendChild(ta);
    return f;
  }

  // ---- dispatcher ----------------------------------------------------------

  var BUILDERS = {
    notes: notesFrag,
    volume: function () { return volumeFrag(64); },
    brightness: brightnessFrag,
    clipboard: clipboardFrag,
    stats: statsFrag,
    lock: lockFrag,
    media: mediaFrag,
    lyrics: lyricsFrag,
    spaces: spacesFrag,
    effects: effectsFrag,
  };

  window.hushWidgetPreview = function (kind) {
    var build = BUILDERS[kind] || statsFrag;
    return build();
  };

  window.hushWidgetDims = function (kind) {
    return (DIMS[kind] || [216, 144]).slice();
  };
})();
