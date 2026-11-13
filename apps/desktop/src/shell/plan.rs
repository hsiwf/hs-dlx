//! 停着（暂停或失败）的任务能做什么、先做什么：按失败的种类、是否直播、上一次运行是不是只写出已录的决定。放弃总在
//! 最后，不在这里。

use hs_m3u8_app::FailureKind;

/// 停着的任务能做的一件事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    /// 改请求头（编辑卡片里保存后再跑）
    EditHeaders,
    /// 不再续录，只把已录的写出来
    WriteRecorded,
    /// 改存本地 HLS 再跑（点播续传，直播只写出）
    SwitchToHls,
    /// 照新建时的请求再跑一次：暂停的叫继续，失败的叫重试
    Resume,
    /// 打开保存目录：挡路的东西就在里面
    OpenFolder,
}

/// 停着的任务的操作，最该先做的排第一。`failure` 为失败的种类（暂停的为 None），`live` 为直播录制，`wrote` 为
/// 上一次运行是只写出已录的，`editing` 为正在改请求头（那时编辑卡片里有保存的按钮，这里不再给改请求头）。
///
/// - 被拒或取到错误页的，先改请求头；放不进 MP4 的，先改存本地 HLS。
/// - 直播接不上之前录的、来源已没有（404/410）或停滞的，先写出已录的：续录多半不成。
/// - 再试也一样的不给重试；上一次是只写出的，重试也只写出。
/// - 直播停着时都能写出已录的；任何停着的任务都能改请求头。
pub(super) fn stopped_plan(
    failure: Option<FailureKind>,
    live: bool,
    wrote: bool,
    editing: bool,
) -> Vec<Action> {
    let headers_first = failure.is_some_and(FailureKind::headers_may_help);
    let live_over = live
        && matches!(
            failure,
            Some(
                FailureKind::SourceChanged | FailureKind::Status(404 | 410) | FailureKind::Stalled
            )
        );
    let mut plan = Vec::new();
    if headers_first && !editing {
        plan.push(Action::EditHeaders);
    }
    if failure == Some(FailureKind::Mp4Unsupported) {
        plan.push(Action::SwitchToHls);
    }
    if live_over {
        plan.push(Action::WriteRecorded);
    }
    if failure.is_none_or(FailureKind::retry_helps) {
        plan.push(if wrote {
            Action::WriteRecorded
        } else {
            Action::Resume
        });
    }
    if live && !plan.contains(&Action::WriteRecorded) {
        plan.push(Action::WriteRecorded);
    }
    if !headers_first && !editing {
        plan.push(Action::EditHeaders);
    }
    if failure == Some(FailureKind::OutputTaken) {
        plan.push(Action::OpenFolder);
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::Action::*;
    use super::*;

    #[test]
    fn the_first_action_fits_the_failure() {
        // 暂停的点播：继续，旁边能改请求头
        assert_eq!(
            stopped_plan(None, false, false, false),
            [Resume, EditHeaders]
        );
        // 403：先改请求头，正在改时不再给
        assert_eq!(
            stopped_plan(Some(FailureKind::Status(403)), false, false, false),
            [EditHeaders]
        );
        assert_eq!(
            stopped_plan(Some(FailureKind::Status(403)), false, false, true),
            []
        );
        // 放不进 MP4 的直播：先改存本地 HLS，写出已录的也在
        assert_eq!(
            stopped_plan(Some(FailureKind::Mp4Unsupported), true, false, false),
            [SwitchToHls, WriteRecorded, EditHeaders]
        );
        // 直播来源没了：先写出已录的，不给重试
        assert_eq!(
            stopped_plan(Some(FailureKind::Status(404)), true, false, false),
            [WriteRecorded, EditHeaders]
        );
        // 只写出时盘满：腾出空间后重试也只写出
        assert_eq!(
            stopped_plan(Some(FailureKind::Disk), true, true, false),
            [WriteRecorded, EditHeaders]
        );
        // 输出路径被占：重试，并能打开保存目录
        assert_eq!(
            stopped_plan(Some(FailureKind::OutputTaken), false, false, false),
            [Resume, EditHeaders, OpenFolder]
        );
        // 再试也一样的点播：只剩改请求头（放弃另给）
        assert_eq!(
            stopped_plan(Some(FailureKind::Unrecoverable), false, false, false),
            [EditHeaders]
        );
    }
}
