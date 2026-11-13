//! 左边的任务列表：每个任务一个进度小环；新建时最上面是一个虚线环的「新任务」。

use std::time::Instant;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, Div, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled as _, canvas, div, px,
};

use super::Shell;
use super::row::{Row, look};
use crate::new_task::Phase;
use crate::ui::{self, LIST_MIN_WIDTH, LIST_WIDTH};
use crate::{ring, theme};

/// 列表小环的边长与环宽。
const ORB: f32 = 52.;
const ORB_WIDTH: f32 = 5.;

impl Shell {
    pub(super) fn render_list(&self, now: Instant, cx: &mut Context<Self>) -> impl IntoElement {
        let new_task = self
            .new_task
            .as_ref()
            .map(|(view, _)| new_task_item(view.read(cx).phase()));
        let dim = self.new_task.is_some();
        let orbs: Vec<_> = self
            .rows
            .iter()
            .rev()
            .map(|row| self.render_orb(row, dim, now, cx))
            .collect();
        div()
            .id("tasks")
            .flex_basis(px(LIST_WIDTH))
            .flex_shrink(1.)
            .min_w(px(LIST_MIN_WIDTH))
            .flex()
            .flex_col()
            .gap(px(4.))
            .py(px(8.))
            .overflow_y_scroll()
            .children(new_task)
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
        let on = self.selected == Some(id) && self.new_task.is_none();
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
