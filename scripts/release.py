"""发版用到的两项，由 .github/workflows/release.yml 调用。

`python3 scripts/release.py version`：打印 Cargo.toml 里 [workspace.package] 的 version（形如 X.Y.Z）。
`python3 scripts/release.py notes`：从标准输入读 PR 正文，打印其中「## 发布说明」一节，作为 GitHub Release 的描述。
没有这一节、或去掉 HTML 注释后是空的时失败：不发没有描述的版本。
"""

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HEADING = "## 发布说明"
# 安装包文件名与标签都用它；预发布与构建元数据不用
VERSION = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)")
COMMENT = re.compile(r"<!--.*?-->", re.DOTALL)


def version(manifest: Path = ROOT / "Cargo.toml") -> str:
    """工作区的版本号；读不到或不是 X.Y.Z 时失败。"""
    with manifest.open("rb") as file:
        data = tomllib.load(file)
    value = data.get("workspace", {}).get("package", {}).get("version")
    if not isinstance(value, str) or VERSION.fullmatch(value) is None:
        message = f"{manifest} 的 [workspace.package] version 应形如 X.Y.Z，读到 {value!r}"
        raise SystemExit(message)
    return value


def notes(body: str) -> str:
    """PR 正文里「## 发布说明」之后到下一个二级标题之前的内容，去掉 HTML 注释与首尾空白；没有这一节或为空时失败。"""
    lines = body.replace("\r\n", "\n").split("\n")
    start = next((i for i, line in enumerate(lines) if line.strip() == HEADING), None)
    if start is None:
        message = f"PR 正文里没有「{HEADING}」一节"
        raise SystemExit(message)
    rest = lines[start + 1 :]
    end = next((i for i, line in enumerate(rest) if line.startswith("## ")), len(rest))
    section = COMMENT.sub("", "\n".join(rest[:end])).strip()
    if not section:
        message = f"PR 正文的「{HEADING}」一节是空的"
        raise SystemExit(message)
    return section


def main(argv: list[str]) -> None:
    """按子命令打印版本号或发布说明。"""
    match argv:
        case ["version"]:
            sys.stdout.write(f"{version()}\n")
        case ["notes"]:
            sys.stdout.write(f"{notes(sys.stdin.read())}\n")
        case _:
            message = "用法：release.py version，或 release.py notes < PR 正文"
            raise SystemExit(message)


if __name__ == "__main__":
    main(sys.argv[1:])
