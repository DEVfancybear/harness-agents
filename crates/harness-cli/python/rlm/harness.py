"""Persistent harness-state helpers for Prime Agent's RLM kernel.

The state model is intentionally small: it records prompt notes, memory,
skills, subagent specs, factories, and refinement events in the
session-local harness store by default; pass ``global_=True`` for the
cross-session global store.

ha: prime-agent's kernel keeps this state itself, reading and rewriting
``harness_state.json`` under its own lock. In ha the host keeps it
(``harness_kernel.rs``), beside the ``memory`` tool and ``/refine`` that
write the same file: every call below is one blocking ``harness`` host
request, so they never interleave a read-modify-write and one validator
checks every entry. The API is prime-agent's; only where the work happens
changed. The factory spec dry run stays here, beside ``factory.py``.
"""

from __future__ import annotations

import copy
import os
from dataclasses import dataclass, field, fields
from pathlib import Path
from typing import Any, Literal

from .factory import require_factory_enabled, validate_factory_spec

HarnessKind = Literal["prompt", "memory", "skill", "subagent", "factory"]
HarnessScope = Literal["local", "global"]

_KINDS: tuple[HarnessKind, ...] = ("prompt", "memory", "skill", "subagent", "factory")
_HOST_REQUEST = "harness"
_WRITES = {"create", "update", "upsert", "delete", "record_refinement"}


def _agent_dir() -> Path:
    raw = (
        os.environ.get("PRIME_AGENT_CODING_AGENT_DIR")
        or os.environ.get("PI_CODING_AGENT_DIR")
        or str(Path.home() / ".prime" / "agent")
    )
    return Path(raw).expanduser().resolve()


def _resolve_global_flag(global_: bool = False, extra: dict[str, Any] | None = None) -> bool:
    extra = dict(extra or {})
    if "global" in extra:
        value = extra.pop("global")
        if not isinstance(value, bool):
            raise TypeError(f"global must be a bool, got {type(value).__name__}")
        global_ = value
    if extra:
        unexpected = next(iter(extra))
        raise TypeError(f"unexpected keyword argument {unexpected!r}")
    return bool(global_)


def _strip_scope_prefix(id: str | None, global_: bool) -> tuple[str | None, bool]:
    # overview() displays entries as [local:id]/[global:id]; accept those ids
    # verbatim. A global: prefix routes to the global store unless the caller
    # already forced a scope via global_.
    if isinstance(id, str):
        scope, sep, rest = id.partition(":")
        if sep and rest and scope in ("local", "global"):
            return rest, global_ or scope == "global"
    return id, global_


@dataclass
class HarnessEntry:
    """A reusable prompt, memory, skill, subagent, or factory record."""

    id: str
    kind: HarnessKind
    title: str
    content: str
    path: str = "general"
    scope: HarnessScope = "local"
    reference: dict[str, Any] = field(default_factory=dict)
    arguments: dict[str, Any] = field(default_factory=dict)
    metadata: dict[str, Any] = field(default_factory=dict)
    source: str = "agent"
    created_at: str = ""
    updated_at: str = ""
    version: int = 1


@dataclass
class RefinementEvent:
    """A recorded online harness-refinement pass."""

    id: str
    trigger: str
    changes: list[str]
    evidence: str = ""
    outcome: str = ""
    created_at: str = ""


_ENTRY_FIELDS = {field.name for field in fields(HarnessEntry)}
_REFINEMENT_FIELDS = {field.name for field in fields(RefinementEvent)}


def _entry(raw: Any) -> HarnessEntry | None:
    if not isinstance(raw, dict):
        return None
    return HarnessEntry(**{key: value for key, value in raw.items() if key in _ENTRY_FIELDS})


def _event(raw: Any) -> RefinementEvent:
    return RefinementEvent(**{key: value for key, value in raw.items() if key in _REFINEMENT_FIELDS})


def _validate_python_skill_reference(reference: dict[str, Any] | None, entry_name: str = "") -> dict[str, Any]:
    # Rejections name the entry so the caller can repair the right skill; the
    # host checks the same contract on every write.
    prefix = f"skill entry {entry_name!r} rejected: " if entry_name else ""

    def reject(message: str) -> None:
        raise ValueError(f"{prefix}{message}")

    if not isinstance(reference, dict):
        reject("skill entries require a Python reference")
    normalized = dict(reference)
    if normalized.get("type") != "python":
        reject("skill reference.type must be 'python'")
    if not any(isinstance(normalized.get(key), str) and normalized[key] for key in ("import", "python_import")):
        reject("skill reference requires a Python import")
    if not any(isinstance(normalized.get(key), str) and normalized[key] for key in ("callable", "call_pattern")):
        reject("skill reference requires a callable or call_pattern")
    return normalized


def _describe_entry(id: Any, title: Any) -> str:
    """Best available entry name for rejection messages."""
    if isinstance(id, str) and id:
        return id
    if isinstance(title, str) and title:
        return title
    return "<unnamed>"


def _factory_spec_argument(
    dag: Any, machine: Any
) -> "tuple[Any, Literal['dag', 'machine']]":
    """Pick the factory spec payload and its arguments key from the call.

    Supplying both forms at once is an error. A bare ``dag=None,
    machine=None`` passes ``None`` through in the dag slot so the write-time
    validation rejects it with the standard wording.
    """
    if dag is not None and machine is not None:
        raise ValueError("pass either dag or machine, not both")
    if machine is not None:
        return machine, "machine"
    return dag, "dag"


def _validate_factory_arguments(entry_name: str, arguments: dict[str, Any]) -> None:
    """Shared-path dry run for every factory write.

    ``create_factory``/``update_factory`` validate their own spec, but a
    generic ``create``/``update`` writes ``arguments`` directly; an invalid
    spec must never reach the store through any writer, so the spec found in
    ``arguments`` is validated here too.
    """
    if not isinstance(arguments, dict):
        return
    dag, machine = arguments.get("dag"), arguments.get("machine")
    if dag is not None and machine is not None:
        raise ValueError(f"factory entry {entry_name!r} rejected: pass either dag or machine, not both")
    spec = machine if machine is not None else dag
    if not isinstance(spec, dict):
        raise ValueError(
            f"factory entry {entry_name!r} rejected: factory entries require a dag or machine object in arguments"
        )
    errors = validate_factory_spec(spec)
    if errors:
        raise ValueError(f"factory entry {entry_name!r} rejected: {'; '.join(errors)}")


def _check_factory_write(kind: Any, entry_name: str, arguments: Any) -> Any:
    """The opt-in gate and the spec dry run, before the host stores anything.

    The gate comes first: while ``factory.enabled`` is off (the default)
    every factory write refuses with the one disabled message. The host
    refuses a NEW factory without a spec; an update that omits arguments
    keeps the stored one. Specs are deep-copied, so mutating the caller's
    dict later never changes what was validated.
    """
    if kind != "factory":
        return arguments
    require_factory_enabled()
    if arguments is not None:
        _validate_factory_arguments(entry_name, arguments)
        return copy.deepcopy(arguments)
    return None


class HarnessState:
    """CRUD store for reset-free harness refinement state, kept by the ha host."""

    def __init__(
        self,
        file_path: str | Path | None = None,
        *,
        in_memory: bool = False,
        scope: HarnessScope = "local",
        local_write_error: str | None = None,
    ):
        self.scope: HarnessScope = scope
        self._in_memory = in_memory
        # When set, local mutations raise; reads and global_=True keep working.
        self._local_write_error = local_write_error
        self._state_dir: Path | None = (
            Path(file_path).expanduser().resolve().parent if file_path and not in_memory else None
        )

    def _call(self, op: str, global_: bool = False, **payload: Any) -> Any:
        global_ = global_ or self.scope == "global"
        if op in _WRITES and not global_ and self._local_write_error is not None:
            raise RuntimeError(self._local_write_error)
        if self._in_memory:
            raise RuntimeError(
                self._local_write_error
                or "rlm.harness is kept by the ha host, and this kernel has no host connection"
            )
        from . import repl

        data: dict[str, Any] = {**payload, "op": op, "global": global_}
        if self._state_dir is not None:
            data["state_dir"] = str(self._state_dir)
        # The type goes last so a payload "type" key cannot reroute the request.
        data["type"] = _HOST_REQUEST
        reply = repl.host_request_blocking(data)
        status = reply.get("status") if isinstance(reply, dict) else None
        if status == "ok":
            return reply.get("result")
        if status == "error":
            raise ValueError(str(reply.get("error") or f"harness {op} failed"))
        raise RuntimeError(f"harness {op} returned an unexpected reply: {reply!r}")

    @property
    def file_path(self) -> Path | None:
        if self._in_memory:
            return None
        return Path(self.snapshot()["file_path"])

    @property
    def entries(self) -> dict[HarnessKind, dict[str, HarnessEntry]]:
        if self._in_memory:
            return {kind: {} for kind in _KINDS}
        raw = self.snapshot()["entries"]
        return {
            kind: {entry_id: entry for entry_id, value in raw.get(kind, {}).items() if (entry := _entry(value))}
            for kind in _KINDS
        }

    @property
    def refinements(self) -> list[RefinementEvent]:
        if self._in_memory:
            return []
        return [_event(event) for event in self.snapshot()["refinements"]]

    def load(self) -> "HarnessState":
        # The host reads the file on every call: there is nothing to reload.
        return self

    def save(self) -> "HarnessState":
        # The host writes each change as it makes it.
        return self

    def upsert(
        self,
        kind: HarnessKind,
        title: str,
        content: str,
        *,
        id: str | None = None,
        path: str = "general",
        reference: dict[str, Any] | None = None,
        arguments: dict[str, Any] | None = None,
        metadata: dict[str, Any] | None = None,
        source: str = "agent",
        global_: bool = False,
        **kwargs: Any,
    ) -> HarnessEntry:
        id, global_ = _strip_scope_prefix(id, global_)
        global_ = _resolve_global_flag(global_, kwargs)
        arguments = _check_factory_write(kind, _describe_entry(id, title), arguments)
        return self._write("upsert", kind, title, content, id, path, reference, arguments, metadata, source, global_)

    def _write(
        self,
        op: str,
        kind: Any,
        title: Any,
        content: Any,
        id: Any,
        path: Any,
        reference: Any,
        arguments: Any,
        metadata: Any,
        source: Any,
        global_: bool,
    ) -> HarnessEntry:
        result = self._call(
            op,
            global_,
            kind=kind,
            title=title,
            content=content,
            id=id,
            path=path,
            reference=reference,
            arguments=arguments,
            metadata=metadata,
            source=source,
        )
        entry = _entry(result)
        if entry is None:
            raise RuntimeError(f"harness {op} returned no entry")
        return entry

    def get(self, kind: HarnessKind, id: str, *, global_: bool = False, **kwargs: Any) -> HarnessEntry | None:
        id, global_ = _strip_scope_prefix(id, global_)
        global_ = _resolve_global_flag(global_, kwargs)
        if self._in_memory:
            return None
        return _entry(self._call("get", global_, kind=kind, id=id))

    def delete(self, kind: HarnessKind, id: str, *, global_: bool = False, **kwargs: Any) -> bool:
        id, global_ = _strip_scope_prefix(id, global_)
        global_ = _resolve_global_flag(global_, kwargs)
        if kind == "factory":
            require_factory_enabled()
        return bool(self._call("delete", global_, kind=kind, id=id))

    def list(self, kind: HarnessKind | None = None, *, global_: bool = False, **kwargs: Any) -> list[HarnessEntry]:
        global_ = _resolve_global_flag(global_, kwargs)
        if self._in_memory:
            return []
        return [entry for raw in self._call("list", global_, kind=kind) if (entry := _entry(raw))]

    def create(
        self,
        kind: HarnessKind,
        title: str,
        content: str,
        *,
        id: str | None = None,
        path: str = "general",
        reference: dict[str, Any] | None = None,
        arguments: dict[str, Any] | None = None,
        metadata: dict[str, Any] | None = None,
        source: str = "agent",
        global_: bool = False,
        **kwargs: Any,
    ) -> HarnessEntry:
        id, global_ = _strip_scope_prefix(id, global_)
        global_ = _resolve_global_flag(global_, kwargs)
        arguments = _check_factory_write(kind, _describe_entry(id, title), arguments)
        return self._write("create", kind, title, content, id, path, reference, arguments, metadata, source, global_)

    def update(
        self,
        kind: HarnessKind,
        id: str,
        title: str,
        content: str,
        *,
        path: str | None = None,
        reference: dict[str, Any] | None = None,
        arguments: dict[str, Any] | None = None,
        metadata: dict[str, Any] | None = None,
        source: str = "agent",
        global_: bool = False,
        **kwargs: Any,
    ) -> HarnessEntry:
        id, global_ = _strip_scope_prefix(id, global_)
        global_ = _resolve_global_flag(global_, kwargs)
        arguments = _check_factory_write(kind, _describe_entry(id, title), arguments)
        return self._write("update", kind, title, content, id, path, reference, arguments, metadata, source, global_)

    def create_memory(
        self,
        title: str,
        content: str,
        *,
        id: str | None = None,
        path: str = "general",
        metadata: dict[str, Any] | None = None,
        global_: bool = False,
        **kwargs: Any,
    ) -> HarnessEntry:
        return self.create("memory", title, content, id=id, path=path, metadata=metadata, global_=global_, **kwargs)

    def update_memory(
        self,
        id: str,
        title: str,
        content: str,
        *,
        path: str | None = None,
        metadata: dict[str, Any] | None = None,
        global_: bool = False,
        **kwargs: Any,
    ) -> HarnessEntry:
        return self.update("memory", id, title, content, path=path, metadata=metadata, global_=global_, **kwargs)

    def delete_memory(self, id: str, *, global_: bool = False, **kwargs: Any) -> bool:
        return self.delete("memory", id, global_=global_, **kwargs)

    def create_prompt_note(
        self,
        title: str,
        content: str,
        *,
        id: str | None = None,
        path: str = "policy",
        metadata: dict[str, Any] | None = None,
        global_: bool = False,
        **kwargs: Any,
    ) -> HarnessEntry:
        return self.create("prompt", title, content, id=id, path=path, metadata=metadata, global_=global_, **kwargs)

    def update_prompt_note(
        self,
        id: str,
        title: str,
        content: str,
        *,
        path: str | None = None,
        metadata: dict[str, Any] | None = None,
        global_: bool = False,
        **kwargs: Any,
    ) -> HarnessEntry:
        return self.update("prompt", id, title, content, path=path, metadata=metadata, global_=global_, **kwargs)

    def delete_prompt_note(self, id: str, *, global_: bool = False, **kwargs: Any) -> bool:
        return self.delete("prompt", id, global_=global_, **kwargs)

    def create_skill(
        self,
        title: str,
        content: str,
        *,
        id: str | None = None,
        path: str = "general",
        reference: dict[str, Any] | None = None,
        arguments: dict[str, Any] | None = None,
        metadata: dict[str, Any] | None = None,
        global_: bool = False,
        **kwargs: Any,
    ) -> HarnessEntry:
        return self.create(
            "skill",
            title,
            content,
            id=id,
            path=path,
            reference=_validate_python_skill_reference(reference, _describe_entry(id, title)),
            arguments=arguments,
            metadata=metadata,
            global_=global_,
            **kwargs,
        )

    def update_skill(
        self,
        id: str,
        title: str,
        content: str,
        *,
        path: str | None = None,
        reference: dict[str, Any] | None = None,
        arguments: dict[str, Any] | None = None,
        metadata: dict[str, Any] | None = None,
        global_: bool = False,
        **kwargs: Any,
    ) -> HarnessEntry:
        # Only validate a reference when one is supplied; omitting it preserves the
        # existing reference rather than forcing every title/content-only update to
        # re-send the full Python reference.
        validated_reference = (
            _validate_python_skill_reference(reference, _describe_entry(id, title)) if reference is not None else None
        )
        return self.update(
            "skill",
            id,
            title,
            content,
            path=path,
            reference=validated_reference,
            arguments=arguments,
            metadata=metadata,
            global_=global_,
            **kwargs,
        )

    def delete_skill(self, id: str, *, global_: bool = False, **kwargs: Any) -> bool:
        return self.delete("skill", id, global_=global_, **kwargs)

    def create_subagent(
        self,
        title: str,
        content: str,
        *,
        id: str | None = None,
        path: str = "general",
        metadata: dict[str, Any] | None = None,
        global_: bool = False,
        **kwargs: Any,
    ) -> HarnessEntry:
        return self.create("subagent", title, content, id=id, path=path, metadata=metadata, global_=global_, **kwargs)

    def update_subagent(
        self,
        id: str,
        title: str,
        content: str,
        *,
        path: str | None = None,
        metadata: dict[str, Any] | None = None,
        global_: bool = False,
        **kwargs: Any,
    ) -> HarnessEntry:
        return self.update("subagent", id, title, content, path=path, metadata=metadata, global_=global_, **kwargs)

    def delete_subagent(self, id: str, *, global_: bool = False, **kwargs: Any) -> bool:
        return self.delete("subagent", id, global_=global_, **kwargs)

    def create_factory(
        self,
        title: str,
        content: str,
        *,
        id: str | None = None,
        path: str = "general",
        dag: dict[str, Any] | None = None,
        machine: dict[str, Any] | None = None,
        metadata: dict[str, Any] | None = None,
        global_: bool = False,
        **kwargs: Any,
    ) -> HarnessEntry:
        # The opt-in gate precedes the dry run, so a disabled factory
        # refuses with the one disabled message whatever the spec looks like.
        require_factory_enabled()
        spec, key = _factory_spec_argument(dag, machine)
        errors = validate_factory_spec(spec)
        if errors:
            raise ValueError("; ".join(errors))
        return self.create(
            "factory",
            title,
            content,
            id=id,
            path=path,
            arguments={key: copy.deepcopy(spec)},
            metadata=metadata,
            global_=global_,
            **kwargs,
        )

    def update_factory(
        self,
        id: str,
        title: str,
        content: str,
        *,
        path: str | None = None,
        dag: dict[str, Any] | None = None,
        machine: dict[str, Any] | None = None,
        metadata: dict[str, Any] | None = None,
        global_: bool = False,
        **kwargs: Any,
    ) -> HarnessEntry:
        require_factory_enabled()
        # Only validate a spec when one is supplied; omitting both preserves the
        # stored arguments, exactly like update_skill treats reference.
        if dag is not None or machine is not None:
            spec, key = _factory_spec_argument(dag, machine)
            errors = validate_factory_spec(spec)
            if errors:
                raise ValueError("; ".join(errors))
            arguments = {key: copy.deepcopy(spec)}
        else:
            arguments = None
        return self.update(
            "factory",
            id,
            title,
            content,
            path=path,
            arguments=arguments,
            metadata=metadata,
            global_=global_,
            **kwargs,
        )

    def delete_factory(self, id: str, *, global_: bool = False, **kwargs: Any) -> bool:
        return self.delete("factory", id, global_=global_, **kwargs)

    def record_refinement(
        self,
        trigger: str,
        changes: list[str] | str,
        *,
        evidence: str = "",
        outcome: str = "",
        id: str | None = None,
        global_: bool = False,
        **kwargs: Any,
    ) -> RefinementEvent:
        global_ = _resolve_global_flag(global_, kwargs)
        result = self._call(
            "record_refinement",
            global_,
            trigger=trigger,
            changes=changes,
            evidence=evidence,
            outcome=outcome,
            id=id,
        )
        return _event(result)

    def plan_refinement(
        self,
        observation: str,
        *,
        failing_component: str = "",
        next_step: str = "",
    ) -> list[str]:
        target = f" for {failing_component}" if failing_component else ""
        plan = [
            f"Diagnose the repeated failure or opportunity{target}: {observation}",
            "Update the smallest useful prompt note, memory item, skill, or subagent spec.",
            "Run the next action with the changed harness state, then record the outcome.",
        ]
        if next_step:
            plan.append(f"Immediate validation step: {next_step}")
        return plan

    def overview(self, *, max_entries_per_kind: int = 20, global_: bool = False, **kwargs: Any) -> str:
        global_ = _resolve_global_flag(global_, kwargs)
        return str(self._call("overview", global_, max_entries_per_kind=max_entries_per_kind))

    def search(
        self,
        query: str,
        kind: HarnessKind | None = None,
        limit: int = 10,
        *,
        global_: bool = False,
        **kwargs: Any,
    ) -> list[HarnessEntry]:
        """Return harness entries ranked by weighted term overlap with *query*.

        Terms are scored against an entry's title, content, path, and id;
        matches in more distinct fields count more. Each matched term is
        discounted by its document frequency across the ranked corpus
        (tf-idf style), so a rare, distinctive term outranks terms present in
        most entries.
        """
        global_ = _resolve_global_flag(global_, kwargs)
        if not isinstance(query, str):
            raise TypeError(f"query must be str, got {type(query).__name__}")
        if not isinstance(limit, int) or isinstance(limit, bool) or limit < 1:
            raise TypeError("limit must be a positive int")
        if self._in_memory:
            return []
        return [
            entry
            for raw in self._call("search", global_, query=query, kind=kind, limit=limit)
            if (entry := _entry(raw))
        ]

    def snapshot(self, *, global_: bool = False, **kwargs: Any) -> dict[str, Any]:
        global_ = _resolve_global_flag(global_, kwargs)
        return self._call("snapshot", global_)


_state_cache: dict[tuple[str | None, HarnessScope], HarnessState] = {}


def get_harness_state(
    state_dir: str | Path | None = None, *, global_: bool = False, **kwargs: Any
) -> HarnessState:
    """Return the local harness state, or global when requested."""
    global_ = _resolve_global_flag(global_, kwargs)
    scope: HarnessScope = "global" if global_ else "local"
    directory = str(Path(state_dir).expanduser().resolve()) if state_dir is not None else None
    key = (directory, scope)
    state = _state_cache.get(key)
    if state is None:
        file_path = Path(directory) / "harness_state.json" if directory is not None else None
        state = HarnessState(file_path, scope=scope)
        _state_cache[key] = state
    return state


__all__ = [
    "HarnessEntry",
    "HarnessKind",
    "HarnessScope",
    "HarnessState",
    "RefinementEvent",
    "get_harness_state",
]
