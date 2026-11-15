//! 新建任务的命名：从网页标题、服务器给的文件名或地址得出标题，在保存目录里选一个不与已有输出、任务目录冲突的主名，
//! 拼出输出路径。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use hs_m3u8_core::Url;
use icu_normalizer::ComposingNormalizerBorrowed;
use percent_encoding::percent_decode_str;

use crate::{Error, OutputKind, OutputSpec};

/// 比较路径用的键：转小写再做 NFC 规范化。macOS（APFS）与 Windows 的文件系统默认不分大小写，APFS 也不分 Unicode
/// 的组合写法，键相同的两个路径在那里是同一个文件。
/// 简化：小写而不是完整的大小写折叠（少数字母如 ß 不等同），遇到这样的名字只会多算一次不冲突。
pub fn path_key(path: &str) -> String {
    ComposingNormalizerBorrowed::new_nfc()
        .normalize(&path.to_lowercase())
        .into_owned()
}

/// 地址路径里不足以当名字、要取上一级目录名的末段（不分大小写，已去掉 `.m3u8`）。
const GENERIC: [&str; 5] = ["index", "master", "playlist", "prog_index", "main"];

/// 主名最多多少字节：常见文件系统的文件名上限是 255 字节，给编号（` 99999`）与最长的后缀（`.hsdl`、`.mp4`）留出余量。
const STEM_LIMIT: usize = 200;

/// 列表与文件名用的标题：地址路径的最后一段（百分号解码、去掉 `.m3u8`）；是 index、master 之类的通用名时取上一级
/// 目录名；整理成文件主名（不能出现在文件名里的字符与控制字符换成 `_`，去掉首尾的空白与 `.`，截到 200 字节），
/// 什么都不剩时用主机名。
pub fn title_of(url: &Url) -> String {
    let segments: Vec<String> = url
        .path_segments()
        .map(|segments| {
            segments
                .filter(|s| !s.is_empty())
                .map(|s| percent_decode_str(s).decode_utf8_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let stem = |s: &str| {
        let lower = s.to_ascii_lowercase();
        match lower.strip_suffix(".m3u8") {
            Some(_) => s[..s.len() - ".m3u8".len()].to_owned(),
            None => s.to_owned(),
        }
    };
    let last = segments.last().map(|s| stem(s)).unwrap_or_default();
    let name = if GENERIC.contains(&last.to_ascii_lowercase().as_str()) && segments.len() >= 2 {
        segments[segments.len() - 2].clone()
    } else {
        last
    };
    let clean = clean_title(&name);
    if clean.is_empty() {
        url.host_str().unwrap_or("视频").to_owned()
    } else {
        clean
    }
}

/// 任务的标题：给了 `title`（如视频所在网页的标题）时为它照 [`title_of`] 的规则整理成文件主名后的，什么都不剩或没给
/// 时由地址得出（[`title_of`]）。
pub fn task_title(url: &Url, title: Option<&str>) -> String {
    title
        .map(clean_title)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| title_of(url))
}

/// 把 `text` 整理成能当文件主名的标题：文件名里不能出现的字符与控制字符换成 `_`，去掉首尾的空白与 `.`，超过 200
/// 字节时在字符边界截断（给编号与后缀留出文件名上限 255 字节的余量）。可能为空。
fn clean_title(text: &str) -> String {
    let clean: String = text
        .chars()
        .map(|c| {
            if "/\\:*?\"<>|".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    truncate(clean.trim().trim_matches('.'), STEM_LIMIT).to_owned()
}

/// `text` 的前缀，不超过 `limit` 字节、落在字符边界上。
fn truncate(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let end = (0..=limit)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .expect("0 总是字符边界");
    text[..end].trim_end()
}

/// 扩展名最多多少字节（不含点）：更长的（如地址末段里的一长串编码）不当作扩展名。
const EXTENSION_LIMIT: usize = 16;

/// 文件任务的文件名：服务器给的 `given`（Content-Disposition），没给或整理后什么都不剩时取地址路径的最后一段（百分号
/// 解码后），再没有时用主机名。整理同标题：文件名里不能出现的字符与控制字符换成 `_`，去掉首尾的空白与 `.`，主名超过
/// 200 字节时在字符边界截断；扩展名（最后一个 `.` 之后、不超过 16 字节、只有字母与数字的部分）保留，不计入截断。
pub fn file_name(url: &Url, given: Option<&str>) -> String {
    let last = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|s| !s.is_empty()))
        .map(|s| percent_decode_str(s).decode_utf8_lossy().into_owned());
    [given.map(str::to_owned), last]
        .into_iter()
        .flatten()
        .map(|name| clean_file_name(&name))
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| url.host_str().unwrap_or("文件").to_owned())
}

/// 把 `name` 整理成能用的文件名：主名照 [`clean_title`]，扩展名里不能出现在文件名里的字符换成 `_`。主名什么都不剩
/// 时为空。
fn clean_file_name(name: &str) -> String {
    let (stem, extension) = split_extension(name.trim());
    let stem = clean_title(stem);
    match extension {
        _ if stem.is_empty() => String::new(),
        Some(extension) => format!("{stem}.{}", clean_title(extension)),
        None => stem,
    }
}

/// 拆出扩展名：最后一个 `.` 之后、不超过 [`EXTENSION_LIMIT`] 字节、只有字母与数字的部分；`.` 在开头（隐藏文件的写法）
/// 或没有这样的部分时没有扩展名。
fn split_extension(name: &str) -> (&str, Option<&str>) {
    match name.rsplit_once('.') {
        Some((stem, extension))
            if !stem.is_empty()
                && !extension.is_empty()
                && extension.len() <= EXTENSION_LIMIT
                && extension.chars().all(char::is_alphanumeric) =>
        {
            (stem, Some(extension))
        }
        _ => (name, None),
    }
}

/// 在 `folder` 里给文件名 `name`（[`file_name`] 整理过的）选一个文件名并拼出输出路径，返回选定的文件名与路径：为
/// `name`，被占用时主名后依次加 ` 2`、` 3`……。这个文件名与它的任务目录（`<主名>.hsdl`）在磁盘上已存在、或键在
/// `taken` 里，都算占用（同 [`output_for`]）。
pub fn file_output_for(
    folder: &Path,
    name: &str,
    taken: &HashSet<String>,
) -> Result<(String, PathBuf), Error> {
    let (stem, extension) = split_extension(name);
    let named = |stem: &str| match extension {
        Some(extension) => format!("{stem}.{extension}"),
        None => stem.to_owned(),
    };
    let stem = free_stem(folder, stem, taken, |stem| {
        vec![named(stem), format!("{stem}.hsdl")]
    })?;
    let name = named(&stem);
    let path = folder.join(&name);
    Ok((name, path))
}

/// `folder` 里不被占用的主名：`stem`，被占用时依次加 ` 2`、` 3`……；`names` 给出一个主名占用的各个文件名，其中任一个
/// 在磁盘上已存在、或键（[`path_key`]）在 `taken` 里即算占用。
fn free_stem(
    folder: &Path,
    stem: &str,
    taken: &HashSet<String>,
    names: impl Fn(&str) -> Vec<String>,
) -> Result<String, Error> {
    let occupied = |stem: &str| -> Result<bool, Error> {
        for name in names(stem) {
            let path = folder.join(name);
            if taken.contains(&path_key(&path.to_string_lossy())) {
                return Ok(true);
            }
            let exists = path.try_exists().map_err(|cause| Error::Io {
                action: "检查",
                path: path.clone(),
                cause,
            })?;
            if exists {
                return Ok(true);
            }
        }
        Ok(false)
    };
    let mut chosen = stem.to_owned();
    let mut number = 1;
    while occupied(&chosen)? {
        number += 1;
        chosen = format!("{stem} {number}");
    }
    Ok(chosen)
}

/// 在 `folder` 里给标题 `title` 选主名并拼出输出，返回主名与输出：主名为 `title`，被占用时依次加 ` 2`、` 3`……。
/// 主名下的 MP4、本地 HLS 目录与任务目录（`<主名>.hsdl`）在磁盘上已存在、或键（[`path_key`]）在 `taken`（存储里已有
/// 任务的输出与任务目录）里，都算占用；这样新任务不会与已有任务共用任务目录，即使那个任务还没在磁盘上留下任何东西。
pub fn output_for(
    folder: &Path,
    title: &str,
    kind: OutputKind,
    taken: &HashSet<String>,
) -> Result<(String, OutputSpec), Error> {
    let stem = free_stem(folder, title, taken, |stem| {
        vec![
            format!("{stem}.mp4"),
            stem.to_owned(),
            format!("{stem}.hsdl"),
        ]
    })?;
    let mp4 = folder.join(format!("{stem}.mp4"));
    let hls = folder.join(&stem);
    let output = match kind {
        OutputKind::Mp4 => OutputSpec::Mp4(mp4),
        OutputKind::Hls => OutputSpec::Hls(hls),
        OutputKind::Both => OutputSpec::Both { mp4, hls },
    };
    Ok((stem, output))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(url: &str) -> String {
        title_of(&Url::parse(url).unwrap())
    }

    #[test]
    fn titles_come_from_the_decoded_path() {
        assert_eq!(title("https://a.example/v/keynote.m3u8?t=1"), "keynote");
        assert_eq!(title("https://a.example/v/Keynote.M3U8"), "Keynote");
        assert_eq!(title("https://a.example/coast/ep3/master.m3u8"), "ep3");
        assert_eq!(title("https://a.example/a:b.m3u8"), "a_b");
        assert_eq!(title("https://a.example/影片/index.m3u8"), "影片");
        assert_eq!(title("https://a.example/%E5%BD%B1%E7%89%87.m3u8"), "影片");
        assert_eq!(title("https://a.example/a%2Fb.m3u8"), "a_b");
        assert_eq!(title("https://a.example/"), "a.example");
        assert_eq!(title("https://a.example/..m3u8"), "a.example");
    }

    #[test]
    fn file_names_keep_their_extension() {
        let url = |text: &str| Url::parse(text).unwrap();
        let movie = url("https://a.example/d/%E5%BD%B1%E7%89%87.mkv?sig=1");
        assert_eq!(file_name(&movie, None), "影片.mkv");
        assert_eq!(
            file_name(&movie, Some("Trailer: 4K.MP4")),
            "Trailer_ 4K.MP4"
        );
        // 服务器给的名字里的路径与开头的点去掉，什么都不剩时退回地址
        assert_eq!(
            file_name(&movie, Some("../../etc/passwd")),
            "_.._etc_passwd"
        );
        assert_eq!(file_name(&movie, Some("...")), "影片.mkv");
        assert_eq!(file_name(&url("https://a.example/"), None), "a.example");
        assert_eq!(
            file_name(&url("https://a.example/get.php?id=1"), None),
            "get.php"
        );
        // 不像扩展名的不当作扩展名，截短照主名的规则
        let long = format!("{}.{}", "影".repeat(100), "x".repeat(20));
        let name = file_name(&url("https://a.example/"), Some(&long));
        assert!(name.len() <= STEM_LIMIT, "{}", name.len());
        let name = file_name(
            &url("https://a.example/"),
            Some(&format!("{}.zip", "影".repeat(100))),
        );
        assert!(
            name.ends_with(".zip") && name.len() <= STEM_LIMIT + 4,
            "{name}"
        );
    }

    #[test]
    fn file_outputs_skip_names_and_work_dirs() {
        let dir =
            std::env::temp_dir().join(format!("hs-dlx-app-file-naming-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(dir.join("a.hsdl")).unwrap();
        std::fs::write(dir.join("a 2.zip"), b"").unwrap();
        let none = HashSet::new();
        assert_eq!(
            file_output_for(&dir, "a.zip", &none).unwrap(),
            ("a 3.zip".to_owned(), dir.join("a 3.zip"))
        );
        let taken = HashSet::from([path_key(&dir.join("B").to_string_lossy())]);
        assert_eq!(
            file_output_for(&dir, "b", &taken).unwrap(),
            ("b 2".to_owned(), dir.join("b 2"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn path_keys_ignore_case_and_unicode_forms() {
        assert_eq!(
            path_key("/M/Caf\u{e9}.hsdl"),
            path_key("/m/cafe\u{301}.HSDL")
        );
        assert_ne!(path_key("/m/a.hsdl"), path_key("/m/b.hsdl"));
    }

    #[test]
    fn long_titles_are_cut_on_a_character_boundary() {
        let long = "影".repeat(100);
        let cut = title(&format!("https://a.example/{long}.m3u8"));
        assert!(cut.len() <= STEM_LIMIT, "{}", cut.len());
        assert_eq!(cut, "影".repeat(STEM_LIMIT / 3));
    }

    #[test]
    fn stems_skip_names_on_disk_and_in_the_store() {
        let dir = std::env::temp_dir().join(format!("hs-dlx-app-naming-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(dir.join("talk 2.hsdl")).unwrap();
        let none = HashSet::new();
        assert_eq!(
            output_for(&dir, "talk", OutputKind::Mp4, &none).unwrap(),
            ("talk".to_owned(), OutputSpec::Mp4(dir.join("talk.mp4")))
        );
        // 存储里的路径与这次的只差大小写，也算占用（Unicode 写法见 path_keys_ignore_case_and_unicode_forms）
        let taken = HashSet::from([path_key(&dir.join("TALK.hsdl").to_string_lossy())]);
        assert_eq!(
            output_for(&dir, "talk", OutputKind::Both, &taken).unwrap(),
            (
                "talk 3".to_owned(),
                OutputSpec::Both {
                    mp4: dir.join("talk 3.mp4"),
                    hls: dir.join("talk 3"),
                }
            )
        );
        std::fs::write(dir.join("talk.mp4"), b"").unwrap();
        assert_eq!(
            output_for(&dir, "talk", OutputKind::Hls, &none).unwrap(),
            ("talk 3".to_owned(), OutputSpec::Hls(dir.join("talk 3")))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
