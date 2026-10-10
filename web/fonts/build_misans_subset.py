#!/usr/bin/env python
"""从官方 MiSans 字体包生成内嵌子集：web/fonts/MiSans-{Regular,Medium,Semibold,Bold}.woff2

用法（仓库根目录执行）：
    uv run --with fonttools --with brotli python web/fonts/build_misans_subset.py <MiSans.zip>

说明：
- 源文件 = 官方包里的四档静态 TTF（MiSans/ttf/MiSans-*.ttf），逐档做字形子集。
- 只做字形子集化：不改字形轮廓、不改字体名、保留 name/OS2/GSUB（含 tnum 等宽数字）。
- 四档进 CSS 的映射（与小米官方 web 用法一致，见 web/fonts/README.md）：
  400 Regular / 500 Medium / 600 Semibold / 700 Bold。
- 字符集 = GB2312 全量（两级）+ 拉丁与常用符号块 + 平假名/片假名
           + 本仓库源码（web/*.html、src-tauri/src/*.rs、*.txt）里出现的全部字符，
           保证界面文案在任何一次改版后仍然齐全；缺字由 CSS 回退栈兜底。
- 授权条件见 web/fonts/README.md（小米允许嵌入使用，需在软件中注明）。
"""
import io
import sys
import zipfile
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parents[2]          # 仓库根
OUTDIR = ROOT / "web" / "fonts"
FACES = [("Regular", 400), ("Medium", 500), ("Semibold", 600), ("Bold", 700)]


def load_face(arg: str, face: str) -> io.BytesIO:
    """从 zip / 解压目录 / 单个 ttf 里取 MiSans-<face>.ttf。"""
    p = Path(arg)
    name = f"MiSans-{face}.ttf"
    if p.suffix.lower() == ".zip":
        z = zipfile.ZipFile(p)
        # __MACOSX 里是同一路径的元数据影子文件，只取真正的字体
        names = [n for n in z.namelist() if n.endswith(name) and "__MACOSX" not in n]
        assert len(names) == 1, f"zip 里 {name} 命中 {len(names)} 个"
        return io.BytesIO(z.read(names[0]))
    if p.name.endswith(".ttf"):
        return io.BytesIO(p.read_bytes())
    hits = list(p.rglob(name))                     # 解压目录
    assert len(hits) == 1, f"目录里 {name} 命中 {len(hits)} 个"
    return io.BytesIO(hits[0].read_bytes())


def charset() -> set:
    keep = set()
    # GB2312 两级（6763 汉字 + 符号）
    for hi in range(0xA1, 0xF8):
        for lo in range(0xA1, 0xFF):
            try:
                keep.add(ord(bytes([hi, lo]).decode("gb2312")))
            except Exception:
                pass
    # 源码全部字符（含 wxcities.txt 城市表）
    srcs = sorted(ROOT.glob("web/*.html")) + sorted(ROOT.glob("src-tauri/src/*.rs")) + \
        sorted(ROOT.glob("src-tauri/src/*.txt"))
    for f in srcs:
        keep |= {ord(c) for c in set(f.read_text(encoding="utf-8", errors="replace"))}
    # ASCII + Latin-1 + Latin Ext-A
    keep |= set(range(0x20, 0x7F)) | set(range(0xA0, 0x180))
    # 常用符号 / 标点块（只取字体里有的）
    for a, b in [(0x2000, 0x206F), (0x20A0, 0x20BF), (0x2100, 0x214F), (0x2190, 0x21FF),
                 (0x2200, 0x22FF), (0x2460, 0x24FF), (0x2500, 0x257F), (0x25A0, 0x25FF),
                 (0x2600, 0x27BF), (0x3000, 0x303F), (0x3040, 0x30FF), (0x31C0, 0x31EF),
                 (0xFE10, 0xFE4F), (0xFF00, 0xFFEF)]:
        keep |= set(range(a, b + 1))
    return keep


def main() -> None:
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    want = charset()
    print(f"请求字符集：{len(want)} 个码位")
    first = True
    for face, css_weight in FACES:
        data = load_face(sys.argv[1], face)
        font = TTFont(data, lazy=True)
        cmap = font.getBestCmap()
        keep = {c for c in want if c in cmap}
        if first:
            missing = sorted(c for c in want if c not in cmap)
            miss_text = "".join(chr(c) for c in missing)
            print(f"字体不含 {len(missing)} 个请求字符（正常，由回退栈兜底）：{miss_text}")
            first = False

        opts = subset.Options()
        opts.layout_features = ["*"]        # 保留 tnum/pnum/locl 等全部特性
        opts.name_IDs = ["*"]
        opts.name_legacy = True
        opts.name_languages = ["*"]
        opts.recalc_bounds = False
        opts.drop_tables = ["DSIG"]

        data.seek(0)
        font = subset.load_font(data, opts)
        s = subset.Subsetter(options=opts)
        s.populate(unicodes=keep)
        s.subset(font)
        font.flavor = "woff2"
        out = OUTDIR / f"MiSans-{face}.woff2"
        font.save(out)

        chk = TTFont(out, lazy=True)
        feats = sorted({fr.FeatureTag for fr in chk["GSUB"].table.FeatureList.FeatureRecord}) if "GSUB" in chk else []
        wclass = chk["OS/2"].usWeightClass
        nglyph = chk["maxp"].numGlyphs
        print(f"{out.name}: CSS {css_weight} | {out.stat().st_size / 1e6:.2f} MB | "
              f"字形 {nglyph} | 内含字重 {wclass} | tnum: {'tnum' in feats} | fvar: {'fvar' in chk}")


if __name__ == "__main__":
    main()
