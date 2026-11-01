"""Standalone conformance tests shared verbatim by examples and generated starters."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

import protocol

RELEASE = Path(__file__).resolve().parent


class ProtocolBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_identity_envelope_and_model_preflight(self):
        request = {"wrapper_protocol_version": 1, "model_id": "protocol-fixture",
                   "publication_id": "fresh-rollback_2", "inputs": {}, "input_files": {}}
        request.update({key: "11111111-1111-4111-8111-111111111111" for key in protocol.IDENTITIES})
        path = self.root / "request.json"
        scientific = {"model": {"id": "protocol-fixture"}}
        path.write_text(json.dumps(request))
        protocol.read_request(path, scientific)
        mutations = [{key: bad} for key in protocol.IDENTITIES for bad in (
            "not-a-uuid", "00000000-0000-0000-0000-000000000000",
            "11111111111141118111111111111111", True, None)]
        mutations += [{"model_id": "wrong-model"}, {"publication_id": "bad/id"},
                      {"wrapper_protocol_version": True}, {"wrapper_protocol_version": 1.0},
                      {"wrapper_protocol_version": 2}, {"input_files": []}, {"inputs": []},
                      {"unknown": "private-sentinel"}]
        for patch in mutations:
            with self.subTest(patch=patch):
                path.write_text(json.dumps({**request, **patch}))
                with self.assertRaises(protocol.BoundaryError):
                    protocol.read_request(path, scientific)
        for raw in ('{"x":1,"x":2}', '[]', '{', 'NaN', ' ' * 65537):
            path.write_text(raw)
            with self.assertRaises(protocol.BoundaryError):
                protocol.read_request(path, scientific)
        path.write_text(json.dumps(request).replace('"wrapper_protocol_version": 1',
                                                  '"wrapper_protocol_version": 1e0'))
        with self.assertRaises(protocol.BoundaryError):
            protocol.read_request(path, scientific)

    def test_confinement_rejects_aliases_and_every_symlink_component(self):
        (self.root / "inputs").mkdir()
        (self.root / "inputs/file").write_bytes(b"fixture")
        (self.root / "linked-directory").symlink_to(self.root / "inputs", target_is_directory=True)
        (self.root / "inputs/link").symlink_to(self.root / "inputs/file")
        for relative in ("../escape", "/absolute", "a//b", "a/./b", "a/../b", "a\\b",
                         "a:b", "a\n", "a.", "a ", "e\u0301", "linked-directory/file",
                         "inputs/link", "inputs", "", None, "\ud800"):
            with self.subTest(relative=relative), self.assertRaises(protocol.BoundaryError):
                protocol.confined(self.root, relative)
        self.assertEqual(protocol.confined(self.root, "inputs/file").read_bytes(), b"fixture")

    def test_json_container_depth_is_bounded_before_science(self):
        path = self.root / "nested.json"
        path.write_text("[" * 64 + "0" + "]" * 64)
        protocol.read_json(path)
        for depth in (65, 350):
            path.write_text("[" * depth + "0" + "]" * depth)
            with self.assertRaises(protocol.ModelError):
                protocol.read_json(path)

    def test_storage_values_reserved_keys_nuls_and_decimal_limits(self):
        path = self.root / "storage.json"
        invalid = ['{"$serde_json::private::Number":"1"}', '{"$serde_json::private::RawValue":"1"}',
                   '{"\\u0024serde_json::private::Number":"1"}', '{"key\\u0000":true}',
                   '{"key":"value\\u0000"}', '{"\\ud800":1}', '"\\ud800"',
                   '1e-20000', '1e200000', '0e147456']
        for raw in invalid:
            with self.subTest(raw=raw):
                path.write_text(raw)
                with self.assertRaises(protocol.ModelError):
                    protocol.read_json(path)
        for raw in ('{"x":1e65536}', '{"x":1e40000,"y":1e40000}'):
            path.write_text(raw)
            value = protocol.read_json(path)
            with self.assertRaises(protocol.ModelError):
                protocol.validate_storage(value)

    def test_existing_and_dangling_result_preserve_evidence(self):
        destination = self.root / "result.json"
        destination.write_bytes(b"retained")
        with self.assertRaises(protocol.BoundaryError):
            protocol.preflight_result(destination)
        with self.assertRaises(protocol.BoundaryError):
            protocol.publish(destination, b"replacement")
        self.assertEqual(destination.read_bytes(), b"retained")
        destination.unlink()
        destination.symlink_to(self.root / "missing")
        with self.assertRaises(protocol.BoundaryError):
            protocol.preflight_result(destination)
        with self.assertRaises(protocol.BoundaryError):
            protocol.publish(destination, b"replacement")
        self.assertTrue(destination.is_symlink())
        self.assertFalse((self.root / "missing").exists())

    def test_write_file_sync_and_link_failure_never_publish_partial_bytes(self):
        destination = self.root / "result.json"
        original = protocol.os.fdopen

        class PartialWriter:
            def __enter__(self):
                self.file = original(self.fd, "wb")
                return self

            def __exit__(self, *args):
                return self.file.__exit__(*args)

            def write(self, content):
                return self.file.write(content[:2])

        def partial(fd, mode):
            output = PartialWriter()
            output.fd = fd
            return output

        faults = [mock.patch.object(protocol.os, "fdopen", side_effect=partial),
                  mock.patch.object(protocol.os, "fsync", side_effect=OSError("private-sentinel")),
                  mock.patch.object(protocol.os, "link", side_effect=OSError("private-sentinel"))]
        for fault in faults:
            with fault, self.assertRaises(protocol.BoundaryError):
                protocol.publish(destination, b"complete result bytes")
            self.assertFalse(destination.exists())
            self.assertEqual(list(self.root.glob(".model-result-*")), [])

    def test_directory_sync_failure_preserves_complete_published_file(self):
        destination = self.root / "result.json"
        with mock.patch.object(protocol.os, "fsync", side_effect=[None, OSError("failure")]):
            with self.assertRaises(protocol.BoundaryError):
                protocol.publish(destination, b"complete result bytes")
        self.assertEqual(destination.read_bytes(), b"complete result bytes")
        self.assertEqual(list(self.root.glob(".model-result-*")), [])

    def test_result_limit_and_exclusive_retained_workspace_claim(self):
        with self.assertRaises(protocol.ModelError):
            protocol.publish(self.root / "result.json", b"x" * 65537)
        self.assertFalse((self.root / "result.json").exists())
        protocol.claim_workspace(self.root)
        with self.assertRaises(protocol.BoundaryError):
            protocol.claim_workspace(self.root)

    def test_competing_publishers_choose_one_complete_result(self):
        destination = self.root / "result.json"
        script = ("import sys; sys.path.insert(0,sys.argv[1]); import protocol\n"
                  "try: protocol.publish(sys.argv[2],sys.argv[3].encode())\n"
                  "except protocol.ModelError: sys.exit(2)\n")
        workers = [subprocess.Popen([sys.executable, "-B", "-c", script, str(RELEASE),
                                    str(destination), value], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                   for value in ("first complete result", "second complete result")]
        for worker in workers:
            worker.communicate(timeout=10)
        self.assertEqual(sorted(worker.returncode for worker in workers), [0, 2])
        self.assertIn(destination.read_text(), ("first complete result", "second complete result"))

    def test_interruption_before_and_after_commit_never_exposes_partial_result(self):
        for phase in ("before-link", "after-link"):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                destination, marker = root / "result.json", root / "paused"
                script = """import os, signal, sys
sys.path.insert(0,sys.argv[1])
import protocol
from pathlib import Path
marker=Path(sys.argv[3])
def pause():
    marker.write_text('paused')
    signal.pause()
if sys.argv[4]=='before-link':
    original=protocol.os.link
    def link(*args):
        pause()
        return original(*args)
    protocol.os.link=link
else:
    original=protocol.os.fsync
    calls=0
    def fsync(fd):
        global calls
        calls+=1
        if calls==2: pause()
        return original(fd)
    protocol.os.fsync=fsync
protocol.publish(sys.argv[2],b'complete result bytes')
"""
                worker = subprocess.Popen([sys.executable, "-B", "-c", script, str(RELEASE),
                                           str(destination), str(marker), phase], stderr=subprocess.PIPE)
                try:
                    deadline = time.monotonic() + 10
                    while not marker.exists() and worker.poll() is None and time.monotonic() < deadline:
                        time.sleep(0.01)
                    self.assertTrue(marker.exists())
                finally:
                    worker.terminate()
                    worker.communicate(timeout=5)
                if phase == "before-link":
                    self.assertFalse(destination.exists())
                else:
                    self.assertEqual(destination.read_bytes(), b"complete result bytes")


if __name__ == "__main__":
    unittest.main()
