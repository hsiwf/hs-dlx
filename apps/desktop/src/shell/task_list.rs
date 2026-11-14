//! 左边的任务列表：每个任务一个进度小环；新建时最上面是一个虚线环的「新任务」，在网页里找视频流时有网页那一项。

use std::time::Instant;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, Div, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled as _, canvas, div, px,
};

use super::{Page, Shell};
use crate::new_task::Phase;
use crate::tasks::{Row, Tasks, look};
use crate::{ring, theme, ui};

/// 任务列表的宽度。
const WIDTH: f32 = 300.;
/// 窗口窄时任务列表最多收到的宽度。
pub(super) const MIN_WIDTH: f32 = 240.;

/// 列表小环的边长与环宽。
const ORB: f32 = 52.;
const ORB_WIDTH: f32 = 5.;

impl Shell {
    pub(super) fn render_list(
        &self,
        tasks: &Tasks,
        now: Instant,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let new_task = self
            .new_task()
            .map(|view| new_task_item(view.read(cx).phase()));
        let dim = self.new_task().is_some();
        let sniff = self.sniff.as_ref().map(|(view, _)| {
            let on = matches!(&self.page, Page::Sniff(shown) if shown == view);
            let read = view.read(cx);
            let (title, found) = (read.page_title(), read.found_count());
            sniff_item(title, found, on, dim, cx)
        });
        let selected = self.selected_row(tasks).map(|row| row.task.id);
        let orbs: Vec<_> = tasks
            .rows()
            .iter()
            .rev()
            .map(|row| self.render_orb(row, selected, dim, now, cx))
            .collect();
        div()
            .id("tasks")
            .flex_basis(px(WIDTH))
            .flex_shrink(1.)
            .min_w(px(MIN_WIDTH))
            .flex()
            .flex_col()
            .gap(px(4.))
            .py(px(8.))
            .overflow_y_scroll()
            .children(new_task)
            .children(sniff)
            .children(orbs)
    }

    /// `selected` 为选中的任务（见 [`Shell::selected_row`]）。
    fn render_orb(
        &self,
        row: &Row,
        selected: Option<hs_m3u8_app::TaskId>,
        dim: bool,
        now: Instant,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let id = row.task.id;
        let look = look(row, now);
        let on = selected == Some(id) && matches!(self.page, Page::Task);
        let (fraction, color) = (look.fraction, look.orb);
        ui::list_item(("task", id.0 as u64), on)
            .when(dim, |s| s.opacity(0.55))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.select(id, cx)))
            .child(
                div()
                    .relative()
                    .size(px(ORB))
                    .flex_none()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                ring::paint_arc(
                                    bounds,
                                    ORB_WIDTH,
                                    fraction,
                                    color,
                                    theme::line(),
                                    window,
                                );
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    // 新建时其余任务只留环，中间空着，视线落在新建上
                    .when(!dim, |orb| {
                        orb.child(ui::num(
                            div()
                                .absolute()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(11.))
                                .text_color(theme::soft())
                                .child(look.short),
                        ))
                    }),
            )
            .child(two_lines(
                row.task.spec.title.clone(),
                format!("{} · {}", look.state, look.line),
            ))
    }
}

/// 列表项右边的两行：标题与状态。
fn two_lines(title: String, state: String) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .min_w_0()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(title),
        )
        .child(ui::num(
            div()
                .text_size(px(12.))
                .text_color(theme::muted())
                .truncate()
                .child(state),
        ))
}

/// 网页里的视频流那一项：两个同心实线环（与列表里每条流前面的一样），写网页的标题与找到了几条；`on` 为正显示着。
fn sniff_item(
    title: Option<String>,
    found: usize,
    on: bool,
    dim: bool,
    cx: &Context<Shell>,
) -> Stateful<Div> {
    ui::list_item("sniff", on)
        .when(dim, |item| item.opacity(0.55))
        .cursor_pointer()
        .on_click(cx.listener(|this, _, _, cx| this.show_sniff(cx)))
        .child(ui::rings(ORB, ORB_WIDTH, 20.))
        .child(two_lines(
            title.unwrap_or_else(|| "网页里的视频流".to_owned()),
            if found == 0 {
                "还没找到视频流".to_owned()
            } else {
                format!("找到 {found} 条视频流")
            },
        ))
}

fn new_task_item(phase: Phase) -> impl IntoElement {
    ui::list_item("new-task", true)
        .child(
            div()
                .size(px(ORB))
                .flex_none()
                .rounded_full()
                .border_2()
                .border_dashed()
                .border_color(theme::cyan()),
        )
        .child(two_lines("新任务".to_owned(), phase.label().to_owned()))
}
