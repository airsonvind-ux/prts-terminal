"""生成安装向导的页头图与侧边图（NSIS 用）。

用法（项目根目录）：
    python src-tauri/icons-src/build-installer-images.py

产出（均写入 src-tauri/windows/nsis-images/，24 位 BMP，NSIS 要求无透明通道）：
    header.bmp    150 x 57   向导每一页顶部右侧那条
    sidebar.bmp   164 x 314  欢迎页与完成页左侧竖条

设计沿用应用图标：深底 #26282a、菱形内 #1c1f22、边框 #c8cdd0、字样 #18d1ff。
纯几何，不含任何绘制性元素；改配色或尺寸只改本文件常量再重跑。
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

TILE = (38, 40, 42)        # #26282a 深底
DIA_INNER = (28, 31, 34)   # #1c1f22 菱形内部
FRAME = (200, 205, 208)    # #c8cdd0 边框与浅色字
CYAN = (24, 209, 255)      # #18d1ff 字样

FONT_CANDIDATES = [
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\segoeui.ttf",
    r"C:\Windows\Fonts\arial.ttf",
]


def load_font(size: int) -> ImageFont.FreeTypeFont:
    for path in FONT_CANDIDATES:
        if Path(path).exists():
            try:
                return ImageFont.truetype(path, size)
            except Exception:
                continue
    return ImageFont.load_default()


def draw_mark(draw: ImageDraw.ImageDraw, cx: int, cy: int, half: int, with_text: bool) -> None:
    """画菱形标记（与图标同构）：菱形 + 边框 + 可选居中青字 PRTS。"""
    frame_w = max(1, round(half * 40 / 420))
    pts = [(cx, cy - half), (cx + half, cy), (cx, cy + half), (cx - half, cy)]
    draw.polygon(pts, fill=DIA_INNER, outline=FRAME, width=frame_w)
    if with_text:
        size = max(9, round(half * 0.419))
        font = load_font(size)
        text = "PRTS"
        left, top, right, bottom = draw.textbbox((0, 0), text, font=font)
        draw.text(
            (cx - (right - left) / 2 - left, cy - (bottom - top) / 2 - top - half * 0.06),
            text,
            font=font,
            fill=CYAN,
        )


def draw_word(draw: ImageDraw.ImageDraw, cx: int, cy: int, size: int) -> None:
    font = load_font(size)
    text = "PRTS Terminal"
    left, top, right, bottom = draw.textbbox((0, 0), text, font=font)
    draw.text((cx - (right - left) / 2 - left, cy - (bottom - top) / 2 - top), text, font=font, fill=FRAME)


def build_header(path: Path) -> None:
    w, h = 150, 57
    img = Image.new("RGB", (w, h), TILE)
    d = ImageDraw.Draw(img)
    draw_word(d, 46, 29, 12)
    draw_mark(d, 125, 28, 17, with_text=False)
    img.save(path, "BMP")


def build_sidebar(path: Path) -> None:
    w, h = 164, 314
    img = Image.new("RGB", (w, h), TILE)
    d = ImageDraw.Draw(img)
    draw_mark(d, w // 2, 126, 50, with_text=True)
    draw_word(d, w // 2, 232, 17)
    img.save(path, "BMP")


def main() -> int:
    out = Path(__file__).resolve().parents[1] / "windows" / "nsis-images"
    out.mkdir(parents=True, exist_ok=True)
    build_header(out / "header.bmp")
    build_sidebar(out / "sidebar.bmp")
    print(f"已生成到 {out}")
    for name in ("header.bmp", "sidebar.bmp"):
        p = out / name
        with Image.open(p) as im:
            print(f"  {name}: {im.size[0]}x{im.size[1]}  {im.mode}  {p.stat().st_size} 字节")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
