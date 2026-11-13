//! 对一个任务的操作：读任务目录、继续、放弃、从列表移除，与任务停下后的查看。

use std::path::PathBuf;
use std::time::Instant;

use gpui_kit::Context;
use hs_m3u8_app::{Status, TaskId};

use super::Shell;
use super::row::{Disk, Present};
use crate::rate::Rate;
use crate::signals::Level;

impl Shell {
    /// 任务不在运行时：暂停与失败的读一次任务目录，完成的查一次输出在不在；放弃中的什么也不做（目录正在删）。
    pub(super) fn after_stop(&mut self, id: TaskId, cx: &mut Context<Self>) {
        let Some(row) = self.row(id) else {
            return;
        };
        if row.discarding {
            return;
        }
        match &row.task.status {
            Status::Paused | Status::Failed(_) => self.inspect(id, false, cx),
            Status::Done => {
                let target = row.task.spec.output.options().target;
                let (mp4, hls) = (
                    target.mp4().map(PathBuf::from),
                    target.hls().map(PathBuf::from),
                );
                let checking = cx.background_executor().spawn(async move {
                    let mp4 = mp4.filter(|p| p.is_file());
                    let shown = mp4.clone().or_else(|| hls.filter(|p| p.exists()));
                    Present { mp4, shown }
                });
                cx.spawn(async move |this, cx| {
                    let present = checking.await;
                    // 查的期间窗口关掉时不再需要结果
                    let _closed = this.update(cx, |this, cx| {
                        if let Some(row) = this.row_mut(id) {
                            row.present = Some(present);
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            Status::Running => {}
        }
    }

    /// 读任务目录里已完成的内容；`then_resume` 时读完即继续，读到的片数作本次运行的起点（读不出时不估剩余时间）。
    /// 读完（及继续）后，选了退出、都已停下的即退出。
    pub(super) fn inspect(&mut self, id: TaskId, then_resume: bool, cx: &mut Context<Self>) {
        let Some(reading) = self.row(id).map(|row| row.reading) else {
            return;
        };
        let handle = self.service.inspect(id);
        cx.spawn(async move |this, cx| {
            let result = handle.await;
            // 等待期间窗口关掉时不再需要结果
            let _closed = this.update(cx, |this, cx| {
                let disk = match result {
                    Ok(Ok(saved)) => Disk::Read(saved),
                    Ok(Err(error)) => Disk::Unreadable(error.to_string()),
                    Err(join) => Disk::Unreadable(format!("读的过程中出错：{join}")),
                };
                // 读的期间任务又运行过、停下了的，这次读到的已过时，停下时另读了
                if let Some(row) = this.row_mut(id)
                    && row.reading == reading
                {
                    row.disk = disk;
                }
                if then_resume {
                    this.resume_now(id);
                }
                this.quit_if_idle(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 继续：先读出任务目录里已有的，作本次运行的起点，再开始；读的期间这一行不能再操作。
    pub(super) fn resume(&mut self, id: TaskId, cx: &mut Context<Self>) {
        if self.quitting {
            self.notify("正在退出，不再继续任务".to_owned());
            cx.notify();
            return;
        }
        let Some(row) = self.row_mut(id) else {
            return;
        };
        if !row.idle() {
            return;
        }
        row.resuming = true;
        self.inspect(id, true, cx);
        cx.notify();
    }

    pub(super) fn resume_now(&mut self, id: TaskId) {
        let title = self.title(id);
        let quitting = self.quitting;
        let Some(row) = self.row_mut(id) else {
            return;
        };
        row.resuming = false;
        if row.discarding {
            return;
        }
        if quitting {
            self.notify(format!("正在退出，「{title}」没有继续"));
            return;
        }
        let baseline = match &row.disk {
            Disk::Read(saved) => Some(saved.as_ref().map_or(0, |s| s.segments)),
            Disk::Unread | Disk::Unreadable(_) => None,
        };
        match self.service.resume(id) {
            Ok(()) => {
                let row = self.row_mut(id).expect("上面刚找到这一行");
                row.task.status = Status::Running;
                row.progress = None;
                row.rate = Rate::new(Instant::now(), baseline);
                let text = match baseline {
                    Some(n) if n > 0 => format!("继续：任务目录里已有 {n} 片"),
                    _ => "继续".to_owned(),
                };
                row.signal(Level::Info, text);
            }
            Err(error) => self.notify(format!("继续「{title}」失败：{error}")),
        }
    }

    /// 放弃任务：要删已下载内容的，第一次点只是要求确认。
    pub(super) fn discard(&mut self, id: TaskId, confirmed: bool, cx: &mut Context<Self>) {
        if !confirmed {
            self.confirm_discard = Some(id);
            cx.notify();
            return;
        }
        self.confirm_discard = None;
        let title = self.title(id);
        if let Some(row) = self.row_mut(id) {
            row.discarding = true;
        }
        let handle = self.service.discard(id);
        cx.spawn(async move |this, cx| {
            let result = handle.await;
            // 等待期间窗口关掉时不再需要结果；放弃本身在服务里照常做完
            let _closed = this.update(cx, |this, cx| {
                let problems = match result {
                    Ok(Ok(leftovers)) => {
                        this.remove_row(id);
                        leftovers
                            .iter()
                            .map(|leftover| format!("放弃「{title}」时没删掉：{leftover}"))
                            .collect()
                    }
                    Ok(Err(error)) => vec![format!("放弃「{title}」失败：{error}")],
                    Err(join) => vec![format!("放弃「{title}」中途出错：{join}")],
                };
                if let Some(row) = this.row_mut(id) {
                    row.discarding = false;
                }
                if !problems.is_empty() && this.quitting {
                    this.cancel_quit("放弃没做干净，没有退出".to_owned());
                }
                for text in problems {
                    this.notify(text);
                }
                this.after_stop(id, cx);
                this.quit_if_idle(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 已完成的任务从列表移除：只删记录，输出归用户。
    pub(super) fn forget(&mut self, id: TaskId, cx: &mut Context<Self>) {
        match self.service.forget(id) {
            Ok(()) => self.remove_row(id),
            Err(error) => {
                let title = self.title(id);
                self.notify(format!("移除「{title}」失败：{error}"));
            }
        }
        cx.notify();
    }
}
