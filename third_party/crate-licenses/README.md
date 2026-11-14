包里的许可证文件对不上所声明许可证（见 scripts/licenses.py 的 `uncovered`）的 crate 补的许可证原文，由
scripts/licenses.py 收录进 apps/desktop/LICENSES/THIRD-PARTY-RUST.txt，在那里标为本项目补的。目录名为 `<crate>-<版本>`。
文件取自该版本发布时的提交（包里 `.cargo_vcs_info.json` 记的）在源码仓库根目录的许可证文件（包所在目录没有时）；
源码里也没有的，取 SPDX License List（spdx/license-list-data v3.29.0 的 text/）的标准文本，逐条写明。

- ffmpeg-sys-next 9.0.0：包里与源码仓库都没有许可证文件，Cargo.toml 写的是 WTFPL；取同一作者的 ffmpeg-next 9.0.0
  包里的 LICENSE（WTFPL 原文）

- accesskit 0.24.1：AccessKit/accesskit@a55d3e1a18bb9ef0e4bccc9083fb13c3e0ad8969
- accesskit_atspi_common 0.19.1、accesskit_consumer 0.38.0、accesskit_macos 0.26.3、accesskit_unix 0.22.1、
  accesskit_windows 0.34.0：AccessKit/accesskit@c88605b96d04431f9c3c792464a0f2f253480e94
- gpui-kit 0.9.1：longbridge/gpui-kit@39ab849a93b5f15507b969164d49891db8d5b470
- gpui-kit-assets 0.9.1：包里只有 Lucide 图标的 LICENSE-LUCIDE，它自己的 Apache-2.0 取同一提交
  longbridge/gpui-kit@39ab849a93b5f15507b969164d49891db8d5b470 根目录的 LICENSE-APACHE
- harfrust 0.5.2：harfbuzz/harfrust@efdae3142ab76a2f1524d72cff9e3dfdc5afd7ca
- lyon 1.0.19、lyon_path 1.0.19：nical/lyon@56219c95c658d4f39de7b190d98e57d5773ba241
- lyon_algorithms 1.0.22、lyon_geom 1.0.20、lyon_tessellation 1.0.23：
  nical/lyon@db98d26a1a90076a7a894262dcb51117eef643c8
- mac 0.1.1：包与源码仓库（reem/rust-mac）都没有许可证文件，Cargo.toml 写的是 MIT/Apache-2.0、README 写「like rust
  itself」；按 Apache-2.0 取用，原文取自 rust-lang/rust@5f4effd579500175af844241eb179e756190f1e7 的 LICENSE-APACHE
- objc2 一族（madsmtm/objc2），各版本的文件取自：
  - objc-sys 0.3.5：madsmtm/objc2@4fc083f1c6d6784577e38b0ee8dbd344481e2fd2 的 LICENSE.txt
  - objc2 0.5.3：madsmtm/objc2@5237cbf081d04ef75a1f6a207b4018e7c1ae7438 的 LICENSE.txt
  - objc2-app-kit 0.2.2、objc2-foundation 0.2.2：madsmtm/objc2@e282618be4c3a3b9542957e0c8540e9588472ce8 的 LICENSE.txt
  - 以下各版本的源码仓库根目录只有 LICENSE.md：它说明各 crate 按 MIT（或 Zlib、Apache-2.0、MIT 任选）授权，只给链接、
    没有许可证原文。收录它的同时补一份 LICENSE-MIT，为 SPDX 的 MIT 标准文本（去掉了模板里的版权行，上游没写版权人）。
    - block2 0.6.2：madsmtm/objc2@b4167b582b2f75f9a1be75495c41b765344fd03c
    - dispatch2 0.3.1：madsmtm/objc2@8852b424193ca41602281b3d7540d7c8ed51e49a
    - objc2 0.6.5：madsmtm/objc2@d7d2fa23ceaa5e6096c923b081040e5d81b3b9df
    - objc2-app-kit、objc2-cloud-kit、objc2-core-data、objc2-core-foundation、objc2-core-graphics、objc2-core-image、
      objc2-core-services、objc2-core-text、objc2-core-video、objc2-foundation、objc2-metal、objc2-quartz-core、
      objc2-security、objc2-uniform-type-identifiers、objc2-user-notifications、objc2-web-kit 0.3.2：
      madsmtm/objc2@7b1abfd750a2cacaea71d6a56ecfb83cb7de560b
    - objc2-encode 4.1.0、objc2-exception-helper 0.1.1：madsmtm/objc2@8d214f5477365ffcbcbb7de058c86ed9a518efb7
- pathfinder_geometry 0.5.1：发布时的提交 servo/pathfinder@a5e98fac00f433fe2ff4b2135383d82491b8bfc5 已不在仓库里；
  取同一仓库 pathfinder_simd 0.5.6 的发布提交 servo/pathfinder@6c3c0466f451c5bd2007087728cd168798cd64e8 根目录的
  （同为 MIT 或 Apache-2.0）
- pathfinder_simd 0.5.6：servo/pathfinder@6c3c0466f451c5bd2007087728cd168798cd64e8
- profiling 1.0.18：aclysma/profiling@8271551172eb6fa4cba47369aedd93790c623df9
- rust-i18n-support 4.2.4：longbridge/rust-i18n@5f3ea5ba03395f2f2908e3587e01788cfdfd3555
- seahash 4.1.0：包与源码仓库（gitlab.redox-os.org/redox-os/seahash@94b632aeac099031c373599313d5b5f0acbbaec0）都没有
  许可证文件，Cargo.toml 写的是 MIT；按 MIT 的标准文本，版权行写 Cargo.toml 的 authors
- spirv 0.4.0+sdk-1.4.341.0：gfx-rs/rspirv@8afc3d0ac8e158128cd1410bb2e4b4c26ab11bb4
- svg_fmt 0.4.5：nical/rust_debug@93f414cd572b5c2781df93dbdc068ed13443d05a
- taffy 0.15.1：DioxusLabs/taffy@0064bd54cbe38bf1d8bcd6ec6ab0704d45fb048a
- webview2-com 0.39.1、webview2-com-sys 0.39.1：wravery/webview2-rs@edc2caf886175ccaebe86078c9cfe1ae2a187328
- xim-ctext 0.3.0：Riey/xim-rs@65f77e44524ba487285a63aac4080faf2c05b4ed
- xim-parser 0.2.2：Riey/xim-rs@7300de35a111de2df1a694bf70454532a8419685
- zune-core 0.4.12：etemesi254/zune-image@f8fbb123d5ed04441e8324a555bfcda0cb1bd28f（Cargo.toml 没写仓库，取自同一作者的
  zune-image）
- zune-inflate 0.2.54：etemesi254/zune-image@69502ce83fdfecdd0beefd677e2abb3781b29d98（同上）
- zune-jpeg 0.4.21：etemesi254/zune-image@fa2c767a01d7d9373911d0bf63e0588553d67e0e

这些 crate 升级后这里没有新版本的目录、包里的文件又对不上时，scripts/licenses.py 即失败，照上面的办法取新版本的文件。
