//! 退出时的询问：关主窗口（有任务在跑时可以在后台继续）或退出应用（托盘、macOS 的程序坞与菜单、注销、关机）时问，
//! 选了退出就交给 [`crate::tasks::Tasks`] 等任务停下、补写状态后退出。

use gpui_kit::{Context, PromptButton, PromptLevel, Window};

use super::Shell;
use crate::tasks::{Quit, Tasks};

/// 要走的是什么。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Leave {
    /// 关主窗口：有任务在跑时可以在后台继续
    Window,
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

/// 在后台继续时怎么回到窗口。
#[cfg(target_os = "macos")]
const BACK: &str = "点菜单栏或程序坞里的图标回到窗口";
#[cfg(target_os = "windows")]
const BACK: &str = "点通知区域的图标或再次打开应用回到窗口";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const BACK: &str = "点托盘图标或再次打开应用回到窗口";

impl Shell {
    /// 走之前：已决定退出的照常走；有任务在运行时问一句（关窗时第一项是在后台继续：窗口关掉、任务照跑），选了退出就
    /// 暂停（或停止录制并写出）它们，等都停下再退出。没有任务在运行时先把没写进存储的状态补写进去，补写不进时问要不要
    /// 仍然退出。正在问、在等任务停下时不再问。返回 true 时调用方立即照办（关窗即退出）。
    pub fn should_leave(
        &mut self,
        leave: Leave,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let tasks = self.tasks.read(cx);
        match tasks.quit() {
            Quit::Leaving => return true,
            // 已在问；或已在等任务停下，右上角的提示里可以立即退出
            Quit::Asking { .. } | Quit::Waiting => return false,
            Quit::Staying => {}
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
        let recording = tasks.recording();
        // 回车选第一项：能撤回的（在后台继续、暂停）在前；「取消」是取消按钮，Esc 即它
        let mut choices = Vec::new();
        if leave == Leave::Window {
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
        let detail = match leave {
            Leave::Window => format!("在后台继续时任务接着跑，{BACK}；{paused}"),
            Leave::App => paused.to_owned(),
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
            let close = tasks.update(cx, |tasks, cx| answered(tasks, choice, cx));
            if close {
                // 窗口已不在时没有要关的
                let _closed = this.update_in(cx, |_, window, _| window.remove_window());
            }
        })
        .detach();
        false
    }
}

/// 有任务在跑时问出了 `choice`：照办，要在后台继续时为 true（由调用方关窗）。提示框开着期间再次打开过应用的，在后台
/// 继续即留着窗口，选了退出也不退出。
fn answered(tasks: &mut Tasks, choice: Choice, cx: &mut Context<Tasks>) -> bool {
    let close = match choice {
        Choice::Cancel => {
            tasks.stay();
            false
        }
        Choice::Background => {
            let reopened = tasks.quit() == (Quit::Asking { reopened: true });
            tasks.stay();
            !reopened
        }
        Choice::Pause | Choice::StopRecordings => {
            if !tasks.reopened_while_asking() {
                tasks.quit_after_stopping(choice == Choice::StopRecordings, cx);
            }
            false
        }
    };
    cx.notify();
    close
}
