import json
from pathlib import Path
import tempfile
import unittest
import uuid
import wrapper


class CollectionExample(unittest.TestCase):
    def test_nested_and_empty_collections_keep_identities(self):
        for paths in ([], ["mesh/a.bin", "snapshot-002.bin"]):
            with self.subTest(paths=paths), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "inputs").mkdir()
                members = []
                for path in paths:
                    source = root / "inputs" / path
                    source.parent.mkdir(parents=True, exist_ok=True)
                    source.write_bytes(b"verified fixture")
                    members.append({"path": path, "media_type": "application/octet-stream",
                                    "scientific_metadata": {"step": 2}})
                (root / "manifest.json").write_text(json.dumps(members))
                identity = str(uuid.uuid4())
                request = {k: str(uuid.uuid4()) for k in ("run_id", "assignment_id", "attempt_id", "launch_intent_id")}
                request.update(wrapper_protocol_version=1, model_id="collection-example",
                               publication_id="example", inputs={"dataset": {"collection_id": identity}},
                               input_files={}, input_collections={"dataset": {"collection_id": identity,
                               "root": "inputs", "manifest": "manifest.json"}})
                (root / "request.json").write_text(json.dumps(request))
                wrapper.execute(root / "request.json", root / "result.json")
                result = json.loads((root / "result.json").read_text())
                self.assertEqual(result["attempt_id"], request["attempt_id"])
                self.assertEqual(result["outputs"]["member_count"], len(paths))
                for path in paths:
                    self.assertEqual((root / "snapshots" / path).read_bytes(), b"verified fixture")

    def test_unsafe_and_linked_members_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for path in ("../escape", "/absolute", "a//b"):
                with self.assertRaises(ValueError):
                    wrapper.confined(root, path)
            (root / "file").write_text("fixture")
            (root / "link").symlink_to(root / "file")
            with self.assertRaises(ValueError):
                wrapper.confined(root, "link")
