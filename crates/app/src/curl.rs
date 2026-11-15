//! 浏览器开发者工具「复制为 cURL」得到的命令：拆成要下载的地址与请求头。
//!
//! 认两种写法，按 Chrome、Firefox、Safari 开发者工具生成命令的规则：
//! - bash（macOS、Linux 上复制的，与 Chrome 的「复制为 cURL (bash)」）：单引号、双引号、`$'…'`（含 `\xHH`、`\uHHHH`、
//!   八进制）、反斜杠转义与续行；变量、命令替换、重定向不展开，报错。
//! - Windows cmd（「复制为 cURL (cmd)」）：命令里有 `^"` 或以 `^` 结尾的行时按它拆。先按 cmd.exe 处理 `^` 转义、`^` 续行
//!   与命令分隔，再按 MS C 运行库拆参数（`""` 与 `\"` 为引号本身）。`%名称%` 不展开。
//!
//! 只认不带请求体的 GET 请求。命令里的 `-H`、`-b 名=值`、`-A`、`-e`、`-u` 转成请求头，同 curl：`-H` 给了同名的
//! （含 Cookie、Authorization）就不用后几种转成的，多个 `-b` 合成一条 Cookie。由连接或下载器决定的请求头去掉并列出
//! （[`Request::dropped`]）。

use base64::Engine as _;
use hs_m3u8_core::{Url, check_option_header};

/// 从命令里拆出的请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub url: Url,
    /// 交给核心的请求头，已按 [`check_option_header`] 校验，名字不分大小写各不相同
    pub headers: Vec<(String, String)>,
    /// 命令里有、没带上的请求头名（按命令里的写法，不分大小写去重）：Host、Content-Length、Connection 等由连接决定，
    /// Accept-Encoding、Range、If-* 由下载器决定
    pub dropped: Vec<String>,
}

impl Request {
    /// 这些请求头能否用于地址为 `target` 的任务：带 Cookie 或 Authorization 而命令的地址与 `target` 不同源时不能——
    /// 核心把它们只发往任务的来源，换了来源即把凭据交给了命令以外的服务器。
    pub fn check_target(&self, target: &Url) -> Result<(), CurlError> {
        let credentials = self.headers.iter().any(|(name, _)| {
            name.eq_ignore_ascii_case("cookie") || name.eq_ignore_ascii_case("authorization")
        });
        if credentials && self.url.origin() != target.origin() {
            return Err(CurlError::CrossOrigin {
                command: self.url.origin().ascii_serialization(),
                target: target.origin().ascii_serialization(),
            });
        }
        Ok(())
    }
}

/// 命令认不出或含不支持的请求。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CurlError {
    #[error("不是 curl 命令")]
    NotCurl,
    #[error("引号没有闭合，命令可能没复制全")]
    Unclosed,
    /// 值为 shell 的哪种写法
    #[error("命令里有 {0}，这里不执行 shell，只认 curl 的参数")]
    Shell(&'static str),
    #[error("粘贴了多条命令，一次只认一条 curl 命令")]
    MultipleCommands,
    #[error("命令里没有地址")]
    NoUrl,
    #[error("命令里有多个地址，一次只能下载一个")]
    MultipleUrls,
    #[error("地址要是 http 或 https 链接：{0}")]
    BadUrl(String),
    /// 值为选项（照命令里的写法）
    #[error("{0} 后面缺少值")]
    MissingValue(String),
    /// 值为请求方法
    #[error("只支持 GET 请求，命令的请求方法是 {0}")]
    Method(String),
    /// 值为选项
    #[error("{0} 要发送请求体，只支持不带请求体的 GET 请求")]
    Data(String),
    /// 值为 `-b` 后面的内容
    #[error("-b 后面是 Cookie 文件（{0}），要直接写成 名=值")]
    CookieFile(String),
    /// 值为选项
    #[error("{0} 的内容要从文件读取，要把内容直接写进命令")]
    FromFile(String),
    #[error("-u 只写了用户名，要写成 用户名:口令")]
    NoPassword,
    /// 值为选项
    #[error("不认识的参数 {0}，删掉后再粘贴")]
    UnknownOption(String),
    /// 值为原因
    #[error("{0}")]
    Header(String),
    /// 值为请求头名：curl 会两条都发，核心与请求头编辑都是一个名字一条
    #[error("命令里有两条 {0}，只留要用的一条后再粘贴")]
    DuplicateHeader(String),
    #[error("命令里有不是 UTF-8 的字节")]
    NotUtf8,
    /// 带凭据的命令的来源与任务的不同（见 [`Request::check_target`]）
    #[error(
        "命令的地址（{command}）与任务的（{target}）不同源，其中的 Cookie 或 Authorization 不能用于这个任务"
    )]
    CrossOrigin { command: String, target: String },
}

/// 不带上的请求头（小写）：前一组由连接决定，后一组由下载器决定（核心不接受，见 [`check_option_header`]）。
const DROPPED: [&str; 17] = [
    "host",
    "content-length",
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "te",
    "upgrade",
    "trailer",
    "expect",
    "accept-encoding",
    "range",
    "if-none-match",
    "if-modified-since",
    "if-match",
    "if-unmodified-since",
    "if-range",
];

/// `text` 的第一个词是 `curl` 或 `curl.exe`（不分大小写）：看作 curl 命令，交给 [`parse`]。
pub fn is_command(text: &str) -> bool {
    let first = text.split_whitespace().next().unwrap_or_default();
    first.eq_ignore_ascii_case("curl") || first.eq_ignore_ascii_case("curl.exe")
}

/// 把 curl 命令拆成请求。
pub fn parse(text: &str) -> Result<Request, CurlError> {
    let text = text.trim().replace("\r\n", "\n");
    let words = if is_cmd(&text) {
        split_crt(&cmd_line(&text)?)?
    } else {
        split_bash(&text)?
    };
    let mut words = words.into_iter();
    if !words.next().is_some_and(|first| is_command(&first)) {
        return Err(CurlError::NotCurl);
    }
    Options::read(words)?.into_request()
}

/// 按 Windows cmd 的写法拆：Chrome 与 Firefox 生成的 cmd 命令值都包在 `^"…^"` 里、续行为行末的 ` ^`。
fn is_cmd(text: &str) -> bool {
    text.contains("^\"") || text.lines().any(|line| line.trim_end().ends_with('^'))
}

/// 按 bash 的规则把命令拆成词。
fn split_bash(text: &str) -> Result<Vec<String>, CurlError> {
    let mut words = Vec::new();
    // 正在拼的词；None 时在词与词之间
    let mut word: Option<Vec<u8>> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => finish(&mut words, &mut word)?,
            '\n' => {
                end_of_command(&mut chars)?;
            }
            '\\' => match chars.next() {
                // 续行
                Some('\n') => {}
                Some(c) => push(&mut word, c),
                None => push(&mut word, '\\'),
            },
            '\'' => {
                let word = word.get_or_insert_default();
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => push_to(word, c),
                        None => return Err(CurlError::Unclosed),
                    }
                }
            }
            '"' => double_quoted(&mut chars, word.get_or_insert_default())?,
            '$' => match chars.peek() {
                Some('\'') => {
                    chars.next();
                    ansi_c(&mut chars, word.get_or_insert_default())?;
                }
                // `$"…"` 按语言环境翻译，没有翻译时同 `"…"`
                Some('"') => {
                    chars.next();
                    double_quoted(&mut chars, word.get_or_insert_default())?;
                }
                Some(&next) if expands(next) => return Err(CurlError::Shell("变量或命令替换")),
                _ => push(&mut word, '$'),
            },
            '`' => return Err(CurlError::Shell("命令替换")),
            ';' | '&' | '|' => return Err(CurlError::MultipleCommands),
            '<' | '>' => return Err(CurlError::Shell("重定向")),
            '(' | ')' => return Err(CurlError::Shell("子 shell")),
            // 词首的 # 起注释，到行尾
            '#' if word.is_none() => while chars.next_if(|&c| c != '\n').is_some() {},
            c => push(&mut word, c),
        }
    }
    finish(&mut words, &mut word)?;
    Ok(words)
}

/// `$` 后面跟着 `next` 时 bash 会展开（变量、`${…}`、`$(…)`、特殊参数）。
fn expands(next: char) -> bool {
    next.is_ascii_alphanumeric() || "_{(@*#?-$!".contains(next)
}

/// 命令在没有转义的换行处结束：后面还有内容即多条命令。`text` 已去掉首尾空白，换行之后都是空白时不会走到这里。
fn end_of_command(rest: &mut impl Iterator<Item = char>) -> Result<(), CurlError> {
    if rest.any(|c| !c.is_whitespace()) {
        return Err(CurlError::MultipleCommands);
    }
    Ok(())
}

/// 双引号里的内容（开头的引号已读过），读到结束的引号。
fn double_quoted(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    word: &mut Vec<u8>,
) -> Result<(), CurlError> {
    loop {
        match chars.next() {
            Some('"') => return Ok(()),
            // 只转义这几个字符与换行（续行），其余的反斜杠照原样
            Some('\\') => {
                if let Some(c) = chars.next_if(|c| matches!(c, '$' | '`' | '"' | '\\')) {
                    push_to(word, c);
                } else if chars.next_if_eq(&'\n').is_none() {
                    push_to(word, '\\');
                }
            }
            Some('$') => match chars.peek() {
                Some(&next) if expands(next) => return Err(CurlError::Shell("变量或命令替换")),
                _ => push_to(word, '$'),
            },
            Some('`') => return Err(CurlError::Shell("命令替换")),
            Some(c) => push_to(word, c),
            None => return Err(CurlError::Unclosed),
        }
    }
}

/// `$'…'` 里的内容（开头的 `$'` 已读过），读到结束的单引号。`\xHH` 与八进制写出的是字节，不是字符。
fn ansi_c(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    word: &mut Vec<u8>,
) -> Result<(), CurlError> {
    loop {
        let c = match chars.next() {
            Some('\'') => return Ok(()),
            Some(c) => c,
            None => return Err(CurlError::Unclosed),
        };
        if c != '\\' {
            push_to(word, c);
            continue;
        }
        let Some(escaped) = chars.next() else {
            return Err(CurlError::Unclosed);
        };
        match escaped {
            'a' => word.push(0x07),
            'b' => word.push(0x08),
            'e' | 'E' => word.push(0x1b),
            'f' => word.push(0x0c),
            'n' => word.push(b'\n'),
            'r' => word.push(b'\r'),
            't' => word.push(b'\t'),
            'v' => word.push(0x0b),
            '\\' | '\'' | '"' | '?' => push_to(word, escaped),
            'x' => match digits(chars, 16, 2) {
                Some(byte) => word.push(byte as u8),
                None => word.extend_from_slice(b"\\x"),
            },
            'u' | 'U' => {
                let most = if escaped == 'u' { 4 } else { 8 };
                match digits(chars, 16, most) {
                    Some(code) => push_to(word, char::from_u32(code).ok_or(CurlError::NotUtf8)?),
                    None => {
                        word.push(b'\\');
                        push_to(word, escaped);
                    }
                }
            }
            '0'..='7' => {
                // 连同这一位至多三位；只取低 8 位，同 bash
                let mut value = escaped.to_digit(8).unwrap_or_default();
                for _ in 0..2 {
                    let Some(digit) = chars.peek().and_then(|c| c.to_digit(8)) else {
                        break;
                    };
                    chars.next();
                    value = value * 8 + digit;
                }
                word.push(value as u8);
            }
            'c' => match chars.next() {
                Some(control) if control.is_ascii() => word.push(control as u8 & 0x1f),
                Some(other) => {
                    word.extend_from_slice(b"\\c");
                    push_to(word, other);
                }
                None => return Err(CurlError::Unclosed),
            },
            other => {
                word.push(b'\\');
                push_to(word, other);
            }
        }
    }
}

/// 读至多 `most` 位 `radix` 进制的数字；一位都没有时为 None。
fn digits(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    radix: u32,
    most: usize,
) -> Option<u32> {
    let mut value = None;
    for _ in 0..most {
        let Some(digit) = chars.peek().and_then(|c| c.to_digit(radix)) else {
            break;
        };
        chars.next();
        value = Some(value.unwrap_or(0) * radix + digit);
    }
    value
}

fn push(word: &mut Option<Vec<u8>>, c: char) {
    push_to(word.get_or_insert_default(), c);
}

fn push_to(word: &mut Vec<u8>, c: char) {
    word.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
}

/// 正在拼的词拼完了：放进 `words`。
fn finish(words: &mut Vec<String>, word: &mut Option<Vec<u8>>) -> Result<(), CurlError> {
    if let Some(bytes) = word.take() {
        words.push(String::from_utf8(bytes).map_err(|_| CurlError::NotUtf8)?);
    }
    Ok(())
}

/// 按 cmd.exe 的规则处理命令：去掉转义用的 `^`、接上续行，得到交给程序的命令行（再由 [`split_crt`] 拆成参数）。
fn cmd_line(text: &str) -> Result<String, CurlError> {
    let mut line = String::new();
    // 在 cmd 的引号里：`^` 与 `&|<>` 都是字符本身
    let mut quoted = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                end_of_command(&mut chars)?;
                break;
            }
            '"' => {
                quoted = !quoted;
                line.push(c);
            }
            _ if quoted => line.push(c),
            '^' => match chars.next() {
                // 续行：下一行的第一个字符照原样（值里的换行写成 `^` 加两个换行）
                Some('\n') => line.extend(chars.next()),
                Some(c) => line.push(c),
                None => {}
            },
            '&' | '|' => return Err(CurlError::MultipleCommands),
            '<' | '>' => return Err(CurlError::Shell("重定向")),
            c => line.push(c),
        }
    }
    Ok(line)
}

/// 按 MS C 运行库的规则把命令行拆成参数：空格与制表符分隔；引号里的 `""` 为引号本身；反斜杠只在引号前转义，2n 个
/// 反斜杠加引号为 n 个反斜杠与起止的引号，2n+1 个为 n 个反斜杠与引号本身。
fn split_crt(line: &str) -> Result<Vec<String>, CurlError> {
    let mut args = Vec::new();
    let mut arg: Option<String> = None;
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let mut slashes = 1;
                while chars.next_if_eq(&'\\').is_some() {
                    slashes += 1;
                }
                let arg = arg.get_or_insert_default();
                if chars.peek() == Some(&'"') {
                    arg.extend(std::iter::repeat_n('\\', slashes / 2));
                    if slashes % 2 == 1 {
                        chars.next();
                        arg.push('"');
                    }
                } else {
                    arg.extend(std::iter::repeat_n('\\', slashes));
                }
            }
            '"' => {
                let arg = arg.get_or_insert_default();
                if quoted && chars.next_if_eq(&'"').is_some() {
                    arg.push('"');
                } else {
                    quoted = !quoted;
                }
            }
            ' ' | '\t' if !quoted => args.extend(arg.take()),
            c => arg.get_or_insert_default().push(c),
        }
    }
    if quoted {
        return Err(CurlError::Unclosed);
    }
    args.extend(arg);
    Ok(args)
}

/// 认得的选项。
#[derive(Clone, Copy)]
enum Known {
    Url,
    Header,
    Cookie,
    UserAgent,
    Referer,
    User,
    Request,
    Output,
    Globoff,
    /// 不影响请求内容的：`--compressed`、`-L`、`-s`、`-S`、`-O`，与 `-k`（不从粘贴的命令里关掉证书校验）
    Ignored,
    Head,
    /// 要发请求体的
    Data,
}

impl Known {
    fn long(name: &str) -> Option<Known> {
        Some(match name {
            "url" => Known::Url,
            "header" => Known::Header,
            "cookie" => Known::Cookie,
            "user-agent" => Known::UserAgent,
            "referer" => Known::Referer,
            "user" => Known::User,
            "request" => Known::Request,
            "output" => Known::Output,
            "globoff" => Known::Globoff,
            "compressed" | "location" | "silent" | "show-error" | "remote-name" | "insecure" => {
                Known::Ignored
            }
            "head" => Known::Head,
            "data" | "data-raw" | "data-binary" | "data-urlencode" | "data-ascii" | "json"
            | "form" | "form-string" | "upload-file" => Known::Data,
            _ => return None,
        })
    }

    fn short(letter: char) -> Option<Known> {
        Some(match letter {
            'H' => Known::Header,
            'b' => Known::Cookie,
            'A' => Known::UserAgent,
            'e' => Known::Referer,
            'u' => Known::User,
            'X' => Known::Request,
            'o' => Known::Output,
            'g' => Known::Globoff,
            'L' | 's' | 'S' | 'O' | 'k' => Known::Ignored,
            'I' => Known::Head,
            'd' | 'F' | 'T' => Known::Data,
            _ => return None,
        })
    }

    /// 后面带一个值。要发请求体的不读值，直接报错。
    fn takes_value(self) -> bool {
        matches!(
            self,
            Known::Url
                | Known::Header
                | Known::Cookie
                | Known::UserAgent
                | Known::Referer
                | Known::User
                | Known::Request
                | Known::Output
        )
    }
}

/// 从命令的参数里读到的。
#[derive(Default)]
struct Options {
    urls: Vec<String>,
    /// `-g`：地址里的 `[]{}` 不是 curl 的通配，`\[` 之类不还原
    globoff: bool,
    /// `-H` 给的，按出现的顺序
    headers: Vec<(String, String)>,
    /// `-A`、`-e`、`-u` 转成的，同一种后一个替换前一个；与 `-H` 同名时不用
    derived: Vec<(String, String)>,
    /// `-b` 给的，合成一条 Cookie；`-H` 给了 Cookie 时不用
    cookies: Vec<String>,
    dropped: Vec<String>,
}

impl Options {
    /// 读 `curl` 之后的参数。
    fn read(mut words: impl Iterator<Item = String>) -> Result<Options, CurlError> {
        let mut options = Options::default();
        // 读到 `--` 之后都是地址
        let mut positional = false;
        while let Some(word) = words.next() {
            if positional || !word.starts_with('-') || word == "-" {
                options.urls.push(word);
            } else if word == "--" {
                positional = true;
            } else if let Some(name) = word.strip_prefix("--") {
                let known =
                    Known::long(name).ok_or_else(|| CurlError::UnknownOption(word.clone()))?;
                let value = known
                    .takes_value()
                    .then(|| {
                        words
                            .next()
                            .ok_or_else(|| CurlError::MissingValue(word.clone()))
                    })
                    .transpose()?;
                options.apply(known, &word, value)?;
            } else {
                // 短选项可以连写（-sSL）；带值的，值接在字母后面或是下一个参数
                for (at, letter) in word.char_indices().skip(1) {
                    let written = format!("-{letter}");
                    let known = Known::short(letter)
                        .ok_or_else(|| CurlError::UnknownOption(written.clone()))?;
                    if !known.takes_value() {
                        options.apply(known, &written, None)?;
                        continue;
                    }
                    let attached = &word[at + letter.len_utf8()..];
                    let value = if attached.is_empty() {
                        words
                            .next()
                            .ok_or_else(|| CurlError::MissingValue(written.clone()))?
                    } else {
                        attached.to_owned()
                    };
                    options.apply(known, &written, Some(value))?;
                    break;
                }
            }
        }
        Ok(options)
    }

    /// 记下选项 `known`（命令里写作 `written`）；`value` 为它带的值，不带值的为 None。
    fn apply(
        &mut self,
        known: Known,
        written: &str,
        value: Option<String>,
    ) -> Result<(), CurlError> {
        let value = value.unwrap_or_default();
        match known {
            Known::Url => self.urls.push(value),
            Known::Header => self.header(&value, written)?,
            // 没有等号的是 Cookie 文件名
            Known::Cookie if value.contains('=') => self.cookies.push(value),
            Known::Cookie => return Err(CurlError::CookieFile(value)),
            Known::UserAgent => self.derive("User-Agent", value),
            // `;auto`：跟随重定向时换成上一个地址，下载不跟它
            Known::Referer => {
                let referer = value.strip_suffix(";auto").unwrap_or(&value);
                if !referer.is_empty() {
                    self.derive("Referer", referer.to_owned());
                }
            }
            Known::User if value.contains(':') => self.derive(
                "Authorization",
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(&value)
                ),
            ),
            Known::User => return Err(CurlError::NoPassword),
            Known::Request if value == "GET" => {}
            Known::Request => return Err(CurlError::Method(value)),
            Known::Output | Known::Ignored => {}
            Known::Globoff => self.globoff = true,
            Known::Head => return Err(CurlError::Method("HEAD".to_owned())),
            Known::Data => return Err(CurlError::Data(written.to_owned())),
        }
        Ok(())
    }

    /// `-H` 的一条（`written` 为命令里的写法）。`名称:` 没有值的是让 curl 不发它自己会加的这个请求头，没有要带上的；
    /// `名称;` 是值为空的请求头。
    fn header(&mut self, line: &str, written: &str) -> Result<(), CurlError> {
        if line.starts_with('@') {
            return Err(CurlError::FromFile(written.to_owned()));
        }
        let (name, value) = match line.split_once(':') {
            Some((_, value)) if value.trim().is_empty() => return Ok(()),
            Some((name, value)) => (name.trim(), value.trim()),
            None => match line.trim().strip_suffix(';') {
                Some(name) => (name.trim(), ""),
                None => {
                    return Err(CurlError::Header(format!(
                        "{written} 后面要写成「名称: 值」"
                    )));
                }
            },
        };
        let lower = name.to_ascii_lowercase();
        if DROPPED.contains(&lower.as_str()) {
            if !self
                .dropped
                .iter()
                .any(|seen| seen.eq_ignore_ascii_case(name))
            {
                self.dropped.push(name.to_owned());
            }
            return Ok(());
        }
        if self
            .headers
            .iter()
            .any(|(old, _)| old.eq_ignore_ascii_case(name))
        {
            return Err(CurlError::DuplicateHeader(name.to_owned()));
        }
        check_option_header(name, value).map_err(CurlError::Header)?;
        self.headers.push((name.to_owned(), value.to_owned()));
        Ok(())
    }

    fn derive(&mut self, name: &str, value: String) {
        self.derived.retain(|(old, _)| old != name);
        self.derived.push((name.to_owned(), value));
    }

    fn into_request(self) -> Result<Request, CurlError> {
        let mut urls = self.urls.into_iter();
        let url = urls.next().ok_or(CurlError::NoUrl)?;
        if urls.next().is_some() {
            return Err(CurlError::MultipleUrls);
        }
        let url = if self.globoff { url } else { unglob(&url) };
        let url = Url::parse(&url)
            .ok()
            .filter(|parsed| matches!(parsed.scheme(), "http" | "https"))
            .ok_or(CurlError::BadUrl(url))?;
        let cookie = self
            .cookies
            .iter()
            .flat_map(|cookies| cookies.split(';'))
            .map(str::trim)
            .filter(|pair| !pair.is_empty())
            .collect::<Vec<_>>()
            .join("; ");
        let mut derived = self.derived;
        if !cookie.is_empty() {
            derived.push(("Cookie".to_owned(), cookie));
        }
        let mut headers = self.headers;
        for (name, value) in derived {
            if !headers
                .iter()
                .any(|(old, _)| old.eq_ignore_ascii_case(&name))
            {
                check_option_header(&name, &value).map_err(CurlError::Header)?;
                headers.push((name, value));
            }
        }
        Ok(Request {
            url,
            headers,
            dropped: self.dropped,
        })
    }
}

/// 还原地址里为避开 curl 的通配而转义的 `\[ \] \{ \}`（Chrome 与 Safari 这样写，Firefox 改加 `--globoff`）。
fn unglob(url: &str) -> String {
    let mut plain = String::with_capacity(url.len());
    let mut chars = url.chars().peekable();
    while let Some(c) = chars.next() {
        match chars.next_if(|next| c == '\\' && "[]{}".contains(*next)) {
            Some(bracket) => plain.push(bracket),
            None => plain.push(c),
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 样例里的请求（按各浏览器的生成代码生成，bash 的经 bash 5.2 核对过参数）：地址带 `[]{}`，请求头带单双引号、
    /// `!`、`%PATH%`、`$HOME`、结尾的反斜杠、空值，以及要去掉的几种。
    const URL: &str = "https://media.example.com/live/[abc]/index.m3u8?token=x{1}&a=b";
    const COOKIE: &str = "sid=a'b!c; theme=\"dark\"; pct=100%PATH%";
    const AGENT: &str = "Mozilla/5.0 (X11; \"q\") 100%PATH% $HOME it's!";
    const REFERER: &str = "https://www.example.com/watch?v=1&t=2";

    const CHROME_BASH: &str = r##"curl --url 'https://media.example.com/live/\[abc\]/index.m3u8?token=x\{1\}&a=b' \
  -H 'Accept: */*' \
  -b $'sid=a\'b!c; theme="dark"; pct=100%PATH%' \
  -H 'Referer: https://www.example.com/watch?v=1&t=2' \
  -H $'User-Agent: Mozilla/5.0 (X11; "q") 100%PATH% $HOME it\'s!' \
  -H 'X-Empty;' \
  -H 'Range: bytes=0-' \
  -H 'If-None-Match: "etag"' \
  -H 'X-Tail: end\'"##;

    const CHROME_CMD: &str = r##"curl --url ^"https://media.example.com/live/^\[abc^\]/index.m3u8?token=x^\{1^\}^&a=b^" ^
  -H ^"Accept: */*^" ^
  -b ^"sid=a'b^!c; theme=^"^"dark^"^"; pct=100^%^PATH^%^" ^
  -H ^"Referer: https://www.example.com/watch?v=1^&t=2^" ^
  -H ^"User-Agent: Mozilla/5.0 ^(X11; ^"^"q^"^"^) 100^%^PATH^% ^$HOME it's^!^" ^
  -H ^"X-Empty;^" ^
  -H ^"Range: bytes=0-^" ^
  -H ^"If-None-Match: ^"^"etag^"^"^" ^
  -H ^"X-Tail: end^\^\^""##;

    const FIREFOX_BASH: &str = r##"curl 'https://media.example.com/live/[abc]/index.m3u8?token=x{1}&a=b' \
  --globoff \
  --compressed \
  -H 'Accept: */*' \
  -H 'Accept-Encoding: gzip, deflate, br' \
  -H $'Cookie: sid=a\'b\041c; theme="dark"; pct=100%PATH%' \
  -H 'Referer: https://www.example.com/watch?v=1&t=2' \
  -H $'User-Agent: Mozilla/5.0 (X11; "q") 100%PATH% $HOME it\'s\041' \
  -H 'X-Empty: ' \
  -H 'Range: bytes=0-' \
  -H 'If-None-Match: "etag"' \
  -H 'X-Tail: end\'"##;

    const FIREFOX_CMD: &str = r##"curl.exe ^"https://media.example.com/live/^[abc^]/index.m3u8?token=x^{1^}^&a=b^" ^
  --globoff ^
  --compressed ^
  -H ^"Accept: */*^" ^
  -H ^"Accept-Encoding: gzip, deflate, br^" ^
  -H ^"Cookie: sid=a'b^!c; theme=^\^"dark^\^"; pct=100^%^PATH^%^" ^
  -H ^"Referer: https://www.example.com/watch?v=1^&t=2^" ^
  -H ^"User-Agent: Mozilla/5.0 (X11; ^\^"q^\^") 100^%^PATH^% ^$HOME it's^!^" ^
  -H ^"X-Empty: ^" ^
  -H ^"Range: bytes=0-^" ^
  -H ^"If-None-Match: ^\^"etag^\^"^" ^
  -H ^"X-Tail: end^\^\^""##;

    const SAFARI_BASH: &str = r##"curl 'https://media.example.com/live/\[abc\]/index.m3u8?token=x\{1\}&a=b' \
-X 'GET' \
-H 'Accept: */*' \
-H 'Accept-Encoding: gzip, deflate, br' \
-H 'Host: media.example.com' \
-H $'Cookie: sid=a\'b\041c; theme="dark"; pct=100%PATH%' \
-H 'Referer: https://www.example.com/watch?v=1&t=2' \
-H $'User-Agent: Mozilla/5.0 (X11; "q") 100%PATH% $HOME it\'s\041' \
-H 'X-Empty: ' \
-H 'Range: bytes=0-' \
-H 'If-None-Match: "etag"' \
-H 'X-Tail: end\'"##;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    /// Chrome 的 `-b` 排在 `-H` 之后；`X-Empty;` 为空值的请求头。bash 与 cmd 两种写法拆出同一个请求（Windows 换行也认）。
    #[test]
    fn chrome_commands() {
        let expected = Request {
            url: Url::parse(URL).unwrap(),
            headers: pairs(&[
                ("Accept", "*/*"),
                ("Referer", REFERER),
                ("User-Agent", AGENT),
                ("X-Empty", ""),
                ("X-Tail", "end\\"),
                ("Cookie", COOKIE),
            ]),
            dropped: names(&["Range", "If-None-Match"]),
        };
        assert_eq!(parse(CHROME_BASH), Ok(expected.clone()));
        assert_eq!(parse(CHROME_CMD), Ok(expected.clone()));
        assert_eq!(parse(&CHROME_CMD.replace('\n', "\r\n")), Ok(expected));
    }

    /// Firefox 与 Safari 的 `X-Empty: ` 是让 curl 不发这个请求头；Firefox 的地址不转义、加 `--globoff`。
    #[test]
    fn firefox_and_safari_commands() {
        let headers = pairs(&[
            ("Accept", "*/*"),
            ("Cookie", COOKIE),
            ("Referer", REFERER),
            ("User-Agent", AGENT),
            ("X-Tail", "end\\"),
        ]);
        let firefox = Request {
            url: Url::parse(URL).unwrap(),
            headers: headers.clone(),
            dropped: names(&["Accept-Encoding", "Range", "If-None-Match"]),
        };
        assert_eq!(parse(FIREFOX_BASH), Ok(firefox.clone()));
        assert_eq!(parse(FIREFOX_CMD), Ok(firefox));
        assert_eq!(
            parse(SAFARI_BASH),
            Ok(Request {
                url: Url::parse(URL).unwrap(),
                headers,
                dropped: names(&["Accept-Encoding", "Host", "Range", "If-None-Match"]),
            })
        );
    }

    #[test]
    fn only_curl_is_a_command() {
        assert!(is_command("  curl 'https://a.example/'"));
        assert!(is_command("CURL.EXE ^\"https://a.example/^\""));
        assert!(is_command("curl\n"));
        assert!(!is_command("curly https://a.example/"));
        assert!(!is_command("https://a.example/curl"));
        assert_eq!(parse("wget https://a.example/"), Err(CurlError::NotCurl));
    }

    /// 同 curl：`-H` 给了同名的就不用 `-A`、`-e`、`-u`、`-b` 转成的；多个 `-b` 合成一条；`-u` 为 Basic 认证；
    /// 短选项可以连写，值可以接在字母后面。
    #[test]
    fn options_turn_into_headers_like_curl() {
        let request = parse(
            "curl -sSLk -XGET https://a.example/v.m3u8 -A agent -e 'https://r.example/;auto' \
             -u 'user:pa:ss' -b 'a=1' -b 'c=3;' -o out.m3u8 --compressed",
        )
        .unwrap();
        assert_eq!(
            request.headers,
            pairs(&[
                ("User-Agent", "agent"),
                ("Referer", "https://r.example/"),
                ("Authorization", "Basic dXNlcjpwYTpzcw=="),
                ("Cookie", "a=1; c=3"),
            ])
        );
        let request = parse(
            "curl https://a.example/ -A a -H 'user-agent: h' -u u:p -H 'Authorization: Bearer t' \
             -b a=1 -H 'Cookie: b=2' -HAccept:x",
        )
        .unwrap();
        assert_eq!(
            request.headers,
            pairs(&[
                ("user-agent", "h"),
                ("Authorization", "Bearer t"),
                ("Cookie", "b=2"),
                ("Accept", "x"),
            ])
        );
    }

    /// bash 的写法：双引号里的转义、`$'…'` 的字节与字符、`--` 之后都是地址、注释。
    #[test]
    fn bash_quoting() {
        let request = parse(
            r#"curl -H "X-A: \"q\" \\ \$ \x" -H $'X-B: \x41\101中\xe4\xb8\xad\t.' -- 'https://a.example/'  # 注释"#,
        )
        .unwrap();
        assert_eq!(request.url.as_str(), "https://a.example/");
        assert_eq!(
            request.headers,
            pairs(&[("X-A", r#""q" \ $ \x"#), ("X-B", "AA中中\t.")])
        );
        assert_eq!(
            parse(r"curl $'https://a.example/\x41\101中\xe4\xb8\xad'")
                .unwrap()
                .url
                .as_str(),
            "https://a.example/AA%E4%B8%AD%E4%B8%AD"
        );
        assert_eq!(
            parse(r"curl $'https://a.example/\xe9'"),
            Err(CurlError::NotUtf8)
        );
    }

    #[test]
    fn unsupported_requests_are_refused() {
        let cases = [
            (
                "curl https://a.example/ -X POST",
                CurlError::Method("POST".into()),
            ),
            (
                "curl -I https://a.example/",
                CurlError::Method("HEAD".into()),
            ),
            (
                "curl https://a.example/ --data-raw 'a=1'",
                CurlError::Data("--data-raw".into()),
            ),
            (
                "curl https://a.example/ -d a=1",
                CurlError::Data("-d".into()),
            ),
            (
                "curl https://a.example/ -b cookies.txt",
                CurlError::CookieFile("cookies.txt".into()),
            ),
            (
                "curl https://a.example/ -H @headers.txt",
                CurlError::FromFile("-H".into()),
            ),
            ("curl https://a.example/ -u user", CurlError::NoPassword),
            (
                "curl https://a.example/ --retry 3",
                CurlError::UnknownOption("--retry".into()),
            ),
            (
                "curl -sv https://a.example/",
                CurlError::UnknownOption("-v".into()),
            ),
            (
                "curl https://a.example/ -H",
                CurlError::MissingValue("-H".into()),
            ),
            (
                "curl https://a.example/ --header",
                CurlError::MissingValue("--header".into()),
            ),
            ("curl -L", CurlError::NoUrl),
            (
                "curl https://a.example/ https://b.example/",
                CurlError::MultipleUrls,
            ),
            (
                "curl ftp://a.example/x",
                CurlError::BadUrl("ftp://a.example/x".into()),
            ),
            (
                "curl https://a.example/ -H 'X: 1' -H 'x: 2'",
                CurlError::DuplicateHeader("x".into()),
            ),
            (
                "curl https://a.example/ -H 'Cookie: abc'",
                CurlError::Header("请求头 Cookie 应为 `名=值; 名=值` 的写法".into()),
            ),
            (
                "curl https://a.example/ -H 'no colon'",
                CurlError::Header("-H 后面要写成「名称: 值」".into()),
            ),
        ];
        for (command, error) in cases {
            assert_eq!(parse(command), Err(error), "{command}");
        }
    }

    #[test]
    fn shell_beyond_quoting_is_refused() {
        let cases = [
            (
                "curl 'https://a.example/' ; curl 'https://b.example/'",
                CurlError::MultipleCommands,
            ),
            (
                "curl 'https://a.example/'\ncurl 'https://b.example/'",
                CurlError::MultipleCommands,
            ),
            (
                "curl 'https://a.example/' | head",
                CurlError::MultipleCommands,
            ),
            (
                "curl ^\"https://a.example/^\" &\r\ncurl ^\"https://b.example/^\"",
                CurlError::MultipleCommands,
            ),
            (
                "curl ^\"https://a.example/^\"\r\ncurl ^\"https://b.example/^\"",
                CurlError::MultipleCommands,
            ),
            ("curl 'https://a.example/", CurlError::Unclosed),
            ("curl \"https://a.example/", CurlError::Unclosed),
            ("curl $'https://a.example/", CurlError::Unclosed),
            (
                "curl ^\"https://a.example/^\" -H \"X: 1",
                CurlError::Unclosed,
            ),
            (
                "curl https://a.example/$PATH",
                CurlError::Shell("变量或命令替换"),
            ),
            (
                "curl \"https://a.example/${HOST}\"",
                CurlError::Shell("变量或命令替换"),
            ),
            (
                "curl https://a.example/$(id)",
                CurlError::Shell("变量或命令替换"),
            ),
            ("curl https://a.example/`id`", CurlError::Shell("命令替换")),
            ("curl https://a.example/ > out", CurlError::Shell("重定向")),
            (
                "curl ^\"https://a.example/^\" > out",
                CurlError::Shell("重定向"),
            ),
        ];
        for (command, error) in cases {
            assert_eq!(parse(command), Err(error), "{command:?}");
        }
        // 不跟变量名的 $ 与 cmd 引号里的 & 都是字符本身
        assert_eq!(
            parse("curl 'https://a.example/' -H \"X: 5$ & $\"")
                .unwrap()
                .headers,
            pairs(&[("X", "5$ & $")])
        );
        assert_eq!(
            parse("curl \"https://a.example/?a=1&b=2\" ^\n  -H \"X: ^1\"").unwrap(),
            Request {
                url: Url::parse("https://a.example/?a=1&b=2").unwrap(),
                headers: pairs(&[("X", "^1")]),
                dropped: Vec::new(),
            }
        );
    }

    /// 带凭据的命令只能用于同源的任务。
    #[test]
    fn credentials_stay_with_their_origin() {
        let target = Url::parse("https://a.example/live/index.m3u8").unwrap();
        let same = parse("curl https://a.example/seg/1.ts -b sid=1").unwrap();
        assert_eq!(same.check_target(&target), Ok(()));
        let other = parse("curl https://cdn.example/seg/1.ts -u u:p").unwrap();
        assert_eq!(
            other.check_target(&target),
            Err(CurlError::CrossOrigin {
                command: "https://cdn.example".into(),
                target: "https://a.example".into(),
            })
        );
        let plain = parse("curl https://cdn.example/seg/1.ts -e https://a.example/").unwrap();
        assert_eq!(plain.check_target(&target), Ok(()));
    }
}
