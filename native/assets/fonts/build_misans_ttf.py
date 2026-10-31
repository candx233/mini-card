#!/usr/bin/env python3
"""MiSans woff2 → ttf → 子集，给卡片自绘用（DirectWrite 不能直接吃 woff2）。

用法（在项目根跑）：
    python native/assets/fonts/build_misans_ttf.py            # 生成到 native/assets/fonts/
    python native/assets/fonts/build_misans_ttf.py --verify    # 只校验现有产物

口径（与现版 WebView2 一致，别自己发明）：
  · 字符集 = GB2312 全部（一级+二级汉字 6763 + 符号）+ web/*.html 里出现的全部字符。
    GB2312 是必须的：音乐歌名/歌手、待办文本、城市名都是**动态**中文，窄子集会出现豆腐块。
    超出 GB2312 的字和现在一样回退系统字体（Microsoft YaHei），行为不变。
  · 只出卡片真正用到的三档：400 Regular / 500 Medium / 600 Semibold（700 只有托盘菜单用，留在 Tauri）。
  · 指标（ascent/descent/upem）必须与原字体一致，否则行盒基线会整体偏。
"""

import argparse
import io
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]
SRC = ROOT / "web" / "fonts"
DST = ROOT / "native" / "assets" / "fonts"
# 每档用哪套字符集：GB2312 只为「会显示动态中文」的字重付钱
#   400 Regular → 待办条目、歌名（部分）、正文
#   600 Semibold → 音乐歌名/歌手、城市名、日历标题
#   500 Medium  → 只用于温度数字（.w-temp）与节日名（.cal-holiday，固定表）→ 只要界面文案
WEIGHTS = {
    "Regular": ("MiSans-Regular.woff2", "MiSans-Regular.ttf", True),
    # Medium 也必须全量：`font-weight:400/500` 在 CSS 口径下会落到 Medium（见 TextEngine::css_weight），
    # 窄子集里缺字（如「优」）DWrite 会去回退系统字体，字形和推进宽度都对不上，实测渲染成豆腐块。
    "Medium": ("MiSans-Medium.woff2", "MiSans-Medium.ttf", True),
    "Semibold": ("MiSans-Semibold.woff2", "MiSans-Semibold.ttf", True),
}


def gb2312_chars() -> set[str]:
    out: set[str] = set()
    for hi in range(0xA1, 0xF8):
        for lo in range(0xA1, 0xFF):
            try:
                out.add(bytes([hi, lo]).decode("gb2312"))
            except UnicodeDecodeError:
                pass
    return out


def html_chars() -> set[str]:
    out: set[str] = set()
    for p in sorted((ROOT / "web").glob("*.html")):
        out |= set(p.read_text(encoding="utf-8", errors="ignore"))
    return out


def build_charset() -> set[str]:
    extra = set(
        "".join(chr(c) for c in range(0x20, 0x7F))
        + "·—–…‹›«»“”‘’°％±×÷≤≥≠≈∞"
        + "←↑→↓↔⇧⌘⏎⎋"
        + "①②③④⑤⑥⑦⑧⑨⑩"
    )
    return gb2312_chars() | html_chars() | extra


def ui_chars() -> set[str]:
    return html_chars() | set("".join(chr(c) for c in range(0x20, 0x7F))) | set("·—–…‹›“”‘’°％±×÷≤≥≠≈∞←↑→↓")


def build(dry: bool = False) -> int:
    from fontTools import subset
    from fontTools.ttLib import TTFont

    wide = build_charset()
    narrow = ui_chars()
    print(f"字符集：宽（GB2312+界面）{len(wide)} 字 / 窄（仅界面）{len(narrow)} 字")
    DST.mkdir(parents=True, exist_ok=True)
    total = 0
    for name, (src_name, dst_name, use_gb) in WEIGHTS.items():
        unicodes = sorted({ord(c) for c in (wide if use_gb else narrow) if not c.isspace() or c == " "} | {0x20})
        src = SRC / src_name
        if not src.exists():
            print(f"  ✗ 缺源文件 {src}")
            return 2
        raw = src.read_bytes()
        font = TTFont(io.BytesIO(raw))
        # woff2 → ttf（内存里）
        buf = io.BytesIO()
        font.flavor = None
        font.save(buf)
        full_ttf = len(buf.getvalue())
        buf.seek(0)
        font = TTFont(buf)
        opts = subset.Options()
        opts.layout_features = ["*"]        # 保留 kern/tnum 等
        opts.name_IDs = ["*"]
        opts.name_legacy = True
        opts.notdef_outline = True
        opts.recalc_bounds = False
        opts.drop_tables += ["DSIG"]
        sub = subset.Subsetter(options=opts)
        sub.populate(unicodes=unicodes)
        sub.subset(font)
        out = io.BytesIO()
        font.save(out)
        data = out.getvalue()
        dst = DST / dst_name
        if not dry:
            dst.write_bytes(data)
        total += len(data)
        print(
            f"  {dst_name:22s} woff2 {len(raw)/1024:7.0f} KB → 全量 ttf {full_ttf/1024:7.0f} KB"
            f" → 子集 {len(data)/1024:7.0f} KB  ({dst})"
        )
    print(f"合计 {total/1024:.0f} KB（{len(WEIGHTS)} 档）")
    return 0


def verify() -> int:
    from fontTools.ttLib import TTFont

    wide = build_charset()
    narrow = ui_chars()
    ok = True
    for name, (src_name, dst_name, use_gb) in WEIGHTS.items():
        p = DST / dst_name
        if not p.exists():
            print(f"  ✗ 缺 {p}")
            ok = False
            continue
        f = TTFont(p)
        # 原字体本身就缺的字（音乐 ⏮⏭、⌘⇧ 这类符号）属于「本来就该回退系统字体」，不算缺字
        orig = TTFont(io.BytesIO((SRC / src_name).read_bytes()))
        orig_cmap = orig.getBestCmap() or {}
        upem = f["head"].unitsPerEm
        hhea = f["hhea"]
        os2 = f["OS/2"]
        cmap = f.getBestCmap()
        want = wide if use_gb else narrow
        never = sorted({c for c in want if ord(c) not in orig_cmap and not c.isspace()})
        missing = [c for c in want if ord(c) not in cmap and not c.isspace() and c not in never]
        print(
            f"  {dst_name:22s} upem {upem}  hhea asc/desc {hhea.ascent}/{hhea.descent}"
            f"  OS/2 win {os2.usWinAscent}/{os2.usWinDescent}"
            f"  码位 {len(cmap)}  缺字 {len(missing)}  {p.stat().st_size/1024:.0f} KB"
        )
        if upem != 1000 or hhea.ascent != 1044 or hhea.descent != -282:
            print("    ✗ 度量与原字体不一致（行盒基线会整体偏）")
            ok = False
        if missing:
            print(f"    ✗ 本该覆盖却缺字：{''.join(missing[:20])}")
            ok = False
        if never:
            print(f"    · 原字体本身没有（运行时回退系统字体）：{''.join(never)}")
    print("VERIFY:", "PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--verify", action="store_true", help="只校验现有产物")
    ap.add_argument("--dry", action="store_true", help="不写文件，只报大小")
    a = ap.parse_args()
    sys.exit(verify() if a.verify else build(a.dry))
