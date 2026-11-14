"""scripts/release.py：从更新日志取某个版本的发布说明、读工作区版本号。"""

from pathlib import Path

import pytest

from release import ROOT, notes, version

CHANGELOG = """# 更新日志

<!-- 新条目先写进 [Unreleased]，发版时改成版本号与日期 -->

## [Unreleased]

## [0.2.0] - 2026-12-01

### 新功能
- 磁力下载

```sh
## 这一行在代码块里，不是标题
```

## [0.1.0] - 2026-11-15

- 首个预览版
"""


def test_notes_takes_the_version_section_and_skips_code_blocks() -> None:
    assert notes(CHANGELOG, "0.2.0") == "### 新功能\n- 磁力下载\n\n```sh\n## 这一行在代码块里，不是标题\n```"


def test_notes_reads_the_last_section_and_crlf() -> None:
    assert notes(CHANGELOG.replace("\n", "\r\n"), "0.1.0") == "- 首个预览版"


def test_notes_without_the_version_fails() -> None:
    with pytest.raises(SystemExit, match="实有 0 个"):
        notes(CHANGELOG, "0.3.0")


def test_notes_with_a_duplicated_version_fails() -> None:
    with pytest.raises(SystemExit, match="实有 2 个"):
        notes(CHANGELOG + "\n## [0.1.0]\n\n- 又一节\n", "0.1.0")


def test_notes_with_an_empty_section_fails() -> None:
    with pytest.raises(SystemExit, match="空的"):
        notes("## [0.1.0] - 2026-11-15\n\n<!-- 还没写 -->\n\n## [0.0.9]\n- 旧的\n", "0.1.0")


def test_notes_with_an_unclosed_comment_fails() -> None:
    with pytest.raises(SystemExit, match="没闭合"):
        notes("## [0.1.0]\n\n- 修复\n<!-- 忘了闭合\n", "0.1.0")


def test_repository_changelog_has_the_current_version() -> None:
    changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
    assert notes(changelog, version())


def test_notes_fails_on_an_unclosed_code_block() -> None:
    with pytest.raises(SystemExit, match="代码块"):
        notes("## [0.2.0]\n\n```sh\nx\n\n## [0.1.0]\n\n- 旧的\n", "0.2.0")


def test_notes_closes_a_code_block_only_with_the_same_fence() -> None:
    changelog = "## [0.2.0]\n\n````md\n~~~\n```\n## 不是标题\n<!-- 原样保留 -->\n````\n\n## [0.1.0]\n- 旧的\n"
    assert notes(changelog, "0.2.0") == "````md\n~~~\n```\n## 不是标题\n<!-- 原样保留 -->\n````"


def test_notes_drops_comment_only_lines_and_inline_comments() -> None:
    changelog = "## [0.1.0]\n\n<!--\n提示\n-->\n- 修复<!-- 内部 -->崩溃\n"
    assert notes(changelog, "0.1.0") == "- 修复崩溃"


def manifest(tmp_path: Path, text: str) -> Path:
    path = tmp_path / "Cargo.toml"
    path.write_text(text, encoding="utf-8")
    return path


def test_version_reads_the_workspace_package(tmp_path: Path) -> None:
    path = manifest(tmp_path, '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.2.10"\n')
    assert version(path) == "0.2.10"


@pytest.mark.parametrize(
    "text",
    [
        '[package]\nversion = "0.1.0"\n',
        '[workspace.package]\nversion = "0.1"\n',
        '[workspace.package]\nversion = "0.1.0-beta.1"\n',
        '[workspace.package]\nversion = "01.0.0"\n',
    ],
)
def test_version_rejects_missing_or_other_forms(tmp_path: Path, text: str) -> None:
    with pytest.raises(SystemExit, match=r"X\.Y\.Z"):
        version(manifest(tmp_path, text))
