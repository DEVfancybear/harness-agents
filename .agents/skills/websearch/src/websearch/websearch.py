"""Websearch skill implementation."""

from __future__ import annotations

import importlib.util
import json
import os
import time
from pathlib import Path

# httpx is imported inside `_post` on first call: importing it at module level
# would put its full import cost on every kernel bootstrap, in every session,
# even when no search is ever run. `find_spec` keeps the import-time contract —
# importing this module still fails with the same "No module named 'httpx'"
# when httpx is absent, so a kernel without httpx reports the same
# unavailable-skill error as before.
if importlib.util.find_spec("httpx") is None:
    raise ModuleNotFoundError("No module named 'httpx'")

SERPER_ENDPOINT = "https://google.serper.dev/search"
DEEPSEEK_TOOLS_ENDPOINT = "https://api.deepseek.com/v1/tools"
DEEPSEEK_MAX_QUERY_CHARS = 128
DEFAULT_ORDER = ("deepseek", "serper")
LABELS = {"deepseek": "DeepSeek Search", "serper": "Google via Serper"}
LOGIN = {"deepseek": "/login deepseek", "serper": "/login serper"}
# A key an API refused (invalid, expired, out of balance) is skipped this long.
REFUSED_KEY_RETRY_SECONDS = 30 * 60
_refused: dict[tuple[str, int], float] = {}


class SearchFailure(Exception):
    """One API did not answer; `key_refused` when it refused the key itself."""

    def __init__(self, message: str, key_refused: bool = False) -> None:
        super().__init__(message)
        self.key_refused = key_refused


def _env_int(name: str, default: int) -> int:
    """Read an int from the environment, falling back to default on bad values."""
    try:
        return int(os.environ[name])
    except (KeyError, ValueError):
        return default


def _agent_dir() -> Path:
    """Resolve the Prime Agent config dir the same way the runtime does."""
    raw = (
        os.environ.get("PRIME_AGENT_CODING_AGENT_DIR")
        or os.environ.get("PI_CODING_AGENT_DIR")
        or str(Path.home() / ".prime" / "agent")
    )
    return Path(raw).expanduser()


def _saved_key(provider: str) -> str:
    # Read the credential file on each call (not just the injected env var) so
    # a key added via /login after the kernel started is still picked up.
    ha_file = os.environ.get("HA_CREDENTIAL_FILE", "").strip()
    try:
        auth = json.loads(
            (Path(ha_file) if ha_file else _agent_dir() / "auth.json").read_text()
        )
        cred = auth.get(provider) if isinstance(auth, dict) else None
        if isinstance(cred, dict) and cred.get("type") == "api_key":
            return _resolve_config_value(str(cred.get("key") or ""))
    except (OSError, ValueError):
        pass
    return ""


def _resolve_api_key(provider: str = "serper") -> str:
    """The key of one search API: its variable wins, else the saved one."""
    variable = {"serper": "SERPER_API_KEY", "deepseek": "DEEPSEEK_API_KEY"}[provider]
    env_key = os.environ.get(variable, "").strip()
    if env_key:
        return env_key
    return _saved_key(provider)


def _resolve_config_value(value: str) -> str:
    # Stored keys may be a literal or an env-var name; "!command" refs can't be run
    # safely here, so skip them (the agent injects those resolved at build time).
    value = value.strip()
    if not value or value.startswith("!"):
        return ""
    return (os.environ.get(value) or value).strip()


def _order() -> list[str]:
    """The search APIs in the order they are asked: HA_WEB_SEARCH, else the default."""
    raw = os.environ.get("HA_WEB_SEARCH", "").strip()
    ids = [part.strip().lower() for part in raw.split(",")] if raw else list(DEFAULT_ORDER)
    order: list[str] = []
    for api in ids:
        api = "serper" if api == "google" else api
        if api in LABELS and api not in order:
            order.append(api)
    return order


def _refused_since(api: str, key: str) -> float | None:
    at = _refused.get((api, hash(key)))
    if at is None:
        return None
    since = time.monotonic() - at
    if since >= REFUSED_KEY_RETRY_SECONDS:
        _refused.pop((api, hash(key)), None)
        return None
    return since


def _deepseek_query(query: str) -> str:
    """At most 128 characters, cut at a word: DeepSeek Search's limit."""
    if len(query) <= DEEPSEEK_MAX_QUERY_CHARS:
        return query
    cut = query[:DEEPSEEK_MAX_QUERY_CHARS]
    space = cut.rfind(" ")
    return cut[:space].rstrip() if space > DEEPSEEK_MAX_QUERY_CHARS // 2 else cut


def _format_serper_results(data: dict, query: str, num_results: int = 5) -> str:
    """Format a Serper API response into readable text."""
    sections: list[str] = []

    kg = data.get("knowledgeGraph")
    if kg:
        kg_lines: list[str] = []
        title = (kg.get("title") or "").strip()
        if title:
            kg_lines.append(f"Knowledge Graph: {title}")
        description = (kg.get("description") or "").strip()
        if description:
            kg_lines.append(description)
        for key, value in (kg.get("attributes") or {}).items():
            text = str(value).strip()
            if text:
                kg_lines.append(f"{key}: {text}")
        if kg_lines:
            sections.append("\n".join(kg_lines))

    for i, result in enumerate((data.get("organic") or [])[:num_results]):
        title = (result.get("title") or "").strip() or "Untitled"
        lines = [f"Result {i}: {title}"]
        link = (result.get("link") or "").strip()
        if link:
            lines.append(f"URL: {link}")
        snippet = (result.get("snippet") or "").strip()
        if snippet:
            lines.append(snippet)
        sections.append("\n".join(lines))

    people_also_ask = data.get("peopleAlsoAsk") or []
    if people_also_ask:
        max_q = max(1, min(3, len(people_also_ask)))
        questions: list[str] = []
        for item in people_also_ask[:max_q]:
            question = (item.get("question") or "").strip()
            if not question:
                continue
            entry = f"Q: {question}"
            answer = (item.get("snippet") or "").strip()
            if answer:
                entry += f"\nA: {answer}"
            questions.append(entry)
        if questions:
            sections.append("People Also Ask:\n" + "\n".join(questions))

    if not sections:
        return f"No results returned for query: {query}"

    return "\n\n---\n\n".join(sections)


def _format_deepseek_results(data: dict, query: str, num_results: int = 5) -> str:
    """Format a DeepSeek Search response like the Serper one."""
    sections: list[str] = []
    for i, result in enumerate((data.get("results") or [])[:num_results]):
        title = (result.get("title") or "").strip() or "Untitled"
        lines = [f"Result {i}: {title}"]
        url = (result.get("url") or "").strip()
        if url:
            lines.append(f"URL: {url}")
        about = [
            (result.get(key) or "").strip()
            for key in ("site_name", "date")
            if (result.get(key) or "").strip()
        ]
        if about:
            lines.append(" - ".join(about))
        summary = (result.get("summary") or result.get("snippet") or "").strip()
        if summary:
            lines.append(summary)
        sections.append("\n".join(lines))
    if not sections:
        return f"No results returned for query: {query}"
    return "\n\n---\n\n".join(sections)


async def _post(url: str, payload: dict, headers: dict, timeout: int, refuses_key) -> dict:
    import httpx

    try:
        async with httpx.AsyncClient(timeout=timeout) as client:
            resp = await client.post(url, json=payload, headers=headers)
    except httpx.HTTPError as e:
        raise SearchFailure(f"the request failed: {e}") from e
    if resp.status_code >= 400:
        body = resp.text[:300]
        raise SearchFailure(
            f"answered {resp.status_code} ({body})",
            key_refused=refuses_key(resp.status_code, body),
        )
    try:
        return resp.json()
    except ValueError as e:
        raise SearchFailure("did not answer with JSON") from e


async def _fetch_serper(query: str, api_key: str, timeout: int = 45, num_results: int = 5) -> str:
    """Execute a single Serper API search."""
    data = await _post(
        SERPER_ENDPOINT,
        {"q": query},
        {"X-API-KEY": api_key, "Content-Type": "application/json"},
        timeout,
        lambda status, body: status in (401, 402, 403) or "credits" in body.lower(),
    )
    return _format_serper_results(data, query, num_results=num_results)


async def _fetch_deepseek(query: str, api_key: str, timeout: int = 45, num_results: int = 5) -> str:
    """Execute a single DeepSeek Search API call (POST /v1/tools/search)."""
    base = os.environ.get("HA_DEEPSEEK_TOOLS_URL", "").strip() or DEEPSEEK_TOOLS_ENDPOINT
    data = await _post(
        base.rstrip("/") + "/search",
        {"query": _deepseek_query(query), "count": num_results, "need_content": False},
        {"Authorization": f"Bearer {api_key}", "Content-Type": "application/json"},
        timeout,
        lambda status, _body: status in (401, 402),
    )
    return _format_deepseek_results(data, query, num_results=num_results)


async def run(
    query: str,
    *,
    max_output: int = 8192,
    timeout: int | None = None,
    num_results: int | None = None,
) -> str:
    """Search the web with the search APIs that have a key, in order.

    DeepSeek Search (the DeepSeek key) then Google via Serper (the Serper key)
    by default; HA_WEB_SEARCH reorders them. One that fails hands the query to
    the next, and a key an API refused is skipped for a while.

    Args:
        query: Search query.
        max_output: Truncate output to this many chars.
        timeout: HTTP timeout in seconds.
        num_results: Results to return.

    Returns:
        Formatted search results.
    """
    if timeout is None:
        timeout = _env_int("PRIME_AGENT_WEBSEARCH_TIMEOUT", 45)
    if num_results is None:
        num_results = _env_int("PRIME_AGENT_WEBSEARCH_NUM_RESULTS", 5)

    fetchers = {"deepseek": _fetch_deepseek, "serper": _fetch_serper}
    notes: list[str] = []
    renew: list[str] = []
    result = None
    source = ""
    keyed = False
    for api in _order():
        key = _resolve_api_key(api)
        if not key:
            continue
        keyed = True
        since = _refused_since(api, key)
        if since is not None:
            notes.append(f"{LABELS[api]} skipped: it refused its key {int(since // 60)} min ago")
            renew.append(LOGIN[api])
            continue
        try:
            result = await fetchers[api](query, key, timeout=timeout, num_results=num_results)
            source = LABELS[api]
            break
        except SearchFailure as e:
            if e.key_refused:
                _refused[(api, hash(key))] = time.monotonic()
                renew.append(LOGIN[api])
            notes.append(f"{LABELS[api]} failed: {e}")
        except Exception as e:  # noqa: BLE001 - one API's fault must not end the search
            notes.append(f"{LABELS[api]} failed: {e}")

    if not keyed:
        return (
            "This Python skill needs a DeepSeek or Serper API key, and none is configured.\n"
            "Use the web_search tool instead: it needs no key, and web_fetch opens the results.\n"
            "The user can run /login deepseek (or set DEEPSEEK_API_KEY), or get a free\n"
            "Serper key at https://serper.dev and run /login serper (or set SERPER_API_KEY)."
        )
    renew_hint = (
        f" Ask the user to renew the key with {' or '.join(dict.fromkeys(renew))}." if renew else ""
    )
    if result is None:
        return (
            f"Error searching for '{query}': {'; '.join(notes)}.{renew_hint}\n"
            "The web_search tool falls back to a keyless search."
        )

    output = f'Results for query "{query}" ({source}):\n\n'
    if notes:
        output += f"({'; '.join(notes)}; these come from {source}.{renew_hint})\n\n"
    output += result

    if len(output) > max_output:
        total = len(output)
        marker = f"\n... [output truncated, {total} chars total] ...\n"
        # Reserve room for the marker so the result stays within max_output.
        half = max(0, (max_output - len(marker)) // 2)
        output = output[:half] + marker + output[len(output) - half:]
        if len(output) > max_output:  # marker alone exceeds the budget
            output = output[:max_output]

    return output
