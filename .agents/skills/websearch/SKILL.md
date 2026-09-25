---
name: websearch
version: e260085
description: Search Google via the Serper API. Configure access via /login, then MCP Connections, then Serper (web search). Takes one query and returns titles, URLs, snippets, and knowledge-graph data.
---

# Web Search

Search the web via the Serper Google Search API.

## Setup

Get a free API key at https://serper.dev and set it as `SERPER_API_KEY` in the
environment Harness Agents runs in; the kernel inherits it.

If web search reports a missing key, tell the user how to set `SERPER_API_KEY`,
or use the `web_search` tool, which needs no key.

Optional overrides (environment variables):

- `PRIME_AGENT_WEBSEARCH_TIMEOUT` - HTTP timeout in seconds (default 45).
- `PRIME_AGENT_WEBSEARCH_NUM_RESULTS` - number of organic results to return (default 5).

## Usage

Call the prepared `websearch` import directly in the Python kernel:

```python
print(await websearch("latest Prime Agent release"))
```
