"""发版用到的两项，由 .github/workflows/release.yml 调用。

`python3 scripts/release.py version [Cargo.toml 的路径]`：打印工作区的 [workspace.package] version（形如 X.Y.Z），
不给路径时读仓库根目录的。
`python3 scripts/release.py notes X.Y.Z`：打印 CHANGELOG.md 里 `## [X.Y.Z]` 一节的内容，作为 GitHub Release 的描述。没有
这一节、或它是空的时失败：不发没有描述的版本。
"""

import re
import sys
import tomllib
from collections.abc import Iterator
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# 安装包文件名与标签都用它；预发布与构建元数据不用
VERSION = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)")
# 版本一节的标题：`## [X.Y.Z]`，后面可跟 ` - YYYY-MM-DD`
SECTION = re.compile(r"## \[(?P<version>[^\]]+)\](?: - \d{4}-\d{2}-\d{2})?\s*")
# 代码块的开头与结尾（CommonMark）：至多 3 个空格缩进，3 个以上的 ` 或 ~；` 开头的这一行后面不能再有 `（否则是行内
# 代码）；结尾只能有这一串，且不短于开头
FENCE_OPEN = re.compile(r" {0,3}(?:(?P<backticks>`{3,})[^`]*|(?P<tildes>~{3,}).*)")
FENCE_CLOSE = re.compile(r" {0,3}(?P<mark>`{3,}|~{3,})\s*")


def version(manifest: Path = ROOT / "Cargo.toml") -> str:
    """工作区的版本号；读不到或不是 X.Y.Z 时失败。"""
    with manifest.open("rb") as file:
        data = tomllib.load(file)
    value = data.get("workspace", {}).get("package", {}).get("version")
    if not isinstance(value, str) or VERSION.fullmatch(value) is None:
        message = f"{manifest} 的 [workspace.package] version 应形如 X.Y.Z，读到 {value!r}"
        raise SystemExit(message)
    return value


def strip_comments(line: str, *, in_comment: bool) -> tuple[str, bool]:
    """去掉一行里的 HTML 注释。

    `in_comment` 为这一行开始时是否在一段跨行的注释里；返回去掉后的内容与行末是否仍在注释里。
    """
    kept: list[str] = []
    rest = line
    while True:
        if in_comment:
            end = rest.find("-->")
            if end < 0:
                return "".join(kept), True
            rest = rest[end + 3 :]
            in_comment = False
        start = rest.find("<!--")
        if start < 0:
            kept.append(rest)
            return "".join(kept), False
        kept.append(rest[:start])
        rest = rest[start + 4 :]
        in_comment = True


def closes(line: str, fence: str) -> bool:
    """`line` 是否闭合以 `fence` 开头的代码块：同一种符号、不短于开头，且这一行只有这一串。"""
    close = FENCE_CLOSE.fullmatch(line)
    return close is not None and close["mark"][0] == fence[0] and len(close["mark"]) >= len(fence)


def markdown_lines(changelog: str) -> Iterator[tuple[bool, str]]:
    """逐行给出（是不是二级标题，去掉 HTML 注释后的行）。

    代码块（含开头那一行）原样给出，不算标题，也不去注释；整行都是注释的行不给出。到末尾仍有没闭合的代码块或 HTML 注释
    时失败。只认顶层的代码块：列表项、引用里的代码块按普通行处理（会去掉其中的注释）；行内代码里的 `<!--` 也按注释
    开头算。
    """
    fence: str | None = None
    in_comment = False
    for raw in changelog.replace("\r\n", "\n").split("\n"):
        if fence is not None:
            if closes(raw, fence):
                fence = None
            yield False, raw
            continue
        opening = None if in_comment else FENCE_OPEN.fullmatch(raw)
        if opening is not None:
            fence = opening["backticks"] or opening["tildes"]
            yield False, raw
            continue
        commented = in_comment or "<!--" in raw
        line, in_comment = strip_comments(raw, in_comment=in_comment)
        if commented and not line.strip():
            continue
        yield line.startswith("## "), line
    if fence is not None:
        message = f"更新日志末尾还有没闭合的代码块（{fence}）"
        raise SystemExit(message)
    if in_comment:
        message = "更新日志末尾还有没闭合的 HTML 注释（<!--）"
        raise SystemExit(message)


def notes(changelog: str, wanted: str) -> str:
    """更新日志里 `## [wanted]` 之后到下一个二级标题之前的内容（按 `markdown_lines` 逐行），去掉首尾空白。

    没有这一节、有两节、为空，或更新日志里有没闭合的代码块或 HTML 注释时失败。
    """
    sections: list[list[str]] = []
    current: list[str] | None = None
    for heading, line in markdown_lines(changelog):
        if heading:
            match = SECTION.fullmatch(line)
            current = [] if match is not None and match["version"] == wanted else None
            if current is not None:
                sections.append(current)
        elif current is not None:
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
        case ["version", manifest]:
            sys.stdout.write(f"{version(Path(manifest))}\n")
        case ["notes", wanted]:
            changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
            sys.stdout.write(f"{notes(changelog, wanted)}\n")
        case _:
            message = "用法：release.py version [Cargo.toml 的路径]，或 release.py notes X.Y.Z"
            raise SystemExit(message)


if __name__ == "__main__":
    main(sys.argv[1:])
