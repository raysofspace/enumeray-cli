"""Public verification inventory and isolation regressions without running builds."""
import copy
import importlib.util
from pathlib import Path
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('public_verifier', Path(__file__).with_name('verify.py'))
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)


class PublicVerificationTests(unittest.TestCase):
    def test_catalogue_classifies_native_suites_and_both_backend_cases(self):
        catalogue = verify.load_catalogue()
        suites, ignored = verify.validate(catalogue)
        self.assertGreater(suites, 10)
        self.assertEqual(ignored, 2)
        self.assertEqual({item['check'] for item in catalogue['ignored'].values()}, {'backends', 'mounts'})

    def test_full_profile_keeps_backend_fixtures_explicit(self):
        catalogue = verify.load_catalogue()
        checks = catalogue['profiles']['full']
        self.assertTrue({'contracts', 'client-cli', 'connector', 'python'} <= set(checks))
        self.assertTrue(all(not catalogue['checks'][name].get('explicit') for name in checks))
        variants = catalogue['checks']['contracts']['commands']
        self.assertTrue(any('--no-default-features' in argv for argv in variants))
        self.assertTrue(any('openapi' in argv for argv in variants))

    def test_unregistered_suite_and_ignored_case_are_rejected(self):
        for section in ('suites', 'ignored'):
            catalogue = copy.deepcopy(verify.load_catalogue())
            catalogue[section].pop(next(iter(catalogue[section])))
            with self.assertRaisesRegex(ValueError, 'Unclassified or obsolete'):
                verify.validate(catalogue)

    def test_full_cannot_silently_enable_backend_fixture(self):
        catalogue = copy.deepcopy(verify.load_catalogue())
        catalogue['profiles']['full'].append('backends')
        with self.assertRaisesRegex(ValueError, 'explicit backend'):
            verify.validate(catalogue)

    def test_server_dependency_is_rejected_and_normal_edges_are_requested(self):
        for package in ('enumeray-api', 'sqlx-core', 'aws-sdk-s3', 'axum'):
            with self.subTest(package=package), mock.patch.object(verify.subprocess, 'check_output', return_value=package + ' v1\n') as call:
                with self.assertRaisesRegex(ValueError, 'server libraries'):
                    verify.independent()
                self.assertIn('normal', call.call_args.args[0])


if __name__ == '__main__':
    unittest.main()
