"""hs-m3u8 的 FFmpeg 构建脚本：Cargo.lock 固定的 hs-m3u8 提交里的 third_party/ffmpeg/build.sh（cargo 取下的副本）。

FFmpeg 用它编译，组件与核心库出自同一个提交。直接运行时打印脚本的路径，用正斜杠（Windows 上为 C:/…，MSYS2 的 bash
也认），供 Makefile 与 CI 用；scripts/licenses.py 从中取 FFmpeg 的版本。
"""

import json
import re
import subprocess
import sys
from pathlib import Path
from typing import NamedTuple

ROOT = Path(__file__).resolve().parent.parent
PACKAGE = "hs-m3u8-core"
# Cargo.lock 里 git 依赖的来源：git+<仓库>?rev=<写的 rev>#<完整的提交号>
SOURCE = re.compile(r"git\+(https://\S+?)\?rev=\w+#([0-9a-f]{40})")


class FfmpegScript(NamedTuple):
    """hs-m3u8 的 FFmpeg 构建脚本。"""

    path: Path
    repository: str
    commit: str

    def url(self) -> str:
        """脚本在 hs-m3u8 仓库网页上的地址。"""
        return f"{self.repository}/blob/{self.commit}/third_party/ffmpeg/build.sh"


def cargo(*args: str) -> str:
    """在仓库根目录运行 cargo，返回按 UTF-8 解码的标准输出（不随系统的区域编码）；失败时抛 CalledProcessError。"""
    return subprocess.run(["cargo", *args], cwd=ROOT, stdout=subprocess.PIPE, encoding="utf-8", check=True).stdout


def ffmpeg_script() -> FfmpegScript:
    """取构建脚本；工作区不是恰好依赖一个以 git rev 固定的 hs-m3u8-core、或那个提交里没有这个脚本时失败。"""
    output = cargo("metadata", "--format-version", "1", "--locked")
    packages = [package for package in json.loads(output)["packages"] if package["name"] == PACKAGE]
    if len(packages) != 1:
        message = f"Cargo.lock 里的 {PACKAGE} 有 {len(packages)} 个，应为 1 个"
        raise SystemExit(message)
    package = packages[0]
    source = SOURCE.fullmatch(package["source"] or "")
    if source is None:
        message = f"{PACKAGE} 的来源不是以 rev 固定的 git 仓库：{package['source']}"
        raise SystemExit(message)
    # 包在 hs-m3u8 仓库的 crates/core 下
    path = Path(package["manifest_path"]).parents[2] / "third_party/ffmpeg/build.sh"
    if not path.is_file():
        message = f"{PACKAGE} 的仓库副本里没有 {path}"
        raise SystemExit(message)
    return FfmpegScript(path=path, repository=source[1], commit=source[2])


if __name__ == "__main__":
    sys.stdout.write(f"{ffmpeg_script().path.as_posix()}\n")
