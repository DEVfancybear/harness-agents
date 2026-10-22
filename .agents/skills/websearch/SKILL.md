---
name: websearch
version: 8d58e5b
description: Search the web from Python with DeepSeek Search (the DeepSeek key) or Google via Serper (the Serper key), whichever has a key, moving to the next when one fails; without either use the web_search tool. Takes one query and returns titles, URLs, dates and summaries.
---

# Web Search

Search the web with the search APIs that have a key, in order:

1. **DeepSeek Search** (`POST https://api.deepseek.com/v1/tools/search`). It uses the
   DeepSeek key: `DEEPSEEK_API_KEY`, else the key `/login deepseek` saved. Each result
   has a title, URL, site, date and a summary written for models.
2. **Google via Serper** (`https://google.serper.dev/search`). It uses the Serper key:
   `SERPER_API_KEY`, else the key `/login serper` saved. A free key is at
   https://serper.dev.

`HA_WEB_SEARCH=serper,deepseek` changes the order; `[web] search` in the config does the
same for the `web_search` tool. An API that fails hands the query to the next one. A key
an API refuses is skipped for 30 minutes; refusals include 401 (invalid), 402 (DeepSeek:
balance used up) and "Not enough credits" (Serper). The answer names the API that
answered and why the others did not.

If neither API has a key, tell the user how to add one, or use the `web_search` tool,
which falls back to a keyless search.

DeepSeek Search takes queries of at most 128 characters, so longer ones are cut at a
word. It understands `site:`, `-site:`, `"exact phrase"`, `-term`, `lang:en,zh`,
`after:2026-01-01` and `before:2026` in the query.

Optional overrides (environment variables):

- `PRIME_AGENT_WEBSEARCH_TIMEOUT` - HTTP timeout in seconds (default 45).
- `PRIME_AGENT_WEBSEARCH_NUM_RESULTS` - number of results to return (default 5).
- `HA_DEEPSEEK_TOOLS_URL` - another base for the DeepSeek tools API (a proxy).

## Usage

Call the prepared `websearch` import directly in the Python kernel:

```python
print(await websearch("latest Prime Agent release"))
print(await websearch('site:github.com "harness agents" after:2026-09'))
```
