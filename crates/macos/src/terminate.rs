//! 应用被要求退出（从程序坞或菜单退出、注销、关机，以及 GPUI 的 `App::quit`）时先问应用。
//!
//! 这些请求都经 `-[NSApplication terminate:]`，它先问应用代理的 `applicationShouldTerminate:`。GPUI 的应用代理
//! （gpui-pre-macos 0.3.12 注册的 `GPUIApplicationDelegate` 类）没有实现这个方法，系统就直接退出、只在退出前通知
//! 一次（`App::on_app_quit`），应用来不及问用户。这里给那个类补上这个方法。

use std::cell::RefCell;
use std::ffi::{CStr, CString};

use objc2::encode::Encode as _;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{MainThreadMarker, ffi, sel};

/// GPUI 注册的应用代理类名，见 gpui-pre-macos 0.3.12 的 `platform.rs`。
const DELEGATE_CLASS: &CStr = c"GPUIApplicationDelegate";

/// `NSApplicationTerminateReply` 的取值：取消这次退出、立即退出。
const TERMINATE_CANCEL: usize = 0;
const TERMINATE_NOW: usize = 1;

/// 登记失败的原因。都只在升级 GPUI 后可能出现。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("找不到 GPUI 的应用代理类 {}", DELEGATE_CLASS.to_string_lossy())]
    NoDelegateClass,
    #[error("GPUI 的应用代理已经实现了 applicationShouldTerminate:")]
    AlreadyImplemented,
}

type Ask = Box<dyn FnMut() -> bool>;

thread_local! {
    /// 登记的回调；只在界面线程上登记与调用。
    static ASK: RefCell<Option<Ask>> = const { RefCell::new(None) };
}

/// 应用被要求退出时先调 `ask`：返回 true 照常退出，false 取消这次退出（注销、关机随之取消）。`ask` 要退出时自己再
/// 调 `App::quit`，那次同样经过 `ask`。
///
/// 只能在界面线程上调，且一个进程只能登记一次（第二次报 [`Error::AlreadyImplemented`]）。`ask` 在系统的退出请求里
/// 同步调用：不能等待，要问用户就先返回 false、问完再退出。
pub fn ask_before_terminate(ask: impl FnMut() -> bool + 'static) -> Result<(), Error> {
    let _main = MainThreadMarker::new().expect("只能在界面线程上登记退出前的询问");
    let class = AnyClass::get(DELEGATE_CLASS).ok_or(Error::NoDelegateClass)?;
    if class
        .instance_method(sel!(applicationShouldTerminate:))
        .is_some()
    {
        return Err(Error::AlreadyImplemented);
    }
    // 返回 NSUInteger，参数为 self、_cmd、sender
    let types = CString::new(format!(
        "{}{}{}{}",
        usize::ENCODING,
        <*mut AnyObject>::ENCODING,
        Sel::ENCODING,
        <*mut AnyObject>::ENCODING,
    ))
    .expect("类型编码里没有 NUL");
    type Method = extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize;
    // SAFETY: 两边都是函数指针、大小相同；Objective-C 按上面的类型编码（self、_cmd、一个对象参数，返回 NSUInteger）
    // 调用它，与 `should_terminate` 的签名一致。
    let imp = unsafe { std::mem::transmute::<Method, Imp>(should_terminate) };
    // SAFETY: 类由 GPUI 在启动时注册、一直有效；只在界面线程上改；上面已确认它没有这个方法，加上不会替换别的实现；
    // 方法的类型编码与 `imp` 的签名一致。
    let added = unsafe {
        ffi::class_addMethod(
            std::ptr::from_ref(class).cast_mut(),
            sel!(applicationShouldTerminate:),
            imp,
            types.as_ptr(),
        )
    };
    if added == Bool::NO {
        return Err(Error::AlreadyImplemented);
    }
    // 退出请求只在界面线程的事件循环里到来，加上方法与登记回调之间不会被调
    ASK.with_borrow_mut(|slot| *slot = Some(Box::new(ask)));
    Ok(())
}

extern "C-unwind" fn should_terminate(_: *mut AnyObject, _: Sel, _: *mut AnyObject) -> usize {
    // 调回调时不持有借用；取不到即回调还没返回又来了一次退出请求，AppKit 不会这样重入
    let mut ask = ASK
        .with_borrow_mut(Option::take)
        .expect("退出前的询问已登记且没有重入");
    let quit = ask();
    ASK.with_borrow_mut(|slot| *slot = Some(ask));
    if quit {
        TERMINATE_NOW
    } else {
        TERMINATE_CANCEL
    }
}
