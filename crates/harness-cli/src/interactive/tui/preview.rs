//! Dev-only preview frames: the design, painted without a terminal.
//!
//! `cargo test -p harness-cli --bin ha preview_dump -- --ignored --nocapture`
//! paints a handful of representative frames through the real renderer and writes
//! each one to `target/tui-preview/<scene>.ansi`, serialised cell by cell with SGR
//! colours. A file is therefore exactly what the console would show, which lets a
//! reviewer read (or screenshot) the layout without running the app.
//!
//! The scenes are the states a redesign has to keep readable: a fresh session, a
//! finished turn with tools and a diff, text still streaming, an approval panel,
//! the slash menu, an expanded tool, and a failure.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::backend::{Backend, TestBackend};
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};

use super::theme::{Depth, Theme};
use super::{RealRenderer, history};
use crate::interactive::commands::MenuItem;
use crate::interactive::events::{AppPhase, HistoryItem, Modal, RunOutcome, ToolState, UiState};

/// One frame: what is already in the scrollback, and the viewport state.
struct Scene {
    name: &'static str,
    items: Vec<HistoryItem>,
    state: UiState,
}

/// Paint every scene into `target/tui-preview`.
#[test]
#[ignore = "writes preview frames for a human to look at; run it by hand"]
fn preview_dump() {
    let theme = Theme::prime(Depth::TrueColor);
    let directory = output_directory();
    std::fs::create_dir_all(&directory).expect("create the preview directory");
    // A roomy console and the narrow one the guide calls the minimum, because a
    // redesign has to hold at both: the wide frame is what the design is, and the
    // narrow frame is what a reader with a small window actually gets.
    let sizes = [("", 100_u16, 44_u16), ("-80x24", 80, 24)];
    for (suffix, width, height) in sizes {
        for scene in scenes() {
            let frame = paint(&scene, width, height, &theme);
            let path = directory.join(format!("{}{suffix}.ansi", scene.name));
            std::fs::write(&path, frame).expect("write the frame");
            // The same scene as text, whole: the screen above clips the scrollback to
            // what fits, and a redesign is also read by scrolling a turn end to end.
            let text = directory.join(format!("{}{suffix}.txt", scene.name));
            std::fs::write(&text, transcript(&scene, width, &theme)).expect("write the transcript");
            println!("wrote {}", path.display());
        }
    }
}

fn output_directory() -> PathBuf {
    // `CARGO_TARGET_TMPDIR` is set for integration tests; a unit test inside the
    // binary gets nothing, so fall back to the workspace's own target directory -
    // the one place a build already owns and a checkout ignores.
    let base = option_env!("CARGO_TARGET_TMPDIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        PathBuf::from,
    );
    base.join("tui-preview")
}

/// Every row a scene's items render to, as text and without clipping.
fn transcript(scene: &Scene, width: u16, theme: &Theme) -> String {
    let mut rows: Vec<String> = Vec::new();
    for item in &scene.items {
        for line in history::render(item, width, theme, scene.state.detail) {
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            rows.push(text.trim_end().to_owned());
        }
    }
    rows.join("\n")
}

/// Paint one scene through the real renderer and serialise the screen it produced.
///
/// The renderer is the one the app uses, so the frame includes the inline viewport's
/// real placement: rows pushed into the scrollback above, the viewport at the bottom.
fn paint(scene: &Scene, width: u16, height: u16, theme: &Theme) -> String {
    let mut backend = TestBackend::new(width, height);
    // The app opens where the shell left the cursor, which on a console with a
    // prompt at the bottom is the last row: the inline viewport is then anchored
    // there, exactly as the terminal anchors it.
    backend
        .set_cursor_position(ratatui::layout::Position::new(0, height - 1))
        .expect("place the cursor");
    let mut renderer = RealRenderer::open_with(backend, theme).expect("open the viewport");
    // The detail mode is the renderer's own: ctrl+o toggles it in the app.
    renderer.detail = scene.state.detail;
    for item in &scene.items {
        renderer.insert_history(item).expect("insert the row");
    }
    renderer.draw_state(&scene.state).expect("draw the frame");
    ansi(renderer.terminal.backend().buffer())
}

/// The buffer as text: one SGR run per style change, then the row's symbols.
fn ansi(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut style: Option<(Color, Color, Modifier)> = None;
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            let current = (cell.fg, cell.bg, cell.modifier);
            if style != Some(current) {
                out.push_str(&sgr(cell));
                style = Some(current);
            }
            out.push_str(cell.symbol());
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

fn sgr(cell: &Cell) -> String {
    let mut codes: Vec<String> = vec!["0".to_owned()];
    for (flag, code) in [
        (Modifier::BOLD, "1"),
        (Modifier::DIM, "2"),
        (Modifier::ITALIC, "3"),
        (Modifier::UNDERLINED, "4"),
        (Modifier::REVERSED, "7"),
        (Modifier::CROSSED_OUT, "9"),
    ] {
        if cell.modifier.contains(flag) {
            codes.push(code.to_owned());
        }
    }
    if let Some(code) = colour_code(cell.fg, 38) {
        codes.push(code);
    }
    if let Some(code) = colour_code(cell.bg, 48) {
        codes.push(code);
    }
    format!("\x1b[{}m", codes.join(";"))
}

fn colour_code(colour: Color, layer: u8) -> Option<String> {
    let code = match colour {
        Color::Reset => return None,
        Color::Rgb(red, green, blue) => format!("{layer};2;{red};{green};{blue}"),
        Color::Indexed(index) => format!("{layer};5;{index}"),
        Color::Black => format!("{layer};5;0"),
        Color::Red => format!("{layer};5;1"),
        Color::Green => format!("{layer};5;2"),
        Color::Yellow => format!("{layer};5;3"),
        Color::Blue => format!("{layer};5;4"),
        Color::Magenta => format!("{layer};5;5"),
        Color::Cyan => format!("{layer};5;6"),
        Color::Gray => format!("{layer};5;7"),
        Color::DarkGray => format!("{layer};5;8"),
        Color::LightRed => format!("{layer};5;9"),
        Color::LightGreen => format!("{layer};5;10"),
        Color::LightYellow => format!("{layer};5;11"),
        Color::LightBlue => format!("{layer};5;12"),
        Color::LightMagenta => format!("{layer};5;13"),
        Color::LightCyan => format!("{layer};5;14"),
        Color::White => format!("{layer};5;15"),
    };
    Some(code)
}

// ---------------------------------------------------------------------------
// Scenes
// ---------------------------------------------------------------------------

fn scenes() -> Vec<Scene> {
    vec![
        welcome(),
        turn(),
        streaming(),
        approval(),
        menu(),
        expanded(),
        diff(),
        failure(),
        skills(),
        busy(),
    ]
}

/// A fresh session: the banner, an empty composer and the idle status row.
fn welcome() -> Scene {
    Scene {
        name: "01-welcome",
        items: vec![banner()],
        state: state(AppPhase::Ready),
    }
}

/// One finished turn: prose, three tools (one with a diff), a notice and the
/// end-of-turn line, with a draft waiting in the composer.
fn turn() -> Scene {
    let mut state = state(AppPhase::Ready);
    state.buffer = "giờ thêm test cho giỏ hàng rỗng".to_owned();
    state.cursor = state.buffer.chars().count();
    state.last_request = Some("Sửa lỗi giỏ hàng tính sai tổng tiền".to_owned());
    state.last_run_elapsed = Duration::from_secs(42);
    Scene {
        name: "02-turn",
        items: vec![
            banner(),
            HistoryItem::User {
                text: "Giỏ hàng tính sai tổng tiền khi áp mã giảm giá. Sửa đi rồi chạy test."
                    .to_owned(),
            },
            HistoryItem::Assistant {
                text: ANSWER.to_owned(),
            },
            HistoryItem::Tool {
                name: "read_file".to_owned(),
                summary: "src/lib/cart.ts".to_owned(),
                input: "{\n  \"path\": \"src/lib/cart.ts\"\n}".to_owned(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(320),
                },
            },
            HistoryItem::ToolOutput {
                name: "read_file".to_owned(),
                text: CART_FILE.to_owned(),
                path: Some("src/lib/cart.ts".to_owned()),
            },
            HistoryItem::Tool {
                name: "apply_patch".to_owned(),
                summary: "src/lib/cart.ts".to_owned(),
                input: "{\n  \"path\": \"src/lib/cart.ts\"\n}".to_owned(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(180),
                },
            },
            HistoryItem::ToolOutput {
                name: "apply_patch".to_owned(),
                text: PATCH.to_owned(),
                path: Some("src/lib/cart.ts".to_owned()),
            },
            HistoryItem::Tool {
                name: "bash".to_owned(),
                summary: "npm test".to_owned(),
                input: "{\n  \"command\": \"npm test\"\n}".to_owned(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(1_420),
                },
            },
            HistoryItem::ToolOutput {
                name: "bash".to_owned(),
                text: TEST_OUTPUT.to_owned(),
                path: None,
            },
            HistoryItem::Assistant {
                text: "Xong: `total` cộng theo `quantity` rồi mới áp giảm giá, 12/12 test pass."
                    .to_owned(),
            },
            HistoryItem::Notice {
                message: "refine · 1 entry updated: cart total applies the discount last"
                    .to_owned(),
            },
            HistoryItem::Run {
                outcome: RunOutcome::Done,
                steps: 3,
                tool_calls: 4,
                elapsed: Duration::from_secs(42),
            },
        ],
        state,
    }
}

/// A turn in flight: streamed text, an open tool card, a queued draft.
fn streaming() -> Scene {
    let mut state = state(AppPhase::Running);
    state.live_text = STREAMING.to_owned();
    state.open_tools = vec![("bash".to_owned(), "npm test -- --coverage".to_owned())];
    state.buffer = "nhớ kiểm tra giỏ hàng rỗng nữa".to_owned();
    state.cursor = state.buffer.chars().count();
    state.queued_input = true;
    state.steps = 2;
    state.tool_calls = 3;
    state.run_started_at = started(12);
    Scene {
        name: "03-streaming",
        items: vec![
            banner(),
            HistoryItem::User {
                text: "Thêm test cho giỏ hàng rỗng.".to_owned(),
            },
            HistoryItem::Tool {
                name: "read_file".to_owned(),
                summary: "src/lib/cart.test.ts".to_owned(),
                input: "{\n  \"path\": \"src/lib/cart.test.ts\"\n}".to_owned(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(210),
                },
            },
            HistoryItem::ToolOutput {
                name: "read_file".to_owned(),
                text: "read_file src/lib/cart.test.ts:\n1:import { describe, it, expect } from \"vitest\";\n2:import { total } from \"./cart\";\n3:\n4:describe(\"total\", () => {".to_owned(),
                path: Some("src/lib/cart.test.ts".to_owned()),
            },
        ],
        state,
    }
}

/// An approval panel: the gate that owns the keyboard and the frame.
fn approval() -> Scene {
    let mut state = state(AppPhase::WaitingApproval);
    state.modal = Some(Modal::Approval {
        request_id: "req-7f3a".to_owned(),
        action: "apply_patch".to_owned(),
        summary: "src/lib/cart.ts  (+2 −1)".to_owned(),
        workspace: "C:/work/demo-shop".to_owned(),
        scope: "once".to_owned(),
        expires_at: Instant::now() + Duration::from_secs(284),
        read_only: false,
        scroll: 0,
    });
    state.run_started_at = started(31);
    state.steps = 4;
    state.tool_calls = 6;
    Scene {
        name: "04-approval",
        items: vec![
            banner(),
            HistoryItem::User {
                text: "Sửa lỗi giỏ hàng tính sai tổng tiền.".to_owned(),
            },
            HistoryItem::Assistant {
                text: "Tôi sửa `total` trong `src/lib/cart.ts` để cộng theo `quantity` rồi mới áp giảm giá.".to_owned(),
            },
        ],
        state,
    }
}

/// The slash-command menu, which belongs to the draft above the composer.
/// The `/skills` panel: grouped, with glyphs, versions, badges and wrapped text.
fn skills() -> Scene {
    use crate::interactive::events::{BadgeKind, RefLine};
    let item = |name: &str, meta: &str, active: bool, detail: &str| RefLine::Item {
        glyph: "✦".to_owned(),
        name: name.to_owned(),
        meta: meta.to_owned(),
        badges: if active {
            vec![("active".to_owned(), BadgeKind::Ok)]
        } else {
            Vec::new()
        },
        detail: detail.to_owned(),
    };
    let rich = vec![
        RefLine::Heading {
            title: "● Active".to_owned(),
            note: "2 in this conversation".to_owned(),
        },
        RefLine::Item {
            glyph: "●".to_owned(),
            name: "brainstorming".to_owned(),
            meta: "6.4.1".to_owned(),
            badges: vec![("sha 2eb74439".to_owned(), BadgeKind::Neutral)],
            detail: String::new(),
        },
        RefLine::Blank,
        RefLine::Heading {
            title: "✦ Bundled with ha".to_owned(),
            note: "4".to_owned(),
        },
        item(
            "brainstorming",
            "6.4.1",
            true,
            "You MUST use this before any creative work - creating features, building components, adding functionality, or modifying behavior. Explores user intent first.",
        ),
        item(
            "websearch",
            "e260085",
            false,
            "Search Google via the Serper API. Takes one query and returns titles, URLs, snippets and knowledge-graph data.",
        ),
        item(
            "writing-plans",
            "6.4.1",
            false,
            "Use when you have a spec or requirements for a multi-step task, before touching code",
        ),
        RefLine::Blank,
        RefLine::Hint(
            "The model activates a matching skill by itself; /skill:<name> runs one yourself."
                .to_owned(),
        ),
    ];
    let lines = rich.iter().map(RefLine::plain).collect();
    let mut state = state(AppPhase::Ready);
    state.modal = Some(Modal::Overlay {
        title: "/skills".to_owned(),
        lines,
        rich: Some(rich),
        scroll: 0,
    });
    Scene {
        name: "09-skills",
        items: vec![banner()],
        state,
    }
}

/// Several tool cards in a row, of every family: the case where cards ran together.
fn busy() -> Scene {
    let call = |name: &str, summary: &str, millis: u64| HistoryItem::Tool {
        name: name.to_owned(),
        summary: summary.to_owned(),
        input: String::new(),
        state: ToolState::Ok {
            elapsed: Duration::from_millis(millis),
        },
    };
    let out = |name: &str, path: Option<&str>, text: &str| HistoryItem::ToolOutput {
        name: name.to_owned(),
        path: path.map(str::to_owned),
        text: text.to_owned(),
    };
    Scene {
        name: "10-busy",
        items: vec![
            banner(),
            HistoryItem::User {
                text: "Rà soát dự án và sửa lỗi giỏ hàng".to_owned(),
            },
            call(
                "read_file",
                "limit=260 offset=130 path=docs/REVIEW-2026-09-28.md · allowed by mode full-auto",
                206,
            ),
            out(
                "read_file",
                Some("docs/REVIEW-2026-09-28.md"),
                "read_file docs/REVIEW-2026-09-28.md:
131: |---|---|---|
132: | P0 | **SEO**: `sitemap.ts` | Cao |
133: | P0 | **Lưu đơn bền vững (SQLite)** | Cao |
134: | P1 | Voucher | Trung bình |",
            ),
            call("git_status", "", 202),
            out(
                "git_status",
                None,
                "git_status:
## master...origin/master [behind 3]
 M .gitignore
 M README.md
 M src/app/page.tsx",
            ),
            call("list_files", "path=src", 199),
            out(
                "list_files",
                None,
                "list_files: src/app/api/categories/route.ts, src/app/api/orders/route.ts, src/app/api/products/route.ts",
            ),
            call(
                "search_text",
                "query=globalThis.__shopHaOrders path=src",
                88,
            ),
            call("run_shell", "command=npm test timeout_ms=60000", 1400),
            out(
                "run_shell",
                None,
                "run_shell:
> demo-shop@0.1.0 test
> vitest run
 Test Files  1 passed (1)
      Tests  12 passed (12)",
            ),
            call("delegate", "role=explorer task=Map the order flow", 40),
            call("web_search", "query=next.js sqlite orders", 610),
            HistoryItem::Run {
                outcome: RunOutcome::Done,
                steps: 4,
                tool_calls: 6,
                elapsed: Duration::from_secs(21),
            },
        ],
        state: state(AppPhase::Ready),
    }
}

fn menu() -> Scene {
    let mut state = state(AppPhase::Ready);
    state.buffer = "/re".to_owned();
    state.cursor = 3;
    state.suggestions = vec![
        MenuItem {
            label: "/resume".to_owned(),
            description: "chọn một phiên đã lưu và tiếp tục".to_owned(),
            tag: None,
            completion: "/resume ".to_owned(),
        },
        MenuItem {
            label: "/refine".to_owned(),
            description: "ghi lại bài học của phiên này vào harness".to_owned(),
            tag: Some("local".to_owned()),
            completion: "/refine ".to_owned(),
        },
        MenuItem {
            label: "/reload".to_owned(),
            description: "nạp lại cấu hình, kỹ năng và MCP".to_owned(),
            tag: None,
            completion: "/reload".to_owned(),
        },
    ];
    Scene {
        name: "05-menu",
        items: vec![
            banner(),
            HistoryItem::Assistant {
                text: ANSWER.to_owned(),
            },
        ],
        state,
    }
}

/// Expanded detail: a call's whole input in a box, its output in another.
fn expanded() -> Scene {
    let mut state = state(AppPhase::Ready);
    state.detail = crate::interactive::events::Detail::Expanded;
    state.buffer = "/more".to_owned();
    state.cursor = 5;
    Scene {
        name: "06-expanded",
        items: vec![
            HistoryItem::Tool {
                name: "bash".to_owned(),
                summary: "npm test".to_owned(),
                input: "{\n  \"command\": \"npm test -- --reporter=verbose\",\n  \"timeout_ms\": 120000\n}"
                    .to_owned(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(1_420),
                },
            },
            HistoryItem::ToolOutput {
                name: "bash".to_owned(),
                text: TEST_OUTPUT.to_owned(),
                path: None,
            },
        ],
        state,
    }
}

/// A patch, expanded: the diff keeps its added and removed tints inside the box.
fn diff() -> Scene {
    let mut state = state(AppPhase::Ready);
    state.detail = crate::interactive::events::Detail::Expanded;
    Scene {
        name: "08-diff",
        items: vec![
            HistoryItem::Tool {
                name: "apply_patch".to_owned(),
                summary: "src/lib/cart.ts".to_owned(),
                input: "{\n  \"path\": \"src/lib/cart.ts\"\n}".to_owned(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(180),
                },
            },
            HistoryItem::ToolOutput {
                name: "apply_patch".to_owned(),
                text: PATCH.to_owned(),
                path: Some("src/lib/cart.ts".to_owned()),
            },
        ],
        state,
    }
}

/// A failure: a tool that errored with a reason, and the error row under it.
fn failure() -> Scene {
    let mut state = state(AppPhase::Ready);
    state.fallback_reason = Some("tui tắt: không phải terminal".to_owned());
    Scene {
        name: "07-failure",
        items: vec![
            HistoryItem::User {
                text: "Chạy build production.".to_owned(),
            },
            HistoryItem::Tool {
                name: "bash".to_owned(),
                summary: "npm run build".to_owned(),
                input: "{\n  \"command\": \"npm run build\"\n}".to_owned(),
                state: ToolState::Failed {
                    elapsed: Duration::from_millis(890),
                    detail: "error TS2304: Cannot find name 'applyDiscount'.".to_owned(),
                },
            },
            HistoryItem::Error {
                message: "turn failed: tool bash exited 1".to_owned(),
            },
            HistoryItem::Run {
                outcome: RunOutcome::Failed("tool bash exited 1".to_owned()),
                steps: 2,
                tool_calls: 2,
                elapsed: Duration::from_secs(9),
            },
        ],
        state,
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const ANSWER: &str = r"## Nguyên nhân

Tổng tiền được tính **trước** khi áp mã giảm giá, nên `total` trả về giá gốc:

```ts
export function total(items: Item[]) {
  return items.reduce((sum, item) => sum + item.price, 0);
}
```

Đã sửa và chạy `npm test`:

- 12/12 test pass
- không đổi chữ ký hàm
- thêm một test cho giỏ hàng rỗng
";

const STREAMING: &str = "Tôi đang thêm test cho giỏ hàng rỗng:

- `total([])` phải trả về `0`
- không được ném lỗi khi `code` là `null`
";

const CART_FILE: &str = "read_file src/lib/cart.ts:\n12:export function total(items: Item[]) {\n13:  return items.reduce((sum, item) => sum + item.price, 0);\n14:}\n15:\n16:export function applyDiscount(gross: number, code: string | null) {\n17:  return code ? gross * 0.9 : gross;\n18:}";

const PATCH: &str = "--- a/src/lib/cart.ts\n+++ b/src/lib/cart.ts\n@@ -12,7 +12,8 @@ export function total(items: Item[]) {\n-  return items.reduce((sum, item) => sum + item.price, 0);\n+  const gross = items.reduce((sum, item) => sum + item.price * item.quantity, 0);\n+  return applyDiscount(gross, code);\n }";

const TEST_OUTPUT: &str = "npm test\n\n> demo-shop@0.1.0 test\n> vitest run\n\n ✓ src/lib/cart.test.ts (12)\n\n Test Files  1 passed (1)\n      Tests  12 passed (12)\n   Duration  1.42s";

fn banner() -> HistoryItem {
    HistoryItem::Banner {
        lines: vec![
            "Harness Agents 0.1.6".to_owned(),
            "Project: C:/work/demo-shop".to_owned(),
            "Service: deepseek-v4-flash via https://api.deepseek.com".to_owned(),
            "Permissions: ask".to_owned(),
            "Git: master".to_owned(),
            "Thinking: high".to_owned(),
            "Nhập yêu cầu.".to_owned(),
        ],
    }
}

fn started(seconds: u64) -> Option<Instant> {
    Instant::now().checked_sub(Duration::from_secs(seconds))
}

fn state(phase: AppPhase) -> UiState {
    UiState {
        phase,
        setup_required: false,
        setup_hint: None,
        header: vec![
            "Project: C:/work/demo-shop".to_owned(),
            "Service: deepseek-v4-flash via https://api.deepseek.com".to_owned(),
            "Permissions: ask".to_owned(),
            "Context: 42% of 200k".to_owned(),
            "Cost: $0.0123".to_owned(),
        ],
        buffer: String::new(),
        cursor: 0,
        live_text: String::new(),
        open_tools: Vec::new(),
        modal: None,
        granted_for_run: false,
        queued_input: false,
        queued_count: 0,
        provider_wait: None,
        last_request: None,
        run_started_at: None,
        last_run_elapsed: Duration::from_secs(7),
        steps: 0,
        max_steps: 8,
        tool_calls: 0,
        max_tool_calls: 16,
        suggestions: Vec::new(),
        suggestion_selected: 0,
        fallback_reason: None,
        tick: 2,
        detail: crate::interactive::events::Detail::Collapsed,
        thinking: Some("high".to_owned()),
        service_tier: None,
        goal: None,
    }
}
