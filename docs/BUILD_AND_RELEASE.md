# Build and release `ha` / Build và release `ha`

**Language / Ngôn ngữ:** [English](#english) · [Tiếng Việt](#tiếng-việt)

## English

### 1. Prerequisites

- Windows 10/11 x64. Linux support is pending: it builds, but it is not a supported platform yet.
- [Rust](https://rustup.rs). The pinned toolchain in [`rust-toolchain.toml`](../rust-toolchain.toml) (1.97.1, with `clippy` and `rustfmt`) is installed automatically by the first `cargo` command.
- PowerShell 7 (`pwsh`) for the scripts, and Git.
- The MSVC build tools that `rustup` asks for (Visual Studio Build Tools, "Desktop development with C++").

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

The installer builds the release binary, verifies the staged copy (digest and `--version`), keeps a backup until the new file is in place, and installs to `%USERPROFILE%\.cargo\bin`. Useful switches:

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
pwsh -NoProfile -File scripts/Verify-Docs.ps1
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P0          # the same gate CI runs
pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M9  # packaging milestone
```

The phase and milestone gates run the whole workspace suite. A test that fails in that crowded run is re-run alone; the gate fails only when it also fails alone, and the log lists it (`GATE_TEST_FAILED`, `GATE_TEST_RETRIED`).

### 5. Build a release candidate

```powershell
pwsh -NoProfile -File scripts/New-HaRelease.ps1                  # release build + bundle
pwsh -NoProfile -File scripts/New-HaRelease.ps1 -PublishDryRun   # also print what publishing would run
```

It writes `target\release-candidate\ha-<version>-windows-x64\` and a `.zip` of it. The bundle contains exactly three files, and the script fails if anything else appears:

| File | Content |
| --- | --- |
| `ha.exe` | The release executable of the revision you built |
| `ha.release.json` | Target, version, rustc, source revision and the executable's digest |
| `checksums.txt` | SHA-256 of every other file in the bundle |

Verify a bundle by hand:

```powershell
Get-FileHash .\target\release-candidate\ha-*-windows-x64\ha.exe -Algorithm SHA256
Get-Content  .\target\release-candidate\ha-*-windows-x64\checksums.txt
```

Options: `-Profile Debug` for a quick local candidate, `-OutputDirectory <dir>`, `-SkipBuild` to package the binary that already exists.

### 6. Publish

Nothing publishes automatically: there is no registry and no release job. `-PublishDryRun` prints the tag (`ha-v<version>`) and the exact `git tag`, `git push` and `gh release create` commands, and whether `gh` or `GH_TOKEN` is available. Run those yourself after the gates are green, and upload `checksums.txt` next to the archive.

## Tiếng Việt

### 1. Cần có

- Windows 10/11 x64. Linux đang chờ hỗ trợ: vẫn build được nhưng chưa phải nền tảng được hỗ trợ.
- [Rust](https://rustup.rs). Toolchain được pin trong [`rust-toolchain.toml`](../rust-toolchain.toml) (1.97.1, kèm `clippy` và `rustfmt`) được cài tự động ở lệnh `cargo` đầu tiên.
- PowerShell 7 (`pwsh`) để chạy script, và Git.
- MSVC build tools mà `rustup` yêu cầu (Visual Studio Build Tools, mục "Desktop development with C++").

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

Installer build bản release, kiểm tra bản sao tạm (digest và `--version`), giữ bản dự phòng cho đến khi file mới vào đúng chỗ, và cài vào `%USERPROFILE%\.cargo\bin`. Các tùy chọn hay dùng:

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
pwsh -NoProfile -File scripts/Verify-Docs.ps1
pwsh -NoProfile -File scripts/Verify-Phase.ps1 -Phase P0          # đúng gate CI chạy
pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M9  # milestone đóng gói
```

Gate phase và milestone chạy toàn bộ test workspace. Test nào fail trong lượt chạy đông đúc đó sẽ được chạy lại riêng; gate chỉ đỏ khi test cũng fail lúc chạy riêng, và log ghi rõ (`GATE_TEST_FAILED`, `GATE_TEST_RETRIED`).

### 5. Build bản release candidate

```powershell
pwsh -NoProfile -File scripts/New-HaRelease.ps1                  # build release + đóng gói
pwsh -NoProfile -File scripts/New-HaRelease.ps1 -PublishDryRun   # in thêm các lệnh phát hành sẽ chạy
```

Script ghi ra `target\release-candidate\ha-<version>-windows-x64\` và một file `.zip`. Gói chứa đúng ba file, và script báo lỗi nếu có file khác:

| File | Nội dung |
| --- | --- |
| `ha.exe` | File thực thi release của revision bạn đã build |
| `ha.release.json` | Target, version, rustc, source revision và digest của file thực thi |
| `checksums.txt` | SHA-256 của mọi file khác trong gói |

Tự kiểm tra một gói:

```powershell
Get-FileHash .\target\release-candidate\ha-*-windows-x64\ha.exe -Algorithm SHA256
Get-Content  .\target\release-candidate\ha-*-windows-x64\checksums.txt
```

Tùy chọn: `-Profile Debug` cho bản thử nhanh, `-OutputDirectory <dir>`, `-SkipBuild` để đóng gói bản đã build sẵn.

### 6. Phát hành

Không có gì tự động phát hành: không có registry, không có job release. `-PublishDryRun` in ra tag (`ha-v<version>`), đúng các lệnh `git tag`, `git push`, `gh release create`, và cho biết `gh` hoặc `GH_TOKEN` có sẵn không. Tự chạy các lệnh đó sau khi các gate xanh, và tải `checksums.txt` lên cạnh file zip.
