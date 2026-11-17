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

/// 主名最多多少字节：常见文件系统的文件名上限是 255 字节，给编号（` 99999`）与最长的后缀（文件任务的任务目录
/// `.<扩展名，至多 16 字节>.hsdl`，22 字节）留出余量。
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
pub(crate) fn clean_title(text: &str) -> String {
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
/// `name`，被占用时主名后依次加 ` 2`、` 3`……。这个文件名与它的任务目录（[`file_work_dir`]）在磁盘上已存在、或键在
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
        let name = named(stem);
        let work_dir = file_work_dir(&folder.join(&name))
            .file_name()
            .expect("任务目录以 <文件名>.hsdl 结尾")
            .to_string_lossy()
            .into_owned();
        vec![name, work_dir]
    })?;
    let name = named(&stem);
    let path = folder.join(&name);
    Ok((name, path))
}

/// 文件任务的任务目录的后缀。
const WORK_DIR_SUFFIX: &str = ".hsdl";

/// 文件任务 `path`（以文件名结尾）的任务目录：同级的 `<文件名>.hsdl`，文件名整个保留。由应用指定，不用核心的默认位置
/// （核心按最后一个 `.` 去掉扩展名，`app-1.2.3-linux` 与 `app-1.2.4-linux` 会落到同一个 `app-1.2.hsdl`）；取名时按它
/// 查占用（[`file_output_for`]）。
pub fn file_work_dir(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .expect("文件任务的输出以文件名结尾")
        .to_os_string();
    name.push(WORK_DIR_SUFFIX);
    path.with_file_name(name)
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

/// 在 `folder` 里给 BT 内容的根选名字并拼出路径，返回根名与路径。`name` 为种子名，先整理成能用的名字：单文件的照
/// [`file_name`] 的规则（保留扩展名），多文件的（根是目录）照标题的规则、整个当主名；什么都不剩时用 `fallback`（种子的
/// 编号）。根名在磁盘上已存在、或键（[`path_key`]）在 `taken` 里即算占用，这时主名后依次加 ` 2`、` 3`……
pub fn torrent_root_for(
    folder: &Path,
    name: &str,
    single_file: bool,
    fallback: &str,
    taken: &HashSet<String>,
) -> Result<(String, PathBuf), Error> {
    root_for(folder, name, single_file, fallback, taken, false)
}

/// 在 `folder` 里给云离线内容的根选名字并拼出路径，返回根名与路径：`name` 为网盘上的文件或文件夹名，`fallback` 为
/// 网盘上离线任务的编号，规则同 [`torrent_root_for`]，另把根的任务目录（[`file_work_dir`]）一起算占用。
pub fn cloud_root_for(
    folder: &Path,
    name: &str,
    single_file: bool,
    fallback: &str,
    taken: &HashSet<String>,
) -> Result<(String, PathBuf), Error> {
    root_for(folder, name, single_file, fallback, taken, true)
}

/// [`torrent_root_for`] 与 [`cloud_root_for`]：`with_work_dir` 时 `<根名>.hsdl` 也算占用。
fn root_for(
    folder: &Path,
    name: &str,
    single_file: bool,
    fallback: &str,
    taken: &HashSet<String>,
    with_work_dir: bool,
) -> Result<(String, PathBuf), Error> {
    let cleaned = if single_file {
        clean_file_name(name)
    } else {
        clean_title(name)
    };
    let cleaned = if cleaned.is_empty() {
        fallback.to_owned()
    } else {
        cleaned
    };
    let (stem, extension) = if single_file {
        split_extension(&cleaned)
    } else {
        (cleaned.as_str(), None)
    };
    let named = |stem: &str| match extension {
        Some(extension) => format!("{stem}.{extension}"),
        None => stem.to_owned(),
    };
    let stem = free_stem(folder, stem, taken, |stem| {
        let root = named(stem);
        if with_work_dir {
            vec![format!("{root}{WORK_DIR_SUFFIX}"), root]
        } else {
            vec![root]
        }
    })?;
    let root = named(&stem);
    let path = folder.join(&root);
    Ok((root, path))
}

/// 网盘文件夹里一个文件的位置：从内容的根往下的各级文件夹（名字与网盘上的编号），最后是文件的名字与编号。
pub struct CloudPlace {
    pub folders: Vec<(String, String)>,
    pub file: (String, String),
}

/// 网盘文件夹里各文件相对内容的根的路径，以 `/` 分隔，与 `places` 一一对应：文件夹名照标题的规则、文件名照
/// [`file_name`] 的规则整理，什么都不剩时用网盘上的编号；同一文件夹里整理后重名的（不分大小写与 Unicode 写法，
/// 见 [`path_key`]）按先后在主名后加 ` 2`、` 3`……，网盘上的同一个文件夹总是同一个名字。
pub fn cloud_paths(places: &[CloudPlace]) -> Vec<String> {
    use std::collections::HashMap;
    // 每个本地目录（相对路径）里已用的名字的键，与网盘上的文件夹编号分到的名字
    let mut used: HashMap<String, HashSet<String>> = HashMap::new();
    let mut folder_names: HashMap<String, String> = HashMap::new();
    let mut pick = |parent: &str, cleaned: String, fallback: &str, file: bool| -> String {
        let cleaned = if cleaned.is_empty() {
            fallback.to_owned()
        } else {
            cleaned
        };
        let (stem, extension) = if file {
            split_extension(&cleaned)
        } else {
            (cleaned.as_str(), None)
        };
        let names = used.entry(parent.to_owned()).or_default();
        let mut number = 1;
        loop {
            let stem = if number == 1 {
                stem.to_owned()
            } else {
                format!("{stem} {number}")
            };
            let name = match extension {
                Some(extension) => format!("{stem}.{extension}"),
                None => stem,
            };
            if names.insert(path_key(&name)) {
                return name;
            }
            number += 1;
        }
    };
    places
        .iter()
        .map(|place| {
            let mut path = String::new();
            for (name, id) in &place.folders {
                let assigned = match folder_names.get(id) {
                    Some(assigned) => assigned.clone(),
                    None => {
                        let assigned = pick(&path, clean_title(name), id, false);
                        folder_names.insert(id.clone(), assigned.clone());
                        assigned
                    }
                };
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(&assigned);
            }
            let (name, id) = &place.file;
            let file = pick(&path, clean_file_name(name), id, true);
            if path.is_empty() {
                file
            } else {
                format!("{path}/{file}")
            }
        })
        .collect()
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
        std::fs::create_dir_all(dir.join("a.zip.hsdl")).unwrap();
        std::fs::write(dir.join("a 2.zip"), b"").unwrap();
        // 核心的默认位置会是 a.hsdl：不算占用
        std::fs::create_dir_all(dir.join("c.hsdl")).unwrap();
        assert_eq!(
            file_output_for(&dir, "c.zip", &HashSet::new()).unwrap().0,
            "c.zip"
        );
        // 最后一段不像扩展名的：任务目录照样随编号变
        std::fs::create_dir_all(dir.join("app-1.2.3-linux.hsdl")).unwrap();
        assert_eq!(
            file_output_for(&dir, "app-1.2.3-linux", &HashSet::new())
                .unwrap()
                .0,
            "app-1.2.3-linux 2"
        );
        assert_eq!(
            file_work_dir(&dir.join("app-1.2.3-linux 2")),
            dir.join("app-1.2.3-linux 2.hsdl")
        );
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
    fn torrent_roots_skip_names_and_keep_dots_in_directories() {
        let dir =
            std::env::temp_dir().join(format!("hs-dlx-app-torrent-naming-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(dir.join("Show.S01")).unwrap();
        std::fs::write(dir.join("movie.mkv"), b"").unwrap();
        let none = HashSet::new();
        // 目录名里的点不当扩展名
        assert_eq!(
            torrent_root_for(&dir, "Show.S01", false, "h", &none).unwrap(),
            ("Show.S01 2".to_owned(), dir.join("Show.S01 2"))
        );
        assert_eq!(
            torrent_root_for(&dir, "movie.mkv", true, "h", &none).unwrap(),
            ("movie 2.mkv".to_owned(), dir.join("movie 2.mkv"))
        );
        let taken = HashSet::from([path_key(&dir.join("A_B").to_string_lossy())]);
        assert_eq!(
            torrent_root_for(&dir, "a:b", false, "h", &taken).unwrap().0,
            "a_b 2"
        );
        assert_eq!(
            torrent_root_for(&dir, " .. ", false, "abc123", &none)
                .unwrap()
                .0,
            "abc123"
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

    fn place(folders: &[(&str, &str)], file: (&str, &str)) -> CloudPlace {
        CloudPlace {
            folders: folders
                .iter()
                .map(|(name, id)| ((*name).to_owned(), (*id).to_owned()))
                .collect(),
            file: (file.0.to_owned(), file.1.to_owned()),
        }
    }

    /// 各段照文件名的规则整理；同一目录里整理后重名的（不分大小写）加编号，同一个网盘文件夹总是同一个名字；什么都不剩
    /// 的用编号。
    #[test]
    fn cloud_paths_are_cleaned_and_distinct() {
        let places = [
            place(&[], ("a:b.mkv", "V1")),
            place(&[], ("A_B.mkv", "V2")),
            place(&[("Sub", "F1")], ("x.srt", "V3")),
            place(&[("sub", "F2")], ("x.srt", "V4")),
            place(&[("Sub", "F1"), ("..", "F3")], ("...", "V5")),
        ];
        assert_eq!(
            cloud_paths(&places),
            [
                "a_b.mkv",
                "A_B 2.mkv",
                "Sub/x.srt",
                "sub 2/x.srt",
                "Sub/F3/V5",
            ]
        );
    }
}
