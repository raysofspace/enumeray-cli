"""Run directly from this standalone directory; no repository or service required."""
import contextlib
import copy
import io
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import wrapper
from protocol import BoundaryError, ModelError

RELEASE = Path(__file__).resolve().parent
REQUEST = json.loads((RELEASE / "fixtures/request.json").read_text())
MANIFEST = json.loads((RELEASE / "fixtures/workspace/manifest.json").read_text())


class CollectionExample(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        shutil.copytree(RELEASE / "fixtures/workspace", self.root, dirs_exist_ok=True)
        self.request = copy.deepcopy(REQUEST)
        self.request_path, self.result_path = self.root / "request.json", self.root / "result.json"

    def execute(self, raw=None):
        self.request_path.write_text(raw if raw is not None else json.dumps(self.request))
        with contextlib.redirect_stderr(io.StringIO()) as diagnostics:
            code = wrapper.main(["execute", "--request", str(self.request_path), "--result", str(self.result_path)])
        self.assertNotIn(str(self.root), diagnostics.getvalue())
        self.assertNotIn("private-sentinel", diagnostics.getvalue())
        self.assertLess(len(diagnostics.getvalue()), 256)
        return code

    def reject_without_science(self, raw=None):
        with mock.patch.object(wrapper, "evaluate") as science:
            self.assertEqual(self.execute(raw), 2)
            science.assert_not_called()
        self.assertFalse(self.result_path.exists())
        self.assertFalse((self.root / "snapshots").exists())
        self.assertFalse((self.root / "snapshots-members.json").exists())
        for member in MANIFEST:
            self.assertEqual((self.root / "inputs" / member["path"]).read_bytes(),
                             (RELEASE / "fixtures/workspace/inputs" / member["path"]).read_bytes())

    def test_nested_fixture_and_empty_collection(self):
        self.assertEqual(len(MANIFEST), 2)
        self.assertTrue(all('/' in member['path'] for member in MANIFEST))
        self.assertEqual(self.execute(), 0)
        expected = json.loads((RELEASE / "fixtures/expected-result.json").read_text())
        self.assertEqual(json.loads(self.result_path.read_text()), expected)
        for member in MANIFEST:
            self.assertEqual((self.root / "snapshots" / member["path"]).read_bytes(),
                             (self.root / "inputs" / member["path"]).read_bytes())
        declared = json.loads((self.root / "snapshots-members.json").read_text())
        self.assertEqual(declared, [{key: member[key] for key in ("path", "media_type", "scientific_metadata")}
                                    for member in MANIFEST])
        self.assertEqual(self.result_path.stat().st_mode & 0o777, 0o600)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "inputs").mkdir()
            (root / "manifest.json").write_text("[]")
            (root / "request.json").write_text(json.dumps(REQUEST))
            result = wrapper.execute(root / "request.json", root / "result.json")
            self.assertEqual(result["outputs"], {"member_count": 0})
            self.assertEqual((root / "snapshots-members.json").read_text(), "[]\n")

    def test_invalid_request_and_mapping_are_rejected_before_science(self):
        mutations = [{"run_id": "not-a-uuid"}, {"model_id": "wrong-model"},
                     {"publication_id": "bad/id"}, {"wrapper_protocol_version": True},
                     {"wrapper_protocol_version": 1.0}, {"inputs": []}, {"input_files": {"bad": {}}},
                     {"input_collections": []}, {"input_collections": {"unknown": {}}},
                     {"inputs": {"dataset": {"collection_id": "not-a-uuid"}}},
                     {"inputs": {"dataset": {"collection_id": REQUEST["inputs"]["dataset"]["collection_id"], "extra": 1}}},
                     {"unexpected": "private-sentinel"}]
        for patch in mutations:
            with self.subTest(patch=patch):
                self.request = {**copy.deepcopy(REQUEST), **patch}
                self.reject_without_science()
        for field, value in (("root", "../escape"), ("root", "/absolute"), ("root", None),
                             ("manifest", "../manifest.json"), ("manifest", "/absolute"),
                             ("collection_id", "66666666-6666-4666-8666-666666666666"),
                             ("unknown", 1)):
            self.request = copy.deepcopy(REQUEST)
            self.request["input_collections"]["dataset"][field] = value
            self.reject_without_science()
        self.request = copy.deepcopy(REQUEST)
        del self.request["attempt_id"]
        self.reject_without_science()
        self.request = copy.deepcopy(REQUEST)
        for raw in ('[]', '{', json.dumps(REQUEST).replace('"wrapper_protocol_version": 1',
                    '"wrapper_protocol_version": 1, "wrapper_protocol_version": 1'),
                    json.dumps(REQUEST) + " " * 65536):
            self.reject_without_science(raw)

    def test_invalid_late_manifest_member_creates_no_output(self):
        mutations = [{"path": bad} for bad in ("../escape", "/absolute", "a//b", "a/./b", "a\\b", "a:b", "a.", "a ")]
        mutations += [{"scientific_metadata": []}, {"media_type": "text/plain"},
                      {"size_bytes": -1}, {"size_bytes": 1.5}, {"size_bytes": True},
                      {"size_bytes": 999}, {"checksum_sha256": "not-sha256"},
                      {"artifact_id": "not-a-uuid"}, {"version_id": ""},
                      {"unknown": "private-sentinel"}]
        for patch in mutations:
            with self.subTest(patch=patch):
                members = copy.deepcopy(MANIFEST)
                members[-1].update(patch)
                (self.root / "manifest.json").write_text(json.dumps(members))
                self.reject_without_science()
        for members in ({}, [MANIFEST[0], MANIFEST[0]], list(reversed(MANIFEST)),
                        [dict(MANIFEST[0], path="mesh"), MANIFEST[1]]):
            (self.root / "manifest.json").write_text(json.dumps(members))
            self.reject_without_science()
        raw = json.dumps(MANIFEST).replace('"path": "series/snapshot-002.bin"',
              '"path": "series/snapshot-002.bin", "path": "series/snapshot-002.bin"')
        (self.root / "manifest.json").write_text(raw)
        self.reject_without_science()
        for depth in (65, 350):
            members = copy.deepcopy(MANIFEST)
            value = {}
            for _ in range(depth):
                value = {"nested": value}
            members[-1]["scientific_metadata"] = value
            (self.root / "manifest.json").write_text(json.dumps(members))
            self.reject_without_science()

    def test_symlinked_input_root_manifest_and_member_are_rejected(self):
        for field in ("root", "manifest"):
            original = self.request["input_collections"]["dataset"][field]
            (self.root / "linked").symlink_to(self.root / original)
            self.request["input_collections"]["dataset"][field] = "linked"
            self.reject_without_science()
            self.request["input_collections"]["dataset"][field] = original
            (self.root / "linked").unlink()
        member = self.root / "inputs/series/snapshot-002.bin"
        retained = self.root / "retained-input"
        member.rename(retained)
        member.symlink_to(retained)
        self.reject_without_science()

    def test_metadata_rejects_service_storage_violations_before_science(self):
        metadata = [{"$serde_json::private::Number": "1"}, {"$serde_json::private::RawValue": "1"},
                    {"key\0": True}, {"key": "value\0"}]
        for value in metadata:
            members = copy.deepcopy(MANIFEST)
            members[-1]["scientific_metadata"] = value
            (self.root / "manifest.json").write_text(json.dumps(members))
            self.reject_without_science()
        for numeric in ('1e-20000', '1e200000', '1e65536', '1e40000,"extra":1e40000'):
            members = copy.deepcopy(MANIFEST)
            members[-1]["scientific_metadata"] = {"number": "NUMBER"}
            raw = json.dumps(members).replace('"NUMBER"', numeric)
            (self.root / "manifest.json").write_text(raw)
            self.reject_without_science()

    def test_result_cannot_alias_collection_outputs(self):
        self.request_path.write_text(json.dumps(REQUEST))
        for filename in ("snapshots", "snapshots-members.json", "Snapshots", "SNAPSHOTS-MEMBERS.JSON"):
            with mock.patch.object(wrapper, "evaluate") as science:
                with self.assertRaises(BoundaryError):
                    wrapper.execute(self.request_path, self.root / filename)
                science.assert_not_called()
            self.assertFalse((self.root / "snapshots").exists())
            self.assertFalse((self.root / filename).exists())

    def test_existing_result_outputs_and_symlink_destinations_preserve_bytes(self):
        for path in (self.result_path, self.root / "snapshots-members.json"):
            path.write_bytes(b"retained evidence")
            with mock.patch.object(wrapper, "evaluate") as science:
                self.assertEqual(self.execute(), 2)
                science.assert_not_called()
            self.assertEqual(path.read_bytes(), b"retained evidence")
            self.assertFalse((self.root / "snapshots").exists())
            path.unlink()
        self.result_path.symlink_to(self.root / "missing")
        self.assertEqual(self.execute(), 2)
        self.assertTrue(self.result_path.is_symlink())
        self.assertFalse((self.root / "missing").exists())
        self.result_path.unlink()
        (self.root / "snapshots").mkdir()
        (self.root / "snapshots/retained").write_bytes(b"existing scientific output")
        with mock.patch.object(wrapper, "evaluate") as science:
            self.assertEqual(self.execute(), 2)
            science.assert_not_called()
        self.assertEqual((self.root / "snapshots/retained").read_bytes(), b"existing scientific output")
        self.assertFalse(self.result_path.exists())

    def test_scientific_failure_is_explicit_and_safe(self):
        with mock.patch.object(wrapper, "evaluate", side_effect=ModelError("private-sentinel")):
            self.assertEqual(self.execute(), 1)
        result = json.loads(self.result_path.read_text())
        self.assertEqual(result["outcome"], "failed")
        self.assertEqual(result["outputs"], {})
        self.assertEqual(result["failure"]["code"], "model_failure")
        self.assertNotIn("private-sentinel", self.result_path.read_text())

    def test_publication_failure_is_not_recast_as_a_scientific_result(self):
        with mock.patch.object(wrapper, "copy_member", side_effect=BoundaryError("private-sentinel")):
            self.assertEqual(self.execute(), 2)
        self.assertFalse(self.result_path.exists())
        self.assertEqual(list((self.root / "snapshots").iterdir()), [])
        self.assertEqual(self.execute(), 2)  # retained tree fences interrupted work

    def test_competing_wrappers_publish_one_consistent_output_set(self):
        self.request_path.write_text(json.dumps(REQUEST))
        argv = [sys.executable, "-B", str(RELEASE / "wrapper.py"), "execute",
                "--request", str(self.request_path), "--result", str(self.result_path)]
        workers = [subprocess.Popen(argv, cwd=self.root, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                   for _ in range(2)]
        for worker in workers:
            _, error = worker.communicate(timeout=10)
            self.assertNotIn(b"Traceback", error)
        self.assertEqual(sorted(worker.returncode for worker in workers), [0, 2])
        self.assertEqual(json.loads(self.result_path.read_text())["outputs"], {"member_count": 2})
        for member in MANIFEST:
            self.assertEqual((self.root / "snapshots" / member["path"]).read_bytes(),
                             (self.root / "inputs" / member["path"]).read_bytes())


if __name__ == "__main__":
    unittest.main()
