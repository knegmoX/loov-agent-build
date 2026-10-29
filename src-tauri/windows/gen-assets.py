"""生成 Windows 安装界面用的位图（NSIS 只能显示 BMP，没有透明通道，所以叠在白底上）。

源图：icons/icon.png（由 icons/loov-app-icon.svg 经 `tauri icon` 生成）。
72/108/144 px 分别对应 100%/150%/200% 缩放，安装程序按窗口 DPI 选用。
用法：python3 src-tauri/windows/gen-assets.py
"""
from pathlib import Path
from PIL import Image

here = Path(__file__).resolve().parent
src = Image.open(here.parent / 'icons' / 'icon.png').convert('RGBA')
for px in (72, 108, 144):
    bg = Image.new('RGBA', (px, px), (255, 255, 255, 255))
    bg.alpha_composite(src.resize((px, px), Image.LANCZOS))
    bg.convert('RGB').save(here / 'assets' / f'logo-{px}.bmp')
    print('logo', px)
