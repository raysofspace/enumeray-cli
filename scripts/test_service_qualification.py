"""Release authorization verifies signed source identities before delivery."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('service_qualification', Path(__file__).with_name('verify-service-qualification.py'))
qualification = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualification)


class QualificationTests(unittest.TestCase):
    def request(self, **changes):
        receipt = {'public_revision': 'a' * 40, 'service_revision': 'b' * 40,
                   'version': '0.1.0', 'service_repository': 'raysofspace/enumeray',
                   'qualification_run': '123'}
        receipt.update(changes)
        return {'receipt': json.dumps(receipt), 'bundle': {'mediaType': 'fixture'}}

    def test_exact_signed_bytes_and_fixed_private_workflow_identity_are_verified(self):
        request = self.request()
        verifier = mock.Mock()
        with tempfile.TemporaryDirectory() as temporary:
            result = qualification.verify_request(request, Path(temporary), run=verifier)
            self.assertEqual((Path(temporary) / 'service-qualification.json').read_text(), request['receipt'])
        self.assertEqual(result, {'revision': 'a' * 40, 'tag': 'binaries-v0.1.0'})
        argv = verifier.call_args.args[0]
        self.assertEqual(argv[:2], ['cosign', 'verify-blob'])
        self.assertEqual(argv[argv.index('--certificate-identity') + 1],
                         'https://github.com/raysofspace/enumeray/.github/workflows/release.yml@refs/tags/binaries-v0.1.0')
        self.assertEqual(argv[argv.index('--certificate-oidc-issuer') + 1], qualification.ISSUER)
        self.assertTrue(verifier.call_args.kwargs['check'])

    def test_forged_or_modified_receipt_cannot_produce_authorized_outputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            run = mock.Mock(side_effect=subprocess.CalledProcessError(1, 'cosign'))
            with self.assertRaises(subprocess.CalledProcessError):
                qualification.verify_request(self.request(public_revision='c' * 40), Path(temporary), run=run)

    def test_invalid_authority_or_source_identity_is_rejected_before_signer(self):
        for change in ({'service_repository': 'other/service'}, {'public_revision': 'main'},
                       {'service_revision': ''}, {'version': '0.1.0\nrevision=forged'},
                       {'qualification_run': ''}):
            verifier = mock.Mock()
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                with self.assertRaises(ValueError):
                    qualification.verify_request(self.request(**change), Path(temporary), run=verifier)
                verifier.assert_not_called()

    def test_unsigned_dispatch_fields_are_not_release_authority(self):
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(ValueError, 'signed service qualification'):
                qualification.verify_request({'public_revision': 'a' * 40, 'version': '0.1.0'}, Path(temporary))

    def test_failed_verification_leaves_no_github_outputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / 'outputs'
            with mock.patch.dict(os.environ, QUALIFIED_REQUEST=json.dumps(self.request()), GITHUB_OUTPUT=str(output)), \
                    mock.patch.object(qualification, 'verify_request', side_effect=ValueError('invalid signature')):
                with self.assertRaises(ValueError):
                    qualification.main()
            self.assertFalse(output.exists())


if __name__ == '__main__':
    unittest.main()
