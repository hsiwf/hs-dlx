# 参与开发

## 环境

- Rust：版本与组件由 `rust-toolchain.toml` 固定。
- [uv](https://docs.astral.sh/uv/)：版本见 `pyproject.toml` 的 `required-version`，运行仓库里的 Python 脚本（第三方声明、图标、
  找 FFmpeg 的构建脚本、发版）。
- Linux：WebKitGTK 4.1、GTK 3、xkbcommon 与 fontconfig 的开发包，包名见 `.github/workflows/check.yml`。
- Windows：带 VC 工具的 Visual Studio 与 MSYS2（`make`、编译 FFmpeg 用），做法见 `.github/actions/msvc-env` 与
  `.github/workflows/check.yml`。
- `make check` 另要 cargo-deny 与 cargo-llvm-cov，`make package` 另要 cargo-packager；版本分别见 `check.yml` 与
  `desktop.yml`。

FFmpeg 用 hs-m3u8（`Cargo.lock` 固定的提交）里的构建脚本编译成静态库，装到 `third_party/ffmpeg/dist`。第一次与升级
hs-m3u8 之后运行 `make ffmpeg`；装的不是当前那份脚本编出来的时，构建前的检查会失败并提示。

## 检查

```sh
make check
```

| 目标 | 内容 |
|---|---|
| `rs_fmt` | `cargo fmt --check` |
| `rs_lint` | 依赖方向（`scripts/check_deps.sh`）、不许用属性或注释压掉检查（`scripts/check_no_suppression.sh`）、cargo-deny（安全公告、许可证、来源）、随安装包分发的第三方声明是否最新 |
| `rs_clippy` | 全部目标，告警即失败 |
| `rs_doc` | 文档注释里的链接都能解析，告警即失败 |
| `rs_test` | 全部测试 |
| `rs_cov` | `crates/app`（任务存储与生命周期）行覆盖不低于 80% |
| `py_check`、`py_test` | 仓库里 Python 脚本的格式、ruff、mypy、basedpyright 与测试 |

CI 在 macOS、Windows、Linux 上跑格式、clippy、文档与测试；与平台无关的几项只在 Linux 上跑。

依赖或 FFmpeg 版本变了之后运行 `make licenses` 重新生成 `apps/desktop/LICENSES`。

## 分支

- `dev`：日常开发。直接提交，或从 `dev` 拉分支再提 PR 回 `dev`。推送与 PR 都跑检查。
- `main`：发布线，只接受来自 `dev` 的 PR，三个平台的检查都通过才能合并。合并时选「Create a merge commit」，不用
  squash 或 rebase：`main` 与 `dev` 的历史保持一致，下一次从 `dev` 提 PR 时不会出现重复的提交。

提交信息写成 `<type>: <描述>`，type 为 `feat`、`fix`、`refactor`、`docs`、`test`、`chore`、`perf`、`ci`。

## 发版

版本号只写在 `Cargo.toml` 的 `[workspace.package] version` 一处，形如 `X.Y.Z`。

1. 在 `dev` 上改版本号，运行 `cargo update --workspace` 让 `Cargo.lock` 跟上（打包用 `--locked` 构建，锁文件没跟上会失败），
   提交 `chore: 版本号改为 X.Y.Z`。
2. 从 `dev` 向 `main` 提 PR，标题写 `release: vX.Y.Z <一句话概括>`，正文按模板写好「## 发布说明」：这一节原样成为
   GitHub Release 的描述，写给用户看。
3. 检查通过后合并。`.github/workflows/release.yml` 接手：
   - 核对版本号，读出发布说明；`vX.Y.Z` 标签已存在（这个 PR 没改版本号）时到此为止，不发版。
   - 由 `desktop.yml` 在三个平台上打安装包：`.dmg`（Apple 芯片）、`-setup.exe`（x64）、`.deb`（amd64）。
   - 全部成功后建 Release：先建草稿、上传安装包与 `SHA256SUMS`，再公开；公开时 GitHub 在合并提交上打 `vX.Y.Z` 标签。
4. 任一步失败时没有公开的 Release、也没有标签：修好原因后在 Actions 里对这次运行点「Re-run failed jobs」，上一次
   留下的草稿会先删掉。原因要改代码时，在 `dev` 上修好、再提一个 PR 合并即可（版本号不变，标签仍不存在，照常发版）。

没改版本号的 PR 合并进 `main` 不发版，「## 发布说明」留空即可。

安装包没有签名与公证；签名需要开发者账号，到时在 `desktop.yml` 里加。

## 手动打包

想在发版之外拿到安装包：Actions → desktop → Run workflow，选分支后运行，安装包在这次运行的产物里。本机打包用
`make package`，产物在 `target/release`。
