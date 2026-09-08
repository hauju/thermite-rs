"""Write the share cards' SVG sources: assets/og.svg and assets/og-demo.svg.

The type is Space Grotesk (assets/fonts/), shaped with HarfBuzz and outlined to paths, so the
SVGs render identically anywhere without the font installed — pango reads the variable font's
single instance as "Space Grotesk Light" and ignores font-weight. Edit the copy at the bottom,
run this, then `just og` to re-render the PNGs.

    uv run --with fonttools,brotli,uharfbuzz scripts/og.py
"""

import io
import math

import uharfbuzz as hb
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

FONT = "assets/fonts/space-grotesk-latin.woff2"
W, H = 1200, 630

_instances = {}


def instance(weight):
    if weight not in _instances:
        f = TTFont(FONT)
        f.flavor = None
        inst = instancer.instantiateVariableFont(f, {"wght": weight})
        buf = io.BytesIO()
        inst.save(buf)
        _instances[weight] = (inst, buf.getvalue())
    return _instances[weight]


def shape(text, weight, size, tracking):
    tt, data = instance(weight)
    upem = tt["head"].unitsPerEm
    font = hb.Font(hb.Face(data))
    font.scale = (upem, upem)
    buf = hb.Buffer()
    buf.add_str(text)
    buf.guess_segment_properties()
    hb.shape(font, buf, {"kern": True, "liga": True})
    scale = size / upem
    order = tt.getGlyphOrder()
    x = 0.0
    runs = []
    for info, pos in zip(buf.glyph_infos, buf.glyph_positions):
        runs.append((order[info.codepoint], x + pos.x_offset * scale, pos.y_offset * scale))
        x += pos.x_advance * scale + tracking * size
    return runs, x - tracking * size


def measure(text, weight, size, tracking=0.0):
    return shape(text, weight, size, tracking)[1]


def text_path(text, weight, size, x, y, anchor="middle", tracking=0.0):
    """Path data for `text` with its baseline at y; returns (d, width, x_left)."""
    runs, width = shape(text, weight, size, tracking)
    tt, _ = instance(weight)
    gs = tt.getGlyphSet()
    scale = size / tt["head"].unitsPerEm
    x0 = {"middle": x - width / 2, "end": x - width, "start": x}[anchor]
    parts = []
    for name, gx, gy in runs:
        pen = SVGPathPen(gs, ntos=lambda v: f"{v:.1f}".rstrip("0").rstrip("."))
        gs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, x0 + gx, y - gy)))
        d = pen.getCommands()
        if d:
            parts.append(d)
    return " ".join(parts), width, x0


def oklch(L, C, h):
    a = C * math.cos(math.radians(h))
    b = C * math.sin(math.radians(h))
    l_ = L + 0.3963377774 * a + 0.2158037573 * b
    m_ = L - 0.1055613458 * a - 0.0638541728 * b
    s_ = L - 0.0894841775 * a - 1.2914855480 * b
    l, m, s = l_**3, m_**3, s_**3
    r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s
    g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s
    bl = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s

    def gam(c):
        c = max(0.0, min(1.0, c))
        return 12.92 * c if c <= 0.0031308 else 1.055 * c ** (1 / 2.4) - 0.055

    return "#%02x%02x%02x" % tuple(round(gam(c) * 255) for c in (r, g, bl))


CAP = 0.7  # Space Grotesk cap height / em
MARK_SHELL = "M10.42 3.9 2.77 17.16a1.83 1.83 0 0 0 1.58 2.74h15.3a1.83 1.83 0 0 0 1.58-2.74L13.58 3.9a1.83 1.83 0 0 0-3.16 0Z"
MARK_CORE = "M12 10.6l3.55 6.15a.62.62 0 0 1-.54.93H8.99a.62.62 0 0 1-.54-.93Z"


def card(name, pill, pill_dot, line1, line2, domain, comment):
    # --- vertical rhythm (all baselines / centres) ---
    pill_cy = 98
    brand_cy = 214
    head_size = 66
    head_y1 = 392
    head_y2 = head_y1 + round(head_size * 1.18)
    domain_y = 574

    out = []
    A = out.append
    A(f"<!--\n{comment}\n-->")
    A(
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" '
        f'role="img" aria-label="{line1} {line2}">'
    )
    # Defs: the icon's plate gradient, the landing hero's glow and masked grid,
    # the mark's own gradients, and the headline gradient (landing-gradient-text).
    A("<defs>")
    A('<linearGradient id="bg" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#1c1712"/><stop offset="1" stop-color="#0a0908"/></linearGradient>')
    A(
        f'<radialGradient id="glow" cx="600" cy="230" r="520" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0" stop-color="{oklch(0.68, 0.19, 45)}" stop-opacity="0.26"/>'
        f'<stop offset="0.4" stop-color="{oklch(0.62, 0.17, 30)}" stop-opacity="0.12"/>'
        f'<stop offset="0.72" stop-color="{oklch(0.62, 0.17, 30)}" stop-opacity="0"/>'
        "</radialGradient>"
    )
    A('<pattern id="grid" width="56" height="56" patternUnits="userSpaceOnUse"><path d="M56 0H0V56" fill="none" stroke="#ffffff" stroke-opacity="0.06"/></pattern>')
    A(
        '<radialGradient id="gridfade" cx="600" cy="90" r="760" gradientUnits="userSpaceOnUse" '
        'gradientTransform="translate(600 90) scale(1 0.62) translate(-600 -90)">'
        '<stop offset="0.3" stop-color="#fff"/><stop offset="0.85" stop-color="#000"/></radialGradient>'
    )
    A(f'<mask id="gridmask"><rect width="{W}" height="{H}" fill="url(#gridfade)"/></mask>')
    A('<linearGradient id="shell" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#ffc233"/><stop offset="1" stop-color="#e2401f"/></linearGradient>')
    A('<linearGradient id="core" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fffdf7"/><stop offset="1" stop-color="#ffb020"/></linearGradient>')

    # Headline line 2 gradient spans exactly its own box.
    d2, w2, x2 = text_path(line2, 700, head_size, 600, head_y2, tracking=-0.025)
    A(
        f'<linearGradient id="molten" gradientUnits="userSpaceOnUse" '
        f'x1="{x2:.0f}" y1="{head_y2 - head_size * CAP:.0f}" x2="{x2 + w2:.0f}" y2="{head_y2:.0f}">'
        f'<stop offset="0" stop-color="{oklch(0.86, 0.15, 85)}"/>'
        f'<stop offset="0.5" stop-color="{oklch(0.72, 0.19, 47)}"/>'
        f'<stop offset="1" stop-color="{oklch(0.66, 0.2, 30)}"/></linearGradient>'
    )
    A("</defs>")

    # Backdrop.
    A(f'<rect width="{W}" height="{H}" fill="url(#bg)"/>')
    A(f'<rect width="{W}" height="{H}" fill="url(#grid)" mask="url(#gridmask)"/>')
    A(f'<rect width="{W}" height="{H}" fill="url(#glow)"/>')

    # Pill: the landing page's kicker badge.
    pill_size = 23
    pw = measure(pill, 500, pill_size)
    dot_w = 26 if pill_dot else 0
    pad = 22
    box_w = pw + dot_w + pad * 2
    box_h = 46
    bx = 600 - box_w / 2
    A(
        f'<rect x="{bx:.1f}" y="{pill_cy - box_h / 2}" width="{box_w:.1f}" height="{box_h}" rx="{box_h / 2}" '
        'fill="#15130f" fill-opacity="0.7" stroke="#2f2820" stroke-width="1.5"/>'
    )
    if pill_dot:
        A(f'<circle cx="{bx + pad + 6:.1f}" cy="{pill_cy}" r="5.5" fill="#4ade80"/>')
        A(f'<circle cx="{bx + pad + 6:.1f}" cy="{pill_cy}" r="9.5" fill="#4ade80" fill-opacity="0.25"/>')
    dp, _, _ = text_path(pill, 500, pill_size, bx + pad + dot_w, pill_cy + pill_size * CAP / 2, anchor="start")
    A(f'<path fill="#f0ece7" fill-opacity="0.78" d="{dp}"/>')

    # Brand row: mark + wordmark, centred as a group.
    mark = 118  # the 24-unit box; the triangle itself spans x 2.77..21.23, y 3.9..19.9
    word_size = 108
    gap = 30
    s = mark / 24
    mw = 18.46 * s
    ww = measure("Thermite", 700, word_size, tracking=-0.025)
    gw = mw + gap + ww
    gx0 = 600 - gw / 2
    A(f'<g transform="translate({gx0 - 2.77 * s:.1f} {brand_cy - 11.9 * s:.1f}) scale({s:.4f})">')
    A(f'<path d="{MARK_SHELL}" fill="none" stroke="url(#shell)" stroke-width="2" stroke-linejoin="round"/>')
    A(f'<path d="{MARK_CORE}" fill="url(#core)" stroke="url(#shell)" stroke-width="1.3" stroke-linejoin="round"/>')
    A("</g>")
    dw, _, _ = text_path("Thermite", 700, word_size, gx0 + mw + gap, brand_cy + word_size * CAP / 2, anchor="start", tracking=-0.025)
    A(f'<path fill="#f0ece7" d="{dw}"/>')

    # Headline.
    d1, _, _ = text_path(line1, 700, head_size, 600, head_y1, tracking=-0.025)
    A(f'<path fill="#f0ece7" d="{d1}"/>')
    A(f'<path fill="url(#molten)" d="{d2}"/>')

    # Domain.
    dd, _, _ = text_path(domain, 500, 26, 600, domain_y)
    A(f'<path fill="#f0ece7" fill-opacity="0.5" d="{dd}"/>')

    A("</svg>")
    with open(name, "w") as fh:
        fh.write("\n".join(out) + "\n")
    print(name, "wordmark", round(ww), "line1", round(measure(line1, 700, head_size, -0.025)), "line2", round(w2))


COMMENT = """  Source for {png} — the 1200×630 Open Graph / Twitter card. Render with `just og`.

  Written by scripts/og.py, which outlines the type (Space Grotesk, assets/fonts/) so
  rendering needs no font; edit the copy there, not here. Copy: {copy}"""

card(
    "assets/og.svg",
    "Sentry-compatible, self-hosted",
    False,
    "The error tracker",
    "your agent works in.",
    "thermite.rs",
    COMMENT.format(png="og.png", copy='"Sentry-compatible, self-hosted / Thermite / The error tracker your agent works in. / thermite.rs"'),
)
card(
    "assets/og-demo.svg",
    "Public sandbox, nothing to install",
    True,
    "Try it live.",
    "Real errors, no signup.",
    "demo.thermite.rs",
    COMMENT.format(png="og-demo.png", copy='"Public sandbox, nothing to install / Thermite / Try it live. Real errors, no signup. / demo.thermite.rs"'),
)
