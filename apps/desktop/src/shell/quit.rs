//! 退出时的询问：关主窗口（有任务在跑时可以在后台继续）或退出应用（托盘、macOS 的程序坞与菜单、注销、关机）时在
//! 窗口里问；要不要问由 [`Tasks::departure`] 定，选了退出就交给 Tasks 等任务停下、补写状态后退出。

use gpui_kit::{Context, PromptButton, PromptLevel, Window};

use super::Shell;
use crate::tasks::{Departure, Quit, Tasks};

/// 要走的是什么。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Leave {
    /// 关主窗口；`back` 为关掉后怎么回到窗口（写进提示），有路回来时才能在后台继续
    Window { back: Option<&'static str> },
    /// 退出应用
    App,
}

/// 有任务在跑时问出的选择。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Background,
    Pause,
    StopRecordings,
    Cancel,
}

/// 答完之后窗口这边还要做的。
enum After {
    Nothing,
    /// 在后台继续：关掉窗口
    Close,
    /// 问的期间又要退出应用：按退出应用再问
    AskQuit,
}

impl Shell {
    /// 要走（`leave`），`departure` 为调用方刚从 Tasks 取的。照办时为 true（调用方随即关窗或退出）；要问时在这个窗口里
    /// 问，为 false，问完由 Tasks 退出、留下或再问。已在问、在等任务停下时不再问（退出应用的请求记下，答完再问）。
    pub fn leave(
        &mut self,
        leave: Leave,
        departure: Departure,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match departure {
            Departure::Now => true,
            Departure::Underway => {
                if leave == Leave::App {
                    self.tasks
                        .update(cx, |tasks, _| tasks.request_quit_while_asking());
                }
                false
            }
            Departure::Unsaved(error) => {
                self.ask_unsaved(&error.to_string(), window, cx);
                false
            }
            Departure::Busy { busy, recording } => {
                self.ask_busy(leave, busy, recording, window, cx);
                false
            }
        }
    }

    /// 没有要等的任务、但有任务的状态写不进存储：问要不要仍然退出。
    fn ask_unsaved(&mut self, error: &str, window: &mut Window, cx: &mut Context<Self>) {
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
    }

    /// 还有 `busy` 个任务没停下（`recording` 为有在录的直播）：问在后台继续（关窗且有路回来时）、全部暂停或停止
    /// 录制并写出后退出，还是取消。
    fn ask_busy(
        &mut self,
        leave: Leave,
        busy: usize,
        recording: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 回车选第一项：能撤回的（在后台继续、暂停）在前；「取消」是取消按钮，Esc 即它
        let back = match leave {
            Leave::Window { back } => back,
            Leave::App => None,
        };
        let mut choices = Vec::new();
        if back.is_some() {
            choices.push((PromptButton::new("在后台继续"), Choice::Background));
        }
        choices.push((PromptButton::new("全部暂停并退出"), Choice::Pause));
        if recording {
            choices.push((
                PromptButton::new("停止录制、写出后退出"),
                Choice::StopRecordings,
            ));
        }
        choices.push((PromptButton::cancel("取消"), Choice::Cancel));
        let paused = if recording {
            "暂停的任务下次打开后可以继续；录制暂停期间直播的内容录不到。"
        } else {
            "暂停的任务下次打开后可以继续。"
        };
        let detail = match back {
            Some(back) => format!("在后台继续时任务接着跑，{back}；{paused}"),
            None => paused.to_owned(),
        };
        let (buttons, choices): (Vec<_>, Vec<_>) = choices.into_iter().unzip();
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("还有 {busy} 个任务没停下"),
            Some(&detail),
            &buttons,
            cx,
        );
        self.tasks.update(cx, |tasks, _| tasks.ask());
        let tasks = self.tasks.clone();
        cx.spawn_in(window, async move |this, cx| {
            // 提示框没答（窗口没了）算取消
            let choice = answer
                .await
                .ok()
                .and_then(|index| choices.get(index).copied())
                .unwrap_or(Choice::Cancel);
            match tasks.update(cx, |tasks, cx| answered(tasks, choice, cx)) {
                After::Nothing => {}
                // 窗口已不在时没有要关的
                After::Close => {
                    let _closed = this.update_in(cx, |_, window, _| window.remove_window());
                }
                After::AskQuit => {
                    let asked = this.update_in(cx, |this, window, cx| {
                        let departure = this.tasks.read(cx).departure();
                        if this.leave(Leave::App, departure, window, cx) {
                            this.tasks.update(cx, |tasks, cx| tasks.leave(cx));
                        }
                    });
                    // 窗口已不在、问不了：不退出，在等的系统退出请求随之取消
                    if asked.is_err() {
                        tasks.update(cx, |tasks, _| tasks.stay());
                    }
                }
            }
        })
        .detach();
    }
}

/// 有任务在跑时问出了 `choice`：照办，返回窗口这边还要做的。提示框开着期间再次打开过应用的，选了退出也不退出、在后台
/// 继续也不关窗；期间又要退出应用的（问的是关窗），在后台继续与取消都改为按退出应用再问。
fn answered(tasks: &mut Tasks, choice: Choice, cx: &mut Context<Tasks>) -> After {
    let (reopened, quit_requested) = match tasks.quit() {
        Quit::Asking {
            reopened,
            quit_requested,
        } => (reopened, quit_requested),
        Quit::Staying | Quit::Waiting | Quit::Leaving => (false, false),
    };
    let after = match choice {
        Choice::Pause | Choice::StopRecordings => {
            if !tasks.reopened_while_asking() {
                tasks.quit_after_stopping(choice == Choice::StopRecordings, cx);
            }
            After::Nothing
        }
        Choice::Background | Choice::Cancel if quit_requested && !reopened => {
            tasks.asked();
            After::AskQuit
        }
        Choice::Background if !reopened => {
            tasks.stay();
            After::Close
        }
        Choice::Background | Choice::Cancel => {
            tasks.stay();
            After::Nothing
        }
    };
    cx.notify();
    after
}
