# Support matrix — strict execution backend

**Đo trên host nào, bằng gì, khi nào.** Ma trận này là kết quả của probe thật (`ha sandbox probe`,
`crates/harness-tools/src/execution/probe.rs`), không phải một tuyên bố thiết kế. Mỗi verdict có `probe_id` và
`observation` đi kèm trong `CapabilityMatrix`; ở đây chỉ in lại verdict.

[ADR-N12](../adr/ADR-N12-STRICT-BACKEND-AND-CAPABILITIES.vi.md) · [SPEC M12](../specs/M12.vi.md) · [Evidence M12](../evidence/M12.vi.md)

## 1. Host đã đo

| Trường | Giá trị (đọc từ máy, không viết từ trí nhớ) |
|---|---|
| `os` | `windows` |
| `os_version` | `10.0.26200.9457 (25H2)` — `cmd /C ver` + `reg query … CurrentVersion` |
| `arch` | `x86_64` |
| `backend` | `process-wrap` |
| `backend_version` | `10.0.0` (hằng số trong code, có test đối chiếu `Cargo.lock`) |

`Win32_OperatingSystem.Caption` nói `Microsoft Windows 11 Pro` trong khi registry `ProductName` vẫn nói
`Windows 10 Pro`; cả hai được ghi lại vì chính máy không thống nhất. Một host khác **phải** tạo ra một matrix khác:
`os_version` được đo ở mỗi lần probe và test `m12_01_capability_matrix_is_measured_on_this_host` khẳng định câu trả
lời của nền tảng nằm trong những gì matrix ghi.

## 2. Ma trận (Windows, build 26200)

| Capability | Verdict | Ai enforce | Cơ chế / bằng chứng |
|---|---|---|---|
| `process_containment` | **enforced** | OS | Job Object; child tạo `CREATE_SUSPENDED` rồi assign **trước** khi resume. Probe `P-CONT`: descendant ghi tick dừng hẳn sau khi run trả về, trên cả nhánh cancel và nhánh direct-child-thoát-trước |
| `process_tree_kill` | **enforced** | OS | `TerminateJobObject` + `wait`; probe `P-TREE`: 4 writer sống, cancel ở ~1.6 s, tick đứng yên qua các cửa sổ liên tiếp |
| `environment_allowlist` | **enforced** | host (không phải OS) | `env_clear()` + allowlist + binding đã grant, ngay trước spawn; probe `P-ENV`: canary ngoài allowlist **không** tới được child, biến trong allowlist thì có |
| `deadline_enforced` | **enforced** | host | `tokio::select!` timeout ⇒ kill cả cây; probe `P-DEADLINE` |
| `output_bounds` | **enforced** | host | spool writer theo quota + redaction khi ghi; probe `P-OUTPUT` (256 KiB vào, quota 64 KiB) |
| `filesystem_read_confinement` | **unsupported** | — | probe `P-FS-R`: child **đã đọc** canary ngoài run root và trả về nội dung (digest kèm theo) |
| `filesystem_write_confinement` | **unsupported** | — | probe `P-FS-W`: child **đã tạo** file ngoài run root |
| `network_egress_denial` | **unsupported** | — | probe `P-NET`: child **đã** nối `127.0.0.1` và gửi token tới listener của probe |
| `credential_socket_denial` | **unsupported** | — | probe `P-SOCK`: child **đã** mở named pipe mà probe dựng lên như một credential agent |
| `resource_limit_memory` | **unsupported** | — | structural: backend pin chỉ set `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, không expose limit; API đặt limit (`SetInformationJobObject`) cần `unsafe` FFI, workspace **forbid**. Quan sát: child cấp phát 256 MiB mà không bị chặn |
| `resource_limit_process_count` | **unsupported** | — | structural: không có active-process limit; quan sát: child spawn 4 descendant, cả 4 còn sống |

**Profile:**

| Profile | Kết quả trên host này |
|---|---|
| `containment` | phục vụ được — mọi capability nó đòi đều `enforced` |
| `full` (nghĩa của `isolation: strict`) | **bị từ chối**, mã `strict_isolation_unavailable`, nêu tên 6 capability thiếu |

### Khối máy đọc được (test A36 đối chiếu với matrix đo được)

```text
<!-- support-matrix:begin -->
platform=windows
backend=process-wrap
backend_version=10.0.0
measured=true
process_containment=enforced
process_tree_kill=enforced
environment_allowlist=enforced
deadline_enforced=enforced
output_bounds=enforced
filesystem_read_confinement=unsupported
filesystem_write_confinement=unsupported
network_egress_denial=unsupported
credential_socket_denial=unsupported
resource_limit_memory=unsupported
resource_limit_process_count=unsupported
strict_profile_full=refused
<!-- support-matrix:end -->
```

## 3. Nền tảng khác

```text
<!-- support-matrix:begin -->
platform=linux
backend=process-wrap
backend_version=10.0.0
measured=false
strict_profile_full=refused
<!-- support-matrix:end -->
```

**Linux chưa được đo lần nào.** Máy này không có WSL/Docker, và workflow CI `verify-milestones` **chưa chạy lần
nào**, nên không tồn tại bằng chứng Linux nào trong dự án. Cho tới khi có một lần probe thật trên Linux, mọi
capability ở đó được coi là `unsupported` và strict bị từ chối. Test A36 **cố ý đỏ** nếu nó chạy trên một nền tảng
có khối `measured=false`: chạy được probe ở đâu thì phải ghi kết quả ở đó.

`posix_process_session` (process group + `kill`) là cơ chế containment tương ứng trên Unix và đã nằm trong đường chạy
từ M4, nhưng **chưa** được đo trên host Unix nào — nên nó không được ghi là `enforced` ở đâu cả.

## 4. Residual risk (nói ra, không giấu)

- Một tool call chạy trên host này **có** quyền của user đang chạy `ha`: nó đọc/ghi được mọi thứ user đó đọc/ghi
  được, và nối được ra mạng. Containment **không** biến một hành động chưa duyệt thành an toàn; nó bảo đảm vòng đời
  và môi trường.
- `environment_allowlist` được enforce bởi **host**, không bởi OS: nó chặn kế thừa, không chặn một process cùng user
  đọc bộ nhớ của process khác.
- Một descendant thừa hưởng handle stdio *inheritable* của cha (hành vi Windows). Đó là lý do runner **giết cây
  trước khi drain output**; probe `P-CONT` ghi lại điều này.
- `tree_cleanup = reaped_on_exit` **không** chứng minh job rỗng khi direct child thoát trước descendant — xem SPEC M12
  §4b. Không rò rỉ orphan (đã đo), nhưng nhãn thì mạnh hơn bằng chứng.
- Chưa có bằng chứng đa nền tảng, chưa có E2E trong browser/CI, và **chưa** có connector/thông báo ra ngoài.

## 5. Điều gì sẽ đổi ma trận này

Một backend confinement **thật**, với probe xanh **trên host đang chạy**:

1. **AppContainer** (`CreateAppContainerProfile` + `STARTUPINFOEX` + capability SID, bỏ `internetClient` để chặn
   egress): cần một crate **opt-out** khỏi `unsafe_code = "forbid"`, ACL grant cho workspace, và probe chứng minh con
   **không** đọc được canary ngoài grant.
2. **WSL / container / VM** (`bubblewrap`, `nsjail`, OCI): chỉ khi host có chúng và version được đo thật.

Cả hai **ngoài quyền của lượt M12** (SPEC §12) và không có trên máy này. Việc đổi ma trận là việc của một ADR kế tiếp
lật D1/D9 của ADR-N12, kèm probe mới — không phải việc sửa một dòng trong tài liệu này.

## 6. Cách tái tạo

```powershell
cargo run --locked -p harness-cli --bin ha -- sandbox probe --probe-child target/debug/m12_probe_child.exe
cargo run --locked -p harness-cli --bin ha -- sandbox probe --probe-child target/debug/m12_probe_child.exe --require full   # exit khác 0
cargo test -p harness-cli --test milestone_m12 --locked a36_strict_confinement
```
