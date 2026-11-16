//! 离线下载与网盘里的文件。

use hs_m3u8_core::{Method, Url};

use super::client::Client;
use super::{Error, wire};

/// 网盘上的离线任务。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    /// 网盘给的名字：多为种子的名字，网盘还没取到种子信息时可能是信息哈希
    pub name: String,
    pub phase: Phase,
    /// 0 到 100
    pub progress: u8,
    /// 失败时网盘给的原因；没有时为空
    pub message: String,
    /// 下到的文件或文件夹；还没建出时为 None
    pub file: Option<String>,
    /// 总字节数；不知道时为 None
    pub size: Option<u64>,
    /// 提交时的链接
    pub url: String,
}

/// 离线任务的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 排队或刚建（含网盘报的「未知」）
    Pending,
    Running,
    /// 在网盘上被暂停了
    Paused,
    Failed,
    Complete,
}

/// 网盘里的文件或文件夹。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    /// 网盘上的名字，原样：可能含路径分隔符等本机不能用的字符，用作文件名前由调用方清理
    pub name: String,
    pub folder: bool,
    /// 字节数；文件夹为 0
    pub size: u64,
    /// 文件的 GCID（见 [`super::gcid`]），40 位小写十六进制；文件夹为 None
    pub gcid: Option<String>,
    /// 原文件的直链（带签名、有时效；取链与下载要走同一出口）：只有 [`Client::entry`] 取的文件有
    pub link: Option<Url>,
}

/// 每页列多少项。
const PAGE: u32 = 500;

impl Client {
    /// 提交离线下载 `link`（磁力链接），放进网盘的 My Pack。请求发出后才失败（超时、连接中断）时网盘可能已建了任务，
    /// 由调用方用 [`Client::find_job`] 查。
    pub async fn submit(&self, link: &str) -> Result<Job, Error> {
        let body = serde_json::to_value(wire::NewOffline {
            kind: "drive#file",
            upload_type: "UPLOAD_TYPE_URL",
            url: wire::OfflineUrl { url: link },
            folder_type: "DOWNLOAD",
        })
        .expect("请求体总能写成 JSON");
        let url = self.endpoints().drive("/drive/v1/files")?;
        let submitted: wire::Submitted = self
            .api(Method::Post, url, Some(&body), "POST:/drive/v1/files")
            .await?;
        let task = submitted
            .task
            .ok_or_else(|| Error::Unrecognized("提交离线下载的响应里没有任务".to_owned()))?;
        Job::of(task)
    }

    /// 离线任务 `id`；网盘上已没有（被用户删了）时为 None。
    pub async fn job(&self, id: &str) -> Result<Option<Job>, Error> {
        let filters = serde_json::json!({ "id": { "in": id } }).to_string();
        let mut found = self.jobs(&filters).await?;
        match found.iter().position(|job| job.id == id) {
            Some(index) => Ok(Some(found.swap_remove(index))),
            None => Ok(None),
        }
    }

    /// 最近的离线任务里提交的是 `link` 的那个（提交得到结果之前就断了时，查它到底建没建）；没有时为 None。
    pub async fn find_job(&self, link: &str) -> Result<Option<Job>, Error> {
        Ok(self
            .jobs("{}")
            .await?
            .into_iter()
            .find(|job| job.url == link))
    }

    /// 离线任务的第一页（最近的在前），按 `filters`（JSON）筛。
    async fn jobs(&self, filters: &str) -> Result<Vec<Job>, Error> {
        let mut url = self.endpoints().drive("/drive/v1/tasks")?;
        url.query_pairs_mut()
            .append_pair("type", "offline")
            .append_pair("limit", "100")
            .append_pair("filters", filters);
        let list: wire::TaskList = self
            .api(Method::Get, url, None, "GET:/drive/v1/tasks")
            .await?;
        list.tasks.into_iter().map(Job::of).collect()
    }

    /// 文件夹 `folder` 里的文件与文件夹（不含回收站里的与没传完的），按网盘给的次序。
    pub async fn children(&self, folder: &str) -> Result<Vec<Entry>, Error> {
        checked_id(folder)?;
        let filters = serde_json::json!({
            "phase": { "eq": "PHASE_TYPE_COMPLETE" },
            "trashed": { "eq": false },
        })
        .to_string();
        let mut entries = Vec::new();
        let mut page_token = String::new();
        loop {
            let mut url = self.endpoints().drive("/drive/v1/files")?;
            url.query_pairs_mut()
                .append_pair("parent_id", folder)
                .append_pair("limit", &PAGE.to_string())
                .append_pair("filters", &filters)
                .append_pair("page_token", &page_token);
            let page: wire::FileList = self
                .api(Method::Get, url, None, "GET:/drive/v1/files")
                .await?;
            for file in page.files {
                entries.push(Entry::of(file)?);
            }
            if page.next_page_token.is_empty() {
                return Ok(entries);
            }
            if page.next_page_token == page_token {
                return Err(Error::Unrecognized("列目录的下一页还是同一页".to_owned()));
            }
            page_token = page.next_page_token;
        }
    }

    /// 文件或文件夹 `id`，文件带原文件的直链。
    pub async fn entry(&self, id: &str) -> Result<Entry, Error> {
        let mut url = self
            .endpoints()
            .drive(&format!("/drive/v1/files/{}", checked_id(id)?))?;
        url.query_pairs_mut().append_pair("usage", "FETCH");
        let file: wire::File = self
            .api(Method::Get, url, None, "GET:/drive/v1/files")
            .await?;
        Entry::of(file)
    }
}

/// 拼进路径的编号只能是字母、数字、`-` 与 `_`：网盘给的编号都是这样，别的字符拼进路径会改变地址的含义。
fn checked_id(id: &str) -> Result<&str, Error> {
    let fine = !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    fine.then_some(id)
        .ok_or_else(|| Error::Unrecognized(format!("编号 {id:?} 不是认得的写法")))
}

/// 十进制的字节数；空串为 None。
fn size(text: &str, what: &str) -> Result<Option<u64>, Error> {
    if text.is_empty() {
        return Ok(None);
    }
    text.parse()
        .map(Some)
        .map_err(|_| Error::Unrecognized(format!("{what}的大小 {text:?} 不是非负整数")))
}

impl Job {
    fn of(task: wire::Task) -> Result<Job, Error> {
        let phase = match task.phase.as_str() {
            "PHASE_TYPE_PENDING" | "PHASE_TYPE_UNKNOW" => Phase::Pending,
            "PHASE_TYPE_RUNNING" => Phase::Running,
            "PHASE_TYPE_PAUSED" => Phase::Paused,
            "PHASE_TYPE_ERROR" => Phase::Failed,
            "PHASE_TYPE_COMPLETE" => Phase::Complete,
            other => {
                return Err(Error::Unrecognized(format!("离线任务的阶段 {other:?}")));
            }
        };
        let progress = u8::try_from(task.progress)
            .ok()
            .filter(|progress| *progress <= 100)
            .ok_or_else(|| Error::Unrecognized(format!("离线任务的进度 {}", task.progress)))?;
        let file = (!task.file_id.is_empty())
            .then(|| checked_id(&task.file_id).map(str::to_owned))
            .transpose()?;
        Ok(Job {
            size: size(&task.file_size, "离线任务")?,
            id: checked_id(&task.id)?.to_owned(),
            name: task.name,
            phase,
            progress,
            message: task.message,
            file,
            url: task.params.url,
        })
    }
}

impl Entry {
    fn of(file: wire::File) -> Result<Entry, Error> {
        let folder = match file.kind.as_str() {
            "drive#folder" => true,
            "drive#file" => false,
            other => return Err(Error::Unrecognized(format!("文件的种类 {other:?}"))),
        };
        let gcid = if folder {
            None
        } else {
            let gcid = file.hash.to_ascii_lowercase();
            let fine = gcid.len() == 40 && gcid.bytes().all(|byte| byte.is_ascii_hexdigit());
            if !fine {
                return Err(Error::Unrecognized(format!(
                    "{} 的 GCID {:?}",
                    file.name, file.hash
                )));
            }
            Some(gcid)
        };
        let link = (!file.web_content_link.is_empty())
            .then(|| {
                Url::parse(&file.web_content_link)
                    .map_err(|error| Error::Unrecognized(format!("{} 的直链：{error}", file.name)))
            })
            .transpose()?;
        Ok(Entry {
            id: checked_id(&file.id)?.to_owned(),
            size: if folder {
                0
            } else {
                size(&file.size, &file.name)?
                    .ok_or_else(|| Error::Unrecognized(format!("{} 没有大小", file.name)))?
            },
            name: file.name,
            folder,
            gcid,
            link,
        })
    }
}
