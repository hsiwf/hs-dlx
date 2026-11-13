//! 左边的任务列表：每个任务一个进度小环；新建时最上面是一个虚线环的「新任务」，嗅探页开着（没显示）时有一项回到
//! 网页。

use std::time::Instant;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, Div, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled as _, canvas, div, px,
};

use super::row::{Row, look};
use super::{Page, Shell};
use crate::new_task::Phase;
use crate::{ring, theme, ui};

/// 任务列表的宽度。
const WIDTH: f32 = 300.;
/// 窗口窄时任务列表最多收到的宽度。
pub(super) const MIN_WIDTH: f32 = 240.;

/// 列表小环的边长与环宽。
const ORB: f32 = 52.;
const ORB_WIDTH: f32 = 5.;

impl Shell {
    pub(super) fn render_list(&self, now: Instant, cx: &mut Context<Self>) -> impl IntoElement {
        let new_task = self
            .new_task
            .as_ref()
            .map(|(view, _)| new_task_item(view.read(cx).phase()));
        let sniff = self.sniff.as_ref().map(|(view, _)| {
            let title = view.read(cx).page_title();
            sniff_item(title, cx)
        });
        let dim = matches!(self.page, Page::NewTask { .. });
        let orbs: Vec<_> = self
            .rows
            .iter()
            .rev()
            .map(|row| self.render_orb(row, dim, now, cx))
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

    fn render_orb(
        &self,
        row: &Row,
        dim: bool,
        now: Instant,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = row.task.id;
        let look = look(row, now);
        let on = self.selected == Some(id) && self.page == Page::Task;
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

/// 回到嗅探页的一项：两个同心实线环（与嗅探页里流前面的一样），写网页的标题。
fn sniff_item(title: String, cx: &mut Context<Shell>) -> Stateful<Div> {
    let ring = |size: f32, width: f32, color| {
        div()
            .absolute()
            .left(px((ORB - size) / 2.))
            .top(px((ORB - size) / 2.))
            .size(px(size))
            .rounded_full()
            .border(px(width))
            .border_color(color)
    };
    ui::list_item("sniff", false)
        .cursor_pointer()
        .on_click(cx.listener(|this, _, _, cx| this.show(Page::Sniff, cx)))
        .child(
            div()
                .relative()
                .size(px(ORB))
                .flex_none()
                .child(ring(ORB, ORB_WIDTH, theme::cyan()))
                .child(ring(ORB - 20., 2., theme::cyan_dim())),
        )
        .child(two_lines(
            if title.is_empty() {
                "网页".to_owned()
            } else {
                title
            },
            "找视频流 · 点这里回去".to_owned(),
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
