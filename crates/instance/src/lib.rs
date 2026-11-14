//! 单实例：同一份任务存储同时只由一个进程打开（存储旁的锁文件，见 hs-dlx-app 的 `Store::open`），打开它的是主实例。
//! 主实例在本机套接字（Unix 为域套接字，Windows 为命名管道）上收再次打开应用时转交的链接，并把这个地址记在存储旁的
//! `<存储文件名>.instance` 里；再次打开的实例照这个文件连上去交出它的链接（没有时只请主实例到前台），收到确认后退出。
//!
//! 只有本用户能交：
//! - Unix：套接字放在运行时目录（`XDG_RUNTIME_DIR`，没有时为临时目录）下只有本用户能进的 `hs-dlx` 目录里，双方都
//!   核对对端的用户就是任务存储文件的属主。
//! - Windows：管道只许属主与 SYSTEM 访问；管道名带 128 位随机数、只记在本用户的数据目录里，别的用户猜不到，抢不先建。
//!
//! 协议：4 字节大端长度 + UTF-8 链接（长度 0 为只到前台），链接最长 64 KiB；主实例回 1 个字节：1 为界面接手了，0 为
//! 正在退出、不接手；别的情况（界面 2 秒没接手、读不完）不回，关掉连接。连接、读写与等界面接手各限 2 秒。

use std::path::{Path, PathBuf};
use std::time::Duration;

use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::{GenericFilePath, ListenerOptions, Name};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot};

/// 链接最长多少字节：带签名参数的地址可达数 KB，再长的不是链接。
const MAX_LINK: u32 = 64 * 1024;
/// 连上主实例、一次转交的读写，各最多多久：对端不说话时不让双方一直等。
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(2);
/// 主实例等界面接手最多多久：界面线程卡住（如退出时等任务停下并补写存储）时让交出的一方及时报没交过去。
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(2);
/// 收连接出错后隔多久再收：出错多为进程的文件描述符用尽，立即重试只会空转。
const ACCEPT_BACKOFF: Duration = Duration::from_secs(1);
/// 主实例回的字节：界面接手了。
const ACCEPTED: u8 = 1;
/// 主实例回的字节：正在退出，不接手。
const LEAVING: u8 = 0;

/// 再次打开应用时交来的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handoff {
    /// 只请主实例到前台（启动时没有给链接）
    Activate,
    /// 探测这个链接（启动参数原样，未校验，不为空）
    Link(String),
}

/// 界面对一次转交的答复。
#[derive(Debug)]
enum Answer {
    Accepted,
    Leaving,
}

/// 交来的一次转交。界面接手（[`Delivery::accept`]）后才给交出的一方回确认；正在退出的回不接手
/// （[`Delivery::refuse_leaving`]）。两者都由交出的一方把结果告诉用户。
#[derive(Debug)]
pub struct Delivery {
    handoff: Handoff,
    answer: oneshot::Sender<Answer>,
}

impl Delivery {
    /// 接手：给交出的一方回确认，返回交来的。交来之后 2 秒还没接手的，交出的一方已报没交过去、把链接告诉了用户，
    /// 这时为 None，界面不该再处理它。
    pub fn accept(self) -> Option<Handoff> {
        self.answer
            .send(Answer::Accepted)
            .is_ok()
            .then_some(self.handoff)
    }

    /// 正在退出、不接手：交出的一方报主实例正在退出（[`HandOverError::Leaving`]）。
    pub fn refuse_leaving(self) {
        // 收转交的一方已不等时，交出的一方照常报没交过去
        let _gone = self.answer.send(Answer::Leaving);
    }
}

/// 把链接交给主实例没成。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandOverError {
    /// 主实例正在退出，不再接手：等它退出后再打开即可
    #[error("已经打开的那个正在退出")]
    Leaving,
    /// 别的原因（链接为空或太长、读不出主实例的地址、连不上、对端不是本用户、超时、界面没接手）
    #[error("{0}")]
    Failed(String),
}

impl From<String> for HandOverError {
    fn from(reason: String) -> HandOverError {
        HandOverError::Failed(reason)
    }
}

/// 主实例收到的：交来的，或一次没收成的转交的原因（对端不是本用户、超时、内容太长或不是 UTF-8、收连接出错）。
pub type Received = Result<Delivery, String>;

/// 主实例收转交的接收端，在任何执行器上都可等待。
pub type Handoffs = mpsc::UnboundedReceiver<Received>;

/// 主实例开始收转交：`store` 为它已打开（持锁）的任务存储文件。建好监听、记下地址后返回；在名为 hs-dlx-instance 的
/// 后台线程上收，交给返回的接收端，一次只收一个。接收端释放后，线程在下一次有连接时退出，否则随进程结束。建不起
/// 监听或记不下地址时为原因：再次打开的实例交不过来，各自报错。
pub fn listen(store: &Path) -> Result<Handoffs, String> {
    let endpoint = Endpoint::new(store)?;
    let (ready, started) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
    let (sender, receiver) = mpsc::unbounded_channel();
    let record = record_path(store);
    std::thread::Builder::new()
        .name("hs-dlx-instance".to_owned())
        .spawn(move || {
            // 下面给 ready 的发送都发生在 listen 返回之前（它在等这一条），接收端一定还在
            let runtime = match runtime() {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _waiting = ready.send(Err(error));
                    return;
                }
            };
            runtime.block_on(async move {
                let listener = match endpoint.listen().and_then(|listener| {
                    endpoint.record(&record)?;
                    Ok(listener)
                }) {
                    Ok(listener) => {
                        let _waiting = ready.send(Ok(()));
                        listener
                    }
                    Err(error) => {
                        let _waiting = ready.send(Err(error));
                        return;
                    }
                };
                endpoint.accept_all(&listener, &sender).await;
            });
        })
        .map_err(|error| format!("起不了收转交的线程：{error}"))?;
    started
        .recv()
        .map_err(|_| "收转交的线程没报就退出了".to_owned())??;
    Ok(receiver)
}

/// 再次打开的实例把 `link` 交给主实例（None 时只请它到前台）：`store` 为主实例打开着的任务存储文件。主实例的界面接手
/// 后回了确认才算交过去。
pub fn hand_over(store: &Path, link: Option<&str>) -> Result<(), HandOverError> {
    let message = match link {
        Some("") => return Err("链接是空的".to_owned().into()),
        Some(link) => link.as_bytes(),
        None => &[],
    };
    let length = u32::try_from(message.len())
        .ok()
        .filter(|&length| length <= MAX_LINK)
        .ok_or_else(|| format!("链接超过 {} KiB，不是链接", MAX_LINK / 1024))?;
    let endpoint = Endpoint::recorded(store)?;
    runtime()?.block_on(async {
        let stream =
            match tokio::time::timeout(EXCHANGE_TIMEOUT, Stream::connect(endpoint.name()?)).await {
                Ok(Ok(stream)) => stream,
                Ok(Err(error)) => return Err(format!("连不上已经打开的那个：{error}").into()),
                Err(_) => {
                    return Err(
                        format!("{} 秒没能连上已经打开的那个", EXCHANGE_TIMEOUT.as_secs()).into(),
                    );
                }
            };
        endpoint.check_peer(&stream)?;
        let exchange = async {
            let mut stream = &stream;
            stream.write_u32(length).await?;
            stream.write_all(message).await?;
            stream.flush().await?;
            stream.read_u8().await
        };
        // 对端读、等界面接手、回确认都有各自的时限，这里等它们加起来
        let waited = EXCHANGE_TIMEOUT * 2 + ACCEPT_TIMEOUT;
        let failed = match tokio::time::timeout(waited, exchange).await {
            Ok(Ok(ACCEPTED)) => return Ok(()),
            Ok(Ok(LEAVING)) => return Err(HandOverError::Leaving),
            Ok(Ok(other)) => format!("已经打开的那个回了认不出的确认 {other}"),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                "已经打开的那个没有接手".to_owned()
            }
            Ok(Err(error)) => format!("交给已经打开的那个时出错：{error}"),
            Err(_) => format!("已经打开的那个 {} 秒没有回应", waited.as_secs()),
        };
        Err(failed.into())
    })
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("起不了转交用的运行时：{error}"))
}

/// 记主实例地址的文件：存储文件旁的 `<存储文件名>.instance`。
#[doc(hidden)]
pub fn record_path(store: &Path) -> PathBuf {
    let mut path = store.as_os_str().to_owned();
    path.push(".instance");
    PathBuf::from(path)
}

/// 主实例收转交的地址。
struct Endpoint {
    /// Unix 上为套接字文件，Windows 上为 `\\.\pipe\` 下的管道名
    path: PathBuf,
    /// 任务存储文件的属主：双方都只认这个用户
    #[cfg(unix)]
    owner: u32,
}

impl Endpoint {
    /// 主实例给存储 `store` 取的地址，名里有存储路径的哈希（各版本一致）。Unix 上只用它（异常退出留下的套接字下次照
    /// 名覆盖）；Windows 上另加 128 位随机数。
    fn new(store: &Path) -> Result<Endpoint, String> {
        let key = fnv1a(store.as_os_str().as_encoded_bytes());
        #[cfg(unix)]
        {
            let owner = owner_of(store)?;
            Ok(Endpoint {
                path: private_dir(owner)?.join(format!("{key:016x}.sock")),
                owner,
            })
        }
        #[cfg(windows)]
        {
            let mut random = [0_u8; 16];
            getrandom::fill(&mut random).map_err(|error| format!("取不到随机数：{error}"))?;
            let name: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
            Ok(Endpoint {
                path: PathBuf::from(format!(r"\\.\pipe\hs-dlx-{key:016x}-{name}")),
            })
        }
    }

    /// 存储 `store` 旁记着的主实例地址；没有记录（主实例还没开始收，或收不了）时为原因。
    fn recorded(store: &Path) -> Result<Endpoint, String> {
        let record = record_path(store);
        let text = std::fs::read_to_string(&record).map_err(|error| {
            format!(
                "读不出已经打开的那个记下的地址 {}：{error}",
                record.display()
            )
        })?;
        Ok(Endpoint {
            path: PathBuf::from(text.trim_end()),
            #[cfg(unix)]
            owner: owner_of(store)?,
        })
    }

    /// 把地址记进 `record`：先写到旁边的临时文件再改名，读的一方不会读到写了一半的。Unix 上只有本人可读写。
    fn record(&self, record: &Path) -> Result<(), String> {
        let mut temporary = record.as_os_str().to_owned();
        temporary.push(".new");
        let temporary = PathBuf::from(temporary);
        let text = self
            .path
            .to_str()
            .ok_or_else(|| format!("收转交的地址 {} 不是 UTF-8", self.path.display()))?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let written = options.open(&temporary).and_then(|mut file| {
            std::io::Write::write_all(&mut file, text.as_bytes())?;
            file.sync_all()
        });
        written
            .and_then(|()| std::fs::rename(&temporary, record))
            .map_err(|error| format!("记不下收转交的地址 {}：{error}", record.display()))
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

    /// 一个接一个地收转交，交给 `sender`，直到它的接收端释放。收连接出错时报给界面，隔 [`ACCEPT_BACKOFF`] 再收；同一
    /// 原因接连出现只报一次。
    async fn accept_all(&self, listener: &Listener, sender: &mpsc::UnboundedSender<Received>) {
        let mut last_failure: Option<String> = None;
        loop {
            let failure = match listener.accept().await {
                Ok(stream) => {
                    last_failure = None;
                    self.serve(stream, sender).await.err()
                }
                Err(error) => {
                    let reason = format!("收转交时出错：{error}");
                    let repeated = last_failure.as_ref() == Some(&reason);
                    last_failure = Some(reason.clone());
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                    (!repeated).then_some(reason)
                }
            };
            // 接收端已释放：界面关了，没有谁再要
            let gone = match failure {
                Some(reason) => sender.send(Err(reason)).is_err(),
                None => sender.is_closed(),
            };
            if gone {
                break;
            }
        }
    }

    /// 对端是不是存储的属主；Windows 上由管道的访问权限与猜不到的管道名保证，这里不再查。
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

    /// 收一次转交：核对对端、读出链接、交给界面，照界面的答复回字节。交给界面之前出的错为 Err；回答复没成的也报出来
    /// （界面已接手时，对端会以为没交过去）。界面 2 秒没答复时不回，对端照常报没交过去。
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
        let (answer, answered) = oneshot::channel();
        if sender.send(Ok(Delivery { handoff, answer })).is_err() {
            // 界面已经关了：不回确认，对端照常报没交过去
            return Ok(());
        }
        // 超时即释放接收端：之后才接手的界面从 accept 得知交出的一方已不等
        let reply = match tokio::time::timeout(ACCEPT_TIMEOUT, answered).await {
            Ok(Ok(Answer::Accepted)) => ACCEPTED,
            Ok(Ok(Answer::Leaving)) => LEAVING,
            Ok(Err(_)) | Err(_) => return Ok(()),
        };
        let confirm = async {
            let mut stream = &stream;
            stream.write_u8(reply).await?;
            stream.flush().await
        };
        match tokio::time::timeout(EXCHANGE_TIMEOUT, confirm).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(format!("没能给交链接的一方回答复：{error}")),
            Err(_) => Err("给交链接的一方回答复超时".to_owned()),
        }
    }
}

/// 任务存储文件的属主。
#[cfg(unix)]
fn owner_of(store: &Path) -> Result<u32, String> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(store)
        .map(|metadata| metadata.uid())
        .map_err(|error| format!("读不出任务存储 {}：{error}", store.display()))
}

/// 放套接字的目录：运行时目录（没有时为临时目录）下的 `hs-dlx`，没有时建为 0700。已有的要属于 `owner`、别人进不去，
/// 否则为原因：别人建的目录里的套接字可能被换掉。
#[cfg(unix)]
fn private_dir(owner: u32) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    let dir = dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("hs-dlx");
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

/// 64 位 FNV-1a：各版本的应用对同一份存储要算出同一个套接字名，不能用标准库里不保证跨版本稳定的哈希。
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
        // FNV-1a 64 的公开测试值：改了算法，新版本就连不上旧版本留下的套接字名（异常退出后覆盖不到）
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
