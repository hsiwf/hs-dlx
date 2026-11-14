# hs-dlx

桌面下载器（macOS、Windows、Linux）。现在下载 HLS（m3u8）：点播与直播录制、在网页里找视频流、断点续传，输出 MP4
或本地 HLS；HLS 的部分用 [hs-m3u8](https://github.com/x-haose/hs-m3u8) 的核心库。

## 构建

要 Rust（版本见 rust-toolchain.toml）、[uv](https://docs.astral.sh/uv/)（仓库里的 Python 脚本用），Linux 上另要
WebKitGTK 4.1、GTK 3、xkbcommon 与 fontconfig 的开发包（见 .github/workflows/check.yml）。

```sh
make ffmpeg          # 用 hs-m3u8 的构建脚本编译静态链接的精简 FFmpeg，装到 third_party/ffmpeg/dist
cargo run -p hs-dlx
make check           # 全部检查（要 cargo-deny 与 cargo-llvm-cov，版本见 check.yml）
make package         # 本机平台的安装包（要 cargo-packager，版本见 desktop.yml）
```

## 许可证

MIT，见 LICENSE。安装包里静态链接与随带的第三方软件的声明见 apps/desktop/LICENSES。
