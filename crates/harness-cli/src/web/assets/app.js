/* The client reducer.
 *
 * Two rules here are protocol, not presentation, and both come from the plan:
 *
 * 1. A cursor is the journal sequence. After a `gap` the client reloads the
 *    committed projection before it trusts the tail again, because a partial
 *    replay that looks complete is worse than a visible gap.
 * 2. Terminal is monotonic. An event that says "running" for a run that has
 *    already finished is ignored, so a late reconnect cannot redraw a finished
 *    run as live.
 */

const state = {
  sessionId: null,
  lastEventId: 0,
  runState: new Map(),
  events: [],
  stream: null,
};

const TERMINAL = new Set(["completed", "failed", "canceled"]);

function setConnection(text) {
  const node = document.getElementById("connection");
  if (node) node.textContent = text;
}

/** Apply one event to the reducer, returning whether anything changed. */
function applyEvent(event) {
  if (!event || typeof event.id !== "number") return false;
  if (event.id <= state.lastEventId) return false; // duplicate or out of order
  state.lastEventId = event.id;
  state.events.push(event);
  if (state.events.length > 200) state.events.shift();

  const runId = event.correlation_id || event.event_id;
  const previous = state.runState.get(runId);
  const next = classify(event);
  if (previous && TERMINAL.has(previous) && !TERMINAL.has(next)) {
    // A terminal state is never left. The event is recorded, the state is not.
    return true;
  }
  if (next) state.runState.set(runId, next);
  return true;
}

/** Which lifecycle state an event names, from its type and never from prose. */
function classify(event) {
  const type = String(event.event_type || "");
  if (type.includes("terminal") || type.includes("accepted")) return "completed";
  if (type.includes("failed")) return "failed";
  if (type.includes("cancel")) return "canceled";
  if (type.includes("step") || type.includes("run.started")) return "running";
  return null;
}

function render() {
  const list = document.getElementById("events");
  if (!list) return;
  list.replaceChildren(
    ...state.events.slice(-40).map((event) => {
      const item = document.createElement("li");
      const id = document.createElement("code");
      id.textContent = `#${event.id}`;
      const kind = document.createElement("span");
      kind.textContent = ` ${event.event_type}`;
      item.append(id, kind);
      return item;
    }),
  );
}

async function loadSessions() {
  const response = await fetch("/api/sessions");
  const body = await response.json();
  const list = document.getElementById("sessions");
  if (!list) return;
  list.replaceChildren(
    ...body.sessions.map((session) => {
      const item = document.createElement("li");
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = `${session.session_id} — ${session.input_count} input`;
      button.addEventListener("click", () => selectSession(session.session_id));
      item.append(button);
      list.append(item);
      return item;
    }),
  );
}

/** Reload committed state, which is what a gap requires before the tail. */
async function reloadProjection(sessionId) {
  const response = await fetch(`/api/sessions/${sessionId}/state`);
  const body = await response.json();
  const summary = document.getElementById("state-summary");
  if (summary) {
    summary.textContent = body.projection
      ? `revision ${body.projection.revision}, tới sequence ${body.projection.through_event_seq}, ${body.projection.plan_items} mục kế hoạch`
      : "chưa có projection đã ghi";
  }
  state.events = [];
  for (const event of body.events || []) applyEvent(event);
  render();
}

function openStream(sessionId) {
  if (state.stream) state.stream.close();
  const url = `/api/sessions/${sessionId}/events?cursor=${state.lastEventId}`;
  const stream = new EventSource(url);
  state.stream = stream;

  stream.addEventListener("open", () => setConnection("đã kết nối"));
  stream.addEventListener("error", () =>
    setConnection("mất kết nối — sẽ thử lại"),
  );
  stream.addEventListener("gap", async (message) => {
    // The gap is explicit: the client must not pretend it saw everything.
    const detail = JSON.parse(message.data);
    setConnection(
      `thiếu sự kiện từ ${detail.cursor} tới ${detail.oldest_retained} — đang tải lại`,
    );
    await reloadProjection(sessionId);
    openStream(sessionId);
  });
  stream.addEventListener("journal", (message) => {
    if (applyEvent(JSON.parse(message.data))) render();
  });
}

async function selectSession(sessionId) {
  state.sessionId = sessionId;
  state.lastEventId = 0;
  state.runState.clear();
  await reloadProjection(sessionId);
  openStream(sessionId);
}

document.addEventListener("DOMContentLoaded", () => {
  loadSessions().catch(() => setConnection("không đọc được danh sách phiên"));
});

// Exported for a test harness that runs this file outside a browser.
if (typeof module !== "undefined" && module.exports) {
  module.exports = { applyEvent, classify, state };
}
