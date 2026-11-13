//! 给等待加时限：GPUI 的执行器没有现成的「先完成的那个」。

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

/// 等 `work` 与 `deadline` 先完成的那个：`work` 先完成时为它的结果，`deadline` 先到时为 None。`work` 由调用方钉住、
/// 传引用：超时后它照旧可以接着等。
pub async fn within<T>(
    mut work: Pin<&mut impl Future<Output = T>>,
    deadline: impl Future<Output = ()>,
) -> Option<T> {
    let mut deadline = std::pin::pin!(deadline);
    std::future::poll_fn(|context| {
        if let Poll::Ready(value) = work.as_mut().poll(context) {
            return Poll::Ready(Some(value));
        }
        deadline.as_mut().poll(context).map(|()| None)
    })
    .await
}
