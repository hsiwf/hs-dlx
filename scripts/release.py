"""发版用到的两项，由 .github/workflows/release.yml 调用。

`python3 scripts/release.py version`：打印 Cargo.toml 里 [workspace.package] 的 version（形如 X.Y.Z）。
`python3 scripts/release.py notes X.Y.Z`：打印 CHANGELOG.md 里 `## [X.Y.Z]` 一节的内容，作为 GitHub Release 的描述。没有
这一节、或去掉 HTML 注释后是空的时失败：不发没有描述的版本。
"""

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# 安装包文件名与标签都用它；预发布与构建元数据不用
VERSION = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)")
COMMENT = re.compile(r"<!--.*?-->", re.DOTALL)
# 版本一节的标题：`## [X.Y.Z]`，后面可跟 ` - YYYY-MM-DD`
SECTION = re.compile(r"## \[(?P<version>[^\]]+)\](?: - \d{4}-\d{2}-\d{2})?\s*")
FENCE = re.compile(r"\s*(```|~~~)")


def version(manifest: Path = ROOT / "Cargo.toml") -> str:
    """工作区的版本号；读不到或不是 X.Y.Z 时失败。"""
    with manifest.open("rb") as file:
        data = tomllib.load(file)
    value = data.get("workspace", {}).get("package", {}).get("version")
    if not isinstance(value, str) or VERSION.fullmatch(value) is None:
        message = f"{manifest} 的 [workspace.package] version 应形如 X.Y.Z，读到 {value!r}"
        raise SystemExit(message)
    return value


def notes(changelog: str, wanted: str) -> str:
    """更新日志里 `## [wanted]` 之后到下一个二级标题之前的内容（代码块里的行不算标题），去掉 HTML 注释与首尾空白。

    没有这一节、有两节、为空，或留下没闭合的 HTML 注释时失败。
    """
    lines = COMMENT.sub("", changelog.replace("\r\n", "\n")).split("\n")
    if any("<!--" in line for line in lines):
        message = "更新日志里有没闭合的 HTML 注释（<!--）"
        raise SystemExit(message)
    sections: list[list[str]] = []
    current: list[str] | None = None
    fenced = False
    for line in lines:
        if FENCE.match(line):
            fenced = not fenced
        elif not fenced and line.startswith("## "):
            heading = SECTION.fullmatch(line)
            current = [] if heading is not None and heading["version"] == wanted else None
            if current is not None:
                sections.append(current)
            continue
        if current is not None:
            current.append(line)
    if len(sections) != 1:
        message = f"更新日志里 [{wanted}] 一节应有 1 个，实有 {len(sections)} 个"
        raise SystemExit(message)
    text = "\n".join(sections[0]).strip()
    if not text:
        message = f"更新日志的 [{wanted}] 一节是空的"
        raise SystemExit(message)
    return text


def main(argv: list[str]) -> None:
    """按子命令打印版本号或发布说明。"""
    match argv:
        case ["version"]:
            sys.stdout.write(f"{version()}\n")
        case ["notes", wanted]:
            changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
            sys.stdout.write(f"{notes(changelog, wanted)}\n")
        case _:
            message = "用法：release.py version，或 release.py notes X.Y.Z"
            raise SystemExit(message)


if __name__ == "__main__":
    main(sys.argv[1:])
