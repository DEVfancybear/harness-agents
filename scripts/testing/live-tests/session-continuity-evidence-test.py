"""Offline synthetic fixtures only; no app, provider, credentials or termination."""
from contextlib import closing
import importlib.util
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('evidence', ROOT / 'session-continuity-evidence.py')
e = importlib.util.module_from_spec(spec)
spec.loader.exec_module(e)

class EvidenceControls(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='continuity-offline-')
        self.r = Path(self.tmp.name)
        self.home = self.r / 'home'
        self.data = self.home / 'data'
        self.project = self.r / 'p'
        self.project.mkdir()
        self.db = self.data / 'projects/x/harness.sqlite3'
        self.db.parent.mkdir(parents=True)
        with closing(sqlite3.connect(self.db)) as c, c:
            c.execute('create table tool_intents(id integer)')
            c.execute('create table events(event_json text)')
            c.execute('create table task_projections(working_state_json text)')
        self.state = self.data / 'harness/harness_state.json'
        self.put(self.state, {'schema': 1, 'entries': {'memory': {}, 'prompt': {}}})
    def tearDown(self):
        self.tmp.cleanup()
    def put(self, p, value):
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(json.dumps(value), encoding='utf-8')
    def clean(self):
        e.clean(str(self.home), str(self.project), ['CW-FIXTURE'])
    def test_model_tool_request_rejected_before_any_intent(self):
        with closing(sqlite3.connect(self.db)) as c, c:
            c.execute('insert into events values(?)', (json.dumps({'event_type': 'model.response', 'payload': {'tool_calls': [{'name': 'task_state'}]}}),))
        with self.assertRaisesRegex(AssertionError, 'model requested tools'):
            self.clean()
    def test_empty_actual_state_passes_without_task_states_sql(self):
        self.clean()
        report = e.load(self.r / 'clean-evidence.json')
        self.assertTrue(report['passed'])
        self.assertFalse(report['databases'][0]['task_states_table_present'])
    def test_actual_entries_kind_map_rejected(self):
        self.put(self.state, {'entries': {'memory': {'m': {'content': 'CW-FIXTURE'}}}})
        with self.assertRaisesRegex(AssertionError, 'nonempty continual'):
            self.clean()
    def test_disk_task_state_rejected_even_when_sql_projection_empty(self):
        self.put(self.data / 'sessions/s/harness/task_state.json', {'objective': 'CW-FIXTURE', 'items': []})
        with self.assertRaisesRegex(AssertionError, 'nonempty task_state'):
            self.clean()
    def test_real_progress_handoff_rejected(self):
        p = self.project / '.harness/progress.md'
        p.parent.mkdir()
        p.write_text('CW-FIXTURE', encoding='utf-8')
        with self.assertRaisesRegex(AssertionError, 'progress handoff'):
            self.clean()
    def test_unknown_new_task_states_schema_requires_inspection(self):
        with closing(sqlite3.connect(self.db)) as c, c:
            c.execute('create table task_states(unknown_payload text)')
        with self.assertRaisesRegex(AssertionError, 'new task_states schema'):
            self.clean()
        report = e.load(self.r / 'clean-evidence.json')
        self.assertEqual(report['databases'][0]['task_states_columns'], ['unknown_payload'])
    def test_default_on_is_omitted_by_native_settings_writer(self):
        self.put(self.home / 'settings.json', {'defaultThinkingLevel': 'max'})
        e.settings(str(self.home), 'on', str(self.r / 'settings.json'))
        self.assertTrue(e.load(self.r / 'settings.json')['harnessRefinements'])
    def test_root_normalized_settings_path(self):
        self.put(self.home / 'settings.json', {'harnessRefinements': False, 'defaultThinkingLevel': 'max'})
        e.settings(str(self.home), 'off', str(self.r / 'settings.json'))
        self.assertEqual(e.load(self.r / 'settings.json')['path'], str(self.home.resolve() / 'settings.json'))
    def test_ambiguous_settings_layout_rejected(self):
        self.put(self.home / 'settings.json', {'harnessRefinements': False, 'defaultThinkingLevel': 'max'})
        self.put(self.home / 'config/settings.json', {'harnessRefinements': False})
        with self.assertRaisesRegex(AssertionError, 'ambiguous settings'):
            e.settings(str(self.home), 'off', str(self.r / 'settings.json'))
    def test_tools_rejected(self):
        with closing(sqlite3.connect(self.db)) as c, c:
            c.execute('insert into tool_intents values(1)')
        with self.assertRaisesRegex(AssertionError, 'tool intents'):
            self.clean()
    def test_historical_command_sentinel_rejected(self):
        (self.project / 'HISTORICAL-COMMAND-RAN.txt').write_text('ran', encoding='utf-8')
        with self.assertRaisesRegex(AssertionError, 'historical command executed'):
            self.clean()

    def bound_fixture(self):
        with closing(sqlite3.connect(self.db)) as c, c:
            c.execute('create table sessions(session_id text, task_id text)')
            c.execute('insert into sessions values(?,?)', ('session1', 'task1'))
            c.execute('insert into sessions values(?,?)', ('session_unrelated_latest', 'task_unrelated_latest'))
            c.execute('create table frozen_requests(request_id text, session_id text, provider_id text, model text, request_json text)')
        self.put(self.data / 'workers/w.journal', {
            'store_dir': str(self.db.parent), 'project_root': str(self.project),
            'agents': [{'id': 'agent1', 'name': 'c-fixture', 'conversation': 'task1'}]})
        self.binding = self.r / 'binding.json'
        e.bind(str(self.home), 'c-fixture', str(self.binding))
        return e.load(self.binding)
    def test_bind_uses_exact_journal_name_not_global_latest(self):
        b = self.bound_fixture()
        self.assertEqual((b['session'], b['task'], b['agent']), ('session1', 'task1', 'agent1'))
        self.assertEqual(Path(b['db']), self.db)
    def add_request(self, rid, sid, req):
        with closing(sqlite3.connect(self.db)) as c, c:
            c.execute('insert into frozen_requests values(?,?,?,?,?)', (rid, sid, 'openai-codex', 'gpt-6-luna', json.dumps(req)))
    def test_frozen_reconstructs_deltas_and_excludes_other_sessions(self):
        self.bound_fixture()
        base = {'messages': [{'role': 'system', 'content': 'CW-FIXTURE'}, {'role': 'user', 'content': 'old'}], 'tool_schemas': ['schema']}
        self.add_request('r1', 'session1', base)
        self.add_request('other', 'session_unrelated_latest', {'messages': [{'role': 'user', 'content': 'WRONG'}]})
        self.add_request('r2', 'session1', {'delta_of': 'r1', 'delta_prefix': 1, 'delta_tools': True, 'messages': [{'role': 'user', 'content': 'new'}]})
        rows = e.frozen(self.binding)
        self.assertEqual([r['request_id'] for r in rows], ['r1', 'r2'])
        self.assertEqual(rows[1]['request']['messages'], [base['messages'][0], {'role': 'user', 'content': 'new'}])
        self.assertEqual(rows[1]['request']['tool_schemas'], ['schema'])
        self.assertNotIn('delta_of', rows[1]['request'])
    def test_context_uses_messages_not_tool_schema_descriptions(self):
        self.bound_fixture()
        self.add_request('r1', 'session1', {'messages': [{'role': 'user', 'content': 'query'}], 'tool_schemas': ['CW-FIXTURE']})
        e.context(str(self.binding), 'CW-FIXTURE', 'absent', str(self.r / 'context.json'))
        self.assertFalse(e.load(self.r / 'context.json')['needle_present'])

    def test_answer_is_bound_to_new_session_and_completed(self):
        self.bound_fixture()
        with closing(sqlite3.connect(self.db)) as c, c:
            c.execute('drop table events')
            c.execute('create table events(session_id text, sequence integer, event_json text)')
            events = [('other', 1, {'event_type': 'model.response', 'payload': {'text': 'WRONG'}}),
                      ('session1', 2, {'event_type': 'model.response', 'payload': {'text': 'CODE=UNKNOWN', 'tool_calls': []}})]
            for sid, sequence, event in events:
                c.execute('insert into events values(?,?,?)', (sid, sequence, json.dumps(event)))
        with self.assertRaisesRegex(AssertionError, 'did not complete'):
            e.answer(str(self.binding), str(self.r / 'answer.txt'))
        with closing(sqlite3.connect(self.db)) as c, c:
            c.execute('insert into events values(?,?,?)', ('session1', 3, json.dumps({'event_type': 'session.completed'})))
        e.answer(str(self.binding), str(self.r / 'answer.txt'))
        self.assertEqual((self.r / 'answer.txt').read_text(), 'CODE=UNKNOWN\n')

if __name__ == '__main__':
    unittest.main(verbosity=2)
