#!/usr/bin/env python3
"""Apple / Windows のアイコン規則に合わせた派生を作成する。

- iOS:  1024x1024 フルブリード (角丸なし・全面ベタ)。システムがマスクする。
- macOS: Big Sur 式スクircle (連続曲率) ボディ + 約10%の余白。icon.icns も生成。
- Windows: 角丸なしフルスクエアの .ico に 16/24/32/48/64/128/256 を同梱。
"""
import math
from io import BytesIO
from pathlib import Path
import cairosvg
from PIL import Image

ROOT = Path(__file__).resolve().parent
if ROOT.name == "tools":
    ROOT = ROOT.parent
SVG = ROOT / "svg"
PNG = ROOT / "png"
PREV = ROOT / "preview"

BUBBLE = ('<rect x="160" y="216" width="704" height="544" rx="136"/>'
          '<path d="M380,745 L520,745 L366,868 Q352,881 348,864 L368,745 Z"/>')
YAKU = ('<text x="512" y="652" font-family="Noto Sans CJK JP" font-weight="bold" '
        'font-size="420" fill="url(#glyph)" text-anchor="middle">訳</text>')
DEFS = '''<defs>
    <linearGradient id="tile" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#57A1FF"/><stop offset="1" stop-color="#1F54D0"/>
    </linearGradient>
    <linearGradient id="glyph" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#3D7BE8"/><stop offset="1" stop-color="#2050C0"/>
    </linearGradient>
  </defs>'''

def squircle_path(cx, cy, size, n=5.0, per_quadrant=32):
    """連続曲率の角丸 (スーパー楕円)。4象限を回転しながら点を取る。"""
    a = size / 2.0
    pts = []
    for q in range(4):
        base = q * math.pi / 2.0
        for i in range(per_quadrant):
            t = base + (i / per_quadrant) * (math.pi / 2.0)
            c, s = math.cos(t), math.sin(t)
            x = a * math.copysign(abs(c) ** (2.0 / n), c)
            y = a * math.copysign(abs(s) ** (2.0 / n), s)
            pts.append((cx + x, cy + y))
    d = "M" + " L".join(f"{x:.2f},{y:.2f}" for x, y in pts) + " Z"
    return d

def fullbleed_svg():
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  {DEFS}
  <rect width="1024" height="1024" fill="url(#tile)"/>
  <g fill="#ffffff">{BUBBLE}</g>
  {YAKU}
</svg>
'''

def mac_svg():
    body = squircle_path(512, 512, 824)
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  {DEFS}
  <path d="{body}" fill="url(#tile)"/>
  <g transform="translate(100,100) scale(0.8046875)">
    <g fill="#ffffff">{BUBBLE}</g>
    {YAKU}
  </g>
</svg>
'''

def render_svg(text: str, size: int) -> Image.Image:
    data = cairosvg.svg2png(bytestring=text.encode(), output_width=size, output_height=size)
    return Image.open(BytesIO(data)).convert("RGBA")

def main():
    (SVG / "ptranslate-icon-fullbleed.svg").write_text(fullbleed_svg())
    (SVG / "ptranslate-icon-mac.svg").write_text(mac_svg())

    # iOS: フルブリード
    render_svg(fullbleed_svg(), 1024).save(PNG / "icon-ios-1024.png")

    # macOS: スクircle + 余白
    render_svg(mac_svg(), 1024).save(PNG / "icon-mac-1024.png")
    # Pillow は既定で 32/64/128/256(x2)/512(x2)/1024 の PNG を icns に同梱する
    master = render_svg(mac_svg(), 1024)
    try:
        master.save(PNG / "icon.icns", format="icns")
        back = Image.open(PNG / "icon.icns")
        print("icns written, sizes:", sorted(set(s[0] for s in back.info.get("sizes", []))))
    except Exception as e:
        print("icns FAIL:", e)

    # Windows: フルスクエア .ico (16〜256)
    ico_sizes = [16, 24, 32, 48, 64, 128, 256]
    icos = [render_svg(fullbleed_svg(), s) for s in ico_sizes]
    icos[-1].save(PNG / "icon-win.ico", format="ICO", sizes=[(s, s) for s in ico_sizes])
    print("icon-win.ico written:", ico_sizes)

    # プレビューシート
    sheet = Image.new("RGB", (1080, 330), (244, 246, 249))
    from PIL import ImageDraw
    d = ImageDraw.Draw(sheet)
    f = ImageFont = __import__("PIL.ImageFont", fromlist=["truetype"]).truetype(
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 15)
    cells = [("元 (Web タイル)", None), ("iOS (フルブリード)", "icon-ios-1024.png"),
             ("macOS (スクircle+余白)", "icon-mac-1024.png"), ("Windows (.ico 256)", None)]
    ox = 30
    tile = render_svg((SVG / "ptranslate-icon.svg").read_text(), 240)
    for i, (label, p) in enumerate(cells):
        if p:
            im = Image.open(PNG / p).convert("RGBA").resize((240, 240))
        elif i == 0:
            im = tile
        else:
            im = icos[-1].resize((240, 240))
        bg = Image.new("RGBA", (240, 240), (235, 238, 242, 255))
        bg.paste(im, (0, 0), im)
        sheet.paste(bg.convert("RGB"), (ox, 40))
        d.text((ox + 4, 292), label, fill=(60, 66, 80), font=f)
        ox += 264
    sheet.save(PREV / "sheet-platform.png")
    print("sheet saved")

if __name__ == "__main__":
    main()
