//! 退出：关窗时问、等任务停下、补写状态后退出。

use gpui_kit::{Context, PromptButton, PromptLevel, Window};
use hs_m3u8_app::Status;

use super::row::Row;
use super::{Quit, Shell};

impl Shell {
    /// 有任务在运行、继续、放弃或新建：退出要等它们。
    pub(super) fn busy(&self) -> usize {
        self.creating
            + self
                .rows
                .iter()
                .filter(|r| r.task.status == Status::Running || !r.idle())
                .count()
    }

    /// 关窗前：有任务在运行时问一句。选了退出就暂停（或停止录制并写出）它们，等都停下再退出。
    /// 没有任务在运行时先把没写进存储的状态补写进去，补写不进时问要不要仍然退出。
    pub(super) fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let busy = self.busy();
        if busy == 0 {
            let Err(error) = self.service.flush() else {
                return true;
            };
            let answer = window.prompt(
                PromptLevel::Warning,
                "有任务的状态没能写进存储",
                Some(&format!(
                    "现在退出，这些任务下次打开时显示为暂停。原因：{error}"
                )),
                &[PromptButton::new("仍然退出"), PromptButton::cancel("取消")],
                cx,
            );
            cx.spawn(async move |this, cx| {
                if let Ok(0) = answer.await {
                    // 提示框开着期间视图已不在时，应用已在退出
                    let _closed = this.update(cx, |this, cx| this.leave(cx));
                }
            })
            .detach();
            return false;
        }
        if self.quitting() {
            // 已在等任务停下或已决定退出；右上角的提示里可以立即退出
            return false;
        }
        let recording = self.rows.iter().any(Row::stoppable);
        // 回车选第一项，放能撤回的「暂停」；「取消」是取消按钮，Esc 即它
        let mut answers = vec![PromptButton::new("全部暂停并退出")];
        if recording {
            answers.push(PromptButton::new("停止录制、写出后退出"));
        }
        answers.push(PromptButton::cancel("取消"));
        let cancel = answers.len() - 1;
        let detail = if recording {
            "暂停的任务下次打开后可以继续；录制暂停期间直播的内容录不到。"
        } else {
            "暂停的任务下次打开后可以继续。"
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("还有 {busy} 个任务没停下"),
            Some(detail),
            &answers,
            cx,
        );
        cx.spawn(async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice == cancel {
                return;
            }
            let stop_recordings = recording && choice == 1;
            // 提示框开着期间视图已不在时没有要做的
            let _closed = this.update(cx, |this, cx| {
                this.quit = Quit::Waiting;
                for row in &this.rows {
                    if row.task.status != Status::Running {
                        continue;
                    }
                    if stop_recordings && row.stoppable() {
                        this.service.stop(row.task.id);
                    } else {
                        this.service.pause(row.task.id);
                    }
                }
                this.quit_if_idle(cx);
                cx.notify();
            });
        })
        .detach();
        false
    }

    /// 在退出（等任务停下或已决定退出）：不开始新的运行。
    pub(super) fn quitting(&self) -> bool {
        self.quit != Quit::Staying
    }

    /// 不退出了：失败与没删干净的东西留给用户看；`why` 写进提示。已决定退出的照常退出。
    pub(super) fn cancel_quit(&mut self, why: String) {
        if self.quit == Quit::Waiting {
            self.quit = Quit::Staying;
        }
        self.notify(why);
    }

    /// 退出，不再撤回。
    pub(super) fn leave(&mut self, cx: &mut Context<Self>) {
        self.quit = Quit::Leaving;
        cx.quit();
    }

    /// 选了退出、任务都停下（继续、放弃与新建也做完）时，把没写进存储的状态补写进去后退出；补写不进时不退出。
    pub(super) fn quit_if_idle(&mut self, cx: &mut Context<Self>) {
        if self.quit != Quit::Waiting || self.busy() > 0 {
            return;
        }
        match self.service.flush() {
            Ok(()) => self.leave(cx),
            Err(error) => self.cancel_quit(format!(
                "有任务的状态没能写进存储（{error}），没有退出；再关窗口时可以选择仍然退出"
            )),
        }
    }
}
