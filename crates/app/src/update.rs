//! 应用自己的更新：查 GitHub 上最新的 Release，下载本平台的安装包（经设置里的代理，能续传），用编进应用的 minisign 公钥
//! 验它的签名，再装上：macOS 把新的 .app 换到旧的位置，Windows 退出后静默运行安装程序；之后由 [`Restart`] 等本进程退出
//! 再打开新版本。Linux 的 .deb 要 root 才能装，只提示去下载页。
//!
//! 签名的可信注释为 `hs-dlx <版本> <文件名>`（发版时写入，见 .github/workflows/release.yml），逐字核对：别的版本或别的
//! 文件的有效签名不能拿来冒充。

use std::path::{Path, PathBuf};
use std::process::Command;

use hs_m3u8_core::{Engine, Proxy, Source, Url};
use serde::Deserialize;

use crate::Error;

/// 查最新的 Release 时响应体最多多少字节：GitHub 的 Release 说明连同各附件的信息远小于它。
const RELEASE_LIMIT: u64 = 1 << 20;
/// 签名文件最多多少字节：minisign 的签名约 300 字节。
const SIGNATURE_LIMIT: u64 = 4 << 10;
/// 读安装包验签时每次读多少字节。
const READ_CHUNK: usize = 1 << 20;

/// 更新从哪里查、用哪个公钥验。
#[derive(Debug, Clone)]
pub struct UpdateFeed {
    /// GitHub REST API 的最新 Release（`https://api.github.com/repos/<所有者>/<仓库>/releases/latest`）
    pub latest: Url,
    /// minisign 公钥（公钥文件的第二行，base64）
    pub public_key: String,
}

/// 版本号 `主.次.修订`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// 读 `主.次.修订`（各为十进制数）；别的写法（含预发布、构建号）为 None。
    pub fn parse(text: &str) -> Option<Version> {
        let mut parts = text.split('.').map(|part| {
            (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                .then(|| part.parse::<u64>().ok())
                .flatten()
        });
        let version = Version {
            major: parts.next()??,
            minor: parts.next()??,
            patch: parts.next()??,
        };
        parts.next().is_none().then_some(version)
    }

    /// 本应用的版本。
    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("工作区的版本号是 主.次.修订")
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// 比本应用新的 Release。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// Release 的网页
    pub page: Url,
    /// 发布说明（Markdown）
    pub notes: String,
    /// 本平台的安装包；None 时本平台不能自动装（Linux），到网页下载
    pub package: Option<Package>,
}

/// Release 里本平台的安装包与它的签名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// 文件名，如 `hs-dlx_0.3.0_aarch64.dmg`
    pub name: String,
    pub url: Url,
    pub signature: Url,
    /// 字节数
    pub size: u64,
}

/// 本平台的安装包的文件名在版本号之后的部分；None 为本平台不能自动装。
const PACKAGE_SUFFIX: Option<&str> = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("_aarch64.dmg")
} else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
    Some("_x64-setup.exe")
} else {
    None
};

/// GitHub REST API 的 Release，只取用到的字段。
#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

/// 读 Release 里的地址；认不出时报 [`Error::Update`]。
fn url_of(text: &str) -> Result<Url, Error> {
    Url::parse(text)
        .map_err(|error| Error::Update(format!("最新版本的信息里的地址认不出：{text}（{error}）")))
}

/// 查 `feed` 上最新的 Release，经代理 `proxy`；不比 `current` 新时为 None。取不到、不是 GitHub 的 Release、版本号认不出，
/// 或本平台能自动装而 Release 里没有本平台的安装包与签名时报 [`Error::Update`]。
pub(crate) async fn check(
    engine: &Engine,
    feed: &UpdateFeed,
    proxy: &Proxy,
    current: Version,
) -> Result<Option<Release>, Error> {
    let mut source = Source::new(feed.latest.clone());
    source.http.proxy = proxy.clone();
    source.http.headers = vec![
        (
            "Accept".to_owned(),
            "application/vnd.github+json".to_owned(),
        ),
        ("X-GitHub-Api-Version".to_owned(), "2022-11-28".to_owned()),
    ];
    let fetched = engine
        .fetch(&source, RELEASE_LIMIT)
        .await
        .map_err(|error| Error::Update(format!("查不到最新版本：{error}")))?;
    release_of(&fetched.body, current, PACKAGE_SUFFIX)
}

/// 读 GitHub 的 Release（见 [`check`]），本平台的安装包文件名为 `hs-dlx_<版本><suffix>`。
fn release_of(
    body: &[u8],
    current: Version,
    suffix: Option<&str>,
) -> Result<Option<Release>, Error> {
    let release: GitHubRelease = serde_json::from_slice(body)
        .map_err(|error| Error::Update(format!("最新版本的信息认不出：{error}")))?;
    if release.draft || release.prerelease {
        return Err(Error::Update(format!(
            "最新版本 {} 是草稿或预发布",
            release.tag_name
        )));
    }
    let version = release
        .tag_name
        .strip_prefix('v')
        .and_then(Version::parse)
        .ok_or_else(|| Error::Update(format!("最新版本的标签认不出：{}", release.tag_name)))?;
    if version <= current {
        return Ok(None);
    }
    let package = suffix
        .map(|suffix| {
            let name = format!("hs-dlx_{version}{suffix}");
            let signature = format!("{name}.minisig");
            let find = |wanted: &str| {
                release
                    .assets
                    .iter()
                    .find(|asset| asset.name == wanted)
                    .ok_or_else(|| Error::Update(format!("{version} 的 Release 里没有 {wanted}")))
            };
            let (asset, signed) = (find(&name)?, find(&signature)?);
            Ok::<_, Error>(Package {
                name,
                url: url_of(&asset.browser_download_url)?,
                signature: url_of(&signed.browser_download_url)?,
                size: asset.size,
            })
        })
        .transpose()?;
    Ok(Some(Release {
        version,
        page: url_of(&release.html_url)?,
        notes: release.body.unwrap_or_default(),
        package,
    }))
}

/// 取安装包的签名，用 `public_key` 解出并核对它是 `release` 的 `package` 的（可信注释）：返回解出的签名，下载完再验
/// 内容。取不到、认不出、公钥对不上或注释不符时报 [`Error::Update`]。
pub(crate) async fn signature(
    engine: &Engine,
    proxy: &Proxy,
    public_key: &str,
    release: &Release,
    package: &Package,
) -> Result<(minisign_verify::PublicKey, minisign_verify::Signature), Error> {
    let key = minisign_verify::PublicKey::from_base64(public_key)
        .map_err(|error| Error::Update(format!("更新的公钥认不出：{error}")))?;
    let mut source = Source::new(package.signature.clone());
    source.http.proxy = proxy.clone();
    let fetched = engine
        .fetch(&source, SIGNATURE_LIMIT)
        .await
        .map_err(|error| Error::Update(format!("取不到 {} 的签名：{error}", package.name)))?;
    let text = String::from_utf8(fetched.body)
        .map_err(|_| Error::Update(format!("{} 的签名不是 UTF-8 的文字", package.name)))?;
    let signature = signed_for(&text, release.version, &package.name)?;
    Ok((key, signature))
}

/// 解出签名并核对可信注释为 `hs-dlx <version> <name>`。
fn signed_for(
    text: &str,
    version: Version,
    name: &str,
) -> Result<minisign_verify::Signature, Error> {
    let signature = minisign_verify::Signature::decode(text)
        .map_err(|error| Error::Update(format!("{name} 的签名认不出：{error}")))?;
    let expected = format!("hs-dlx {version} {name}");
    if signature.trusted_comment() != expected {
        return Err(Error::Update(format!(
            "{name} 的签名不是这个文件的（签的是「{}」）",
            signature.trusted_comment()
        )));
    }
    Ok(signature)
}

/// 验 `path` 的内容与签名 `signature` 相符（minisign 的 prehashed 签名）；不符或读不了时报 [`Error::Update`]。读整个文件，
/// 在阻塞线程上调用。
pub(crate) fn verify(
    path: &Path,
    key: &minisign_verify::PublicKey,
    signature: &minisign_verify::Signature,
) -> Result<(), Error> {
    use std::io::Read as _;
    let name = path.display();
    let mut verifier = key
        .verify_stream(signature)
        .map_err(|error| Error::Update(format!("{name} 的签名用不了：{error}")))?;
    let mut file = std::fs::File::open(path)
        .map_err(|error| Error::Update(format!("读不了 {name}：{error}")))?;
    let mut buffer = vec![0; READ_CHUNK];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| Error::Update(format!("读不了 {name}：{error}")))?;
        if read == 0 {
            break;
        }
        verifier.update(&buffer[..read]);
    }
    verifier
        .finalize()
        .map_err(|_| Error::Update(format!("{name} 与签名不符：不是发布的那个文件，没有安装")))
}

/// 下载的安装包放在哪个目录：系统的临时目录下，每次启动时清掉（见 [`sweep`]）。
pub(crate) fn download_dir() -> PathBuf {
    std::env::temp_dir().join("hs-dlx-update")
}

/// 装好之后重新打开应用：等本进程退出，再打开新版本（Windows 为运行安装程序，它装完再打开）。
pub struct Restart(Command);

impl Restart {
    /// 起等待的进程（独立于本进程，本进程退出后照样运行）；起不来时报 [`Error::Update`]。之后本进程正常退出即可。
    pub fn spawn(mut self) -> Result<(), Error> {
        self.0
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|error| Error::Update(format!("起不来重开应用的进程：{error}")))
    }
}

/// 装上验过签名的安装包 `package`，返回之后怎么重开。装不上时报 [`Error::Update`]，已装的版本不受影响。在阻塞线程上
/// 调用。
pub(crate) fn install(package: &Path) -> Result<Restart, Error> {
    platform::install(package)
}

/// 启动时清掉上次更新留下的：下载的安装包，与 macOS 上换下来的旧版本。删不掉时为说明。
pub fn sweep() -> Result<(), String> {
    let mut left = Vec::new();
    let mut remove = |path: PathBuf| match std::fs::remove_dir_all(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => left.push(format!("{}（{error}）", path.display())),
    };
    remove(download_dir());
    if let Some(old) = platform::replaced() {
        remove(old);
    }
    if left.is_empty() {
        Ok(())
    } else {
        Err(format!("上次更新留下的没删掉：{}", left.join("、")))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::Restart;
    use crate::Error;

    /// 新版本先拷到旧的旁边的这个名字，验过再换上。
    const STAGED: &str = ".hs-dlx-update.app";
    /// 换下来的旧版本，下次启动时删（[`super::sweep`]）。
    const REPLACED: &str = ".hs-dlx-old.app";
    /// 磁盘映像里的应用。
    const BUNDLE: &str = "hs-dlx.app";

    fn update(message: String) -> Error {
        Error::Update(message)
    }

    /// 正在运行的 .app；不是从 .app 运行的时为 None。
    fn bundle() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let bundle = exe.parent()?.parent()?.parent()?;
        (bundle.extension()? == "app").then(|| bundle.to_path_buf())
    }

    pub(super) fn replaced() -> Option<PathBuf> {
        bundle().map(|bundle| bundle.with_file_name(REPLACED))
    }

    /// 运行 `program`，失败时报出它的输出。
    fn run(program: &str, args: &[&std::ffi::OsStr], what: &str) -> Result<(), Error> {
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|error| update(format!("{what}：起不来 {program}：{error}")))?;
        if output.status.success() {
            return Ok(());
        }
        Err(update(format!(
            "{what}：{program} 失败（{}）：{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }

    /// 挂着的磁盘映像，释放时卸下。
    struct Mounted(PathBuf);

    impl Drop for Mounted {
        fn drop(&mut self) {
            // 卸不下只占着一个挂载点，系统重启即无；不影响更新的结果
            let _left = run(
                "hdiutil",
                &["detach".as_ref(), self.0.as_os_str(), "-force".as_ref()],
                "卸下磁盘映像",
            );
        }
    }

    pub(super) fn install(package: &Path) -> Result<Restart, Error> {
        let bundle =
            bundle().ok_or_else(|| update("不是从 .app 运行的，不能自动更新".to_owned()))?;
        if bundle
            .components()
            .any(|part| part.as_os_str() == "AppTranslocation")
        {
            return Err(update(
                "应用是被系统隔离运行的（直接从下载的磁盘映像或下载目录打开）：先把 hs-dlx 拖进「应用程序」，从那里打开再更新"
                    .to_owned(),
            ));
        }
        let mount = super::download_dir().join("mount");
        std::fs::create_dir_all(&mount)
            .map_err(|error| update(format!("建不了 {}：{error}", mount.display())))?;
        run(
            "hdiutil",
            &[
                "attach".as_ref(),
                "-nobrowse".as_ref(),
                "-readonly".as_ref(),
                "-noautoopen".as_ref(),
                "-mountpoint".as_ref(),
                mount.as_os_str(),
                package.as_os_str(),
            ],
            "挂载新版本的磁盘映像",
        )?;
        let mounted = Mounted(mount);
        let source = mounted.0.join(BUNDLE);
        if !source.is_dir() {
            return Err(update(format!("新版本的磁盘映像里没有 {BUNDLE}")));
        }
        let staged = bundle.with_file_name(STAGED);
        let replaced = bundle.with_file_name(REPLACED);
        for path in [&staged, &replaced] {
            match std::fs::remove_dir_all(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(update(format!("删不掉 {}：{error}", path.display())));
                }
            }
        }
        // ditto 保留符号链接、扩展属性与代码签名
        run(
            "ditto",
            &[source.as_os_str(), staged.as_os_str()],
            &format!(
                "把新版本拷到 {}（没有写入权限时先把应用移到自己能改的位置）",
                staged.display()
            ),
        )?;
        drop(mounted);
        std::fs::rename(&bundle, &replaced)
            .map_err(|error| update(format!("换不下旧版本 {}：{error}", bundle.display())))?;
        if let Err(error) = std::fs::rename(&staged, &bundle) {
            // 换回旧的；也换不回时两个都还在，写明在哪
            return Err(match std::fs::rename(&replaced, &bundle) {
                Ok(()) => update(format!("换不上新版本：{error}")),
                Err(back) => update(format!(
                    "换不上新版本（{error}），旧版本也没换回去（{back}）：旧版本在 {}，新版本在 {}",
                    replaced.display(),
                    staged.display()
                )),
            });
        }
        // 等本进程退出（caffeinate -w 用 kqueue 等，不轮询）再打开新版本
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("/usr/bin/caffeinate -w \"$1\"; /usr/bin/open \"$2\"")
            .arg("hs-dlx-restart")
            .arg(std::process::id().to_string())
            .arg(&bundle);
        Ok(Restart(command))
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::os::windows::process::CommandExt as _;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::Restart;
    use crate::Error;

    /// 不弹控制台窗口、不随本进程的控制台结束（CREATE_NO_WINDOW | DETACHED_PROCESS）。
    const DETACHED: u32 = 0x0800_0000 | 0x0000_0008;

    pub(super) fn replaced() -> Option<PathBuf> {
        None
    }

    /// PowerShell 单引号字符串的写法：里面的单引号写两遍。
    fn quoted(path: &Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', "''"))
    }

    pub(super) fn install(package: &Path) -> Result<Restart, Error> {
        // 等本进程退出再装：安装程序见到应用还在运行会直接结束它，等于强退。/S 静默、/R 装完打开、/NS 不重建快捷方式
        let script = format!(
            "Wait-Process -Id {} -ErrorAction SilentlyContinue; Start-Process -FilePath {} -ArgumentList '/S','/R','/NS'",
            std::process::id(),
            quoted(package)
        );
        let mut command = Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
            ])
            .arg(script)
            .creation_flags(DETACHED);
        Ok(Restart(command))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use std::path::{Path, PathBuf};

    use super::Restart;
    use crate::Error;

    pub(super) fn replaced() -> Option<PathBuf> {
        None
    }

    pub(super) fn install(_package: &Path) -> Result<Restart, Error> {
        Err(Error::Update(
            "这个平台不能自动更新，到下载页下载新版本".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_three_numbers() {
        assert_eq!(
            Version::parse("0.10.2"),
            Some(Version {
                major: 0,
                minor: 10,
                patch: 2
            })
        );
        for bad in ["0.3", "0.3.0.1", "0.3.0-beta", "v0.3.0", "0..1", "0.3.+1"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert!(Version::parse("0.10.0") > Version::parse("0.9.9"));
    }

    fn github(tag: &str, assets: &[&str]) -> Vec<u8> {
        let assets: Vec<serde_json::Value> = assets
            .iter()
            .map(|name| {
                serde_json::json!({
                    "name": name,
                    "size": 42,
                    "browser_download_url": format!("https://example.test/{name}"),
                })
            })
            .collect();
        serde_json::json!({
            "tag_name": tag,
            "html_url": "https://example.test/release",
            "body": "notes",
            "draft": false,
            "prerelease": false,
            "assets": assets,
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn releases_pick_this_platforms_package() {
        let current = Version::parse("0.2.0").unwrap();
        let body = github(
            "v0.3.0",
            &[
                "hs-dlx_0.3.0_aarch64.dmg",
                "hs-dlx_0.3.0_aarch64.dmg.minisig",
                "hs-dlx_0.3.0_x64-setup.exe",
            ],
        );
        let release = release_of(&body, current, Some("_aarch64.dmg"))
            .unwrap()
            .unwrap();
        assert_eq!(release.version.to_string(), "0.3.0");
        let package = release.package.unwrap();
        assert_eq!(package.name, "hs-dlx_0.3.0_aarch64.dmg");
        assert_eq!(
            package.signature.as_str(),
            "https://example.test/hs-dlx_0.3.0_aarch64.dmg.minisig"
        );
        // 没有签名的安装包不用
        let missing = release_of(&body, current, Some("_x64-setup.exe")).unwrap_err();
        assert!(
            missing
                .to_string()
                .contains("hs-dlx_0.3.0_x64-setup.exe.minisig")
        );
        // 不能自动装的平台只给网页
        let page = release_of(&body, current, None).unwrap().unwrap();
        assert_eq!(page.package, None);
        // 不比现在的新
        let same = Version::parse("0.3.0").unwrap();
        assert_eq!(release_of(&body, same, Some("_aarch64.dmg")).unwrap(), None);
        assert!(release_of(&github("0.3.0", &[]), current, None).is_err());
        assert!(release_of(b"<html>", current, None).is_err());
    }

    /// 测试用的密钥对（与发布用的无关）签的 `b"hs-dlx update test\n"`，可信注释为 `hs-dlx 99.0.0 <文件名>`。
    const TEST_PUBLIC_KEY: &str = include_str!("../tests/fixtures/update/test.pub");
    const TEST_SIGNATURE: &str =
        include_str!("../tests/fixtures/update/hs-dlx_99.0.0_aarch64.dmg.minisig");
    const TEST_PACKAGE: &[u8] = include_bytes!("../tests/fixtures/update/package");

    #[test]
    fn signatures_are_checked_against_version_name_and_content() {
        let key = minisign_verify::PublicKey::decode(TEST_PUBLIC_KEY).unwrap();
        let version = Version::parse("99.0.0").unwrap();
        let name = "hs-dlx_99.0.0_aarch64.dmg";
        let signature = signed_for(TEST_SIGNATURE, version, name).unwrap();
        // 别的版本、别的文件不认
        let older = Version::parse("98.0.0").unwrap();
        assert!(signed_for(TEST_SIGNATURE, older, name).is_err());
        assert!(signed_for(TEST_SIGNATURE, version, "hs-dlx_99.0.0_x64-setup.exe").is_err());

        let dir = std::env::temp_dir().join(format!("hs-dlx-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, TEST_PACKAGE).unwrap();
        verify(&path, &key, &signature).unwrap();
        let mut tampered = TEST_PACKAGE.to_vec();
        tampered[0] ^= 1;
        std::fs::write(&path, tampered).unwrap();
        assert!(
            verify(&path, &key, &signature)
                .unwrap_err()
                .to_string()
                .contains("与签名不符")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
