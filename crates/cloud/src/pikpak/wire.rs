//! 接口的请求体与响应体，字段照接口原样；只列用到的，其余字段忽略。数字有的以字符串给出（大小），读出后在
//! [`super`] 里转换。

use serde::{Deserialize, Serialize};

/// 失败时的响应体（HTTP 状态码多为 4xx；有的接口 200 也带它）。
#[derive(Debug, Deserialize)]
pub(crate) struct ErrorBody {
    /// 错误名，如 `unauthenticated`、`captcha_invalid`；没有时为空
    #[serde(default)]
    pub error: String,
    /// 错误码；同一个码可能对应不同错误名（9 既是 captcha 无效，也是文件在回收站里）
    #[serde(default)]
    pub error_code: i64,
    #[serde(default)]
    pub error_description: String,
}

/// `POST /v1/auth/token` 与设备授权换到的令牌。
#[derive(Debug, Deserialize)]
pub(crate) struct Token {
    pub access_token: String,
    pub refresh_token: String,
    /// access_token 的有效秒数
    pub expires_in: i64,
    /// 用户编号
    pub sub: String,
}

/// `POST /v1/auth/device/code` 的响应。
#[derive(Debug, Deserialize)]
pub(crate) struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    /// 有效秒数
    pub expires_in: Option<i64>,
    /// 最短轮询间隔，秒
    pub interval: Option<i64>,
}

/// `POST /v1/shield/captcha/init` 的响应。
#[derive(Debug, Deserialize)]
pub(crate) struct Captcha {
    #[serde(default)]
    pub captcha_token: String,
    /// 非空即要人机验证：在浏览器里打开它
    #[serde(default)]
    pub url: String,
}

/// 提交离线下载的请求体（`POST /drive/v1/files`）：不给 parent_id 时放进 My Pack。
#[derive(Debug, Serialize)]
pub(crate) struct NewOffline<'a> {
    pub kind: &'static str,
    pub upload_type: &'static str,
    pub url: OfflineUrl<'a>,
    pub folder_type: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct OfflineUrl<'a> {
    pub url: &'a str,
}

/// 提交离线下载的响应。
#[derive(Debug, Deserialize)]
pub(crate) struct Submitted {
    pub task: Option<Task>,
}

/// 离线任务。
#[derive(Debug, Deserialize)]
pub(crate) struct Task {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// `PHASE_TYPE_PENDING`、`RUNNING`、`PAUSED`、`ERROR`、`COMPLETE` 等
    pub phase: String,
    /// 0 到 100
    #[serde(default)]
    pub progress: i64,
    /// 失败时为原因
    #[serde(default)]
    pub message: String,
    /// 下到的文件或文件夹（多文件的种子）；还没建出时为空
    #[serde(default)]
    pub file_id: String,
    /// 总字节数的十进制；不知道时为空
    #[serde(default)]
    pub file_size: String,
    #[serde(default)]
    pub params: TaskParams,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct TaskParams {
    /// 提交时的链接
    #[serde(default)]
    pub url: String,
}

/// 列离线任务（`GET /drive/v1/tasks`）的一页。
#[derive(Debug, Deserialize)]
pub(crate) struct TaskList {
    #[serde(default)]
    pub tasks: Vec<Task>,
}

/// 列目录（`GET /drive/v1/files`）的一页。
#[derive(Debug, Deserialize)]
pub(crate) struct FileList {
    #[serde(default)]
    pub files: Vec<File>,
    /// 为空即最后一页
    #[serde(default)]
    pub next_page_token: String,
}

/// 文件或文件夹。
#[derive(Debug, Deserialize)]
pub(crate) struct File {
    pub id: String,
    /// `drive#file` 或 `drive#folder`
    pub kind: String,
    pub name: String,
    /// 字节数的十进制；文件夹为 0 或空
    #[serde(default)]
    pub size: String,
    /// GCID（见 [`super::gcid`]），大小写不定；文件夹为空
    #[serde(default)]
    pub hash: String,
    /// 原文件的直链（带签名、有时效）；只在取单个文件且带 `usage=FETCH` 时有
    #[serde(default)]
    pub web_content_link: String,
}
