#!/usr/bin/env python3
"""gen_cursors.py — render the hushell cursor set (dot-grid, round 2).

The family, per the owner's feedback on round 1:
  circle.cur    the BASIC pointer — a white-outlined circle (translucent
                slate fill, soft shadow). The round-1 glint read as "some
                weird blue thing" behind the cursor — GONE, the basic
                pointer is deliberately static now.
  sq.cur        the CLICKABLE state — the white-outlined square rides the
                system hand role (every CSS cursor:pointer in the shell's
                webviews maps to OCR_HAND, so the morph costs zero IPC).
  ibeam.cur     the text caret — MINIMAL: a single rounded line (white
                core over a slate edge, so it reads on dark surfaces and
                white input fields alike). No serifs, no fill, no shadow.
  loading.ani   the LOADING state (OCR_APPSTARTING) — a SPINNING TRIANGLE
                as a self-animating .ani (12 frames × 4 jiffies ≈ 0.8 s
                per revolution). Windows plays it natively; it costs
                nothing while idle and no timer of ours ever runs.
  loading_static.cur  the triangle's rest frame — cursor.rs installs it
                on the loading role if Windows ever rejects the .ani
                (round 2's writer produced one it rejected, and the
                whole set silently fell back to stock).

The round-1 double-click blur-spin read as a tumbling cube — gone by
owner's call, so there are no motion/spin frame loops here anymore.

Everything renders 8x supersampled and downscales (LANCZOS). Every shape
centers on the pointer point — the hotspot is (16,16) and the square→
circle morph never makes the pointer jump.

.cur writer: ICONDIR(type 2) + ICONDIRENTRY(hotspot in wPlanes/wBitCount)
+ BITMAPINFOHEADER(biHeight = 2*h) + 32-bit BGRA XOR (bottom-up) + all-zero
1bpp AND mask. .ani writer: RIFF('ACON'){ anih, seq, rate,
LIST('fram'){ icon*.cur } } — the frame list MUST live inside a LIST chunk
(a bare fram chunk is skipped by Windows' parser: it found no frames and
LoadImageW rejected round 2's file outright).

Rate values are in JIFFIES (1/60 s) — the format's native unit, per the
classic ANIHEADER (jifRate) and rate-chunk definition. The "milliseconds
convention" in some generator docs is a myth: 60 written there reads as
60 jiffies = 1 s per frame (12 s per revolution).
"""
import math
import os
import struct

from PIL import Image, ImageDraw, ImageFilter

S = 8          # supersample
OUT = "/home/z/my-project/hushell-dev/src-tauri/cursors"
TRI_TMP = "/home/z/my-project/scripts/tri"   # .ani frame intermediates

OUTLINE = (255, 255, 255, 252)  # white outline — the family's stroke
FILL = (38, 40, 46, 150)        # translucent dark slate — contrast on white
SHADOW = (10, 12, 16, 60)       # soft baked shadow

C = 16.0            # the pointer point: every shape centers on it
SQUARE_SIDE = 13.0
SQUARE_R = 2.0      # square corner rounding
CIRCLE_D = 15.0
TRI_R = 7.0         # triangle circumradius


def canvas(w=32, h=32):
    return Image.new("RGBA", (w * S, h * S), (0, 0, 0, 0))


def square_layer(angle=0.0):
    """the white-outlined square, centered on the pointer point"""
    lay = canvas()
    d = ImageDraw.Draw(lay)
    half = SQUARE_SIDE / 2
    box = [(C - half) * S, (C - half) * S, (C + half) * S, (C + half) * S]
    d.rounded_rectangle(box, radius=SQUARE_R * S, fill=FILL)
    d.rounded_rectangle(box, radius=SQUARE_R * S,
                        outline=OUTLINE, width=int(1.8 * S))
    if angle:
        lay = lay.rotate(angle, resample=Image.BICUBIC, center=(C * S, C * S))
    return lay


def circle_layer():
    """the basic pointer — same stroke, same center"""
    lay = canvas()
    d = ImageDraw.Draw(lay)
    r = CIRCLE_D / 2
    box = [(C - r) * S, (C - r) * S, (C + r) * S, (C + r) * S]
    d.ellipse(box, fill=FILL)
    d.ellipse(box, outline=OUTLINE, width=int(1.9 * S))
    return lay


def triangle_layer(angle=0.0):
    """the loading shape — white-outlined triangle, point up at rest"""
    lay = canvas()
    d = ImageDraw.Draw(lay)
    pts = []
    for k in range(3):
        a = math.radians(90 + 120 * k)
        pts.append(((C + TRI_R * math.cos(a)) * S, (C - TRI_R * math.sin(a)) * S))
    d.polygon(pts, fill=FILL)
    d.line(pts + [pts[0]], fill=OUTLINE, width=int(1.7 * S), joint="curve")
    if angle:
        lay = lay.rotate(angle, resample=Image.BICUBIC, center=(C * S, C * S))
    return lay


def shadow_under(base):
    sh = Image.new("RGBA", base.size, (0, 0, 0, 0))
    alpha = base.split()[3]
    tint = Image.new("RGBA", base.size, SHADOW)
    sh.paste(tint, (int(0.8 * S), int(1.0 * S)), alpha)
    sh = sh.filter(ImageFilter.GaussianBlur(1.1 * S))
    return Image.alpha_composite(sh, base)


def downscale(img, w=32, h=32):
    return img.resize((w, h), Image.LANCZOS)


def ibeam_layer():
    """the MINIMAL text caret — one rounded line centered on the pointer
    point. The owner asked for a new minimalistic text cursor: the serif'd
    family I-beam is gone. White core + slate edge = contrast on any
    surface without a fill panel or a baked shadow."""
    lay = canvas()
    d = ImageDraw.Draw(lay)
    h = 18.0                       # the line's span, centered on C
    core = 1.6                     # white core width
    edge = 3.6                     # slate edge width (~1 unit per side —
                                   # defines the silhouette on white)
    y0, y1 = (C - h / 2) * S, (C + h / 2) * S
    d.rounded_rectangle([(C - edge / 2) * S, y0, (C + edge / 2) * S, y1],
                        radius=edge / 2 * S, fill=(10, 12, 16, 225))
    d.rounded_rectangle([(C - core / 2) * S, y0, (C + core / 2) * S, y1],
                        radius=core / 2 * S, fill=OUTLINE)
    return lay


def write_cur(img, hotspot, path):
    """32-bit ARGB .cur writer (hotspot = (x, y))"""
    w, h = img.size
    px = img.load()
    xor = bytearray(w * h * 4)
    i = 0
    for row in range(h - 1, -1, -1):          # bottom-up
        for col in range(w):
            r, g, b, a = px[col, row]
            xor[i:i + 4] = struct.pack("<BBBB", b, g, r, a)
            i += 4
    stride = ((w + 31) // 32) * 4             # 1bpp AND mask, all zero
    and_mask = bytes(stride * h)
    bmpheader = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0,
                            len(xor) + len(and_mask), 0, 0, 0, 0)
    entry = struct.pack("<BBBBHHII", w % 256, h % 256, 0, 0,
                        hotspot[0], hotspot[1], len(bmpheader) + len(xor) + len(and_mask), 22)
    icondir = struct.pack("<HHH", 0, 2, 1)
    with open(path, "wb") as f:
        f.write(icondir + entry + bmpheader + xor + and_mask)


def _chunk(cid, payload):
    pad = b"\x00" if len(payload) % 2 else b""
    return cid + struct.pack("<I", len(payload)) + payload + pad


def write_ani(frame_paths, rate_jif, path):
    """RIFF('ACON'){ anih, seq, rate, LIST('fram'){ icon* } } — a
    self-animating cursor. Each frame is a whole .cur (its own hotspot
    rides along). rate_jif is in JIFFIES (1/60 s)."""
    frames = [open(p, "rb").read() for p in frame_paths]
    n = len(frames)
    anih = struct.pack("<9I", 36, n, n, 32, 32, 32, 1, rate_jif, 1)  # flFlags: AF_ICON
    seq = _chunk(b"seq ", struct.pack(f"<{n}I", *range(n)))
    rate = _chunk(b"rate", struct.pack(f"<{n}I", *([rate_jif] * n)))
    fram = b"".join(_chunk(b"icon", f) for f in frames)
    body = _chunk(b"anih", anih) + seq + rate + _chunk(b"LIST", b"fram" + fram)
    with open(path, "wb") as f:
        f.write(b"RIFF" + struct.pack("<I", 4 + len(body)) + b"ACON" + body)


os.makedirs(OUT, exist_ok=True)
os.makedirs(TRI_TMP, exist_ok=True)

# ---- the basic pointer + the clickable square ------------------------------
circle = downscale(shadow_under(circle_layer()))
write_cur(circle, (16, 16), f"{OUT}/circle.cur")

sq = downscale(shadow_under(square_layer()))
write_cur(sq, (16, 16), f"{OUT}/sq.cur")

# ---- the text caret ---------------------------------------------------------
ib = downscale(ibeam_layer())          # no shadow — the line stays clean
write_cur(ib, (16, 16), f"{OUT}/ibeam.cur")

# ---- the loading state: spinning triangle, self-animating -------------------
N_TRI = 12
TRI_RATE_JIF = 4                       # jiffies (1/60 s): 4 ≈ 66.7 ms/frame,
                                       # 12 frames ≈ 0.8 s per revolution
tri_paths = []
tri_imgs = []
for i in range(N_TRI):
    ang = -360.0 * i / N_TRI           # clockwise sweep
    img = downscale(shadow_under(triangle_layer(ang)))
    p = f"{TRI_TMP}/tri_{i:02d}.cur"
    write_cur(img, (16, 16), p)
    tri_paths.append(p)
    tri_imgs.append(img)
write_ani(tri_paths, TRI_RATE_JIF, f"{OUT}/loading.ani")
write_cur(tri_imgs[0], (16, 16), f"{OUT}/loading_static.cur")  # rest frame

# ---- preview sheet ----------------------------------------------------------
def sheet(rows, path, bg1=(210, 214, 220, 255), bg2=(168, 172, 180, 255)):
    cell = 40
    cols = max(len(r) for r in rows)
    W, H = cols * cell, len(rows) * cell
    im = Image.new("RGB", (W, H))
    for y in range(H):
        for x in range(W):
            im.putpixel((x, y), bg1 if ((x // 8 + y // 8) % 2 == 0) else bg2)
    for ri, row in enumerate(rows):
        for ci, img in enumerate(row):
            if img is None:
                continue
            im.paste(img, (ci * cell + 4, ri * cell + 4), img)
    im.save(path)
    return im

sh = sheet([[circle, sq, ib] + [None] * 13, tri_imgs],
           "/home/z/my-project/scripts/cursor_preview3.png")
sh.resize((sh.width * 4, sh.height * 4), Image.NEAREST).save(
    "/home/z/my-project/scripts/cursor_zoom3.png")
print(f"generated: circle + sq + ibeam + loading_static + {N_TRI}-frame loading.ani ({TRI_RATE_JIF} jif/frame) -> {OUT}")
