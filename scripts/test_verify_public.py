"""Public verification inventory and isolation regressions without running builds."""
import copy
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('public_verifier', Path(__file__).with_name('verify.py'))
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)


class PublicVerificationTests(unittest.TestCase):
    def test_release_targets_require_native_macos15_and_separate_homebrew_delivery(self):
        workflow = (verify.ROOT / '.github/workflows/release.yml').read_text()
        self.assertIn("runner: '[\"macos-15\"]'", workflow)
        self.assertIn("runner: '[\"macos-15-intel\"]'", workflow)
        self.assertNotIn('macos14', workflow)
        self.assertNotIn('ENUMERAY_CONDA_TOKEN', workflow)
        self.assertLess(workflow.index('native_docker_arch='), workflow.index('docker run --rm'))
        self.assertIn('build-release-native.sh', workflow)
        tap = (verify.ROOT / 'distribution/homebrew/tap/.github/workflows/verify.yml').read_text()
        self.assertIn('fetch --published', tap)
        self.assertLess(tap.index('cmp Formula/enumeray.rb'), tap.index('brew install'))
        self.assertIn('macos-15-intel', tap)
        self.assertIn('journal.sha256', tap)

    def test_catalogue_classifies_native_suites_and_both_backend_cases(self):
        catalogue = verify.load_catalogue()
        suites, ignored = verify.validate(catalogue)
        self.assertGreater(suites, 10)
        self.assertEqual(ignored, 2)
        self.assertEqual({item['check'] for item in catalogue['ignored'].values()}, {'backends', 'mounts'})

    def test_full_profile_keeps_backend_fixtures_explicit(self):
        catalogue = verify.load_catalogue()
        checks = catalogue['profiles']['full']
        self.assertTrue({'contracts', 'client-cli', 'connector-native', 'connector-integration', 'python'} <= set(checks))
        self.assertEqual(set(checks), {name for name, check in catalogue['checks'].items()
                                      if not check.get('explicit')})
        self.assertEqual(len(checks), len(set(checks)))
        self.assertTrue(all(not catalogue['checks'][name].get('explicit') for name in checks))
        variants = catalogue['checks']['contracts']['commands']
        self.assertTrue(any('--no-default-features' in argv for argv in variants))
        self.assertTrue(any('openapi' in argv for argv in variants))

    def test_connector_profile_preserves_both_native_and_integration_suites(self):
        catalogue = verify.load_catalogue()
        checks = catalogue['profiles']['connector']
        self.assertEqual(set(checks), {'connector-native', 'connector-integration'})
        expected = {
            'connector_runner': 'connector-native',
            'connector_host': 'connector-integration',
            'connector_service': 'connector-integration',
            'connector_host_enrollment': 'connector-integration',
        }
        for target, check in expected.items():
            with self.subTest(target=target):
                self.assertIn(check, catalogue['profiles']['full'])
                self.assertEqual(catalogue['suites']['crates/cli/tests/' + target + '.rs'], check)
                commands = catalogue['checks'][check]['commands']
                self.assertTrue(any(target in command for command in commands))

    def test_native_platform_executes_client_and_native_connector_behavior(self):
        catalogue = verify.load_catalogue()
        checks = catalogue['profiles']['native-platform']
        self.assertTrue(set(catalogue['profiles']['native-clients']) <= set(checks))
        self.assertIn('connector-native', checks)
        with mock.patch.object(verify.subprocess, 'run', return_value=mock.Mock(returncode=0)) as run, \
             mock.patch.object(verify.shutil, 'which', return_value='/tool'), \
             contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(verify.main(['native-platform']), 0)
        commands = [call.args[0] for call in run.call_args_list]
        self.assertIn(['cargo', 'test', '--locked', '-p', 'enumeray-connector', '--lib', '--all-features'], commands)
        self.assertTrue(any('-p' in command and 'enumeray-client' in command and '--all-targets' in command
                            for command in commands))
        self.assertTrue(any('enumeray-cli' in command and 'connector_runner' in command for command in commands))
        self.assertFalse(any('connector_service' in command or 'connector_host' in command
                             or 'connector_host_enrollment' in command for command in commands))
        self.assertTrue(all(command[:2] == ['cargo', 'test'] and '--doc' not in command for command in commands))

    def test_native_plan_combines_profiles_without_repeating_checks(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output), mock.patch.object(verify.subprocess, 'run') as run:
            self.assertEqual(verify.main(['plan', 'native-platform', 'connector', '--json']), 0)
        self.assertEqual(json.loads(output.getvalue())['checks'],
                         ['contracts', 'client-cli', 'connector-native', 'connector-integration'])
        run.assert_not_called()

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
