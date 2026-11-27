"""Builds the static Space Grotesk 500/600/700 TTFs in assets/fonts.

Upstream ships static Medium and Bold but no SemiBold, so all three weights are
instanced from Google Fonts' official variable font (OFL) for consistency:
  https://github.com/google/fonts/blob/main/ofl/spacegrotesk/SpaceGrotesk%5Bwght%5D.ttf

Usage: python -I tools/make_space_grotesk_statics.py <SpaceGrotesk[wght].ttf> assets/fonts
Needs fontTools (pip install fonttools).

The STAT table is dropped: DirectWrite reads it, and a pruned STAT made the
600 instance match as Bold.
"""

import os
import sys

from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

src, outdir = sys.argv[1], sys.argv[2]
for weight, style in [(500, "Medium"), (600, "SemiBold"), (700, "Bold")]:
    font = instantiateVariableFont(TTFont(src), {"wght": weight})
    name = font["name"]
    for nid in (1, 2, 3, 4, 6, 16, 17, 21, 22, 25):
        name.removeNames(nameID=nid)
    if style == "Bold":
        family, sub = "Space Grotesk", "Bold"
    else:
        family, sub = "Space Grotesk " + style, "Regular"
    name.setName(family, 1, 3, 1, 0x409)
    name.setName(sub, 2, 3, 1, 0x409)
    name.setName("Space Grotesk", 16, 3, 1, 0x409)
    name.setName(style, 17, 3, 1, 0x409)
    name.setName("Space Grotesk " + style, 4, 3, 1, 0x409)
    name.setName("SpaceGrotesk-" + style, 6, 3, 1, 0x409)
    name.setName("SpaceGrotesk-" + style + ";lumen-static-instance", 3, 3, 1, 0x409)
    font["OS/2"].usWeightClass = weight
    sel = font["OS/2"].fsSelection & ~(0x1 | 0x20 | 0x40)
    sel |= 0x20 if style == "Bold" else 0x40
    font["OS/2"].fsSelection = sel
    font["head"].macStyle = 1 if style == "Bold" else 0
    if "STAT" in font:
        del font["STAT"]
    font.save(os.path.join(outdir, f"SpaceGrotesk-{style}.ttf"))
    print("wrote", style)
