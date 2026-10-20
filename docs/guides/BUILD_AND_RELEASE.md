# Build and release `ha` / Build và release `ha`

**Language / Ngôn ngữ:** [English](#english) · [Tiếng Việt](#tiếng-việt)

## English

### 1. Prerequisites

- Windows 10/11 x64. Linux support is pending: it builds, but it is not a supported platform yet.
- [Rust](https://rustup.rs). The pinned toolchain in [`rust-toolchain.toml`](../../rust-toolchain.toml) (1.97.1, with `clippy` and `rustfmt`) is installed automatically by the first `cargo` command.
- PowerShell 7 (`pwsh`) for the scripts, and Git.
- The MSVC build tools that `rustup` asks for (Visual Studio Build Tools, "Desktop development with C++").
- [`uv`](https://docs.astral.sh/uv/) for the Python kernel's environment. You do not need to install it yourself: the installer and the release bundle place a pinned `uv.exe` (0.12.19; downloaded archives are checksum-verified, existing matching-version executables are reused) next to `ha.exe`, and `ha` looks there first, then `HA_UV`, then `PATH`. On a machine without network access, install uv (`winget install astral-sh.uv`) or copy `uv.exe` next to `ha.exe`.

### 2. Build

```powershell
cargo build -p harness-cli --bin ha --locked              # debug: target\debug\ha.exe
cargo build -p harness-cli --bin ha --locked --release    # release: target\release\ha.exe
```

Run a build without installing it: `cargo run -p harness-cli --bin ha -- <args>`, or `.\target\release\ha.exe <args>`.

### 3. Install so `ha` works in every terminal

```powershell
pwsh -NoProfile -File scripts/install/Install-Ha.ps1
```

The installer builds the release binary, verifies the staged copy (digest and `--version`), keeps a backup until the new file is in place, and installs to `%USERPROFILE%\.cargo\bin`. It also places the pinned `uv.exe` beside `ha.exe` (downloaded from the uv GitHub release and checked against its SHA-256; from a bundle it is copied); `-SkipUv` leaves it out, and a failed download is a warning, not a failed install. Useful switches:

| Switch | Effect |
| --- | --- |
| `-Profile Debug` | Install a debug build (faster to build) |
| `-Destination <dir>` | Install somewhere else |
| `-ModifyUserPath` | Add the install directory to the **user** PATH if it is missing (the only switch that changes your environment) |
| `-UseCargoInstall` | Install through `cargo install --path crates/harness-cli --locked` instead |
| `-SkipBuild` | Install the binary that is already built |
| `-Uninstall` | Remove what the installer installed |

**The one mistake that looks like "my changes do nothing":** `ha` runs whichever `ha.exe` comes first on PATH, normally `%USERPROFILE%\.cargo\bin\ha.exe`. `cargo build --release` writes `target\release\ha.exe` and does not replace the installed copy. After pulling or changing code, run the installer again, then confirm:

```powershell
Get-Command ha -All | Select-Object Source
ha --version
```

Close every running `ha` before installing: Windows does not let a running executable be replaced, and the installer reports it rather than killing the app.

### 4. Check before you ship

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
# Prepare and manually verify the candidate in section 5 before this unskipped suite:
cargo test --workspace --all-targets --locked
python -m unittest discover -s crates/harness-cli/python/tests -v
```

The Python regressions include mock desktop tests and do not send GUI input. `cargo test` skips the `#[ignore]`d terminal (PTY) cases because they need a real console. Run that opt-in suite from a local console with `pwsh -NoProfile -File scripts/testing/Invoke-HaPtyAcceptance.ps1` (see [TUI.md](TUI.md)); `-Filter <name>` runs a subset. Live provider and desktop probes also remain opt-in.

[`ci.yml`](../../.github/workflows/ci.yml) runs the `scripts/verify/Verify-Phase.ps1` (P0, P3, P5, P6, P7), `scripts/verify/Verify-Milestone.ps1` and `scripts/verify/Verify-HaLaunch.ps1` gates on Windows (the Ubuntu job is non-blocking), and [`docs.yml`](../../.github/workflows/docs.yml) runs `scripts/verify/Verify-Docs.ps1 -SelfTest`. P7 runs broad workspace tests but explicitly skips `m9_04_release_candidate_has_checksums_and_is_not_published`; it then checks required discovery/cases and documentation self-tests, not the candidate. CI's M9 job builds a candidate before its gate. Attributed failures in the broad suite are retried alone up to twice; compile/crash/unattributed failures or failures that persist alone still fail the gate. Inspect `GATE_TEST_FAILED` and `GATE_TEST_RETRIED` rather than treating a retry as a clean first pass.

### 5. Build a release candidate

For a new `X.Y.Z`, bump `[workspace.package].version` in `Cargo.toml` and all 11 workspace packages in `Cargo.lock`. Leave `npm/harness-agents/package.json` at its source placeholder `0.1.0`: the packer derives the published version from the bundle. Review and commit source/docs/version changes, then require a clean worktree **before** building. The script records `HEAD`, not uncommitted changes, and does not enforce cleanliness itself. Do not use `-SkipBuild` for a new release.

```powershell
pwsh -NoProfile -File scripts/release/New-HaRelease.ps1                  # release build + bundle
pwsh -NoProfile -File scripts/release/New-HaRelease.ps1 -PublishDryRun   # also print what publishing would run
```

It writes `target\release-candidate\ha-<version>-windows-x64\` and a `.zip` of it. The bundle contains exactly these files, and the script fails if anything else appears:

| File | Content |
| --- | --- |
| `ha.exe` | The release executable of the revision you built |
| `uv.exe` | The pinned `uv` used by the Python kernel (downloaded archives checksum-verified; existing matching-version executables reused) |
| `ha.release.json` | Target, version, rustc, source revision and the executable's digest |
| `checksums.txt` | SHA-256 of every other file in the bundle |

Verify the intended `target/release-candidate/ha-X.Y.Z-windows-x64` directory and its ZIP by hand. Require exactly the four files above. The bundled `ha.exe --version` and manifest `version` must both read `ha X.Y.Z`; require `profile: Release`, `target: x86_64-pc-windows-msvc`, `build_commit` equal to the clean `git rev-parse HEAD`, and `published: false`. Compare every `checksums.txt` entry with the actual file SHA-256 and the manifest's `sha256` with `ha.exe`. Record the ZIP digest printed by the script and compare it with the downloaded release.

Before the unskipped workspace suite or M9 gate, move aside **all other immediate directories** under `target/release-candidate`, including stale candidates and the script's `uv-cache`; leave only the intended candidate directory. M9 picks the first directory, not the newest/requested version. It checks hashes and some metadata but does not assert intended version, `Release` profile or equality of commit to HEAD; manual checks remain necessary. Then run section 4's unskipped suite and, for candidate acceptance:

```powershell
pwsh -NoProfile -File scripts/verify/Verify-Milestone.ps1 -Milestone M9
```

If reviewed source or version changes, commit, rebuild, isolate and check the new candidate. `published: false` remains candidate provenance, not a live registry-status flag.

Options: `-Profile Debug` for a quick local candidate, `-OutputDirectory <dir>`, `-SkipBuild` to package the binary that already exists.

### 6. Publish

`New-HaRelease.ps1` never publishes; the GitHub release is created by hand, and npm publication follows its published-release event (section 7):

1. Complete the version/clean-commit preparation and candidate build/manual checks in section 5, then the unskipped suite in section 4 and candidate acceptance. Do not publish a stale binary.
2. Tag that exact commit and push: `git tag -a ha-vX.Y.Z -m "ha X.Y.Z"` and `git push origin ha-vX.Y.Z`.
3. Publish the GitHub release with the intended ZIP, bundle `checksums.txt` and notes: `gh release create ha-vX.Y.Z <zip> <checksums.txt> --title "ha X.Y.Z" --notes-file <release-notes.md>`.
4. Let the OIDC npm workflow download and package that ZIP; it does not rebuild Rust. Tag creation alone does not publish npm. Keep tag, bundle and npm versions aligned for a normal release; do not manually publish unrelated bytes under the same version.

`-PublishDryRun` prints the tag, the archive's SHA-256 and the `git tag`, `git push` and `gh release create` commands it would suggest, and whether `gh` or `GH_TOKEN` is available. Publishing the release triggers [`npm-publish.yml`](../../.github/workflows/npm-publish.yml).

### 7. Publish to npm

`ha` is installed with `npm install -g harness-agents` (Windows x64 only; the package declares `os: win32`, `cpu: x64`, so npm refuses the install elsewhere). There is a single package, `harness-agents`, with no per-platform optional packages: it carries the `ha` launcher, `ha.exe`, `uv.exe`, `ha.release.json` and `checksums.txt`. It is built from the release bundle, never from a fresh build, so npm ships the bytes the GitHub release checksums cover.

#### Publishing from GitHub (the normal path)

[`npm-publish.yml`](../../.github/workflows/npm-publish.yml) runs when a GitHub release is published (or by hand with a tag, and an optional `package_version`). It stores no npm token: npm trusts the workflow through OpenID Connect (Trusted Publisher) and records a provenance statement linking the package to this repository. On a Windows runner with Node 24 and npm 11.5.1 or newer it downloads the release's `ha-*-windows-x64.zip` with `gh`, packs it with `New-HaNpmPackages.ps1` and runs `npm publish --provenance`. A release without that zip is skipped with a notice, and a version already on npm is a no-op rather than a failure.

Set it up once on npmjs.com: package `harness-agents` > Settings > Trusted Publisher > GitHub Actions, owner `DEVfancybear`, repository `harness-agents`, workflow `npm-publish.yml`.

#### Publishing from your terminal

For the first publish of a name, or a version that differs from the release:

```powershell
pwsh -NoProfile -File scripts/release/New-HaRelease.ps1      # the bundle
pwsh -NoProfile -File scripts/release/New-HaNpmPackages.ps1  # checks the bundle's checksums, writes target\npm\*.tgz
npm login
npm publish target\npm\harness-agents-<version>.tgz --access public
```

The script prints the exact command. The package takes the bundle's version; `-PackageVersion` sets a higher one to republish the same build, because npm never accepts a version twice. The sources are in [`npm/`](../../npm/harness-agents/package.json); the `version` in that `package.json` is a placeholder, and the packing script writes the real one.

## Tiếng Việt

### 1. Cần có

- Windows 10/11 x64. Linux đang chờ hỗ trợ: vẫn build được nhưng chưa phải nền tảng được hỗ trợ.
- [Rust](https://rustup.rs). Toolchain được pin trong [`rust-toolchain.toml`](../../rust-toolchain.toml) (1.97.1, kèm `clippy` và `rustfmt`) được cài tự động ở lệnh `cargo` đầu tiên.
- PowerShell 7 (`pwsh`) để chạy script, và Git.
- MSVC build tools mà `rustup` yêu cầu (Visual Studio Build Tools, mục "Desktop development with C++").
- [`uv`](https://docs.astral.sh/uv/) để dựng môi trường cho Python kernel. Bạn không cần tự cài: installer và gói release đặt sẵn một `uv.exe` (0.12.19) đã pin phiên bản (archive tải mới được kiểm checksum, executable có sẵn đúng version được dùng lại) cạnh `ha.exe`; `ha` tìm uv ở đó trước, rồi tới `HA_UV`, rồi `PATH`. Máy không có mạng thì cài uv (`winget install astral-sh.uv`) hoặc chép `uv.exe` vào cạnh `ha.exe`.

### 2. Build

```powershell
cargo build -p harness-cli --bin ha --locked              # debug: target\debug\ha.exe
cargo build -p harness-cli --bin ha --locked --release    # release: target\release\ha.exe
```

Chạy bản vừa build mà không cài: `cargo run -p harness-cli --bin ha -- <args>`, hoặc `.\target\release\ha.exe <args>`.

### 3. Cài để gõ `ha` ở mọi terminal

```powershell
pwsh -NoProfile -File scripts/install/Install-Ha.ps1
```

Installer build bản release, kiểm tra bản sao tạm (digest và `--version`), giữ bản dự phòng cho đến khi file mới vào đúng chỗ, và cài vào `%USERPROFILE%\.cargo\bin`. Installer cũng đặt `uv.exe` đã pin phiên bản cạnh `ha.exe` (tải từ bản phát hành uv trên GitHub và kiểm SHA-256; nếu cài từ gói thì chép từ gói); `-SkipUv` bỏ bước này, và tải lỗi chỉ là cảnh báo, không làm hỏng việc cài. Các tùy chọn hay dùng:

| Tùy chọn | Tác dụng |
| --- | --- |
| `-Profile Debug` | Cài bản debug (build nhanh hơn) |
| `-Destination <dir>` | Cài vào thư mục khác |
| `-ModifyUserPath` | Thêm thư mục cài vào PATH **người dùng** nếu còn thiếu (tùy chọn duy nhất thay đổi môi trường của bạn) |
| `-UseCargoInstall` | Cài bằng `cargo install --path crates/harness-cli --locked` |
| `-SkipBuild` | Cài bản đã build sẵn |
| `-Uninstall` | Gỡ những gì installer đã cài |

**Lỗi hay gặp khiến tưởng "sửa code mà không thấy thay đổi":** lệnh `ha` chạy file `ha.exe` đứng đầu trong PATH, thường là `%USERPROFILE%\.cargo\bin\ha.exe`. `cargo build --release` chỉ ghi `target\release\ha.exe`, không thay bản đã cài. Sau khi pull hoặc sửa code, chạy lại installer rồi kiểm tra:

```powershell
Get-Command ha -All | Select-Object Source
ha --version
```

Đóng mọi `ha` đang chạy trước khi cài: Windows không cho thay file thực thi đang chạy, và installer sẽ báo lỗi chứ không tắt app của bạn.

### 4. Kiểm tra trước khi phát hành

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
# Chuẩn bị và kiểm thủ công candidate ở mục 5 trước suite không skip này:
cargo test --workspace --all-targets --locked
python -m unittest discover -s crates/harness-cli/python/tests -v
```

Regression Python có test desktop mock, không gửi GUI input. `cargo test` bỏ qua các ca terminal (PTY) đánh dấu `#[ignore]` vì chúng cần console thật. Chạy bộ opt-in này từ console cục bộ bằng `pwsh -NoProfile -File scripts/testing/Invoke-HaPtyAcceptance.ps1` (xem [TUI.md](TUI.md)); `-Filter <tên>` chạy một phần. Probe provider live và desktop cũng vẫn cần opt-in.

[`ci.yml`](../../.github/workflows/ci.yml) chạy các gate `scripts/verify/Verify-Phase.ps1` (P0, P3, P5, P6, P7), `scripts/verify/Verify-Milestone.ps1` và `scripts/verify/Verify-HaLaunch.ps1` trên Windows (job Ubuntu không chặn), còn [`docs.yml`](../../.github/workflows/docs.yml) chạy `scripts/verify/Verify-Docs.ps1 -SelfTest`. P7 chạy test workspace diện rộng nhưng skip rõ `m9_04_release_candidate_has_checksums_and_is_not_published`; sau đó kiểm discovery/case bắt buộc và self-test docs, không chứng nhận candidate. Job M9 của CI build candidate trước khi chạy gate. Test lỗi xác định được trong suite diện rộng được retry riêng tối đa hai lần; lỗi compile/crash/không xác định được test hoặc lỗi vẫn còn khi chạy riêng vẫn làm gate đỏ. Đọc cả `GATE_TEST_FAILED` và `GATE_TEST_RETRIED`, không coi retry là lượt chạy đầu sạch lỗi.

### 5. Build bản release candidate

Với bản `X.Y.Z` mới, nâng `[workspace.package].version` trong `Cargo.toml` và version của cả 11 package workspace trong `Cargo.lock`. Giữ `npm/harness-agents/package.json` ở placeholder nguồn `0.1.0`: packer lấy version phát hành từ bundle. Review và commit thay đổi source/docs/version, rồi yêu cầu worktree sạch **trước khi** build. Script ghi `HEAD`, không ghi thay đổi chưa commit và không tự kiểm worktree sạch. Không dùng `-SkipBuild` cho release mới.

```powershell
pwsh -NoProfile -File scripts/release/New-HaRelease.ps1                  # build release + đóng gói
pwsh -NoProfile -File scripts/release/New-HaRelease.ps1 -PublishDryRun   # in thêm các lệnh phát hành sẽ chạy
```

Script ghi ra `target\release-candidate\ha-<version>-windows-x64\` và một file `.zip`. Gói chứa đúng các file sau, và script báo lỗi nếu có file khác:

| File | Nội dung |
| --- | --- |
| `ha.exe` | File thực thi release của revision bạn đã build |
| `uv.exe` | Bản `uv` đã pin cho Python kernel (archive tải mới được kiểm checksum; executable có sẵn đúng version được dùng lại) |
| `ha.release.json` | Target, version, rustc, source revision và digest của file thực thi |
| `checksums.txt` | SHA-256 của mọi file khác trong gói |

Kiểm thủ công đúng thư mục `target/release-candidate/ha-X.Y.Z-windows-x64` và ZIP của nó. Chỉ được có đúng bốn file trên. `ha.exe --version` trong bundle và trường `version` của manifest đều phải là `ha X.Y.Z`; yêu cầu `profile: Release`, `target: x86_64-pc-windows-msvc`, `build_commit` bằng commit sạch từ `git rev-parse HEAD`, và `published: false`. Đối chiếu mọi entry trong `checksums.txt` với SHA-256 file thực và trường `sha256` của manifest với `ha.exe`. Ghi digest ZIP script in ra và đối chiếu với bản tải từ release.

Trước suite workspace không skip hoặc gate M9, chuyển **mọi thư mục con trực tiếp khác** ra ngoài `target/release-candidate`, kể cả candidate cũ và `uv-cache` do script tạo; chỉ để lại đúng thư mục candidate cần kiểm. M9 chọn thư mục đầu tiên, không chọn version mới nhất hay được yêu cầu. Nó kiểm hash và một phần metadata nhưng không assert version dự định, profile `Release` hoặc commit bằng HEAD; vẫn phải kiểm thủ công. Sau đó chạy suite không skip ở mục 4 và, để nghiệm thu candidate:

```powershell
pwsh -NoProfile -File scripts/verify/Verify-Milestone.ps1 -Milestone M9
```

Nếu source đã review hoặc version thay đổi, commit rồi build, cô lập và kiểm lại candidate mới. `published: false` vẫn là provenance của candidate, không phải cờ trạng thái registry trực tiếp.

Tùy chọn: `-Profile Debug` cho bản thử nhanh, `-OutputDirectory <dir>`, `-SkipBuild` để đóng gói bản đã build sẵn.

### 6. Phát hành

`New-HaRelease.ps1` không tự phát hành; GitHub release được tạo bằng tay, rồi sự kiện publish release kích hoạt npm (mục 7):

1. Hoàn tất chuẩn bị version/commit sạch, build và kiểm thủ công candidate ở mục 5, rồi suite không skip ở mục 4 và nghiệm thu candidate. Không publish binary cũ.
2. Tạo tag tại đúng commit đó và push: `git tag -a ha-vX.Y.Z -m "ha X.Y.Z"` và `git push origin ha-vX.Y.Z`.
3. Publish GitHub release kèm đúng ZIP, `checksums.txt` của bundle và ghi chú: `gh release create ha-vX.Y.Z <zip> <checksums.txt> --title "ha X.Y.Z" --notes-file <release-notes.md>`.
4. Để workflow npm OIDC tải và đóng gói ZIP đó; nó không build lại Rust. Chỉ tạo tag chưa publish npm. Release thông thường phải giữ tag, bundle và npm version khớp nhau; không publish thủ công binary không liên quan dưới cùng version.

`-PublishDryRun` in ra tag, SHA-256 của file zip, các lệnh `git tag`, `git push`, `gh release create` mà nó gợi ý, và cho biết `gh` hoặc `GH_TOKEN` có sẵn không. Việc publish release sẽ kích hoạt [`npm-publish.yml`](../../.github/workflows/npm-publish.yml).

### 7. Phát hành lên npm

`ha` được cài bằng `npm install -g harness-agents` (chỉ Windows x64; package khai báo `os: win32`, `cpu: x64` nên npm từ chối cài ở nơi khác). Chỉ có một package duy nhất, `harness-agents`, không có package tùy chọn theo từng nền tảng: nó mang launcher `ha`, `ha.exe`, `uv.exe`, `ha.release.json` và `checksums.txt`. Nó được đóng từ bundle release chứ không build lại, để npm phát đúng các byte mà checksum của GitHub release bao phủ.

#### Phát hành từ GitHub (đường chính)

[`npm-publish.yml`](../../.github/workflows/npm-publish.yml) chạy khi một GitHub release được publish (hoặc chạy tay với một tag và `package_version` tùy chọn). Workflow không lưu token npm nào: npm tin workflow qua OpenID Connect (Trusted Publisher) và ghi một bản provenance nối package với repo này. Trên runner Windows với Node 24 và npm 11.5.1 trở lên, workflow tải `ha-*-windows-x64.zip` của release bằng `gh`, đóng bằng `New-HaNpmPackages.ps1` rồi chạy `npm publish --provenance`. Release không có zip đó thì bị bỏ qua kèm một thông báo; version đã có trên npm thì không làm gì thay vì báo lỗi.

Thiết lập một lần trên npmjs.com: package `harness-agents` > Settings > Trusted Publisher > GitHub Actions, owner `DEVfancybear`, repository `harness-agents`, workflow `npm-publish.yml`.

#### Phát hành từ terminal

Dùng cho lần publish đầu của một tên, hoặc một version khác với release:

```powershell
pwsh -NoProfile -File scripts/release/New-HaRelease.ps1      # bundle
pwsh -NoProfile -File scripts/release/New-HaNpmPackages.ps1  # kiểm checksum của bundle, ghi target\npm\*.tgz
npm login
npm publish target\npm\harness-agents-<version>.tgz --access public
```

Script in đúng lệnh này. Package lấy version của bundle; `-PackageVersion` đặt version cao hơn để phát lại cùng một bản build, vì npm không bao giờ nhận một version hai lần. Mã nguồn nằm ở [`npm/`](../../npm/harness-agents/package.json); `version` trong `package.json` đó chỉ là giá trị giữ chỗ, script đóng gói ghi version thật.
