//! 对一个任务的操作：读任务目录、继续、改请求头、放弃、从列表移除，与任务停下后的查看。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::{AppContext as _, Context, Window};
use hs_m3u8_app::{Status, TaskId};

use super::Shell;
use super::plan::resume_label;
use super::row::{Disk, Present, Row};
use crate::headers::HeadersEditor;
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
            // 放弃没做完的任务目录可能已删了一部分，读出来的不说明什么
            Status::Running | Status::DiscardUnfinished => {}
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
        if self.quitting() {
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
        self.editing_headers = None;
        self.inspect(id, true, cx);
        cx.notify();
    }

    /// 不再续录，只把直播录制已录的写成输出；退出中不开始。
    pub(super) fn write_recorded(&mut self, id: TaskId, cx: &mut Context<Self>) {
        let title = self.title(id);
        if self.quitting() {
            self.notify(format!("正在退出，「{title}」没有写出"));
            cx.notify();
            return;
        }
        if !self.row(id).is_some_and(Row::idle) {
            return;
        }
        self.editing_headers = None;
        match self.service.write_recorded(id) {
            Ok(task) => {
                let row = self.row_mut(id).expect("上面刚找到这一行");
                // 核心报出进度之前，已录时长先按停下时知道的显示
                row.recorded_before_writing = row.recorded();
                row.task = task;
                row.progress = None;
                row.forget_disk();
                // 写出不联网，没有下载速度与剩余时间可估
                row.rate = Rate::new(Instant::now(), None);
                row.signal(Level::Info, "不再续录，写出已录的".to_owned());
            }
            Err(error) => self.notify(format!("写出「{title}」已录的失败：{error}")),
        }
        cx.notify();
    }

    /// 内容放不进 MP4 时改存本地 HLS 再跑：直播只写出已录的，点播接着下；退出中不改。
    pub(super) fn switch_to_hls(&mut self, id: TaskId, cx: &mut Context<Self>) {
        let title = self.title(id);
        if self.quitting() {
            self.notify(format!("正在退出，「{title}」没有改存本地 HLS"));
            cx.notify();
            return;
        }
        let Some(live) = self.row(id).filter(|row| row.idle()).map(Row::recording) else {
            return;
        };
        match self.service.switch_to_hls(id) {
            Ok(task) => {
                let row = self.row_mut(id).expect("上面刚找到这一行");
                row.set_output(task.spec.output);
            }
            Err(error) => {
                self.notify(format!("「{title}」没能改存本地 HLS：{error}"));
                cx.notify();
                return;
            }
        }
        if live {
            self.write_recorded(id, cx);
        } else {
            self.resume(id, cx);
        }
    }

    pub(super) fn resume_now(&mut self, id: TaskId) {
        let title = self.title(id);
        let quitting = self.quitting();
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
        let (baseline, start) = match &row.disk {
            Disk::Read(saved) => (
                Some(saved.as_ref().map_or(0, |s| s.segments)),
                Some(saved.as_ref().map_or(Duration::ZERO, |s| s.duration)),
            ),
            Disk::Unread | Disk::Unreadable(_) => (None, None),
        };
        let label = resume_label(
            matches!(row.task.status, Status::Failed(_)),
            row.task.write_only,
        );
        match self.service.resume(id) {
            Ok(task) => {
                let row = self.row_mut(id).expect("上面刚找到这一行");
                row.task = task;
                row.progress = None;
                row.rate = Rate::new(Instant::now(), baseline);
                if let Some(start) = start {
                    row.timeline.begin(start);
                }
                let text = match baseline {
                    Some(n) if n > 0 => format!("{label}：任务目录里已有 {n} 片"),
                    _ => label.to_owned(),
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
                // 没删干净的、超时的，服务里已记为放弃没做完：照服务里的状态改这一行（行还在时）
                if this.row(id).is_some() {
                    match this.service.task(id) {
                        Ok(task) => {
                            let row = this.row_mut(id).expect("上面刚找到这一行");
                            row.task.status = task.status;
                        }
                        Err(error) => this.notify(format!(
                            "读不出「{title}」放弃后的状态（{error}），界面上的状态可能不对"
                        )),
                    }
                }
                if let Some(row) = this.row_mut(id) {
                    row.discarding = false;
                }
                if !problems.is_empty() && this.quitting() {
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

    /// 从列表移除已完成的任务（输出归用户）与没有自己的任务目录的任务：只删记录。
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

    /// 打开停着的任务 `id` 的请求头编辑框，填上它现在的请求头。
    pub(super) fn edit_headers(&mut self, id: TaskId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(headers) = self.row(id).map(|row| row.task.spec.headers.clone()) else {
            return;
        };
        let editor = cx.new(|cx| HeadersEditor::new(headers, window, cx));
        editor.update(cx, |editor, cx| editor.focus(window, cx));
        self.editing_headers = Some((id, editor));
        cx.notify();
    }

    /// 存下编辑框里的请求头，`resume` 时接着续传续录；编辑框里还没按回车的一条加不进去时什么也不做（原因显示在编辑
    /// 框里）。
    pub(super) fn save_headers(
        &mut self,
        resume: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((id, editor)) = self.editing_headers.clone() else {
            return;
        };
        let Some(headers) = editor.update(cx, |editor, cx| editor.confirm(window, cx)) else {
            return;
        };
        match self.service.set_headers(id, headers.clone()) {
            Ok(()) => {
                self.editing_headers = None;
                if let Some(row) = self.row_mut(id) {
                    row.task.spec.headers = headers;
                }
                if resume {
                    self.resume(id, cx);
                }
            }
            Err(error) => {
                let title = self.title(id);
                self.notify(format!("改「{title}」的请求头失败：{error}"));
            }
        }
        cx.notify();
    }
}
