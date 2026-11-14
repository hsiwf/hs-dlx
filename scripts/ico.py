"""把几张正方形 PNG 拼成一个 Windows 图标（.ico）：每个尺寸一项，图像数据原样存 PNG（Windows Vista 起支持）。

用法：`python scripts/ico.py 输出.ico 16.png 24.png ... 256.png`。边长取自各 PNG 的文件头，须在 1 到 256 之间、
互不相同。
"""

import struct
import sys
from pathlib import Path

PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
MAX_SIDE = 256
HEADER = struct.Struct("<HHH")
ENTRY = struct.Struct("<BBBBHHII")


def side_of(png: bytes, name: str) -> int:
    """PNG 的边长（IHDR 里的宽，须与高相同）。"""
    if not png.startswith(PNG_SIGNATURE) or png[12:16] != b"IHDR":
        msg = f"{name} 不是 PNG"
        raise ValueError(msg)
    width: int
    height: int
    width, height = struct.unpack(">II", png[16:24])
    if width != height or not 1 <= width <= MAX_SIDE:
        msg = f"{name} 是 {width}×{height}，要 1 到 {MAX_SIDE} 的正方形"
        raise ValueError(msg)
    return width


def pack(pngs: list[tuple[str, bytes]]) -> bytes:
    """按边长从小到大排好的图标文件。"""
    sized = sorted(((side_of(data, name), data) for name, data in pngs), key=lambda item: item[0])
    sides = [side for side, _ in sized]
    if len(set(sides)) != len(sides):
        msg = f"边长有重复：{sides}"
        raise ValueError(msg)
    offset = HEADER.size + ENTRY.size * len(sized)
    entries: list[bytes] = []
    images: list[bytes] = []
    for side, data in sized:
        # 边长 256 在目录里写 0
        byte = side % MAX_SIDE
        entries.append(ENTRY.pack(byte, byte, 0, 0, 1, 32, len(data), offset))
        images.append(data)
        offset += len(data)
    return HEADER.pack(0, 1, len(sized)) + b"".join(entries) + b"".join(images)


def main() -> None:
    """按命令行参数写出图标文件。"""
    out, *inputs = sys.argv[1:]
    if not inputs:
        sys.exit("用法：ico.py 输出.ico 图1.png 图2.png ...")
    Path(out).write_bytes(pack([(name, Path(name).read_bytes()) for name in inputs]))


if __name__ == "__main__":
    main()
