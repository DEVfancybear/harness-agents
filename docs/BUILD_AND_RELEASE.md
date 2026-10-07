# Build and release `ha` / Build và release `ha`

**Language / Ngôn ngữ:** [English](#english) · [Tiếng Việt](#tiếng-việt)

## English

### 1. Prerequisites

- Windows 10/11 x64. Linux support is pending: it builds, but it is not a supported platform yet.
- [Rust](https://rustup.rs). The pinned toolchain in [`rust-toolchain.toml`](../rust-toolchain.toml) (1.97.1, with `clippy` and `rustfmt`) is installed automatically by the first `cargo` command.
- PowerShell 7 (`pwsh`) for the scripts, and Git.
- The MSVC build tools that `rustup` asks for (Visual Studio Build Tools, "Desktop development with C++").
- [`uv`](https://docs.astral.sh/uv/) for the Python kernel's environment. You do not need to install it yourself: the installer and the release bundle place a pinned, checksum-verified `uv.exe` (0.12.19) next to `ha.exe`, and `ha` looks there first, then `HA_UV`, then `PATH`. On a machine without network access, install uv (`winget install astral-sh.uv`) or copy `uv.exe` next to `ha.exe`.

### 2. Build

```powershell
cargo build -p harness-cli --bin ha --locked              # debug: target\debug\ha.exe
cargo build -p harness-cli --bin ha --locked --release    # release: target\release\ha.exe
```

Run a build without installing it: `cargo run -p harness-cli --bin ha -- <args>`, or `.\target\release\ha.exe <args>`.

### 3. Install so `ha` works in every terminal

```powershell
pwsh -NoProfile -File scripts/Install-Ha.ps1
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
cargo test --workspace --locked
```

`cargo test` skips the `#[ignore]`d terminal (PTY) cases because they need a real console. Run that suite from a local console with `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1` (see [TUI.md](TUI.md)); `-Filter <name>` runs a subset.

[`ci.yml`](../.github/workflows/ci.yml) runs the `scripts/Verify-Phase.ps1` (P0, P3, P5, P6, P7), `scripts/Verify-Milestone.ps1` and `scripts/Verify-HaLaunch.ps1` gates on Windows (the Ubuntu job is non-blocking), and [`docs.yml`](../.github/workflows/docs.yml) runs `scripts/Verify-Docs.ps1 -SelfTest`. These gates run the whole workspace suite; a test that fails in that crowded run is re-run alone, and the gate fails only when it also fails alone (`GATE_TEST_FAILED`, `GATE_TEST_RETRIED` in the log).

### 5. Build a release candidate

```powershell
pwsh -NoProfile -File scripts/New-HaRelease.ps1                  # release build + bundle
pwsh -NoProfile -File scripts/New-HaRelease.ps1 -PublishDryRun   # also print what publishing would run
```

It writes `target\release-candidate\ha-<version>-windows-x64\` and a `.zip` of it. The bundle contains exactly these files, and the script fails if anything else appears:

| File | Content |
| --- | --- |
| `ha.exe` | The release executable of the revision you built |
| `uv.exe` | The pinned `uv` the Python kernel builds its environment with (checksum-verified at packaging) |
| `ha.release.json` | Target, version, rustc, source revision and the executable's digest |
| `checksums.txt` | SHA-256 of every other file in the bundle |

Verify a bundle by hand:

```powershell
Get-FileHash .\target\release-candidate\ha-*-windows-x64\ha.exe -Algorithm SHA256
Get-Content  .\target\release-candidate\ha-*-windows-x64\checksums.txt
```

Options: `-Profile Debug` for a quick local candidate, `-OutputDirectory <dir>`, `-SkipBuild` to package the binary that already exists.

### 6. Publish

`New-HaRelease.ps1` never publishes; the GitHub release is created by hand, and publishing to npm then follows automatically (section 7). The flow used for 0.2.2 and 0.2.3:

1. Bump `version` under `[workspace.package]` in `Cargo.toml` and update the workspace crates' versions in `Cargo.lock` (a plain `cargo build` rewrites them), then commit `chore(release): ha X.Y.Z`.
2. Run the checks of section 4, then `pwsh -NoProfile -File scripts/New-HaRelease.ps1`. It writes `target\release-candidate\ha-X.Y.Z-windows-x64.zip`.
3. Tag and push: `git tag -a ha-vX.Y.Z -m "ha X.Y.Z"` and `git push origin ha-vX.Y.Z`.
4. Create the GitHub release with the zip and the bundle's `checksums.txt` plus your notes: `gh release create ha-vX.Y.Z <zip> <checksums.txt> --title "ha X.Y.Z" --notes-file <release-notes.md>`.

`-PublishDryRun` prints the tag, the archive's SHA-256 and the `git tag`, `git push` and `gh release create` commands it would suggest, and whether `gh` or `GH_TOKEN` is available. Publishing the release triggers [`npm-publish.yml`](../.github/workflows/npm-publish.yml).

### 7. Publish to npm

`ha` is installed with `npm install -g harness-agents` (Windows x64 only; the package declares `os: win32`, `cpu: x64`, so npm refuses the install elsewhere). There is a single package, `harness-agents`, with no per-platform optional packages: it carries the `ha` launcher, `ha.exe`, `uv.exe`, `ha.release.json` and `checksums.txt`. It is built from the release bundle, never from a fresh build, so npm ships the bytes the GitHub release checksums cover.

#### Publishing from GitHub (the normal path)

[`npm-publish.yml`](../.github/workflows/npm-publish.yml) runs when a GitHub release is published (or by hand with a tag, and an optional `package_version`). It stores no npm token: npm trusts the workflow through OpenID Connect (Trusted Publisher) and records a provenance statement linking the package to this repository. On a Windows runner with Node 24 and npm 11.5.1 or newer it downloads the release's `ha-*-windows-x64.zip` with `gh`, packs it with `New-HaNpmPackages.ps1` and runs `npm publish --provenance`. A release without that zip is skipped with a notice, and a version already on npm is a no-op rather than a failure.

Set it up once on npmjs.com: package `harness-agents` > Settings > Trusted Publisher > GitHub Actions, owner `DEVfancybear`, repository `harness-agents`, workflow `npm-publish.yml`.

#### Publishing from your terminal

For the first publish of a name, or a version that differs from the release:

```powershell
pwsh -NoProfile -File scripts/New-HaRelease.ps1      # the bundle
pwsh -NoProfile -File scripts/New-HaNpmPackages.ps1  # checks the bundle's checksums, writes target\npm\*.tgz
npm login
npm publish target\npm\harness-agents-<version>.tgz --access public
```

The script prints the exact command. The package takes the bundle's version; `-PackageVersion` sets a higher one to republish the same build, because npm never accepts a version twice. The sources are in [`npm/`](../npm/harness-agents/package.json); the `version` in that `package.json` is a placeholder, and the packing script writes the real one.

## Tiếng Việt

### 1. Cần có

- Windows 10/11 x64. Linux đang chờ hỗ trợ: vẫn build được nhưng chưa phải nền tảng được hỗ trợ.
- [Rust](https://rustup.rs). Toolchain được pin trong [`rust-toolchain.toml`](../rust-toolchain.toml) (1.97.1, kèm `clippy` và `rustfmt`) được cài tự động ở lệnh `cargo` đầu tiên.
- PowerShell 7 (`pwsh`) để chạy script, và Git.
- MSVC build tools mà `rustup` yêu cầu (Visual Studio Build Tools, mục "Desktop development with C++").
- [`uv`](https://docs.astral.sh/uv/) để dựng môi trường cho Python kernel. Bạn không cần tự cài: installer và gói release đặt sẵn một `uv.exe` (0.12.19) đã pin phiên bản và kiểm checksum cạnh `ha.exe`; `ha` tìm uv ở đó trước, rồi tới `HA_UV`, rồi `PATH`. Máy không có mạng thì cài uv (`winget install astral-sh.uv`) hoặc chép `uv.exe` vào cạnh `ha.exe`.

### 2. Build

```powershell
cargo build -p harness-cli --bin ha --locked              # debug: target\debug\ha.exe
cargo build -p harness-cli --bin ha --locked --release    # release: target\release\ha.exe
```

Chạy bản vừa build mà không cài: `cargo run -p harness-cli --bin ha -- <args>`, hoặc `.\target\release\ha.exe <args>`.

### 3. Cài để gõ `ha` ở mọi terminal

```powershell
pwsh -NoProfile -File scripts/Install-Ha.ps1
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
cargo test --workspace --locked
```

`cargo test` bỏ qua các ca terminal (PTY) đánh dấu `#[ignore]` vì chúng cần console thật. Chạy bộ này từ một console cục bộ bằng `pwsh -NoProfile -File scripts/Invoke-HaPtyAcceptance.ps1` (xem [TUI.md](TUI.md)); `-Filter <tên>` chạy một phần.

[`ci.yml`](../.github/workflows/ci.yml) chạy các gate `scripts/Verify-Phase.ps1` (P0, P3, P5, P6, P7), `scripts/Verify-Milestone.ps1` và `scripts/Verify-HaLaunch.ps1` trên Windows (job Ubuntu không chặn), còn [`docs.yml`](../.github/workflows/docs.yml) chạy `scripts/Verify-Docs.ps1 -SelfTest`. Các gate này chạy toàn bộ test workspace; test nào fail trong lượt chạy đông đúc đó sẽ được chạy lại riêng, và gate chỉ đỏ khi test cũng fail lúc chạy riêng (`GATE_TEST_FAILED`, `GATE_TEST_RETRIED` trong log).

### 5. Build bản release candidate

```powershell
pwsh -NoProfile -File scripts/New-HaRelease.ps1                  # build release + đóng gói
pwsh -NoProfile -File scripts/New-HaRelease.ps1 -PublishDryRun   # in thêm các lệnh phát hành sẽ chạy
```

Script ghi ra `target\release-candidate\ha-<version>-windows-x64\` và một file `.zip`. Gói chứa đúng các file sau, và script báo lỗi nếu có file khác:

| File | Nội dung |
| --- | --- |
| `ha.exe` | File thực thi release của revision bạn đã build |
| `uv.exe` | Bản `uv` đã pin mà Python kernel dùng để dựng môi trường (kiểm checksum khi đóng gói) |
| `ha.release.json` | Target, version, rustc, source revision và digest của file thực thi |
| `checksums.txt` | SHA-256 của mọi file khác trong gói |

Tự kiểm tra một gói:

```powershell
Get-FileHash .\target\release-candidate\ha-*-windows-x64\ha.exe -Algorithm SHA256
Get-Content  .\target\release-candidate\ha-*-windows-x64\checksums.txt
```

Tùy chọn: `-Profile Debug` cho bản thử nhanh, `-OutputDirectory <dir>`, `-SkipBuild` để đóng gói bản đã build sẵn.

### 6. Phát hành

`New-HaRelease.ps1` không bao giờ tự phát hành; GitHub release được tạo bằng tay, rồi việc phát hành lên npm chạy tự động (mục 7). Quy trình đã dùng cho 0.2.2 và 0.2.3:

1. Nâng `version` trong `[workspace.package]` của `Cargo.toml` và cập nhật version các crate workspace trong `Cargo.lock` (một lệnh `cargo build` thường sẽ ghi lại), rồi commit `chore(release): ha X.Y.Z`.
2. Chạy các kiểm tra ở mục 4, rồi `pwsh -NoProfile -File scripts/New-HaRelease.ps1`. Script ghi `target\release-candidate\ha-X.Y.Z-windows-x64.zip`.
3. Tạo tag và push: `git tag -a ha-vX.Y.Z -m "ha X.Y.Z"` và `git push origin ha-vX.Y.Z`.
4. Tạo GitHub release kèm zip, `checksums.txt` của bundle và ghi chú: `gh release create ha-vX.Y.Z <zip> <checksums.txt> --title "ha X.Y.Z" --notes-file <release-notes.md>`.

`-PublishDryRun` in ra tag, SHA-256 của file zip, các lệnh `git tag`, `git push`, `gh release create` mà nó gợi ý, và cho biết `gh` hoặc `GH_TOKEN` có sẵn không. Việc publish release sẽ kích hoạt [`npm-publish.yml`](../.github/workflows/npm-publish.yml).

### 7. Phát hành lên npm

`ha` được cài bằng `npm install -g harness-agents` (chỉ Windows x64; package khai báo `os: win32`, `cpu: x64` nên npm từ chối cài ở nơi khác). Chỉ có một package duy nhất, `harness-agents`, không có package tùy chọn theo từng nền tảng: nó mang launcher `ha`, `ha.exe`, `uv.exe`, `ha.release.json` và `checksums.txt`. Nó được đóng từ bundle release chứ không build lại, để npm phát đúng các byte mà checksum của GitHub release bao phủ.

#### Phát hành từ GitHub (đường chính)

[`npm-publish.yml`](../.github/workflows/npm-publish.yml) chạy khi một GitHub release được publish (hoặc chạy tay với một tag và `package_version` tùy chọn). Workflow không lưu token npm nào: npm tin workflow qua OpenID Connect (Trusted Publisher) và ghi một bản provenance nối package với repo này. Trên runner Windows với Node 24 và npm 11.5.1 trở lên, workflow tải `ha-*-windows-x64.zip` của release bằng `gh`, đóng bằng `New-HaNpmPackages.ps1` rồi chạy `npm publish --provenance`. Release không có zip đó thì bị bỏ qua kèm một thông báo; version đã có trên npm thì không làm gì thay vì báo lỗi.

Thiết lập một lần trên npmjs.com: package `harness-agents` > Settings > Trusted Publisher > GitHub Actions, owner `DEVfancybear`, repository `harness-agents`, workflow `npm-publish.yml`.

#### Phát hành từ terminal

Dùng cho lần publish đầu của một tên, hoặc một version khác với release:

```powershell
pwsh -NoProfile -File scripts/New-HaRelease.ps1      # bundle
pwsh -NoProfile -File scripts/New-HaNpmPackages.ps1  # kiểm checksum của bundle, ghi target\npm\*.tgz
npm login
npm publish target\npm\harness-agents-<version>.tgz --access public
```

Script in đúng lệnh này. Package lấy version của bundle; `-PackageVersion` đặt version cao hơn để phát lại cùng một bản build, vì npm không bao giờ nhận một version hai lần. Mã nguồn nằm ở [`npm/`](../npm/harness-agents/package.json); `version` trong `package.json` đó chỉ là giá trị giữ chỗ, script đóng gói ghi version thật.
