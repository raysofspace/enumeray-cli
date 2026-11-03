"""Backend workload profiles preserve capture coverage and bounded control gates."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock


SOURCE = Path(__file__).resolve().parents[1] / 'crates/connector/tests/real-backends/file-wrapper.py'
SPEC = importlib.util.spec_from_file_location('backend_file_fixture', SOURCE)
fixture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fixture)


class CapturedStream(io.StringIO):
    def __init__(self):
        super().__init__()
        self.buffer = io.BytesIO()


@contextlib.contextmanager
def directory(path):
    previous = Path.cwd()
    os.chdir(path)
    try:
        yield
    finally:
        os.chdir(previous)


class BackendWorkloadTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name).resolve()
        (self.root / '.enumeray/tmp').mkdir(parents=True)
        (self.root / '.enumeray/cache').mkdir(parents=True)
        (self.root / 'input.csv').write_text('42')
        self.request = {'version': 1, 'run_id': 'retained-run', 'launch_intent_id': 'retained-intent',
                        'inputs': {}, 'input_files': {'dataset': {'path': str(self.root / 'input.csv')}},
                        'model_id': 'model', 'publication_id': 'candidate'}
        self.environment = {'HOME': str(self.root), 'PYTHONDONTWRITEBYTECODE': '1',
                            'TMPDIR': str(self.root / '.enumeray/tmp'),
                            'XDG_CACHE_HOME': str(self.root / '.enumeray/cache'),
                            'ENUMERAY_ACCEPTANCE_SCRATCH': '1'}
        self.stdout, self.stderr = CapturedStream(), CapturedStream()

    @contextlib.contextmanager
    def execution(self, profile=None, **environment):
        if profile is not None:
            self.request['inputs']['fixture_profile'] = profile
        (self.root / 'request.json').write_text(json.dumps(self.request))
        with directory(self.root), mock.patch.dict(os.environ, {**self.environment, **environment}, clear=True), \
                mock.patch.object(fixture.os, 'getuid', return_value=10002), \
                mock.patch.object(Path, 'read_bytes', autospec=True, side_effect=PermissionError), \
                mock.patch.object(fixture.sys, 'stdout', self.stdout), \
                mock.patch.object(fixture.sys, 'stderr', self.stderr):
            yield

    def assert_result(self):
        result = json.loads((self.root / 'result.json').read_text())
        self.assertEqual(result['run_id'], self.request['run_id'])
        self.assertEqual(result['launch_intent_id'], self.request['launch_intent_id'])
        self.assertEqual(result['outcome'], 'succeeded')
        self.assertNotIn('inputs', result)
        self.assertEqual(result['files']['result']['path'], 'answer.txt')
        self.assertEqual((self.root / 'answer.txt').read_bytes(), b'84')

    def test_default_retains_complete_large_capture_and_multistep_cleanup_payload(self):
        with self.execution():
            fixture.main()
        self.assertEqual(self.stdout.buffer.getvalue(), b'o' * (2 * 1024 * 1024 + 17))
        self.assertEqual(self.stderr.buffer.getvalue(), b'e' * (2 * 1024 * 1024 + 31))
        self.assertEqual((self.root / 'solver.log').read_bytes(), b'solver converged\x00\xff\n')
        scratch = self.root / 'scratch-heavy'
        self.assertEqual((scratch / 'intermediate').stat().st_size, 4 * 1024 * 1024)
        self.assertEqual(sum(path.name.isdecimal() for path in scratch.iterdir()), 600)
        self.assertTrue((scratch / '/'.join(['deep'] * 12)).is_dir())
        self.assertTrue((self.root / 'scratch-alias').is_symlink())
        self.assert_result()

    def test_functional_trial_retains_diagnostics_and_result_without_bulk_or_fixed_delay(self):
        with self.execution('functional', ENUMERAY_ACCEPTANCE_DELAY='15'), \
                mock.patch.object(fixture.time, 'sleep') as sleep:
            fixture.main()
        self.assertEqual(self.stdout.buffer.getvalue(), b'o' * 81)
        self.assertEqual(self.stderr.buffer.getvalue(), b'e' * 95)
        self.assertEqual((self.root / 'solver.log').read_bytes(), b'solver converged\x00\xff\n')
        self.assertFalse((self.root / 'scratch-heavy').exists())
        self.assertFalse((self.root / 'scratch-alias').exists())
        self.assertEqual((self.root / '.enumeray/cache/cache-data').read_bytes(), b'cache')
        sleep.assert_not_called()
        self.assert_result()

    def test_coverage_control_preserves_full_payload_defaults(self):
        workload = fixture.workload({'inputs': {'fixture_profile': 'coverage-controlled'}},
                                   {'ENUMERAY_ACCEPTANCE_DELAY': '15'})
        self.assertEqual(workload['log_bytes'], 2 * 1024 * 1024)
        self.assertEqual(workload['scratch_bytes'], 4 * 1024 * 1024)
        self.assertEqual(workload['scratch_entries'], 600)
        self.assertEqual(workload['delay'], 0)

    def test_controlled_wrapper_is_ready_but_cannot_complete_before_explicit_release(self):
        def release(interval):
            self.assertEqual(interval, 0.025)
            self.assertEqual((self.root / '.acceptance-ready').read_text(), 'ready\n')
            self.assertFalse((self.root / 'result.json').exists())
            self.assertFalse((self.root / 'answer.txt').exists())
            (self.root / '.acceptance-continue').touch()

        with self.execution('controlled', ENUMERAY_ACCEPTANCE_DELAY='30'), \
                mock.patch.object(fixture.time, 'sleep', side_effect=release) as sleep:
            fixture.main()
        sleep.assert_called_once_with(0.025)
        self.assertFalse((self.root / 'scratch-heavy').exists())
        self.assert_result()

    def test_controlled_wrapper_deadline_cannot_produce_a_successful_result(self):
        with self.execution('controlled'), mock.patch.object(fixture.time, 'monotonic', side_effect=[0, 181]):
            with self.assertRaisesRegex(RuntimeError, 'gate deadline exceeded'):
                fixture.main()
        self.assertTrue((self.root / '.acceptance-ready').exists())
        self.assertFalse((self.root / 'result.json').exists())
        self.assertFalse((self.root / 'answer.txt').exists())

    def test_configured_workload_is_bounded_and_honored(self):
        with self.execution(ENUMERAY_ACCEPTANCE_LOG_BYTES='16', ENUMERAY_ACCEPTANCE_SCRATCH_BYTES='32',
                            ENUMERAY_ACCEPTANCE_SCRATCH_ENTRIES='2', ENUMERAY_ACCEPTANCE_DELAY='0.25'), \
                mock.patch.object(fixture.time, 'sleep') as sleep:
            fixture.main()
        self.assertEqual(len(self.stdout.buffer.getvalue()), 33)
        self.assertEqual(len(self.stderr.buffer.getvalue()), 47)
        self.assertEqual((self.root / 'scratch-heavy/intermediate').stat().st_size, 32)
        self.assertEqual(sum(path.name.isdecimal() for path in (self.root / 'scratch-heavy').iterdir()), 2)
        sleep.assert_called_once_with(0.25)
        self.assert_result()

    def test_malformed_or_excessive_workloads_are_rejected(self):
        values = {'ENUMERAY_ACCEPTANCE_LOG_BYTES': ['-1', '1.5', str(8 * 1024 * 1024 + 1)],
                  'ENUMERAY_ACCEPTANCE_SCRATCH_BYTES': [str(64 * 1024 * 1024 + 1)],
                  'ENUMERAY_ACCEPTANCE_SCRATCH_ENTRIES': ['4097'],
                  'ENUMERAY_ACCEPTANCE_GATE_SECONDS': ['0', '601'],
                  'ENUMERAY_ACCEPTANCE_DELAY': ['nan', 'inf', '-1', '181']}
        for name, candidates in values.items():
            for value in candidates:
                with self.subTest(name=name, value=value), self.assertRaises(ValueError):
                    fixture.workload({'inputs': {}}, {name: value})
        for inputs in [None, [], {'fixture_profile': 'unknown'}]:
            with self.subTest(inputs=inputs), self.assertRaises(ValueError):
                fixture.workload({'inputs': inputs}, {})

    def test_light_profiles_do_not_bypass_execution_identity_or_credentials(self):
        for profile in ('functional', 'controlled'):
            with self.subTest(profile=profile), self.execution(profile), \
                    mock.patch.object(fixture.os, 'getuid', return_value=0), self.assertRaises(AssertionError):
                fixture.main()
            with self.subTest(profile=profile), self.execution(profile), \
                    mock.patch.object(Path, 'read_bytes', return_value=b'fixture-credential'), \
                    self.assertRaisesRegex(RuntimeError, 'could read connector credentials'):
                fixture.main()
            with self.subTest(profile=profile), self.execution(profile, ENUMERAY_S3_EXAMPLE='forbidden'), \
                    self.assertRaises(AssertionError):
                fixture.main()
        self.assertFalse((self.root / 'solver.log').exists())
        self.assertFalse((self.root / 'result.json').exists())

    def test_light_profiles_keep_process_tmp_and_cache_isolation_checks(self):
        with self.execution('functional', TMPDIR=str(self.root)), self.assertRaises(AssertionError):
            fixture.main()
        self.assertFalse((self.root / 'result.json').exists())

    def test_large_transfer_mode_still_copies_exact_bytes_and_refuses_relaunch(self):
        payload = b'\x00\xfftransfer\n' * 128
        (self.root / 'input.csv').write_bytes(payload)
        with self.execution('functional', ENUMERAY_ACCEPTANCE_LARGE_FILES='1'):
            fixture.main()
        self.assertEqual((self.root / 'answer.txt').read_bytes(), payload)
        self.assertEqual((self.root / 'science-launched').read_text(), 'one\n')
        with self.execution('functional', ENUMERAY_ACCEPTANCE_LARGE_FILES='1'), \
                self.assertRaises(FileExistsError):
            fixture.main()
        self.assertEqual((self.root / 'science-launched').read_text(), 'one\n')


if __name__ == '__main__':
    unittest.main()
