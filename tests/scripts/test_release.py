"""scripts/release.py：从 PR 正文取发布说明、读工作区版本号。"""

from pathlib import Path

import pytest

from release import notes, version

BODY = """## 改动

改了什么。

## 发布说明

<!-- 填写提示 -->

### 新功能
- 磁力下载

### 修复
- 暂停后再继续不再从头下载

## 验证

make check 通过
"""


def test_notes_takes_the_section_up_to_the_next_heading() -> None:
    assert notes(BODY) == "### 新功能\n- 磁力下载\n\n### 修复\n- 暂停后再继续不再从头下载"


def test_notes_reads_crlf_bodies_and_the_last_section() -> None:
    body = "## 改动\r\n\r\n略\r\n\r\n## 发布说明\r\n\r\n- 修复崩溃\r\n"
    assert notes(body) == "- 修复崩溃"


def test_notes_without_the_section_fails() -> None:
    with pytest.raises(SystemExit, match="没有"):
        notes("## 改动\n\n略\n")


def test_notes_with_only_comments_fails() -> None:
    with pytest.raises(SystemExit, match="空的"):
        notes("## 发布说明\n\n<!--\n写法示例：\n### 新功能\n-->\n\n## 验证\n")


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
