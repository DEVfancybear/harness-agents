# Documentation index

Start with the [project README](../README.md) for installation and a feature
summary. These guides describe the implemented CLI and host; the workspace layout
prepares for future app work, but no separate app is shipped.

Bắt đầu từ [README của dự án](../README.md) để cài đặt và xem tính năng. Các hướng
dẫn mô tả CLI và host hiện có; bố cục workspace chuẩn bị cho app sau này, chưa có
app riêng được phát hành.

## Find by task / Tra theo việc cần làm

| Task / Nhu cầu | Read / Tài liệu |
| --- | --- |
| Commands, configuration, memory, knowledge and automation / Lệnh, cấu hình, trí nhớ và tự động hóa | [Operator guide (EN)](guides/OPERATOR_GUIDE.en.md) · [Hướng dẫn vận hành (VI)](guides/OPERATOR_GUIDE.vi.md) |
| Agent sidebar, child panes and keyboard controls / Sidebar agent, khung agent con và phím tắt | [Terminal interface](guides/TUI.md) |
| Screenshots, mouse and keyboard on Windows / Chụp màn hình, chuột và bàn phím Windows | [Computer use](guides/COMPUTER_USE.md) (in development / đang phát triển) |
| Jev beside the agent and the harness (plugin, off by default) / Jev bên cạnh agent và harness (plugin, mặc định tắt) | [Jev workflow](guides/JEV.en.md) · [Workflow Jev](guides/JEV.vi.md) |
| Build, test, package and publish / Build, kiểm thử, đóng gói và phát hành | [Build and release](guides/BUILD_AND_RELEASE.md) |
| Source ownership, dependencies and folder layout / Trách nhiệm source, dependency và bố cục | [Architecture (EN)](architecture/ARCHITECTURE_OVERVIEW.en.md) · [Kiến trúc (VI)](architecture/ARCHITECTURE_OVERVIEW.vi.md) |
| Skills, extensions and MCP / Skill, extension và MCP | [Plugin architecture (EN)](architecture/PLUGIN_ARCHITECTURE.en.md) · [Kiến trúc plugin (VI)](architecture/PLUGIN_ARCHITECTURE.vi.md) |
| Isolation limits and measured support / Giới hạn cô lập và hỗ trợ đã đo | [Strict execution support](support/STRICT_EXECUTION_SUPPORT.vi.md) |

## Source and test tools / Công cụ source và kiểm thử

Scripts are grouped in `scripts/common/`, `install/`, `release/`, `testing/` and
`verify/`. See the build guide and live-test README above for current commands.
/ Script được nhóm theo trách nhiệm; xem hướng dẫn build và README live-test ở trên.
