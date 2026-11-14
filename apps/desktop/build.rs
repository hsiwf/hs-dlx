//! 编译到 Windows 时把 app.rc（应用图标）编进程序；别的平台什么也不做。Windows 上编不了资源（缺资源编译器）时构建
//! 失败，不发出没有图标的程序。

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/icons/icon.ico");
    if let Err(error) = embed_resource::compile("app.rc", embed_resource::NONE).manifest_required()
    {
        panic!("{error}");
    }
}
