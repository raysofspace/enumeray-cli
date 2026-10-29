"""Release authorization binds purpose, source and baseline before delivery."""
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
        receipt = {'schema_version': 1, 'purpose': 'prepare',
                   'public_revision': 'a' * 40, 'service_revision': 'b' * 40,
                   'version': '0.1.0', 'service_repository': 'raysofspace/enumeray',
                   'public_repository': 'raysofspace/enumeray-cli',
                   'baseline': {'id': 'persistent-v1', 'sha256': 'c' * 64},
                   'qualification_run': '123'}
        receipt.update(changes)
        return {'receipt': json.dumps(receipt), 'bundle': {'mediaType': 'fixture'}}

    def test_exact_bytes_service_workflow_source_ref_and_trigger_are_verified(self):
        request, verifier = self.request(), mock.Mock()
        with tempfile.TemporaryDirectory() as temporary:
            result = qualification.verify_request(request, Path(temporary), run=verifier)
            self.assertEqual((Path(temporary) / 'service-prepare.json').read_text(), request['receipt'])
        self.assertEqual(result['revision'], 'a' * 40)
        self.assertEqual(result['tag'], 'v0.1.0')
        argv = verifier.call_args.args[0]
        for option, expected in {
                '--certificate-identity': 'https://github.com/raysofspace/enumeray/.github/workflows/release.yml@refs/tags/cli-v0.1.0',
                '--certificate-oidc-issuer': qualification.ISSUER,
                '--certificate-github-workflow-sha': 'b' * 40,
                '--certificate-github-workflow-ref': 'refs/tags/cli-v0.1.0',
                '--certificate-github-workflow-trigger': 'push'}.items():
            self.assertEqual(argv[argv.index(option) + 1], expected)
        self.assertTrue(verifier.call_args.kwargs['check'])

    def test_forged_receipt_cannot_produce_authorized_outputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaises(subprocess.CalledProcessError):
                qualification.verify_request(self.request(), Path(temporary),
                    run=mock.Mock(side_effect=subprocess.CalledProcessError(1, 'cosign')))

    def test_public_legacy_or_wrong_version_service_signature_is_rejected(self):
        for certificate_ref in ('refs/tags/v0.1.0', 'refs/tags/binaries-v0.1.0',
                                'refs/tags/cli-v0.2.0'):
            def verifier(command, **kwargs):
                expected_ref = command[command.index('--certificate-github-workflow-ref') + 1]
                if certificate_ref != expected_ref:
                    raise subprocess.CalledProcessError(1, command)

            with self.subTest(certificate_ref=certificate_ref), tempfile.TemporaryDirectory() as temporary:
                with self.assertRaises(subprocess.CalledProcessError):
                    qualification.verify_request(self.request(), Path(temporary), run=verifier)

    def test_invalid_authority_source_baseline_and_version_are_rejected(self):
        changes = ({'service_repository': 'other/service'}, {'public_repository': 'other/client'},
                   {'public_revision': 'main'}, {'service_revision': ''}, {'schema_version': 2},
                   {'version': '0.1.0\nrevision=forged'}, {'version': '0.1.0-rc1'},
                   {'qualification_run': ''}, {'baseline': {'id': 'candidate', 'sha256': 'c' * 64}})
        for change in changes:
            verifier = mock.Mock()
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                with self.assertRaises(ValueError):
                    qualification.verify_request(self.request(**change), Path(temporary), run=verifier)
                verifier.assert_not_called()

    def test_build_receipt_never_authorizes_publication(self):
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(ValueError, 'wrong purpose'):
                qualification.verify_request(self.request(), Path(temporary), run=mock.Mock(), purpose='publish')
            with self.assertRaisesRegex(ValueError, 'manifest digest'):
                qualification.verify_request(self.request(purpose='publish'), Path(temporary), run=mock.Mock(), purpose='publish')
            result = qualification.verify_request(self.request(purpose='publish', manifest_sha256='d' * 64),
                                                   Path(temporary), run=mock.Mock(), purpose='publish')
            self.assertEqual(result['manifest_sha256'], 'd' * 64)

    def test_unsigned_dispatch_fields_are_not_release_authority(self):
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(ValueError, 'signed service qualification'):
                qualification.verify_request({'public_revision': 'a' * 40}, Path(temporary))

    def test_failed_verification_leaves_no_github_outputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / 'outputs'
            with mock.patch.dict(os.environ, QUALIFIED_REQUEST=json.dumps(self.request()), GITHUB_OUTPUT=str(output)), \
                    mock.patch('sys.argv', ['verify-service-qualification.py']), \
                    mock.patch.object(qualification, 'verify_request', side_effect=ValueError('invalid signature')):
                with self.assertRaises(ValueError):
                    qualification.main()
            self.assertFalse(output.exists())


if __name__ == '__main__':
    unittest.main()
