#!/usr/bin/env python3
"""Export all deliverable PNGs + the final brand sheet + social preview."""
from io import BytesIO
from pathlib import Path
import cairosvg
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
SVG = ROOT / "svg"
PNG = ROOT / "png"
PREV = ROOT / "preview"
DOCS = ROOT / "preview"
for p in (PNG, PREV):
    p.mkdir(exist_ok=True)

def render(svg_name: str, width: int, height: int | None = None) -> Image.Image:
    data = cairosvg.svg2png(url=str(SVG / svg_name), output_width=width, output_height=height)
    return Image.open(BytesIO(data)).convert("RGBA")

# --- white wordmark horizontal (derived from the base file) ----------------
base = (SVG / "ptranslate-horizontal.svg").read_text()
white = base.replace("#1D2330", "#FFFFFF").replace("#5D6576", "#9AA3B2")
(SVG / "ptranslate-horizontal-white.svg").write_text(white)

# --- icon PNGs -------------------------------------------------------------
for s in (1024, 512, 256, 192, 180, 128, 64, 48, 32, 16):
    render("ptranslate-icon.svg", s).save(PNG / f"icon-{s}.png")

icos = [render("ptranslate-icon.svg", s) for s in (16, 24, 32, 48)]
icos[0].save(PNG / "favicon.ico", format="ICO", sizes=[(i.width, i.height) for i in icos])

# --- horizontal ------------------------------------------------------------
render("ptranslate-horizontal.svg", 1560, 340).save(PNG / "horizontal.png")
render("ptranslate-horizontal.svg", 3120, 680).save(PNG / "horizontal@2x.png")
render("ptranslate-horizontal-white.svg", 3120, 680).save(PNG / "horizontal-white@2x.png")

# --- mono mark -------------------------------------------------------------
render("ptranslate-mark-mono.svg", 512).save(PNG / "mark-mono.png")

# --- final sheet -----------------------------------------------------------
S = 2
def sc(x): return int(x * S)
W, H = sc(1180), sc(780)
sheet = Image.new("RGB", (W, H), (244, 246, 249))
d = ImageDraw.Draw(sheet)
fm = ImageFont.truetype("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", sc(15))

# left: big icon on light
icon = render("ptranslate-icon.svg", sc(300))
sheet.paste(icon, (sc(40), sc(48)), icon)
d.text((sc(40), sc(360)), "アイコン (300px)", fill=(60, 66, 80), font=fm)

# sizes row on light
light_row_y = sc(420)
ox = sc(40)
for s in (128, 64, 32, 16):
    im = render("ptranslate-icon.svg", sc(s))
    sheet.paste(im, (ox, light_row_y + (sc(128) - sc(s)) // 2), im)
    ox += sc(s) + sc(28)
d.text((sc(40), light_row_y + sc(150)), "128 / 64 / 32 / 16", fill=(90, 96, 110), font=fm)

# sizes on dark: fill dark strip first, then paste onto the sheet
dark = Image.new("RGB", (sc(350), sc(150)), (20, 23, 28))
ox = sc(40)
for s in (128, 64, 32):
    im = render("ptranslate-icon.svg", sc(s))
    dark.paste(im, (ox, (sc(150) - sc(s)) // 2), im)
    ox += sc(s) + sc(30)
sheet.paste(dark, (sc(40), sc(610)))

# right column: horizontal on light
hx, hy = sc(420), sc(48)
hl = render("ptranslate-horizontal.svg", sc(700), sc(152))
sheet.paste(hl, (hx, hy), hl)
d.text((hx, hy + sc(165)), "横型ロゴ (淡色背景用)", fill=(60, 66, 80), font=fm)

# horizontal on dark (white wordmark): fill dark2 first, then paste
dark2 = Image.new("RGB", (sc(720), sc(230)), (20, 23, 28))
white_img = render("ptranslate-horizontal-white.svg", sc(700), sc(152))
dark2.paste(white_img, (sc(10), sc(39)), white_img)
sheet.paste(dark2, (hx, hy + sc(210)))
d.text((hx, hy + sc(455)), "横型ロゴ (濃色背景用)", fill=(60, 66, 80), font=fm)

# mono marks
mono = render("ptranslate-mark-mono.svg", sc(150))
sheet.paste(mono, (hx, hy + sc(490)), mono)
dark3 = Image.new("RGB", (sc(150), sc(150)), (20, 23, 28))
monow = render("ptranslate-mark-mono-white.svg", sc(150))
dark3.paste(monow, (0, 0), monow)
sheet.paste(dark3, (hx + sc(190), hy + sc(490)))
d.text((hx + sc(360), hy + sc(545)), "モノクロ (1 色・ぬき)", fill=(60, 66, 80), font=fm)

sheet.save(PREV / "final-sheet.png")

# --- GitHub social preview (1280x640) --------------------------------------
sw, sh = 1280, 640
soc = Image.new("RGB", (sw, sh), (246, 247, 249))
logo = render("ptranslate-horizontal.svg", 1040, 226)
soc.paste(logo, ((sw - 1040) // 2, 150), logo)
d2 = ImageDraw.Draw(soc)
cap_font = ImageFont.truetype("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 26)
cap = "PLaMo 3 Translate で訳す、Web とデスクトップの翻訳アプリ"
w = d2.textlength(cap, font=cap_font)
d2.text(((sw - w) // 2, 430), cap, fill=(93, 101, 118), font=cap_font)
soc.save(DOCS / "github-social.png")

print("exports done")
