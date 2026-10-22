"""TypeSafe Jev from the Python kernel: typed decisions with confidence.

The same calls the `jev` tool makes, for the reusable script a workflow ends in:

    result = await jev.decide(state, questions)
    batch = await jev.batch(items, questions)   # decided / review / errors

A decision whose confidence reaches the threshold is accepted; the rest are
returned for review (by the model, or a person), never guessed.
"""

from __future__ import annotations

import asyncio
import importlib.util
import json
import os
import time
from pathlib import Path

# httpx is imported on first call, as the websearch skill does: importing it at
# module level would cost every kernel start.
if importlib.util.find_spec("httpx") is None:
    raise ModuleNotFoundError("No module named 'httpx'")

ENDPOINT = "https://api.typesafe.ai/v1/systemone"
DEFAULT_MODEL = "jev-latest"
DEFAULT_THRESHOLD = 0.95
# TypeSafe's published input price: $0.042 per million tokens; output is free.
INPUT_PRICE = 0.042 / 1_000_000
MAX_CONCURRENCY = 20
MAX_ITEMS = 500


class JevError(RuntimeError):
    """A request TypeSafe refused or could not answer."""


def _settings() -> dict:
    path = os.environ.get("HA_SETTINGS_FILE", "").strip()
    if not path:
        return {}
    try:
        value = json.loads(Path(path).read_text(encoding="utf-8")).get("jev")
    except (OSError, ValueError, AttributeError):
        return {}
    return value if isinstance(value, dict) else {}


def _api_key() -> str:
    key = os.environ.get("TYPESAFE_API_KEY", "").strip()
    if key:
        return key
    path = os.environ.get("HA_CREDENTIAL_FILE", "").strip()
    if not path:
        return ""
    try:
        entry = json.loads(Path(path).read_text(encoding="utf-8")).get("typesafe")
    except (OSError, ValueError, AttributeError):
        return ""
    if isinstance(entry, dict) and entry.get("type") == "api_key":
        return str(entry.get("key") or "").strip()
    return ""


def _threshold(value: float | None) -> float:
    if value is None:
        value = _settings().get("threshold", DEFAULT_THRESHOLD)
    try:
        value = float(value)
    except (TypeError, ValueError):
        return DEFAULT_THRESHOLD
    return min(max(value, 0.0), 1.0)


def _model(value: str | None) -> str:
    return value or str(_settings().get("model") or DEFAULT_MODEL)


def judge(answer: dict, threshold: float) -> dict:
    """One answer with its decision, its confidence and whether it is accepted.

    A Noul has no confidence of its own: it is |2p - 1| around its yes
    probability, and the decision is yes from 0.5.
    """
    kind = answer.get("type")
    judged = dict(answer)
    if kind == "noul":
        probability = float(answer.get("noul", 0.5))
        judged["decision"] = "yes" if probability >= 0.5 else "no"
        judged["confidence"] = abs(2 * probability - 1)
    elif kind == "choice":
        judged["decision"] = answer.get("choice")
    elif kind == "score":
        judged["decision"] = answer.get("score")
    else:
        judged["decision"] = answer.get(kind)
    confidence = judged.get("confidence")
    judged["accepted"] = isinstance(confidence, (int, float)) and confidence >= threshold
    return judged


async def _post(client, body: dict) -> dict:
    key = _api_key()
    if not key:
        raise JevError(
            "No TypeSafe key: the user can run /login typesafe or set TYPESAFE_API_KEY."
        )
    url = os.environ.get("HA_TYPESAFE_URL", "").strip() or ENDPOINT
    for attempt in range(3):
        response = await client.post(
            url, json=body, headers={"Authorization": f"Bearer {key}"}
        )
        if response.status_code == 429 and attempt < 2:
            after = response.headers.get("retry-after")
            try:
                wait = float(after) if after else 1.0
            except ValueError:
                wait = 1.0
            await asyncio.sleep(min(wait, 10.0))
            continue
        if response.status_code >= 400:
            raise JevError(f"TypeSafe answered {response.status_code}: {response.text[:300]}")
        try:
            return response.json()
        except ValueError as error:
            raise JevError("TypeSafe did not answer with JSON") from error
    raise JevError("TypeSafe kept answering 429 (rate limited)")


async def decide(
    state,
    questions: dict,
    *,
    threshold: float | None = None,
    model: str | None = None,
    timeout: float = 45,
) -> dict:
    """Ask Jev `questions` (name -> noul / choice / score question) about `state`.

    Returns the model, each answer judged against the threshold, the usage and
    the input cost in dollars.
    """
    import httpx

    limit = _threshold(threshold)
    async with httpx.AsyncClient(timeout=timeout) as client:
        data = await _post(client, {"model": _model(model), "state": state, "questions": questions})
    tokens = (data.get("usage") or {}).get("input_tokens") or 0
    return {
        "model": data.get("model"),
        "threshold": limit,
        "answers": {
            name: judge(answer, limit) for name, answer in (data.get("answers") or {}).items()
        },
        "usage": data.get("usage") or {},
        "cost": tokens * INPUT_PRICE,
    }


async def batch(
    items: list,
    questions: dict,
    *,
    threshold: float | None = None,
    model: str | None = None,
    concurrency: int = MAX_CONCURRENCY,
    timeout: float = 45,
) -> dict:
    """Ask the same questions about many items, at most `concurrency` at once.

    `items` are `{"id": ..., "state": ...}` (or bare states, numbered from 1).
    An item every answer of which reaches the threshold is `decided`; one with
    any answer below it goes to `review`, to be judged by the model or a person.
    """
    import httpx

    if len(items) > MAX_ITEMS:
        raise JevError(f"at most {MAX_ITEMS} items per batch")
    limit = _threshold(threshold)
    gate = asyncio.Semaphore(max(1, min(concurrency, MAX_CONCURRENCY)))
    started = time.monotonic()
    decided, review, errors = [], [], []
    tokens = 0

    async def one(client, index, item):
        nonlocal tokens
        if isinstance(item, dict) and "state" in item:
            item_id, state = item.get("id", index + 1), item["state"]
        else:
            item_id, state = index + 1, item
        async with gate:
            try:
                data = await _post(
                    client, {"model": _model(model), "state": state, "questions": questions}
                )
            except Exception as error:  # noqa: BLE001 - one item's failure is reported, not fatal
                errors.append({"id": item_id, "error": str(error)})
                return
        tokens += (data.get("usage") or {}).get("input_tokens") or 0
        answers = {
            name: judge(answer, limit) for name, answer in (data.get("answers") or {}).items()
        }
        row = {"id": item_id, "answers": answers}
        (decided if answers and all(a["accepted"] for a in answers.values()) else review).append(row)

    async with httpx.AsyncClient(timeout=timeout) as client:
        await asyncio.gather(*(one(client, index, item) for index, item in enumerate(items)))
    key = lambda row: str(row["id"])  # noqa: E731 - a sort key
    return {
        "threshold": limit,
        "decided": sorted(decided, key=key),
        "review": sorted(review, key=key),
        "errors": sorted(errors, key=key),
        "seconds": round(time.monotonic() - started, 2),
        "input_tokens": tokens,
        "cost": tokens * INPUT_PRICE,
    }
