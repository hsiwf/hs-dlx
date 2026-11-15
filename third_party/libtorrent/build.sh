#!/usr/bin/env bash
# 下载并编译 BT 引擎用的 libtorrent 静态库（连同它用的 OpenSSL 静态库与 Boost 头文件）。用法：build.sh [安装目录 [工作目录]]
# - 安装目录默认 third_party/libtorrent/dist，装 include/ 与 lib/；它会被整个删除重建，只接受不存在、为空或由本脚本
#   装过的目录。
# - 工作目录放源码包与解开的源码，默认 third_party/libtorrent/build；源码包已在其中且核对一致时不再下载，解开的源码
#   每次删掉重解。
# Windows：在已加载 MSVC 环境的 MSYS2 bash 中运行，OpenSSL 的配置要 Windows 原生的 perl（Strawberry Perl），产物为
# MSVC 静态库（*.lib）。
set -euo pipefail

LIBTORRENT=2.1.2
OPENSSL=3.5.9
BOOST=1.92.0
# 各源码包的 SHA-256：libtorrent 与 Boost 取自 GitHub 发布页给出的摘要（Boost 另与发布的 .txt 核对），OpenSSL 与发布的
# .sha256 文件核对
LIBTORRENT_SHA256=3362546d9cd71b9e49ee6cac7d3f1f914ce9cdb217c86b63d5b22cbed0334dbc
OPENSSL_SHA256=603f5602e2eef00d77fbd429d34dcd5822bb301757a1bc9cdb24c670f1eb859a
BOOST_SHA256=ea7b982002cc9dfbe59b0b217b206f470dc75f3de0bb2973d844118934d82411

ROOT=$(cd "$(dirname "$0")" && pwd)
PREFIX=${1:-$ROOT/dist}
WORK=${2:-$ROOT/build}
[[ "$PREFIX" == /* ]] || PREFIX=$PWD/$PREFIX
[[ "$WORK" == /* ]] || WORK=$PWD/$WORK
# 本脚本装好后在安装目录里放的标记，即本脚本的副本：Makefile 逐字节比对，发现本地装的是改动前的脚本构建的
MARKER=.hs-dlx-libtorrent

case "$(uname -s)" in
  MINGW* | MSYS*) PLATFORM=windows ;;
  Darwin) PLATFORM=macos ;;
  Linux) PLATFORM=linux ;;
  *) echo "不支持的平台: $(uname -s)" >&2; exit 1 ;;
esac

# macOS：静态库的最低系统版本与桌面应用的一致（apps/desktop/Cargo.toml 的 minimum-system-version），不设时 clang 按
# SDK 的版本编译，链接时报「built for newer macOS version」
if [[ $PLATFORM == macos ]]; then
  export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-11.0}
fi

sha256_of() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# 取 `url` 到工作目录里的 `name`（已有且核对一致时不再下载），核对 SHA-256 为 `sha256`。
fetch() {
  local name=$1 url=$2 sha256=$3
  if [[ ! -f "$WORK/$name" ]] || [[ "$(sha256_of "$WORK/$name")" != "$sha256" ]]; then
    curl --fail --location --silent --show-error -o "$WORK/$name.part" "$url"
    mv "$WORK/$name.part" "$WORK/$name"
  fi
  local actual
  actual=$(sha256_of "$WORK/$name")
  if [[ "$actual" != "$sha256" ]]; then
    echo "$name 的 SHA-256 不符：期望 $sha256，实际 $actual" >&2
    exit 1
  fi
}

if [[ "$PREFIX" != "$ROOT/dist" && -e "$PREFIX" && ! -f "$PREFIX/$MARKER" ]]; then
  if [[ ! -d "$PREFIX" ]]; then
    echo "安装目录 $PREFIX 不是目录" >&2
    exit 1
  fi
  if [[ -n "$(find "$PREFIX" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
    echo "安装目录 $PREFIX 非空，且不是本脚本装的；换一个目录或先自行清空" >&2
    exit 1
  fi
fi

mkdir -p "$WORK"
fetch "libtorrent-rasterbar-$LIBTORRENT.tar.gz" \
  "https://github.com/arvidn/libtorrent/releases/download/v$LIBTORRENT/libtorrent-rasterbar-$LIBTORRENT.tar.gz" \
  "$LIBTORRENT_SHA256"
fetch "openssl-$OPENSSL.tar.gz" \
  "https://github.com/openssl/openssl/releases/download/openssl-$OPENSSL/openssl-$OPENSSL.tar.gz" \
  "$OPENSSL_SHA256"
fetch "boost-$BOOST-b2-nodocs.tar.xz" \
  "https://github.com/boostorg/boost/releases/download/boost-$BOOST/boost-$BOOST-b2-nodocs.tar.xz" \
  "$BOOST_SHA256"

rm -rf -- "${PREFIX:?}" "$WORK/libtorrent-rasterbar-$LIBTORRENT" "$WORK/openssl-$OPENSSL" "$WORK/boost-$BOOST" \
  "$WORK/libtorrent-cmake"
for tarball in "libtorrent-rasterbar-$LIBTORRENT.tar.gz" "openssl-$OPENSSL.tar.gz"; do
  tar -xzf "$WORK/$tarball" -C "$WORK"
done
tar -xJf "$WORK/boost-$BOOST-b2-nodocs.tar.xz" -C "$WORK"
mkdir -p "$PREFIX"
jobs=$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN)

# OpenSSL：只要 libssl 与 libcrypto 的静态库，不要命令行程序、测试与文档
openssl_flags=(no-shared no-tests no-docs no-apps --prefix="$PREFIX" --openssldir="$PREFIX/ssl" --libdir=lib)
cd "$WORK/openssl-$OPENSSL"
case $PLATFORM in
  macos) perl Configure darwin64-arm64-cc "${openssl_flags[@]}" ;;
  linux) perl Configure linux-x86_64 "${openssl_flags[@]}" ;;
  # /MD：与 Rust MSVC 目标一致用动态 C 运行库；no-asm：汇编要另装 NASM
  windows) perl Configure VC-WIN64A no-asm "${openssl_flags[@]}" ;;
esac
if [[ $PLATFORM == windows ]]; then
  nmake
  nmake install_sw
else
  make -j"$jobs"
  make install_sw
fi

# libtorrent：静态库；Boost 只用头文件（Boost.System 1.69 起只有头文件）。不要 I2P、WebTorrent（要 libdatachannel）
# 与为兼容旧接口保留的函数
libtorrent_flags=(
  -G Ninja
  -DCMAKE_BUILD_TYPE=Release
  -DCMAKE_INSTALL_PREFIX="$PREFIX"
  -DCMAKE_INSTALL_LIBDIR=lib
  -DCMAKE_CXX_STANDARD=17
  -DBUILD_SHARED_LIBS=OFF
  -Dstatic_runtime=OFF
  -Dencryption=ON
  -Ddeprecated-functions=OFF
  -Di2p=OFF
  -Dwebtorrent=OFF
  -DOPENSSL_ROOT_DIR="$PREFIX"
  -DOPENSSL_USE_STATIC_LIBS=TRUE
  -DBoost_INCLUDE_DIR="$WORK/boost-$BOOST"
  -DBoost_NO_SYSTEM_PATHS=ON
)
cmake -S "$WORK/libtorrent-rasterbar-$LIBTORRENT" -B "$WORK/libtorrent-cmake" "${libtorrent_flags[@]}"
cmake --build "$WORK/libtorrent-cmake" --parallel "$jobs"
cmake --install "$WORK/libtorrent-cmake"
# libtorrent 的公开头文件包含 Boost 的头文件：一并装进安装目录，绑定只看这一个目录
cp -R "$WORK/boost-$BOOST/boost" "$PREFIX/include/boost"

if [[ $PLATFORM == windows ]]; then
  libs=(torrent-rasterbar.lib libssl.lib libcrypto.lib)
else
  libs=(libtorrent-rasterbar.a libssl.a libcrypto.a)
fi
for lib in "${libs[@]}"; do
  if [[ ! -f "$PREFIX/lib/$lib" ]]; then
    echo "安装目录缺少 $PREFIX/lib/$lib" >&2
    exit 1
  fi
done

cp "$ROOT/build.sh" "$PREFIX/$MARKER"
echo "libtorrent ${LIBTORRENT}（OpenSSL ${OPENSSL}、Boost ${BOOST} 的头文件）已安装到 ${PREFIX}"
