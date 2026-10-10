"""重新生成应用图标（Windows）。

用法（在项目根目录执行）：
    python src-tauri/icons-src/build-icon.py

做两件事：
1. 调用 `npx tauri icon`，由矢量源 prts-icon.svg 生成各平台素材（icns、各档 PNG）；
2. 重写 icon.ico，补齐 Windows 会用到的中间档位。

为什么需要第 2 步：`tauri icon` 只写 16/24/32/48/64/256 六帧，而 Windows 会按
当前显示缩放挑最接近的一帧。125% 缩放下桌面中图标是 60、任务栏是 40、大图标是
120：缺帧时系统会放大邻近帧，边缘与字形都会发虚。这里补齐 40/60/72/96/120/128。

改图标只改 src-tauri/icons-src/prts-icon.svg 里的数值，然后重跑本脚本。
"""

from pathlib import Path
import shutil
import subprocess
import sys

from PIL import Image

# Windows 场景用到的档位（含 125% / 150% 缩放下的 40/60/72/96/120）
ICO_SIZES = [
    (16, 16), (24, 24), (32, 32), (40, 40), (48, 48), (60, 60),
    (64, 64), (72, 72), (96, 96), (120, 120), (128, 128), (256, 256),
]


def main() -> int:
    here = Path(__file__).resolve()
    project_root = here.parents[2]          # src-tauri/icons-src/build-icon.py -> 项目根
    icons_dir = here.parents[1] / "icons"   # src-tauri/icons
    svg = here.parent / "prts-icon.svg"
    master = icons_dir / "icon.png"

    if not svg.exists():
        print(f"找不到矢量源：{svg}", file=sys.stderr)
        return 1

    # 1) 基础素材
    print("· npx tauri icon ...")
    result = subprocess.run(
        ["npx", "tauri", "icon", str(svg)],
        cwd=str(project_root),
        shell=(sys.platform == "win32"),
    )
    if result.returncode != 0:
        print("tauri icon 失败，已中止", file=sys.stderr)
        return result.returncode

    # 本项目只出 Windows，去掉移动端产物
    for name in ("android", "ios"):
        shutil.rmtree(icons_dir / name, ignore_errors=True)

    # 2) 补齐 icon.ico 档位
    if not master.exists():
        print(f"找不到中间素材：{master}", file=sys.stderr)
        return 1
    image = Image.open(master).convert("RGBA")
    ico = icons_dir / "icon.ico"
    image.save(ico, format="ICO", sizes=ICO_SIZES)
    print(f"· icon.ico 已写入 {len(ICO_SIZES)} 帧（源 {image.size[0]}px）")
    print("完成。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
