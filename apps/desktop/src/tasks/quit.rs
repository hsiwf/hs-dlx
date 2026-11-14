//! 退出进行到哪一步：问（窗口里的提示框）之后等任务停下、补写状态再退出；期间出了要用户看的事就不退出。

use gpui_kit::Context;
use hs_m3u8_app::Status;

use super::Tasks;
use super::row::Row;

/// 退出进行到哪一步。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Quit {
    Staying,
    /// 正在问要不要退出（提示框开着）：再来的退出请求不再问；`reopened` 为期间再次打开过应用（接手了转交），选了
    /// 退出也不退出
    Asking {
        reopened: bool,
    },
    /// 用户选了退出：等运行中的任务都停下就退出；期间不开始新的运行，出了要用户看的事就不退出
    Waiting,
    /// 退出已不可撤回（已向系统发出退出）：不再接手转交来的链接
    Leaving,
}

impl Tasks {
    pub fn quit(&self) -> Quit {
        self.quit
    }

    /// 有任务在运行、继续、放弃或新建：退出要等它们。
    pub fn busy(&self) -> usize {
        self.creating
            + self
                .rows
                .iter()
                .filter(|r| r.task.status == Status::Running || !r.idle())
                .count()
    }

    /// 有在运行的直播录制（能停止并写出的）。
    pub fn recording(&self) -> bool {
        self.rows.iter().any(Row::stoppable)
    }

    /// 在退出（等任务停下或已决定退出）：不开始新的运行。
    pub fn quitting(&self) -> bool {
        matches!(self.quit, Quit::Waiting | Quit::Leaving)
    }

    /// 退出不用问：已决定退出，或没在问、没在等、没有要等的任务且状态都已写进存储（补写在这里做）。
    pub fn quits_without_asking(&self) -> bool {
        match self.quit {
            Quit::Leaving => true,
            Quit::Asking { .. } | Quit::Waiting => false,
            Quit::Staying => self.busy() == 0 && self.service.flush().is_ok(),
        }
    }

    /// 把没写进存储的任务状态补写进去。
    pub fn flush(&self) -> Result<(), hs_m3u8_app::Error> {
        self.service.flush()
    }

    /// 开始问要不要退出（窗口里的提示框开着）。
    pub fn ask(&mut self) {
        self.quit = Quit::Asking { reopened: false };
    }

    /// 选了退出：暂停运行中的任务（`stop_recordings` 时直播录制改为停止并写出），等都停下（继续、放弃与新建也做完）
    /// 就退出。
    pub fn quit_after_stopping(&mut self, stop_recordings: bool, cx: &mut Context<Self>) {
        self.quit = Quit::Waiting;
        for row in &self.rows {
            if row.task.status != Status::Running {
                continue;
            }
            if stop_recordings && row.stoppable() {
                self.service.stop(row.task.id);
            } else {
                self.service.pause(row.task.id);
            }
        }
        self.quit_if_idle(cx);
        cx.notify();
    }

    /// 不退出了：失败与没删干净的东西留给用户看；`why` 写进提示。已决定退出的照常退出。
    pub fn cancel_quit(&mut self, why: String) {
        if self.quit == Quit::Waiting {
            self.stay();
        }
        self.add_notice(why);
    }

    /// 选了退出时，提示框开着期间再次打开过应用（见 [`Tasks::receive`]）：不退出了，提示原因。
    pub fn reopened_while_asking(&mut self) -> bool {
        let reopened = matches!(self.quit, Quit::Asking { reopened: true });
        if reopened {
            self.stay();
            self.add_notice("提示框开着时再次打开了应用，没有退出".to_owned());
        }
        reopened
    }

    /// 不退出：在等答复的系统退出请求随之取消（注销、关机不再进行）。
    pub fn stay(&mut self) {
        self.quit = Quit::Staying;
        #[cfg(target_os = "macos")]
        hs_m3u8_macos::reply_to_terminate(false);
    }

    /// 退出，不再撤回：有在等答复的系统退出请求时答复它（注销、关机接着进行），否则发出退出。
    pub fn leave(&mut self, cx: &mut Context<Self>) {
        self.quit = Quit::Leaving;
        #[cfg(target_os = "macos")]
        if hs_m3u8_macos::reply_to_terminate(true) {
            return;
        }
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
