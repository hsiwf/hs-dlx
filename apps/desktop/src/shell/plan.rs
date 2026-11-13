//! 停着（暂停或失败）的任务能做什么、先做什么：按失败的种类、是否直播、是否选了只写出已录的决定。放弃总在最后，
//! 不在这里。

use hs_m3u8_app::FailureKind;

/// 停着的任务能做的一件事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    /// 改请求头（编辑卡片里保存，见 [`Plan::resume_after_headers`]）
    EditHeaders,
    /// 不再续录，只把已录的写出来（不联网）
    WriteRecorded,
    /// 改存本地 HLS 再跑（点播续传，直播只写出）
    SwitchToHls,
    /// 照新建时的请求续传、续录：暂停的叫继续，失败的叫重试，选了只写出的叫续录
    Resume,
    /// 打开保存目录：占着输出路径的东西就在里面
    OpenFolder,
}

/// 停着的任务的操作。
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Plan {
    /// 最该先做的排第一，各不相同
    pub actions: Vec<Action>,
    /// 改请求头保存后接着续传续录：只写出已录的不联网、放不进 MP4 的要先换输出，都用不上新的请求头，只保存
    pub resume_after_headers: bool,
}

/// 停着的任务的操作。`failure` 为失败的种类（暂停的为 None），`live` 为直播录制，`write_only` 为选了只写出已录的
/// （[`hs_m3u8_app::Task::write_only`]），`editing` 为正在改请求头（那时编辑卡片里有保存的按钮，这里不再给改请求头）。
///
/// - 再跑一次：选了只写出的写出，其余续传续录；没有可写出的改为续录。
/// - 被拒或取到错误页的，先改请求头；放不进 MP4 的，先改存本地 HLS，此前再跑只会同样失败。
/// - 直播接不上之前录的、来源已没有（404/410）或停滞的，先写出已录的：续录多半不成。
/// - 再试也一样的不给再跑；改了请求头就不一样了，保存后照样接着跑。
/// - 直播停着时都能写出已录的；任何停着的任务都能改请求头。
pub(super) fn stopped_plan(
    failure: Option<FailureKind>,
    live: bool,
    write_only: bool,
    editing: bool,
) -> Plan {
    let mp4 = failure == Some(FailureKind::Mp4Unsupported);
    let rerun = if write_only && failure != Some(FailureKind::NothingToWrite) {
        Action::WriteRecorded
    } else {
        Action::Resume
    };
    let headers_first = failure.is_some_and(FailureKind::headers_may_help);
    let live_over = live
        && matches!(
            failure,
            Some(
                FailureKind::SourceChanged | FailureKind::Status(404 | 410) | FailureKind::Stalled
            )
        );
    let mut actions = Vec::new();
    let mut add = |action| {
        if !actions.contains(&action) {
            actions.push(action);
        }
    };
    if headers_first && !editing {
        add(Action::EditHeaders);
    }
    if mp4 {
        add(Action::SwitchToHls);
    }
    if live_over {
        add(Action::WriteRecorded);
    }
    if !mp4
        && (failure.is_none_or(FailureKind::retry_helps)
            || failure == Some(FailureKind::NothingToWrite))
    {
        add(rerun);
    }
    if live && !mp4 && failure != Some(FailureKind::NothingToWrite) {
        add(Action::WriteRecorded);
    }
    if !editing {
        add(Action::EditHeaders);
    }
    if failure == Some(FailureKind::OutputTaken) {
        add(Action::OpenFolder);
    }
    Plan {
        actions,
        resume_after_headers: !mp4 && rerun == Action::Resume,
    }
}

#[cfg(test)]
mod tests {
    use super::Action::*;
    use super::*;

    fn actions(failure: Option<FailureKind>, live: bool, write_only: bool) -> Vec<Action> {
        stopped_plan(failure, live, write_only, false).actions
    }

    #[test]
    fn the_first_action_fits_the_failure() {
        // 暂停的点播：继续，旁边能改请求头
        assert_eq!(actions(None, false, false), [Resume, EditHeaders]);
        // 403：先改请求头，保存后重试；正在改时不再给
        let forbidden = Some(FailureKind::Status(403));
        assert_eq!(actions(forbidden, false, false), [EditHeaders]);
        let editing = stopped_plan(forbidden, false, false, true);
        assert_eq!(editing.actions, []);
        assert!(editing.resume_after_headers);
        // 放不进 MP4：先改存本地 HLS，此前写出、重试都会同样失败
        let mp4 = stopped_plan(Some(FailureKind::Mp4Unsupported), true, false, false);
        assert_eq!(mp4.actions, [SwitchToHls, EditHeaders]);
        assert!(!mp4.resume_after_headers);
        // 直播来源没了：先写出已录的，不给重试
        assert_eq!(
            actions(Some(FailureKind::Status(404)), true, false),
            [WriteRecorded, EditHeaders]
        );
        // 输出路径被占：重试，并能打开保存目录
        assert_eq!(
            actions(Some(FailureKind::OutputTaken), false, false),
            [Resume, EditHeaders, OpenFolder]
        );
        // 再试也一样的点播：只剩改请求头（放弃另给），改了请求头保存后重试
        let stuck = stopped_plan(Some(FailureKind::Unrecoverable), false, false, false);
        assert_eq!(stuck.actions, [EditHeaders]);
        assert!(stuck.resume_after_headers);
    }

    #[test]
    fn writing_only_reruns_as_writing_until_there_is_nothing_to_write() {
        // 只写出时盘满：腾出空间后再跑也只写出，改了请求头只保存
        let disk = stopped_plan(Some(FailureKind::Disk), true, true, false);
        assert_eq!(disk.actions, [WriteRecorded, EditHeaders]);
        assert!(!disk.resume_after_headers);
        // 没有可写出的：改为续录，不再给写出
        let nothing = stopped_plan(Some(FailureKind::NothingToWrite), true, true, false);
        assert_eq!(nothing.actions, [Resume, EditHeaders]);
        assert!(nothing.resume_after_headers);
    }
}
