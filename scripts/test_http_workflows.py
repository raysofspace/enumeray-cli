"""Workflow regressions for retained reuse intent and paginated inspection."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / 'examples/http-contract/raw-http.py'
SPEC = importlib.util.spec_from_file_location('raw_http', SOURCE)
raw = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(raw)
FIRST = '11111111-1111-4111-8111-111111111111'
SECOND = '22222222-2222-4222-8222-222222222222'


class PagedApi:
    operation = raw.Api.operation
    origin = 'https://enumeray.example.org'
    user_id = '11111111-1111-4111-8111-111111111111'

    def __init__(self):
        self.members = {}
        self.bindings = {}
        self.read_cursors = []
        self.creates = 0

    def request(self, method, path, body=None, key=None):
        if method == 'POST':
            self.creates += 1
            return {'collection_id': '33333333-3333-4333-8333-333333333333', 'request': body}
        if method == 'PUT' and path.endswith('/artifact'):
            index = int(path.split('/')[-2])
            if index in self.bindings and self.bindings[index] != body['artifact_id']:
                raise ValueError('binding conflict')
            self.bindings[index] = body['artifact_id']
            return {'bound': True}
        if method == 'PUT' and '/members/' in path:
            self.members[int(path.split('/')[-1])] = body[0]
            return {'registered': True}
        if method == 'PUT' and path.endswith('/freeze'):
            return {'frozen': True}
        if method == 'GET' and '/artifacts/' in path:
            return {'content': {'version_id': 'v1'}}
        if method == 'GET' and '/members' in path:
            cursor = int(path.split('cursor=')[1]) if 'cursor=' in path else 0
            self.read_cursors.append(cursor)
            return {'items': [{'artifact_id': self.bindings[cursor], 'version_id': 'v1'}],
                    'next_cursor': cursor + 1 if cursor + 1 < len(self.members) else None}
        pins = [dict(member, artifact_id=self.bindings[index], version_id='v1')
                for index, member in sorted(self.members.items())]
        return {'state': 'ready', 'manifest_digest': raw.identity.digest(raw.identity.manifest_bytes(pins, True)), 'available': True}


class HttpWorkflowTests(unittest.TestCase):
    def test_interrupted_state_publication_is_atomic_and_resumes_original_key(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            target = root / 'request.json'
            original = b'{"exact":"original"}'
            with patch.object(raw.os, 'link', side_effect=InterruptedError):
                with self.assertRaises(InterruptedError):
                    raw.write_private(target, original, True)
            self.assertFalse(target.exists())
            self.assertEqual(list(root.iterdir()), [])
            raw.write_private(target, original, True)
            with patch.object(raw.os, 'replace', side_effect=InterruptedError):
                with self.assertRaises(InterruptedError):
                    raw.write_private(target, b'{"partial":"replacement"}')
            self.assertEqual(target.read_bytes(), original)
            api = PagedApi()
            operation = root / 'nested' / 'operation'
            key = api.operation(operation, original)
            self.assertEqual(api.operation(operation, original), key)
            self.assertEqual((operation / 'idempotency-key.txt').read_text(), key)
            (operation / 'idempotency-key.txt').unlink()
            with patch.object(raw.uuid, 'uuid4', side_effect=AssertionError('must reuse original key')):
                with patch.object(raw, 'sync_directory', wraps=raw.sync_directory) as synced:
                    self.assertEqual(api.operation(operation, original), key)
                self.assertIn(operation, [call.args[0] for call in synced.call_args_list])
                self.assertIn(operation.parent, [call.args[0] for call in synced.call_args_list])
            with self.assertRaisesRegex(ValueError, 'saved operation differs'):
                api.operation(operation, b'{"changed":true}')
            (operation / 'operation.json').unlink()
            with patch.object(raw.uuid, 'uuid4', side_effect=AssertionError('missing identity must stop')):
                with self.assertRaisesRegex(ValueError, 'saved operation is incomplete'):
                    api.operation(operation, original)

    def test_resume_retains_reuse_map_and_follows_member_read_cursors(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            source = root / 'files'
            source.mkdir()
            for name in ['a', 'b']:
                (source / name).write_bytes(b'abc')
            members = [{'path': name, 'media_type': 'text/plain', 'scientific_metadata': {},
                        'size_bytes': 3, 'checksum_sha256': raw.checksum(b'abc')} for name in ['a', 'b']]
            manifest = root / 'manifest.json'
            manifest.write_text(json.dumps(members))
            reuse = root / 'reuse.json'
            reuse.write_text(json.dumps({'a': FIRST}))
            api = PagedApi()
            uploaded = []

            def upload(api, path, directory, metadata, media_type):
                uploaded.append(path.name)
                return {'artifact_id': SECOND, 'version_id': 'v1'}

            with patch.object(raw, 'artifact', upload), patch.object(raw, 'download'):
                first = raw.collection(api, manifest, source, root / 'state', reuse)
                repeated = raw.collection(api, manifest, source, root / 'state')
                self.assertEqual(first, repeated)
                self.assertEqual(uploaded, ['b', 'b'])
                self.assertEqual(api.read_cursors, [0, 1, 0, 1])
                reuse.write_text(json.dumps({'a': SECOND}))
                creates = api.creates
                with self.assertRaisesRegex(ValueError, 'saved operation differs'):
                    raw.collection(api, manifest, source, root / 'state', reuse)
                self.assertEqual(api.creates, creates)
                self.assertEqual(uploaded, ['b', 'b'])


if __name__ == '__main__':
    unittest.main()
