#!/usr/bin/env python3
"""生成 dmg 安装窗口的背景图（含「拖到 Applications」箭头提示）。

发布资源，随仓库提交（packaging/dmg-background.png，与 icon.png 同类，
在 .gitignore 里显式例外）。改设计后重跑本脚本即可：
    python3 scripts/gen_dmg_background.py

布局约定（与 packaging/dmg.DS_Store 模板一致）：
  - 窗口 540×380，背景图同尺寸（Finder 缩放到内容区）
  - 应用图标在左、Applications 快捷方式在右，iconSize=128，
    Iloc（即图标中心）分别为 (130,150) / (410,150)，两图标同一水平线
  - 中间一条虚线箭头指向右（对齐图标中心 y=150），下方一行中文提示
"""

from PIL import Image, ImageDraw, ImageFont

W, H = 540, 380
BG = (236, 236, 238)  # 与 Finder 窗口默认浅灰接近
ARROW = (140, 140, 148)
TEXT = (105, 105, 112)

img = Image.new("RGB", (W, H), BG)
d = ImageDraw.Draw(img)

# 图标中心高度 150（Iloc 即中心），箭头与两图标中心对齐。
# 图标右缘 = 130+64 = 194，Applications 左缘 = 410-64 = 346，箭头落在间隙内。
cy = 150

# 中间虚线箭头（左 → 右）
x0, x1 = 208, 312
seg, gap, thick = 12, 8, 3
x = x0
while x < x1:
    d.rectangle([x, cy - thick // 2, min(x + seg, x1), cy + thick // 2], fill=ARROW)
    x += seg + gap
# 箭头头部（右向三角）
d.polygon([(x1 + 4, cy - 12), (x1 + 4, cy + 12), (x1 + 22, cy)], fill=ARROW)

# 提示文字（中文）。新版 macOS 已把 PingFang.ttc 移到带哈希的 AssetsV2
# 路径（不可硬编码），按稳定路径依次尝试候选 CJK 字体；全部失败则报错，
# 绝不静默退回 load_default()（其内置字体无 CJK 字形，会渲染成豆腐块）。
FONT_CANDIDATES = [
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/STHeiti Medium.ttc",
    "/System/Library/Fonts/Supplemental/Songti.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
]
text = "拖动 LaterScreen 到 Applications 安装"
font = None
for path in FONT_CANDIDATES:
    try:
        font = ImageFont.truetype(path, 22)
        print(f"font: {path}")
        break
    except Exception:
        continue
if font is None:
    raise SystemExit("未找到可用 CJK 字体，请安装后重跑（候选：" + ", ".join(FONT_CANDIDATES) + "）")
tw = d.textlength(text, font=font)
assert tw < W - 40, f"提示文字过宽（{tw:.0f}px），请缩短文案或调小字号"
d.text(((W - tw) / 2, 288), text, font=font, fill=TEXT)

img.save("packaging/dmg-background.png")
print(f"generated packaging/dmg-background.png ({W}x{H})")
