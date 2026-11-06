#!/usr/bin/env python3
"""ptranslate-mark-mono.svg を生成する。

「訳」をフォントからパス化し、バブル (角丸矩形+尾) と 1 つの <path> にまとめる。
contour ごとにネスティング深度を数えて巻き方向を決め、fill-rule=nonzero で
「訳」全体 (言偏の横棒や 彔 のcounterまで) が正しくぬき (透明) になる 1 色マークにする。
  - 深度 0 (外側の筆画)  = バブルと逆向き → ぬき
  - 深度 1 (口の内側など) = バブルと同じ向き → 塗り
"""
from pathlib import Path
from fontTools.ttLib import TTFont
from fontTools.pens.recordingPen import RecordingPen

ROOT = Path(__file__).resolve().parent
if ROOT.name == "tools":
    ROOT = ROOT.parent
FONT = "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc"
FACE = 0  # Noto Sans CJK JP Bold

SIZE, CX, BASE = 420, 512, 652

def transform_point(p, s, x0):
    return (x0 + p[0] * s, BASE - p[1] * s)

def cubic(a, c1, c2, b, t):
    u = 1 - t
    return (u*u*u*a[0] + 3*u*u*t*c1[0] + 3*u*t*t*c2[0] + t*t*t*b[0],
            u*u*u*a[1] + 3*u*u*t*c1[1] + 3*u*t*t*c2[1] + t*t*t*b[1])

def flatten(ops, samples=8):
    """contour を折れ線に落として点列を返す (包含判定用)。"""
    pts = [ops[0][1][0]]
    for op, ps in ops[1:]:
        if op == "closePath":
            continue
        if op == "lineTo":
            pts.extend(ps)
        elif op == "curveTo" and len(ps) == 3:
            c1, c2, b = ps
            for i in range(1, samples + 1):
                pts.append(cubic(pts[-1], c1, c2, b, i / samples))
        else:  # qCurveTo など (CJK 由来なので通常は来ない) は折れ線近似
            pts.extend(ps)
    return pts

def contains(poly, p):
    """レイトスローイン法。poly: 折れ線点列、p: (x, y)。"""
    x, y = p
    inside = False
    n = len(poly)
    for i in range(n):
        x1, y1 = poly[i]
        x2, y2 = poly[(i + 1) % n]
        if (y1 > y) != (y2 > y):
            xint = x1 + (y - y1) * (x2 - x1) / (y2 - y1)
            if xint > x:
                inside = not inside
    return inside

def signed_area(pts):
    a = 0.0
    n = len(pts)
    for i in range(n):
        x1, y1 = pts[i]
        x2, y2 = pts[(i + 1) % n]
        a += x1 * y2 - x2 * y1
    return a / 2.0

def flip_ops(ops):
    """contour の描画順を反転向きにする。"""
    start = ops[0][1][0]
    segs = [s for s in ops[1:] if s[0] != "closePath"]
    if not segs:
        return ops
    out = [("moveTo", [segs[-1][1][-1]])]
    for i in range(len(segs) - 1, -1, -1):
        op, pts = segs[i]
        seg_start = start if i == 0 else segs[i - 1][1][-1]
        ctrls = pts[:-1]
        if op == "lineTo":
            out.append(("lineTo", [seg_start]))
        elif op == "curveTo":
            out.append(("curveTo", list(reversed(ctrls)) + [seg_start]))
        else:
            raise SystemExit(f"unhandled op {op}")
    out.append(("closePath", []))
    return out

def centroid(pts):
    return (sum(p[0] for p in pts) / len(pts), sum(p[1] for p in pts) / len(pts))

def main():
    font = TTFont(FONT, fontNumber=FACE)
    print("face:", font["name"].getDebugName(1))
    cmap = font.getBestCmap()
    gname = cmap[ord("\u8a33")]
    upm = font["head"].unitsPerEm
    advance = font["hmtx"][gname][0]
    s = SIZE / upm
    x0 = CX - advance * s / 2.0

    pen = RecordingPen()
    font.getGlyphSet()[gname].draw(pen)

    contours, cur = [], []
    for op, args in pen.value:
        if op == "moveTo":
            if cur:
                contours.append(cur)
            cur = [("moveTo", [tuple(args[0])])]
        elif op == "closePath":
            cur.append(("closePath", []))
        elif op in ("lineTo", "curveTo", "qCurveTo"):
            cur.append((op, [tuple(p) for p in args]))
        else:
            raise SystemExit(f"unexpected {op}")
    if cur:
        contours.append(cur)

    # 変換
    parts = []
    for c in contours:
        tc = []
        for op, pts in c:
            if op == "closePath":
                tc.append(("closePath", []))
            else:
                tc.append((op, [transform_point(p, s, x0) for p in pts]))
        parts.append(tc)

    flats = [flatten(p) for p in parts]

    # ネスティング深度 → 必要な巻き方向 (バブルは正方向)
    probes = []
    for f in flats:
        v = f[0]
        cx_, cy_ = centroid(f)
        probes.append((v[0] + 0.05 * (cx_ - v[0]), v[1] + 0.05 * (cy_ - v[1])))

    fixed = []
    for i, (p, f) in enumerate(zip(parts, flats)):
        depth = sum(1 for j, fj in enumerate(flats) if j != i and contains(fj, probes[i]))
        area = signed_area(f)
        got = 1 if area > 0 else -1
        want = -1 if depth % 2 == 0 else 1  # 偶数深度 = ぬき (逆)、奇数 = 塗り (正)
        fixed.append(p if got == want else flip_ops(p))

    def r(v):
        return round(v, 1)

    def d_of(ops):
        d = []
        x, y = ops[0][1][0]
        d.append(f"M{r(x)},{r(y)}")
        for op, pts in ops[1:]:
            if op == "lineTo":
                d.append("L" + ",".join(f"{r(p[0])},{r(p[1])}" for p in pts))
            elif op == "curveTo":
                d.append("C" + ",".join(f"{r(p[0])},{r(p[1])}" for p in pts))
            elif op == "qCurveTo":
                d.append("Q" + ",".join(f"{r(p[0])},{r(p[1])}" for p in pts))
        if ops[-1][0] == "closePath":
            d.append("Z")
        return "".join(d)

    rect_d = ("M296,216 L728,216 Q864,216 864,352 L864,624 "
              "Q864,760 728,760 L296,760 Q160,760 160,624 L160,352 "
              "Q160,216 296,216 Z")
    tail_d = "M380,745 L520,745 L366,868 Q352,881 348,864 L368,745 Z"
    glyph_d = " ".join(d_of(c) for c in fixed)

    def svg(ink):
        return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  <!-- モノクロ用: 1 色・ぬき。「訳」はフォントからパス化済み (フォント不要) -->
  <path fill="{ink}" fill-rule="nonzero" d="{rect_d} {tail_d} {glyph_d}"/>
</svg>
'''

    (ROOT / "svg" / "ptranslate-mark-mono.svg").write_text(svg("#1D4FC9"))
    (ROOT / "svg" / "ptranslate-mark-mono-white.svg").write_text(svg("#FFFFFF"))
    depths = [sum(1 for j, fj in enumerate(flats) if j != i and contains(fj, probes[i]))
              for i in range(len(flats))]
    print("contours:", len(fixed), "depths:", depths)
    print("wrote svg/ptranslate-mark-mono.svg (+white)")

if __name__ == "__main__":
    main()
