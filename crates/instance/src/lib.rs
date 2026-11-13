//! 单实例：同一份任务存储同时只由一个进程打开（存储旁的锁文件，见 hs-m3u8-app 的 `Store::open`），打开它的是主实例。
//! 主实例在本机套接字（Unix 为域套接字，Windows 为命名管道）上收后打开的实例转交的链接；后打开的实例连上去交出它的
//! 链接（没有时只请主实例到前台），收到确认后退出。
//!
//! 只有本用户能交：Unix 上套接字放在运行时目录（`XDG_RUNTIME_DIR`，没有时为临时目录）下只有本用户能进的
//! `hs-m3u8` 目录里，双方都核对对端的用户就是任务存储文件的属主；Windows 上管道只许属主与 SYSTEM 访问。
//!
//! 协议：4 字节大端长度 + UTF-8 链接（长度 0 为只到前台），链接最长 64 KiB；主实例交给界面后回 1 个字节 1。一次转交
//! 的读写合计限 2 秒。

use std::path::{Path, PathBuf};
use std::time::Duration;

use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::{GenericFilePath, ListenerOptions, Name};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::mpsc;

/// 链接最长多少字节：带签名参数的地址可达数 KB，再长的不是链接。
const MAX_LINK: u32 = 64 * 1024;
/// 一次转交（连上之后的读写）最多多久：对端不说话时不让主实例一直等。
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(2);
/// 主实例收下时回的字节。
const ACCEPTED: u8 = 1;

/// 后打开的实例交来的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handoff {
    /// 只请主实例到前台（启动时没有给链接）
    Activate,
    /// 探测这个链接（启动参数原样，未校验）
    Link(String),
}

/// 主实例收到的：交来的，或一次没收成的转交的原因（对端不是本用户、超时、内容太长或不是 UTF-8）。
pub type Received = Result<Handoff, String>;

/// 主实例收转交的接收端，在任何执行器上都可等待。
pub type Handoffs = mpsc::UnboundedReceiver<Received>;

/// 主实例开始收转交：`store` 为它已打开（持锁）的任务存储文件。在名为 hs-m3u8-instance 的后台线程上收，交给返回的
/// 接收端，一次只收一个；接收端释放后，线程在下一次有连接时退出，否则随进程结束。建不起监听时为原因：之后打开的
/// 实例交不过来，各自报错。
pub fn listen(store: &Path) -> Result<Handoffs, String> {
    let endpoint = Endpoint::of(store)?;
    let (ready, started) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
    let (sender, receiver) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("hs-m3u8-instance".to_owned())
        .spawn(move || {
            // 下面的发送都发生在 listen 返回之前（它在等这一条），接收端一定还在
            let runtime = match runtime() {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _waiting = ready.send(Err(error));
                    return;
                }
            };
            runtime.block_on(async move {
                let listener = match endpoint.listen() {
                    Ok(listener) => {
                        let _waiting = ready.send(Ok(()));
                        listener
                    }
                    Err(error) => {
                        let _waiting = ready.send(Err(error));
                        return;
                    }
                };
                loop {
                    let received = match listener.accept().await {
                        Ok(stream) => endpoint.serve(stream, &sender).await,
                        Err(error) => Err(format!("收转交时出错：{error}")),
                    };
                    // 交给界面的那一刻接收端已释放：没有谁再要
                    if let Err(error) = received
                        && sender.send(Err(error)).is_err()
                    {
                        break;
                    }
                    if sender.is_closed() {
                        break;
                    }
                }
            });
        })
        .map_err(|error| format!("起不了收转交的线程：{error}"))?;
    started
        .recv()
        .map_err(|_| "收转交的线程没报就退出了".to_owned())??;
    Ok(receiver)
}

/// 后打开的实例把 `link` 交给主实例（None 时只请它到前台）：`store` 为主实例打开着的任务存储文件。主实例回了确认才
/// 算交过去；连不上（主实例刚退出或还没开始收）、对端不是本用户、超时或主实例拒收时为原因。
pub fn hand_over(store: &Path, link: Option<&str>) -> Result<(), String> {
    let endpoint = Endpoint::of(store)?;
    let message = link.unwrap_or_default().as_bytes();
    let length = u32::try_from(message.len())
        .ok()
        .filter(|&length| length <= MAX_LINK)
        .ok_or_else(|| format!("链接超过 {} KiB，不是链接", MAX_LINK / 1024))?;
    runtime()?.block_on(async {
        let stream = Stream::connect(endpoint.name()?)
            .await
            .map_err(|error| format!("连不上已经打开的那个：{error}"))?;
        endpoint.check_peer(&stream)?;
        let exchange = async {
            let mut stream = &stream;
            stream.write_u32(length).await?;
            stream.write_all(message).await?;
            stream.flush().await?;
            stream.read_u8().await
        };
        match tokio::time::timeout(EXCHANGE_TIMEOUT, exchange).await {
            Ok(Ok(ACCEPTED)) => Ok(()),
            Ok(Ok(other)) => Err(format!("已经打开的那个回了认不出的确认 {other}")),
            Ok(Err(error)) => Err(format!("交给已经打开的那个时出错：{error}")),
            Err(_) => Err(format!(
                "已经打开的那个 {} 秒没有回应",
                EXCHANGE_TIMEOUT.as_secs()
            )),
        }
    })
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("起不了转交用的运行时：{error}"))
}

/// 主实例收转交的地址，由任务存储文件的路径定：同一份存储的各个实例算出同一个，用别的存储（如测试时换了主目录）的
/// 互不相干。
struct Endpoint {
    /// Unix 上为套接字文件，Windows 上为 `\\.\pipe\` 下的管道名
    path: PathBuf,
    /// 任务存储文件的属主：双方都只认这个用户
    #[cfg(unix)]
    owner: u32,
}

impl Endpoint {
    fn of(store: &Path) -> Result<Endpoint, String> {
        let key = fnv1a(store.as_os_str().as_encoded_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let owner = std::fs::metadata(store)
                .map_err(|error| format!("读不出任务存储 {}：{error}", store.display()))?
                .uid();
            let dir = private_dir(owner)?;
            Ok(Endpoint {
                path: dir.join(format!("{key:016x}.sock")),
                owner,
            })
        }
        #[cfg(windows)]
        Ok(Endpoint {
            path: PathBuf::from(format!(r"\\.\pipe\hs-m3u8-{key:016x}")),
        })
    }

    fn name(&self) -> Result<Name<'_>, String> {
        self.path
            .as_path()
            .to_fs_name::<GenericFilePath>()
            .map_err(|error| format!("收转交的地址 {} 不能用：{error}", self.path.display()))
    }

    /// 建监听。Unix 上覆盖上次异常退出留下的套接字文件：调用方持着存储锁，不会有别的主实例在用它。
    fn listen(&self) -> Result<Listener, String> {
        let options = ListenerOptions::new()
            .name(self.name()?)
            .try_overwrite(true);
        #[cfg(windows)]
        let options = {
            use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
            use interprocess::os::windows::security_descriptor::SecurityDescriptor;
            // 只许属主（OW）与 SYSTEM（SY）访问，不继承
            let descriptor = SecurityDescriptor::deserialize(widestring::u16cstr!(
                "D:P(A;;GA;;;OW)(A;;GA;;;SY)"
            ))
            .map_err(|error| format!("写不出管道的访问权限：{error}"))?;
            options.security_descriptor(descriptor)
        };
        options
            .create_tokio()
            .map_err(|error| format!("建不起收转交的 {}：{error}", self.path.display()))
    }

    /// 对端是不是存储的属主；Windows 上由管道的访问权限保证，这里不再查。
    fn check_peer(&self, stream: &Stream) -> Result<(), String> {
        #[cfg(unix)]
        {
            let peer = stream
                .peer_creds()
                .map_err(|error| format!("读不出对端是谁：{error}"))?
                .euid();
            if peer != Some(self.owner) {
                return Err(format!(
                    "对端（用户 {peer:?}）不是任务存储的属主（用户 {}）",
                    self.owner
                ));
            }
        }
        #[cfg(windows)]
        let _ = stream;
        Ok(())
    }

    /// 收一次转交：核对对端、读出链接、交给界面、回确认。交给界面之前出的错为 Err；交给界面之后回确认没成的也报出来
    /// （界面已收下，对端会以为没交过去）。
    async fn serve(
        &self,
        stream: Stream,
        sender: &mpsc::UnboundedSender<Received>,
    ) -> Result<(), String> {
        self.check_peer(&stream)?;
        let read = async {
            let mut stream = &stream;
            let length = stream.read_u32().await?;
            if length > MAX_LINK {
                return Ok(Err(format!("交来的链接有 {length} 字节，超过上限")));
            }
            let mut buffer = vec![0; length as usize];
            stream.read_exact(&mut buffer).await?;
            Ok::<_, std::io::Error>(
                String::from_utf8(buffer).map_err(|_| "交来的链接不是 UTF-8".to_owned()),
            )
        };
        let text = match tokio::time::timeout(EXCHANGE_TIMEOUT, read).await {
            Ok(Ok(text)) => text?,
            Ok(Err(error)) => return Err(format!("读交来的链接时出错：{error}")),
            Err(_) => {
                return Err(format!(
                    "交链接的一方 {} 秒没交完",
                    EXCHANGE_TIMEOUT.as_secs()
                ));
            }
        };
        let handoff = if text.is_empty() {
            Handoff::Activate
        } else {
            Handoff::Link(text)
        };
        if sender.send(Ok(handoff)).is_err() {
            // 界面已经关了：不回确认，对端照常报没交过去
            return Ok(());
        }
        let confirm = async {
            let mut stream = &stream;
            stream.write_u8(ACCEPTED).await?;
            stream.flush().await
        };
        match tokio::time::timeout(EXCHANGE_TIMEOUT, confirm).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(format!("收下了交来的链接，但没能回确认：{error}")),
            Err(_) => Err("收下了交来的链接，但回确认超时".to_owned()),
        }
    }
}

/// 放套接字的目录：运行时目录（没有时为临时目录）下的 `hs-m3u8`，没有时建为 0700。已有的要属于 `owner`、别人进不去，
/// 否则为原因：别人建的目录里的套接字可能被换掉。
#[cfg(unix)]
fn private_dir(owner: u32) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    let dir = dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("hs-m3u8");
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("建不了 {}：{error}", dir.display())),
    }
    let metadata = std::fs::symlink_metadata(&dir)
        .map_err(|error| format!("读不出 {}：{error}", dir.display()))?;
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
        return Err(format!(
            "{} 不是只有本用户能进的目录，不在里面收转交",
            dir.display()
        ));
    }
    Ok(dir)
}

/// 64 位 FNV-1a：各版本的应用对同一份存储要算出同一个地址，不能用标准库里不保证跨版本稳定的哈希。
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_key_is_stable_across_versions() {
        // FNV-1a 64 的公开测试值：改了算法，新版本就找不到旧版本开着的主实例
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
