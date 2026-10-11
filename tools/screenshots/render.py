"""Renders ks-recorder JSON screens to PNG screenshots and GIF animations.

usage: python render.py shot <frame.json> <out.png>
       python render.py gif <frames.rec.json> <out.gif> [max_fps]
"""
import json
import os
import sys

from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
FONT_DIR = os.path.join(HERE, "fonts-dl")
SIZE = int(os.environ.get("KS_FONT_SIZE", "17"))
FONTS = {
    (False, False): ImageFont.truetype(os.path.join(FONT_DIR, "JetBrainsMonoNerdFontMono-Regular.ttf"), SIZE),
    (True, False): ImageFont.truetype(os.path.join(FONT_DIR, "JetBrainsMonoNerdFontMono-Bold.ttf"), SIZE),
    (False, True): ImageFont.truetype(os.path.join(FONT_DIR, "JetBrainsMonoNerdFontMono-Italic.ttf"), SIZE),
    (True, True): ImageFont.truetype(os.path.join(FONT_DIR, "JetBrainsMonoNerdFontMono-BoldItalic.ttf"), SIZE),
}
FALLBACK = [
    ImageFont.truetype(r"C:\Windows\Fonts\seguisym.ttf", SIZE),
    ImageFont.truetype(r"C:\Windows\Fonts\CascadiaMono.ttf", SIZE),
]
REG = FONTS[(False, False)]
ASCENT, DESCENT = REG.getmetrics()
CW = round(REG.getlength("M"))
CH = ASCENT + DESCENT + 1

BG = (23, 21, 20)          # terminal background (dark, warm)
FG = (232, 227, 220)       # terminal default foreground
WINDOW = (36, 33, 31)      # window chrome
PAD = 18
TITLE_H = 34

ANSI = [
    (12, 12, 12), (197, 15, 31), (19, 161, 14), (193, 156, 0), (0, 55, 218), (136, 23, 152),
    (58, 150, 221), (204, 204, 204), (118, 118, 118), (231, 72, 86), (22, 198, 12),
    (249, 241, 165), (59, 120, 255), (180, 0, 158), (97, 214, 214), (242, 242, 242),
]


def idx_color(i):
    if i < 16:
        return ANSI[i]
    if i < 232:
        i -= 16
        steps = [0, 95, 135, 175, 215, 255]
        return (steps[i // 36], steps[(i // 6) % 6], steps[i % 6])
    v = 8 + (i - 232) * 10
    return (v, v, v)


def parse(c, default):
    if c is None:
        return default
    if isinstance(c, int):
        return idx_color(c)
    return tuple(int(c[k:k + 2], 16) for k in (1, 3, 5))


def mix(a, b, t):
    return tuple(int(a[i] * (1 - t) + b[i] * t) for i in range(3))


NOTDEF = {}


def has_glyph(font, ch):
    """False when the font would draw its .notdef box for `ch`."""
    if ch.isspace():
        return True
    try:
        if id(font) not in NOTDEF:
            m = font.getmask("􏿽")
            NOTDEF[id(font)] = (m.size, bytes(m))
        m = font.getmask(ch)
        return m.getbbox() is not None and (m.size, bytes(m)) != NOTDEF[id(font)]
    except Exception:
        return False


GLYPH_CACHE = {}


def font_for(ch, bold, italic):
    key = (ch, bold, italic)
    if key in GLYPH_CACHE:
        return GLYPH_CACHE[key]
    f = FONTS[(bold, italic)]
    if not has_glyph(f, ch):
        for fb in FALLBACK:
            if has_glyph(fb, ch):
                f = fb
                break
    GLYPH_CACHE[key] = f
    return f


def draw_box(d, ch, x, y, w, h, color):
    """Box drawing and block elements drawn as geometry so lines join across cells."""
    cx, cy = x + w // 2, y + h // 2
    t = max(1, round(SIZE / 14))
    light = {
        "─": "lr", "│": "ud", "┌": "rd", "┐": "ld", "└": "ru", "┘": "lu", "├": "udr", "┤": "udl",
        "┬": "lrd", "┴": "lru", "┼": "udlr", "╭": "rd", "╮": "ld", "╰": "ru", "╯": "lu",
        "╴": "l", "╶": "r", "╵": "u", "╷": "d",
    }
    if ch in light:
        segs = light[ch]
        rounded = ch in "╭╮╰╯"
        if rounded:
            r = min(w, h) // 2
            # Quarter arc between the two arms.
            if ch == "╭":
                d.arc([cx, cy, cx + 2 * r, cy + 2 * r], 180, 270, fill=color, width=t)
                d.line([cx + r, cy, x + w, cy], fill=color, width=t)
                d.line([cx, cy + r, cx, y + h], fill=color, width=t)
            elif ch == "╮":
                d.arc([cx - 2 * r, cy, cx, cy + 2 * r], 270, 360, fill=color, width=t)
                d.line([x, cy, cx - r, cy], fill=color, width=t)
                d.line([cx, cy + r, cx, y + h], fill=color, width=t)
            elif ch == "╰":
                d.arc([cx, cy - 2 * r, cx + 2 * r, cy], 90, 180, fill=color, width=t)
                d.line([cx + r, cy, x + w, cy], fill=color, width=t)
                d.line([cx, y, cx, cy - r], fill=color, width=t)
            else:
                d.arc([cx - 2 * r, cy - 2 * r, cx, cy], 0, 90, fill=color, width=t)
                d.line([x, cy, cx - r, cy], fill=color, width=t)
                d.line([cx, y, cx, cy - r], fill=color, width=t)
            return True
        if "l" in segs:
            d.line([x, cy, cx, cy], fill=color, width=t)
        if "r" in segs:
            d.line([cx, cy, x + w, cy], fill=color, width=t)
        if "u" in segs:
            d.line([cx, y, cx, cy], fill=color, width=t)
        if "d" in segs:
            d.line([cx, cy, cx, y + h], fill=color, width=t)
        return True
    blocks = {
        "█": (0, 0, 1, 1), "▀": (0, 0, 1, 0.5), "▄": (0, 0.5, 1, 1), "▌": (0, 0, 0.5, 1),
        "▐": (0.5, 0, 1, 1), "▎": (0, 0, 0.25, 1), "▏": (0, 0, 0.125, 1), "▊": (0, 0, 0.75, 1),
        "▍": (0, 0, 0.375, 1), "▋": (0, 0, 0.625, 1), "▉": (0, 0, 0.875, 1),
    }
    if ch in blocks:
        a, b, c, e = blocks[ch]
        d.rectangle([x + a * w, y + b * h, x + c * w - 1, y + e * h - 1], fill=color)
        return True
    if ch in "░▒▓":
        shade = {"░": 0.25, "▒": 0.5, "▓": 0.75}[ch]
        return ("shade", shade)
    return False


def render(frame, chrome=True, title="ks"):
    rows, cols = frame["rows"], frame["cols"]
    tw, th = cols * CW, rows * CH
    if chrome:
        W, H = tw + 2 * PAD, th + 2 * PAD + TITLE_H
        ox, oy = PAD, PAD + TITLE_H
    else:
        W, H, ox, oy = tw, th, 0, 0
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)
    if chrome:
        d.rectangle([0, 0, W, TITLE_H], fill=WINDOW)
        for i, c in enumerate([(255, 95, 87), (254, 188, 46), (40, 200, 64)]):
            cx = 22 + i * 22
            d.ellipse([cx - 6, TITLE_H // 2 - 6, cx + 6, TITLE_H // 2 + 6], fill=c)
        tf = FONTS[(False, False)]
        tl = tf.getlength(title)
        d.text(((W - tl) / 2, (TITLE_H - CH) / 2 + 1), title, font=tf, fill=(160, 152, 144))
    for r, row in enumerate(frame["cells"]):
        for c, (text, fg, bg, flags) in enumerate(row):
            if flags & 64:  # wide continuation
                continue
            bold, italic, under, inverse, dim, wide = (flags & 1, flags & 2, flags & 4, flags & 8, flags & 16, flags & 32)
            f, b = parse(fg, FG), parse(bg, BG)
            if inverse:
                f, b = b, f
            if dim:
                f = mix(f, b, 0.45)
            x, y = ox + c * CW, oy + r * CH
            w = CW * (2 if wide else 1)
            if b != BG:
                d.rectangle([x, y, x + w - 1, y + CH - 1], fill=b)
            if text and text != " ":
                ch = text[0] if len(text) == 1 else text
                box = draw_box(d, ch, x, y, CW, CH, f) if len(text) == 1 else False
                if isinstance(box, tuple):
                    d.rectangle([x, y, x + CW - 1, y + CH - 1], fill=mix(b, f, box[1]))
                elif not box:
                    font = font_for(text, bool(bold), bool(italic))
                    gl = font.getlength(text)
                    dx = max(0, (w - gl) / 2) if gl < w else 0
                    d.text((x + dx, y + 1), text, font=font, fill=f)
            if under:
                d.line([x, y + CH - 2, x + w - 1, y + CH - 2], fill=f)
    if not frame.get("hide_cursor", True):
        cr, cc = frame["cursor"]
        x, y = ox + cc * CW, oy + cr * CH
        d.rectangle([x, y + 2, x + 1, y + CH - 2], fill=FG)
    return img


def rounded(img, radius=12):
    mask = Image.new("L", img.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, img.size[0] - 1, img.size[1] - 1], radius, fill=255)
    out = Image.new("RGBA", img.size)
    out.paste(img, (0, 0), mask)
    return out


def main():
    mode = sys.argv[1]
    title = os.environ.get("KS_TITLE", "ks")
    if mode == "shot":
        frame = json.load(open(sys.argv[2], encoding="utf-8"))
        rounded(render(frame, title=title)).save(sys.argv[3], optimize=True)
    elif mode == "gif":
        frames = json.load(open(sys.argv[2], encoding="utf-8"))
        max_fps = float(sys.argv[4]) if len(sys.argv) > 4 else 12
        min_gap = 1000 / max_fps
        # Keep a frame only if enough time passed since the last kept one (the last frame of a
        # burst always wins: it is drawn when the next kept frame's time comes).
        kept = []
        for f in frames:
            if kept and f["t"] - kept[-1]["t"] < min_gap:
                kept[-1] = dict(f, t=kept[-1]["t"])
            else:
                kept.append(f)
        images = [render(f, title=title) for f in kept]
        durations = [max(20, kept[i + 1]["t"] - kept[i]["t"]) for i in range(len(kept) - 1)] + [2500]
        # One shared palette keeps colours stable across frames.
        sample = Image.new("RGB", (images[0].width, images[0].height * min(4, len(images))))
        for i, im in enumerate(images[:: max(1, len(images) // 4)][:4]):
            sample.paste(im, (0, i * images[0].height))
        pal = sample.quantize(colors=96, method=Image.Quantize.MEDIANCUT)
        q = [im.quantize(palette=pal, dither=Image.Dither.NONE) for im in images]
        q[0].save(sys.argv[3], save_all=True, append_images=q[1:], duration=durations, loop=0,
                  optimize=True, disposal=1)
        print(f"{len(q)} frames, {os.path.getsize(sys.argv[3]) // 1024} KiB")


main()
