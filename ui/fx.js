/* The effects engine — ONE renderer, two stages.
 *
 * The desktop (hushtop.js) and the lock screen (lockscreen.js) mount this
 * engine on their own canvases and both get the exact same snow / leaves /
 * star / wind scene. The desktop carries two canvases: the LANDSCAPE (#fxback —
 * the wind farm, wallpaper that never covers the user's stuff) and the
 * FOREGROUND (#fx — snow / stars above everything). The lock screen has no
 * icons to protect, so it passes the SAME element as back and front and the
 * scene simply paints in the one canvas.
 *
 * set(kind) coerces anything unknown (and the retired "rain") to "none",
 * drives the raf loop, and returns the coerced kind so the caller can sync
 * its own UI. pause()/resume() FREEZE the running scene without destroying
 * it — the raf loop stops, the particles keep their positions (and the
 * canvas keeps its last frame) and resume() continues exactly where the
 * freeze happened, so a stage hand-off (the lock covering the desktop)
 * never reads as a respawn. kick() force-restarts the loop. Snow/star draw
 * as the dark-shell ink tone; leaves carry the reference art's own palette
 * (the gray leaf with paper-tone veins); the wind farm carries its tier
 * palette (the sheet fills #1b1b1f and occludes). The WIND FARM is the lock
 * screen's scene — the desktop gates it away (hushtop.js coerces wind to
 * none), the engine itself stays neutral.
 */
window.hushFx = (function () {
  let back = null;
  let front = null;
  let bkCtx = null;
  let ctx = null;
  let kind = "none";
  let parts = [];
  let raf = 0;
  let prev = 0;
  const RGB = "236,234,222"; // dark-shell ink — the particle tone

  function resize() {
    if (!back || !front) return;
    const dpr = Math.max(1, window.devicePixelRatio || 1);
    front.width = Math.round(innerWidth * dpr);
    front.height = Math.round(innerHeight * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    back.width = Math.round(innerWidth * dpr);
    back.height = Math.round(innerHeight * dpr);
    bkCtx.setTransform(dpr, 0, 0, dpr, 0, 0);
    if (kind !== "none") spawn();
  }

  function spawn() {
    const area = innerWidth * innerHeight;
    parts = [];
    if (kind === "snow") {
      const n = Math.min(150, Math.max(30, Math.round(area / 16000)));
      for (let i = 0; i < n; i++) {
        const x = Math.random() * innerWidth;
        parts.push({
          x,
          bx: x,
          y: Math.random() * innerHeight * 1.4 - innerHeight * 0.4,
          r: 1.2 + Math.random() * 2.6,
          vy: 16 + Math.random() * 42,
          amp: 8 + Math.random() * 30,
          ph: Math.random() * Math.PI * 2,
          ds: 0.3 + Math.random() * 0.9,
          flake: Math.random() < 0.22,
          rot: Math.random() * Math.PI * 2,
          rv: (Math.random() - 0.5) * 2.2,
          a: 0.35 + Math.random() * 0.45,
        });
      }
    } else if (kind === "star") {
      // drifting meteors — deliberately slow (the old speeds read as rushing)
      const n = Math.min(34, Math.max(10, Math.round(area / 52000)));
      for (let i = 0; i < n; i++) {
        const dir = Math.random() < 0.5 ? -1 : 1;
        parts.push({
          x: Math.random() * innerWidth,
          y: Math.random() * innerHeight * 1.3 - innerHeight * 0.3,
          vx: dir * (24 + Math.random() * 52),
          vy: 44 + Math.random() * 80,
          size: 1.6 + Math.random() * 2.0,
          trail: 22 + Math.random() * 40,
          spin: Math.random() < 0.3,
          rot: Math.random() * Math.PI * 2,
          rv: (Math.random() - 0.5) * 1.6,
          a: 0.45 + Math.random() * 0.4,
        });
      }
    } else if (kind === "leaves") {
      // Falling leaves — the reference HTML transcribed to the engine. The
      // example's numbers are kept verbatim: COUNT = max(25, W/30), width
      // 14–32px, opacity 0.4–1, fall 10–20s (0→110vh, spin ±480deg), sway
      // alternate ±drift(20–60px) ±30deg over 2.5–5s, negative delay so the
      // sheet opens mid-fall. Reduced motion slows the whole sky ×2.5, like
      // the farm.
      const slow = matchMedia("(prefers-reduced-motion: reduce)").matches ? 2.5 : 1;
      const n = Math.max(25, Math.round(innerWidth / 30));
      for (let i = 0; i < n; i++) {
        const d = (10 + Math.random() * 10) * slow; // fall duration 10–20s
        parts.push({
          x0: Math.random() * 0.98 * innerWidth, // --x: 0..98vw
          s: 14 + Math.random() * 18, // width 14–32px (height = s * 32/24)
          o: 0.4 + Math.random() * 0.6, // --o
          d,
          off: -Math.random() * d, // --delay: negative → already falling
          sw: (2.5 + Math.random() * 2.5) * slow, // --sw 2.5–5s
          drift: 20 + Math.random() * 40, // --drift 20–60px
          spin: ((Math.random() * 960 - 480) * Math.PI) / 180, // --spin ±480deg
        });
      }
    } else if (kind === "wind") {
      // The turbine farm: one procedural scene rebuilt for ANY viewport. The
      // reference art lives in a 1200x600 frame — it stretches to the sheet
      // (x * W/1200, y * H/600) and every turbine anchors bottom-center,
      // scaling with the sheet height, so the composition holds its
      // proportions at any resolution.
      const slow = matchMedia("(prefers-reduced-motion: reduce)").matches ? 2.5 : 1;
      const mk = (tier, left, bottom, s, dur, fg, mid) => ({
        tier, // 0 far / 1 mid / 2 near — each ridge paints AFTER the tier behind it
        x: left * innerWidth,
        y: (1 - bottom) * innerHeight,
        s: s * (innerHeight / 600),
        dur: dur * slow,
        a0: Math.random() * Math.PI * 2, // desync the rotors
        fg,
        mid,
      });
      parts = [
        // far ridge — dim, slow
        mk(0, 0.15, 0.49, 0.28, 5.0, "#5a5a5a", "#4a4a4a"),
        mk(0, 0.42, 0.52, 0.26, 5.0, "#5a5a5a", "#4a4a4a"),
        mk(0, 0.8, 0.5, 0.3, 5.0, "#5a5a5a", "#4a4a4a"),
        // middle ridge
        mk(1, 0.28, 0.34, 0.5, 4.0, "#8c8c8c", "#707070"),
        mk(1, 0.7, 0.33, 0.5, 4.0, "#8c8c8c", "#707070"),
        // near ridge — bright, brisk
        mk(2, 0.08, 0.16, 1.0, 3.2, "#e6e6e6", "#b3b3b3"),
        mk(2, 0.9, 0.14, 0.9, 3.2, "#e6e6e6", "#b3b3b3"),
      ];
    }
  }

  // The wind farm's three ridges — the reference SVG path data transcribed to
  // beziers (M … C … S … S … L1200 600 L0 600 Z, the S commands expanded by
  // reflection). Coordinates live in the 1200x600 frame and stretch to the
  // sheet (preserveAspectRatio="none", exactly like the reference).
  const HILLS = [
    {
      stroke: "#555555",
      m: [0, 320],
      segs: [
        [150, 280, 280, 300, 420, 310],
        [560, 320, 700, 270, 850, 300],
        [1000, 330, 1100, 330, 1200, 300],
      ],
    },
    {
      stroke: "#888888",
      m: [0, 390],
      segs: [
        [180, 350, 300, 380, 480, 370],
        [660, 360, 760, 340, 920, 380],
        [1080, 420, 1120, 400, 1200, 370],
      ],
    },
    {
      stroke: "#bbbbbb",
      m: [0, 470],
      segs: [
        [200, 430, 380, 460, 560, 450],
        [740, 440, 860, 420, 1000, 455],
        [1140, 490, 1150, 480, 1200, 460],
      ],
    },
  ];

  const PAPER = "#1b1b1f"; // the sheet — ridges fill with it and occlude

  function hill(hill, W, H) {
    const sx = W / 1200;
    const sy = H / 600;
    const c = bkCtx;
    c.beginPath();
    c.moveTo(hill.m[0] * sx, hill.m[1] * sy);
    for (const s of hill.segs) {
      c.bezierCurveTo(s[0] * sx, s[1] * sy, s[2] * sx, s[3] * sy, s[4] * sx, s[5] * sy);
    }
    c.lineTo(W + 2, H + 2); // L1200 600 — the fill body runs off the edges
    c.lineTo(-2, H + 2); // L0 600
    c.closePath();
    c.fillStyle = PAPER;
    c.fill();
    c.strokeStyle = hill.stroke;
    c.lineWidth = 1.5;
    c.stroke();
  }

  // One turbine: a bottom-anchored 300x420 box scaled by t.s — tapered
  // tower, stadium nacelle, three tapered blades + hub spinning about the
  // rotor center, tier colors carried in t.
  function turbine(t, tSec) {
    const c = bkCtx;
    c.save();
    c.translate(t.x, t.y);
    c.scale(t.s, t.s);
    // tower — 18 wide, 270 tall, clipped to polygon(20% 0, 80% 0, 100% 100%, 0 100%)
    c.fillStyle = t.mid;
    c.beginPath();
    c.moveTo(-5.4, -270);
    c.lineTo(5.4, -270);
    c.lineTo(9, 0);
    c.lineTo(-9, 0);
    c.closePath();
    c.fill();
    // nacelle — 40x24, rounded 12, at 128..152 from the box top
    c.fillStyle = t.fg;
    c.beginPath();
    if (c.roundRect) c.roundRect(-20, -292, 40, 24, 12);
    else c.rect(-20, -292, 40, 24);
    c.fill();
    // rotor — spins about (0, -280); three blades rotated a third of a turn
    c.save();
    c.translate(0, -280);
    c.rotate((tSec / t.dur) * Math.PI * 2 + t.a0);
    c.fillStyle = t.fg;
    for (let k = 0; k < 3; k++) {
      c.save();
      c.rotate((k * Math.PI * 2) / 3);
      c.beginPath();
      c.moveTo(-11, 0);
      c.lineTo(11, 0);
      c.lineTo(0, -124);
      c.closePath();
      c.fill();
      c.restore();
    }
    c.fillStyle = t.mid;
    c.beginPath();
    c.arc(0, 0, 11, 0, Math.PI * 2);
    c.fill();
    c.restore();
    c.restore();
  }

  function loop(ts) {
    raf = requestAnimationFrame(loop);
    const dt = Math.min(0.05, (ts - prev) / 1000 || 0.016);
    prev = ts;
    if (kind === "wind") {
      // the landscape layer, painted in the reference's exact order: each
      // ridge draws right after the tier it stands in front of, so a ridge
      // occludes the turbines behind it (the fills are the sheet color)
      bkCtx.clearRect(0, 0, innerWidth, innerHeight);
      const W = innerWidth;
      const H = innerHeight;
      const tSec = ts / 1000;
      for (let i = 0; i < HILLS.length; i++) {
        hill(HILLS[i], W, H);
        for (const tr of parts) {
          if (tr.tier === i) turbine(tr, tSec);
        }
      }
      return;
    }
    ctx.clearRect(0, 0, innerWidth, innerHeight);
    const H = innerHeight + 12;
    if (kind === "snow") {
      for (const p of parts) {
        p.y += p.vy * dt;
        p.ph += p.ds * dt;
        p.x = p.bx + Math.sin(p.ph) * p.amp;
        if (p.flake) p.rot += p.rv * dt;
        if (p.y > H) {
          p.y = -12;
          p.bx = Math.random() * innerWidth;
        }
        ctx.fillStyle = `rgba(${RGB},${p.a})`;
        if (p.flake) {
          ctx.save();
          ctx.translate(p.x, p.y);
          ctx.rotate(p.rot);
          ctx.strokeStyle = `rgba(${RGB},${p.a})`;
          ctx.lineWidth = 1;
          const L = p.r * 2.1;
          ctx.beginPath();
          for (let i = 0; i < 6; i++) {
            const a = (Math.PI / 3) * i;
            ctx.moveTo(0, 0);
            ctx.lineTo(Math.cos(a) * L, Math.sin(a) * L);
          }
          ctx.stroke();
          ctx.restore();
        } else {
          ctx.beginPath();
          ctx.arc(p.x, p.y, p.r, 0, Math.PI * 2);
          ctx.fill();
        }
      }
    } else if (kind === "leaves") {
      // One leaf = the reference SVG (viewBox 0 0 24 32): a vesica body,
      // a center vein and four side veins. The example's two animations
      // collapse into closed-form time math — fall (linear, y −10%→110%,
      // rotation 0→spin) and sway (alternate, cosine stands in for the
      // ease-in-out flip, x/rotation stay in phase exactly like the CSS
      // pair). Every leaf loops its own path; the x base re-rolls on wrap
      // so no visible retracing.
      for (const p of parts) {
        const t = ts / 1000 + p.off;
        const ft = ((t % p.d) + p.d) % p.d / p.d; // 0..1 fall progress
        const u = ((((t / p.sw) % 2) + 2) % 2); // 0..2 sway cycle
        const cph = Math.cos(Math.PI * u); // 1 → −1 → 1 (the alternate)
        const y = (-0.1 + 1.2 * ft) * innerHeight;
        const x = p.x0 - p.drift * cph;
        const rot = p.spin * ft - (Math.PI / 6) * cph; // fall + sway ∓30°, in phase
        if (ft < 0.02 && p.wrapped !== true) {
          // just wrapped — re-roll the column so the same path never repeats
          p.x0 = Math.random() * 0.98 * innerWidth;
          p.wrapped = true;
        } else if (ft >= 0.02) {
          p.wrapped = false;
        }
        ctx.save();
        ctx.translate(x, y);
        ctx.rotate(rot);
        ctx.scale(p.s / 24, p.s / 24);
        ctx.globalAlpha = p.o;
        // body — M12 0 C 24 8, 24 22, 12 32 C 0 22, 0 8, 12 0 Z
        ctx.fillStyle = "#9a9a9a";
        ctx.beginPath();
        ctx.moveTo(12, 0);
        ctx.bezierCurveTo(24, 8, 24, 22, 12, 32);
        ctx.bezierCurveTo(0, 22, 0, 8, 12, 0);
        ctx.closePath();
        ctx.fill();
        // veins — the reference strokes, in local units like the SVG
        ctx.strokeStyle = "#1b1b1f"; // the paper — reads as the example's #000
        ctx.lineCap = "round";
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.moveTo(12, 3);
        ctx.lineTo(12, 29);
        ctx.stroke();
        ctx.lineWidth = 0.8;
        ctx.beginPath();
        ctx.moveTo(12, 10); ctx.lineTo(7, 14);
        ctx.moveTo(12, 10); ctx.lineTo(17, 14);
        ctx.moveTo(12, 18); ctx.lineTo(6, 22);
        ctx.moveTo(12, 18); ctx.lineTo(18, 22);
        ctx.stroke();
        ctx.restore();
      }
    } else if (kind === "star") {
      for (const p of parts) {
        p.x += p.vx * dt;
        p.y += p.vy * dt;
        if (p.spin) p.rot += p.rv * dt;
        if (p.y > H || p.x < -90 || p.x > innerWidth + 90) {
          p.x = Math.random() * innerWidth;
          p.y = -20 - Math.random() * innerHeight * 0.4;
          const dir = Math.random() < 0.5 ? -1 : 1;
          p.vx = dir * (24 + Math.random() * 52);
          p.vy = 44 + Math.random() * 80;
        }
        const sp = Math.hypot(p.vx, p.vy);
        const tx = (p.vx / sp) * p.trail;
        const ty = (p.vy / sp) * p.trail;
        const g = ctx.createLinearGradient(p.x - tx, p.y - ty, p.x, p.y);
        g.addColorStop(0, `rgba(${RGB},0)`);
        g.addColorStop(1, `rgba(${RGB},${p.a})`);
        ctx.strokeStyle = g;
        ctx.lineWidth = 1.4;
        ctx.beginPath();
        ctx.moveTo(p.x - tx, p.y - ty);
        ctx.lineTo(p.x, p.y);
        ctx.stroke();
        // the head: a 4-point spark, spinning on the ones that spin
        ctx.save();
        ctx.translate(p.x, p.y);
        ctx.rotate(p.spin ? p.rot : Math.atan2(p.vy, p.vx));
        ctx.strokeStyle = `rgba(${RGB},${Math.min(1, p.a + 0.25)})`;
        ctx.lineWidth = 1.2;
        ctx.beginPath();
        ctx.moveTo(-p.size, 0); ctx.lineTo(p.size, 0);
        ctx.moveTo(0, -p.size); ctx.lineTo(0, p.size);
        ctx.stroke();
        ctx.restore();
      }
    }
    if (kind === "none") cancelAnimationFrame(raf);
  }

  function set(next) {
    const k = ["snow", "leaves", "star", "wind"].includes(next) ? next : "none";
    if (k === kind) return k;
    kind = k;
    resize();
    if (k === "none") {
      cancelAnimationFrame(raf);
      raf = 0;
      if (ctx) ctx.clearRect(0, 0, front.width, front.height);
      if (bkCtx) bkCtx.clearRect(0, 0, back.width, back.height);
    } else if (!raf) {
      prev = performance.now();
      raf = requestAnimationFrame(loop);
    }
    return k;
  }

  function kick() {
    if (kind === "none") return;
    // Force-restart the render loop: cancel whatever raf id is around (a
    // stale one included), respawn for the current kind and re-arm. The
    // caller uses this when the page may have been throttled/occluded
    // (the transparent lock overlay sits on top of the desktop during a
    // lock) — a loop that died under the overlay comes back here.
    if (raf) cancelAnimationFrame(raf);
    raf = 0;
    resize(); // respawns parts for the current kind and clears both canvases
    prev = performance.now();
    raf = requestAnimationFrame(loop);
  }

  function pause() {
    // Freeze — NOT stop: the loop stops, but kind, parts and the painted
    // frame all survive. The caller fades the canvas out via CSS while the
    // scene underneath holds still; resume() continues the SAME particles
    // from the SAME positions, so the hand-off never reads as a respawn.
    if (raf) {
      cancelAnimationFrame(raf);
      raf = 0;
    }
  }

  function resume() {
    if (kind === "none" || raf) return;
    const now = performance.now();
    // Snow/star integrate dt per frame, so the clamp (0.05 s) already
    // absorbs the whole gap. The leaves are closed-form wall-clock math
    // (t = now/1000 + off) — without a shift they would JUMP by the freeze
    // duration. Shifting every off back by the gap pins the effective time
    // to the frozen value: fall progress AND sway phase continue exactly
    // where the pause caught them.
    const gap = prev ? (now - prev) / 1000 : 0;
    if (gap > 0 && kind === "leaves") {
      for (const p of parts) p.off -= gap;
    }
    prev = now;
    raf = requestAnimationFrame(loop);
  }

  function mount(o) {
    back = o.back;
    front = o.front;
    bkCtx = back.getContext("2d");
    ctx = front.getContext("2d");
    resize();
    window.addEventListener("resize", resize);
  }

  return {
    mount,
    set,
    pause,
    resume,
    kick,
    kind: function () {
      return kind;
    },
  };
})();
