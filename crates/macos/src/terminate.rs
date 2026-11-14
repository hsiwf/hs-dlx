//! 应用被要求退出（从程序坞或菜单退出、注销、关机，以及 GPUI 的 `App::quit`）时先问应用。
//!
//! 这些请求都经 `-[NSApplication terminate:]`，它先问应用代理的 `applicationShouldTerminate:`。GPUI 的应用代理
//! （gpui-pre-macos 0.3.12 注册的 `GPUIApplicationDelegate` 类）没有实现这个方法，系统就直接退出、只在退出前通知
//! 一次（`App::on_app_quit`），应用来不及问用户。这里给那个类补上这个方法：[`install`] 在 GPUI 设好应用代理之前加上
//! （不依赖 AppKit 是否缓存代理能响应哪些方法），回调由 [`ask_before_terminate`] 之后再设。
//!
//! 要问用户时答 [`Reply::Later`]：系统等应用答复（注销、关机暂停），应用问完调 [`reply_to_terminate`]，答退出时
//! 注销、关机接着进行，答不退出时随之取消。

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString};

use objc2::encode::Encode as _;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{MainThreadMarker, class, ffi, msg_send, sel};

/// GPUI 注册的应用代理类名，见 gpui-pre-macos 0.3.12 的 `platform.rs`（进程加载时注册）。
const DELEGATE_CLASS: &CStr = c"GPUIApplicationDelegate";

/// `NSApplicationTerminateReply` 的取值。
const TERMINATE_CANCEL: usize = 0;
const TERMINATE_NOW: usize = 1;
const TERMINATE_LATER: usize = 2;

/// 装不上、登记不了的原因。前两种只在升级 GPUI 后可能出现。
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("找不到 GPUI 的应用代理类 {}", DELEGATE_CLASS.to_string_lossy())]
    NoDelegateClass,
    #[error("GPUI 的应用代理自己实现了 applicationShouldTerminate:")]
    AlreadyImplemented,
    /// 进程启动时没有调 [`install`]（调用方的缺陷）
    #[error("没有在启动时装上退出前的询问")]
    NotInstalled,
}

/// 系统要求退出时的答复。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Now,
    Cancel,
    /// 先不答复，问完再调 [`reply_to_terminate`]
    Later,
}

type Ask = Box<dyn FnMut() -> Reply>;

thread_local! {
    /// [`install`] 的结果；没调过时为 None。只在界面线程上读写。
    static INSTALLED: RefCell<Option<Result<(), Error>>> = const { RefCell::new(None) };
    /// 登记的回调；没登记时照常退出。
    static ASK: RefCell<Option<Ask>> = const { RefCell::new(None) };
    /// 答过 [`Reply::Later`]、还没答复。
    static PENDING: Cell<bool> = const { Cell::new(false) };
}

/// 给 GPUI 的应用代理类加上 `applicationShouldTerminate:`。在界面线程上、GPUI 开始运行之前调一次；没登记回调时照常
/// 退出。结果记下，[`ask_before_terminate`] 时报出。
pub fn install() {
    let _main = MainThreadMarker::new().expect("只能在界面线程上装退出前的询问");
    let result = add_method();
    INSTALLED.with_borrow_mut(|installed| *installed = Some(result));
}

fn add_method() -> Result<(), Error> {
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
    // SAFETY: 类由 GPUI 在进程加载时注册、一直有效；只在界面线程上改；上面已确认它没有这个方法，加上不会替换别的
    // 实现；方法的类型编码与 `imp` 的签名一致。
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
    Ok(())
}

/// 应用被要求退出时先调 `ask`（换掉之前登记的）。`ask` 在系统的退出请求里同步调用：不能等待，要问用户就答
/// [`Reply::Later`]、问完调 [`reply_to_terminate`]。只能在界面线程上调；启动时没装上（[`install`]）时为原因。
pub fn ask_before_terminate(ask: impl FnMut() -> Reply + 'static) -> Result<(), Error> {
    let _main = MainThreadMarker::new().expect("只能在界面线程上登记退出前的询问");
    INSTALLED.with_borrow(|installed| installed.clone().unwrap_or(Err(Error::NotInstalled)))?;
    ASK.with_borrow_mut(|slot| *slot = Some(Box::new(ask)));
    Ok(())
}

/// 答复之前答了 [`Reply::Later`] 的退出请求：`quit` 为照常退出，否则取消（注销、关机随之取消）。没有在等答复的请求
/// 时什么也不做，返回 false。只能在界面线程上调。
pub fn reply_to_terminate(quit: bool) -> bool {
    let _main = MainThreadMarker::new().expect("只能在界面线程上答复退出请求");
    if !PENDING.replace(false) {
        return false;
    }
    // SAFETY: 在界面线程上取共享的 NSApplication；应用已在运行（有在等答复的退出请求），它一定已建出。
    let app: *mut AnyObject = unsafe { msg_send![class!(NSApplication), sharedApplication] };
    // SAFETY: `app` 是有效的 NSApplication，此刻在等这个答复（之前答了 NSTerminateLater）；参数为 BOOL。
    let () = unsafe { msg_send![app, replyToApplicationShouldTerminate: Bool::new(quit)] };
    true
}

extern "C-unwind" fn should_terminate(_: *mut AnyObject, _: Sel, _: *mut AnyObject) -> usize {
    // 没登记回调时照常退出。调回调时不持有借用：回调里不会再来退出请求（AppKit 不同步重入）
    let Some(mut ask) = ASK.with_borrow_mut(Option::take) else {
        return TERMINATE_NOW;
    };
    let reply = ask();
    ASK.with_borrow_mut(|slot| {
        // 回调期间登记了新的回调时留新的
        slot.get_or_insert(ask);
    });
    match reply {
        Reply::Now => TERMINATE_NOW,
        Reply::Cancel => TERMINATE_CANCEL,
        Reply::Later => {
            PENDING.set(true);
            TERMINATE_LATER
        }
    }
}
