"""The separately downloadable reference preserves exact scientific JSON bytes."""
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1] / "examples/http-contract"
SPEC = importlib.util.spec_from_file_location("collection_identity", ROOT / "collection-identity.py")
identity = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(identity)


class CollectionIdentityTests(unittest.TestCase):
    def test_published_vectors_and_exact_inputs(self):
        self.assertEqual(identity.verify_vectors(ROOT / "digest-vectors"), 4)
        members = identity.loads((ROOT / 'digest-vectors/mixed-requested.input.json').read_bytes())
        metadata = identity.loads((ROOT / 'digest-vectors/a-metadata.json').read_bytes())
        self.assertEqual(identity.encode(metadata), identity.encode(members[0]['scientific_metadata']))

    def test_number_spelling_is_lossless_and_not_jcs(self):
        value = identity.loads('[1,1.0,1e0,1E+00,-0,-0.0,90071992547409921234567890.123456789]')
        self.assertEqual(identity.encode(value), '[1,1.0,1e+0,1e+00,0,-0.0,90071992547409921234567890.123456789]')
        self.assertNotEqual(identity.encode(identity.loads('1')), identity.encode(identity.loads('1.0')))

    def test_duplicate_keys_nonfinite_numbers_and_floats_are_rejected(self):
        for source in ['{"a":1,"a":2}', 'NaN', 'Infinity']:
            with self.assertRaises(ValueError):
                identity.loads(source)
        with self.assertRaises(ValueError):
            identity.encode(1.5)

    def test_member_order_normalization_and_required_fields_are_not_repaired(self):
        source = ROOT / 'digest-vectors/mixed-requested.input.json'
        members = identity.loads(source.read_bytes())
        for changed in [members[::-1], [dict(members[0], path='cafe\u0301')],
                        [dict(members[0], scientific_metadata=None)],
                        [dict(members[0], checksum_sha256='not-base64')],
                        [{key: value for key, value in members[0].items() if key != 'scientific_metadata'}]]:
            with self.assertRaises(ValueError):
                identity.manifest_bytes(changed)


if __name__ == '__main__':
    unittest.main()
