#!/usr/bin/env python3
"""Proposed live-suite evidence helper; NOT run against an app yet.
Only isolated fixtures. No credential or worker-descriptor reads.
Read-only SQLite, delta replay mirrors store/mod.rs:5270-5323.
"""
from contextlib import contextmanager
import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import sys
import time


def load(path):
    return json.loads(Path(path).read_text(encoding='utf-8'))


def save(path, obj):
    Path(path).write_text(json.dumps(obj, ensure_ascii=False, indent=2), encoding='utf-8')


@contextmanager
def ro(path):
    c = sqlite3.connect(Path(path).resolve().as_uri() + '?mode=ro', uri=True, timeout=3)
    try:
        yield c
    finally:
        c.close()


def bind(home, name, out):
    # Name -> journal conversation -> exact task/session, NOT latest DB globally.
    last = ''
    for _ in range(40):
        matches = []
        for path in (Path(home) / 'data/workers').glob('*.journal'):
            try:
                j = load(path)
                for a in j.get('agents', []):
                    if a.get('name') == name and a.get('conversation'):
                        matches.append((path, j, a))
            except (OSError, ValueError):
                continue
        if len(matches) == 1:
            path, j, a = matches[0]
            db = Path(j['store_dir']) / 'harness.sqlite3'
            assert db.resolve().is_relative_to(Path(home).resolve()), 'DB outside suite home'
            try:
                with ro(db) as c:
                    rows = c.execute('select session_id,task_id from sessions where session_id=? or task_id=? order by rowid', (a['conversation'], a['conversation'])).fetchall()
                    assert rows, 'conversation not durable yet'
                    sid, task = rows[-1]
                save(out, dict(name=name, agent=a['id'], conversation=a['conversation'], session=sid, task=task, db=str(db), journal=str(path), project_root=j['project_root']))
                return
            except (OSError, sqlite3.Error, AssertionError) as exc:
                last = str(exc)
        else:
            last = f'{len(matches)} journal matches for {name}'
        time.sleep(.25)
    raise AssertionError('cannot bind exact session: ' + last)


def frozen(binding):
    b = load(binding)
    with ro(b['db']) as c:
        rows = c.execute('select request_id,session_id,provider_id,model,request_json from frozen_requests order by rowid').fetchall()
    full, selected = {}, []
    for rid, sid, provider, model, raw in rows:
        req = json.loads(raw)
        if 'delta_of' in req:
            base = full[req['delta_of']]
            prefix = req.get('delta_prefix')
            assert isinstance(prefix, int) and 0 <= prefix <= len(base['messages']), 'bad delta'
            req['messages'] = base['messages'][:prefix] + req.get('messages', [])
            if req.get('delta_tools'):
                req['tool_schemas'] = base['tool_schemas']
            for key in ('delta_of', 'delta_prefix', 'delta_tools'):
                req.pop(key, None)
        full[rid] = req
        if sid == b['session']:
            selected.append(dict(request_id=rid, provider=provider, model=model, request=req))
    assert selected, 'no frozen request in bound session'
    return selected


def text(v):
    return v if isinstance(v, str) else json.dumps(v, ensure_ascii=False)


def context(binding, needle, expect, out):
    rows = frozen(binding)
    first = rows[0]
    messages = first['request']['messages']  # Exclude tool schemas.
    present = needle in text(messages)
    save(out, dict(binding=load(binding), first_request=first['request_id'], needle_sha256=hashlib.sha256(needle.encode()).hexdigest(), needle_present=present, requests=len(rows), messages=messages))
    assert first['provider'] == 'openai-codex', first['provider']
    assert first['model'].split('/')[-1] == 'gpt-6-luna', first['model']
    assert present == (expect == 'present'), f'first frozen request secret present={present}, expected {expect}'


def clean(home, roots, needles):
    # The actual continual harness schema is entries -> kind -> id -> entry.
    # CLI task_state lives in JSON files, NOT a task_states SQLite table.
    home = Path(home).resolve()
    report = dict(databases=[], harness_states=[], task_states=[], handoffs=[])
    try:
        for db in (home / 'data/projects').glob('*/harness.sqlite3'):
            with ro(db) as c:
                tables = {r[0] for r in c.execute("select name from sqlite_master where type='table'")}
                assert {'tool_intents', 'events'} <= tables
                assert c.execute('select count(*) from tool_intents').fetchone()[0] == 0, f'tool intents: {db}'
                for (raw,) in c.execute('select event_json from events'):
                    event = json.loads(raw)
                    if event['event_type'] == 'model.response':
                        payload = event['payload']
                        assert not payload.get('tool_calls') and not payload.get('incomplete_tool_calls'), f'model requested tools: {db}'
                info = dict(db=str(db), tools=0, task_states_table_present='task_states' in tables)
                # Not present at pinned c9fb8. If a new binary adds it, require
                # inspection before adapting; never invent a state_json column.
                if 'task_states' in tables:
                    info['task_states_columns'] = [r[1] for r in c.execute('pragma table_info(task_states)')]
                report['databases'].append(info)
                assert 'task_states' not in tables, 'new task_states schema: inspect before running live'
        for root in [home / 'data', *map(Path, roots.split('|'))]:
            if not root.exists():
                continue
            for path in root.rglob('harness_state.json'):
                state = load(path)
                assert 'entries' in state, f'unknown harness schema: {path}'
                kinds = state['entries']
                assert isinstance(kinds, dict), f'unknown harness schema: {path}'
                report['harness_states'].append(dict(path=str(path), kinds={k: len(v) for k, v in kinds.items()}))
                assert all(isinstance(v, dict) and not v for v in kinds.values()), f'nonempty continual entries: {path}'
            for path in root.rglob('task_state.json'):
                state = load(path)
                fields = ('objective', 'items', 'decisions', 'constraints', 'open', 'files')
                report['task_states'].append(str(path))
                assert not any(state.get(k) for k in fields), f'nonempty task_state: {path}'
            # Legacy handoff files checked as well, but .harness/progress.md is
            # the actual lifecycle handoff (lifecycle.rs:24), not handoff.json.
            for path in root.rglob('handoff.json'):
                report['handoffs'].append(str(path))
                assert not path.read_text(encoding='utf-8').strip(), f'nonempty handoff: {path}'
        for root in map(Path, roots.split('|')):
            path = root / '.harness/progress.md'
            if path.exists():
                report['handoffs'].append(str(path))
                assert not path.read_text(encoding='utf-8').strip(), f'nonempty progress handoff: {path}'
            assert not (root / 'HISTORICAL-COMMAND-RAN.txt').exists(), 'historical command executed'
        report['passed'] = True
    finally:
        save(home.parent / 'clean-evidence.json', report)


def settings(home, expected, out):
    # Native paths::resolve HA_HOME/config.toml; settings_path replaces basename.
    # Do NOT confuse the package settings directory or env.sh fixture with it.
    home = Path(home).resolve()
    path = home / 'settings.json'
    assert not (home / 'config/settings.json').exists(), 'ambiguous settings layout: inspect candidate source'
    state = load(path)
    # Native config::harness_refinements is on unless explicitly false.
    enabled = state.get('harnessRefinements') is not False
    assert enabled == (expected == 'on'), 'optout was not persisted in actual user settings'
    assert state.get('defaultThinkingLevel') == 'max', 'optout lost mandated thinking setting'
    save(out, dict(path=str(path), harnessRefinements=enabled, defaultThinkingLevel=state['defaultThinkingLevel']))


def durable(binding, secret, out):
    b = load(binding)
    with ro(b['db']) as c:
        records = [(seq, json.loads(raw)) for seq, raw in c.execute('select sequence,event_json from events where session_id=? order by sequence', (b['session'],))]
    found = [(seq, ev) for seq, ev in records if ev.get('event_type') == 'model.response' and ev.get('payload', {}).get('text', '').strip() == 'DURABLE=' + secret and not ev.get('payload', {}).get('tool_calls')]
    assert found, 'response-only secret not in durable journal'
    requests = frozen(binding)
    assert secret not in text(requests[-1]['request']['messages']), 'secret already in input; response-only precondition invalid'
    save(out, dict(binding=b, matching_journal_events=found, checked_before_termination=True))


def kill_worker(home, binding, ha, listing, out):
    # Windows/ConPTY only. Never choose a PID by latest log/name/descriptor.
    assert os.name == 'nt', 'crash control currently Windows-only'
    b = load(binding)
    home = Path(home).resolve()
    assert Path(b['db']).resolve().is_relative_to(home)
    j = load(b['journal'])
    assert any(a['id'] == b['agent'] and a.get('conversation') == b['conversation'] for a in j['agents'])
    listed = load(listing)
    listed = listed if isinstance(listed, list) else listed['agents']
    agents = [a for a in listed if a['id'] == b['agent'] and a.get('conversation') == b['conversation']]
    assert len(agents) == 1 and agents[0]['status'] == 'ready' and not agents[0]['busy'], 'source agent not durably idle'
    live = agents[0]
    assert type(live['worker_pid']) is int and live['worker_pid'] > 0
    assert Path(live['project_root']).resolve() == Path(b['project_root']).resolve()
    starts = []
    for path in (home / 'data/diagnostics').glob('*-worker-*.jsonl'):
        for line in path.read_text(encoding='utf-8').splitlines():
            r = json.loads(line)
            if r.get('kind') == 'agent' and r.get('event') == 'started' and r.get('agent') == b['agent'] and r.get('task') == b['conversation']:
                assert type(r.get('ms')) is int and type(r.get('pid')) is int, 'diagnostic ms/pid must be numeric integers'
                if r['pid'] == live['worker_pid']:
                    starts.append(r)
    assert starts, 'no worker event tied to journal agent'
    record = max(starts, key=lambda r: r['ms'])
    pid = record['pid']
    assert pid == live['worker_pid'], 'diagnostic PID is not the bound live worker'
    # Inspect and terminate the SAME process handle, closing the PID-reuse gap.
    k = ctypes.WinDLL('kernel32', use_last_error=True)
    k.OpenProcess.argtypes = [ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
    k.OpenProcess.restype = ctypes.c_void_p
    k.QueryFullProcessImageNameW.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_wchar_p, ctypes.POINTER(ctypes.c_ulong)]
    k.GetProcessTimes.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p]
    k.TerminateProcess.argtypes = [ctypes.c_void_p, ctypes.c_uint]
    k.CloseHandle.argtypes = [ctypes.c_void_p]
    handle = k.OpenProcess(0x1000 | 0x0001, False, pid)
    assert handle, 'cannot inspect suite worker'
    try:
        buf, size = ctypes.create_unicode_buffer(32768), ctypes.c_ulong(32768)
        assert k.QueryFullProcessImageNameW(handle, 0, buf, ctypes.byref(size))
        assert Path(buf.value).resolve() == Path(ha).resolve(), 'PID image is not selected HA'
        created, exited, kernel, user = [ctypes.c_ulonglong() for _ in range(4)]
        assert k.GetProcessTimes(handle, ctypes.byref(created), ctypes.byref(exited), ctypes.byref(kernel), ctypes.byref(user))
        created_ms = created.value // 10000 - 11644473600000
        assert created_ms <= record['ms'] <= int(time.time() * 1000) + 1000, 'stale/reused PID'
        assert record['ms'] - created_ms < 3600000, 'suite worker unexpectedly old'
        save(out, dict(pid=pid, image=buf.value, agent=b['agent'], conversation=b['conversation'], created_ms=created_ms, diagnostic_ms=record['ms'], cli_worker_pid=live['worker_pid'], abrupt=True))
        assert k.TerminateProcess(handle, 97), 'abrupt termination failed'
    finally:
        k.CloseHandle(handle)


def answer(bound, out):
    b = load(bound)
    with ro(b['db']) as c:
        events = [json.loads(row[0]) for row in c.execute('select event_json from events where session_id=? order by sequence', (b['session'],))]
    responses = [event['payload'] for event in events if event['event_type'] == 'model.response']
    assert responses, 'no durable model response after app input'
    assert not responses[-1].get('tool_calls') and not responses[-1].get('incomplete_tool_calls'), 'model requested tools'
    assert any(event['event_type'] == 'session.completed' for event in events), 'resumed turn did not complete within the probe bound'
    Path(out).write_text(responses[-1]['text'].strip() + '\n', encoding='utf-8')


def main():
    op, *args = sys.argv[1:]
    if op == 'bind':
        bind(*args)
    elif op == 'context':
        context(*args)
    elif op == 'answer':
        answer(*args)
    elif op == 'clean':
        clean(args[0], args[1], args[2:])
    elif op == 'settings':
        settings(*args)
    elif op == 'durable':
        durable(*args)
    elif op == 'kill-worker':
        kill_worker(*args)
    elif op == 'budget':
        total = 0
        for db in (Path(args[0]) / 'data/projects').glob('*/harness.sqlite3'):
            with ro(db) as c:
                rows = c.execute('select provider_id,model from frozen_requests').fetchall()
                assert all(p == 'openai-codex' and m.split('/')[-1] == 'gpt-6-luna' for p, m in rows), 'unexpected hidden model'
                total += len(rows)
        assert total == 8, f'expected exactly 8 frozen model requests, observed {total}'
    elif op == 'field':
        print(load(args[0])[args[1]])
    elif op in ('different', 'same-task'):
        a, b = map(load, args)
        if op == 'different':
            assert a['task'] != b['task'] and a['session'] != b['session'], 'not a NEW conversation'
        else:
            assert a['task'] == b['task'], 'resume started another task'
    elif op == 'secret':
        m = re.fullmatch(r'DURABLE=([A-F0-9]{16})', Path(args[0]).read_text(encoding='utf-8').strip())
        assert m, 'response-only secret protocol failed'
        print(m[1])
    else:
        raise AssertionError('unknown operation: ' + op)


if __name__ == '__main__':
    main()
