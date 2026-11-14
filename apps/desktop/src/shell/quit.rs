//! 退出：关窗或系统要求退出（macOS 的程序坞、注销、关机）时问，等任务停下、补写状态后退出。

use gpui_kit::{Context, PromptButton, PromptLevel, Window};

#[cfg(target_os = "macos")]
use hs_m3u8_macos::Reply;

use super::Shell;
use crate::tasks::Quit;

impl Shell {
    /// 系统要退出应用（程序坞、注销、关机，或 [`crate::tasks::Tasks::leave`] 发出的）：已决定退出的照常退出；
    /// 没有要问、要等的照常退出；否则（要问，或已在等任务停下）把窗口带到前面，先不答复，问完、等完再答，注销、关机
    /// 随之接着进行或取消。
    #[cfg(target_os = "macos")]
    fn should_terminate(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Reply {
        if self.tasks.read(cx).quit() == Quit::Leaving || self.should_close(window, cx) {
            return Reply::Now;
        }
        cx.activate(true);
        window.activate_window();
        Reply::Later
    }

    /// 关窗前：有任务在运行时问一句。选了退出就暂停（或停止录制并写出）它们，等都停下再退出。
    /// 没有任务在运行时先把没写进存储的状态补写进去，补写不进时问要不要仍然退出。正在问时不再问。
    pub(super) fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let tasks = self.tasks.read(cx);
        if matches!(tasks.quit(), Quit::Asking { .. }) {
            return false;
        }
        let busy = tasks.busy();
        if busy == 0 {
            let Err(error) = tasks.flush() else {
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
            self.tasks.update(cx, |tasks, _| tasks.ask());
            let tasks = self.tasks.clone();
            cx.spawn(async move |_, cx| {
                let leave = matches!(answer.await, Ok(0));
                tasks.update(cx, |tasks, cx| {
                    if !leave {
                        tasks.stay();
                    } else if !tasks.reopened_while_asking() {
                        tasks.leave(cx);
                    }
                    cx.notify();
                });
            })
            .detach();
            return false;
        }
        if tasks.quitting() {
            // 已在等任务停下或已决定退出；右上角的提示里可以立即退出
            return false;
        }
        let recording = tasks.recording();
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
        self.tasks.update(cx, |tasks, _| tasks.ask());
        let tasks = self.tasks.clone();
        cx.spawn(async move |_, cx| {
            let choice = answer.await.ok().filter(|&choice| choice != cancel);
            tasks.update(cx, |tasks, cx| {
                match choice {
                    None => tasks.stay(),
                    Some(choice) => {
                        if !tasks.reopened_while_asking() {
                            tasks.quit_after_stopping(recording && choice == 1, cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        false
    }
}

/// 系统要退出应用（程序坞、注销、关机）时先问外壳（见 [`Shell::should_terminate`]）：换掉之前登记的。窗口已关（关窗
/// 即退出的那一次）时照常退出。启动时没装上（见 `main`）时为原因。
#[cfg(target_os = "macos")]
pub(super) fn ask_before_terminate(
    window: &mut Window,
    cx: &mut Context<Shell>,
) -> Result<(), hs_m3u8_macos::Error> {
    let (this, handle, mut app) = (
        cx.entity().downgrade(),
        window.window_handle(),
        cx.to_async(),
    );
    hs_m3u8_macos::ask_before_terminate(move || {
        handle
            .update(&mut app, |_, window, cx| {
                this.update(cx, |shell, cx| shell.should_terminate(window, cx))
            })
            .and_then(|reply| reply)
            .unwrap_or(Reply::Now)
    })
}
