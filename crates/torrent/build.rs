//! 编译 C++ 的这一层（src/shim.cpp）并链接静态的 libtorrent、OpenSSL。libtorrent 装在环境变量 `LIBTORRENT_DIR` 指的
//! 目录（由 third_party/libtorrent/build.sh 生成，.cargo/config.toml 里设了默认位置）。
//!
//! libtorrent 的公开宏（ABI 版本、关掉的功能、用 OpenSSL）与要链接的系统库只从它装好的 CMake 配置里读，不在这里另写
//! 一份：两边不一致时 C++ 的类型布局对不上，编译与链接都不报错，运行时才出错。

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=LIBTORRENT_DIR");
    println!("cargo:rerun-if-changed=src/shim.h");
    println!("cargo:rerun-if-changed=src/shim.cpp");
    let dist = PathBuf::from(
        std::env::var_os("LIBTORRENT_DIR")
            .expect("没有设 LIBTORRENT_DIR：先运行 third_party/libtorrent/build.sh（.cargo/config.toml 里设了默认位置）"),
    );
    let targets = dist.join("lib/cmake/LibtorrentRasterbar/LibtorrentRasterbarTargets.cmake");
    println!("cargo:rerun-if-changed={}", targets.display());
    let cmake = std::fs::read_to_string(&targets).unwrap_or_else(|error| {
        panic!(
            "读不出 {}（{error}）：先运行 third_party/libtorrent/build.sh",
            targets.display()
        )
    });

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("cargo 给构建脚本设了目标系统");
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").expect("cargo 给构建脚本设了目标环境");
    let mut build = cxx_build::bridge("src/lib.rs");
    build
        .file("src/shim.cpp")
        .include(dist.join("include"))
        .std("c++17");
    if target_env == "msvc" {
        // shim.cpp 里有中文的字面量：MSVC 默认按系统的代码页读源码
        build.flag("/utf-8");
    }
    for define in property(&cmake, "INTERFACE_COMPILE_DEFINITIONS") {
        // 只在调试配置里定义的（`$<$<CONFIG:Debug>:…>`）不用：库按发布配置编译
        if define.starts_with("$<") {
            continue;
        }
        match define.split_once('=') {
            Some((name, value)) => build.define(name, value),
            None => build.define(&define, None),
        };
    }
    build.compile("hs-dlx-torrent-shim");

    let lib = dist.join("lib");
    println!("cargo:rustc-link-search=native={}", lib.display());
    // 静态库的顺序：用到别的库的在前
    for name in ["torrent-rasterbar", "ssl", "crypto"] {
        println!("cargo:rustc-link-lib=static={}", static_name(&lib, name));
    }
    for library in property(&cmake, "INTERFACE_LINK_LIBRARIES") {
        link_system_library(&library);
    }
    // 静态的 OpenSSL 用到的系统库：它的依赖不在 libtorrent 导出的配置里（那里只写了 OpenSSL::SSL）
    let openssl_system: &[&str] = match target_os.as_str() {
        "windows" => &["ws2_32", "gdi32", "advapi32", "crypt32", "user32"],
        "linux" => &["dl"],
        _ => &[],
    };
    for library in openssl_system {
        println!("cargo:rustc-link-lib={library}");
    }
}

/// CMake 导出的目标里属性 `name` 的各项（`;` 分隔；生成表达式的 `$` 在文件里写作 `\$`，还原成 `$`）。
fn property(cmake: &str, name: &str) -> Vec<String> {
    cmake
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(name))
        .map(|value| {
            value
                .trim()
                .trim_matches('"')
                .replace("\\$", "$")
                .split(';')
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_else(|| panic!("libtorrent 的 CMake 配置里没有 {name}"))
}

/// 静态库 `name` 在 `lib` 里的链接名：MSVC 上 OpenSSL 装成 libssl.lib、libcrypto.lib，其余平台与 libtorrent 都是去掉
/// `lib` 前缀与扩展名的名字。
fn static_name(lib: &Path, name: &str) -> String {
    if lib.join(format!("lib{name}.lib")).is_file() {
        format!("lib{name}")
    } else {
        name.to_owned()
    }
}

/// 链接 CMake 导出的依赖里的系统库：`-framework X` 为 macOS 的框架，`$<LINK_ONLY:x>` 与普通的名字为系统库；
/// `Threads::Threads` 由 Rust 的标准库带上，`OpenSSL::SSL` 与 `Boost::headers` 是上面单独处理的静态库与头文件；只在调试
/// 配置里用的（`$<$<CONFIG:Debug>:…>`）不用，库按发布配置编译。认不出的生成表达式报错：当成库名链接会报一个看不懂的
/// 「找不到库」。
fn link_system_library(library: &str) {
    let library = library
        .strip_prefix("$<LINK_ONLY:")
        .and_then(|inner| inner.strip_suffix('>'))
        .unwrap_or(library);
    if library.starts_with("$<$<CONFIG:Debug>:") {
        return;
    }
    assert!(
        !library.starts_with("$<"),
        "libtorrent 的 CMake 配置里有认不出的链接依赖 {library}：在 build.rs 里加上它的处理"
    );
    if let Some(framework) = library.strip_prefix("-framework ") {
        println!("cargo:rustc-link-lib=framework={framework}");
    } else if !library.contains("::") && !library.is_empty() {
        println!("cargo:rustc-link-lib={library}");
    }
}
