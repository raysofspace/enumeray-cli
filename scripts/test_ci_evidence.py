"""Evidence reuse must prove source, coverage, provenance and freshness."""
from copy import deepcopy
from datetime import datetime, timedelta, timezone
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from urllib.request import Request
import zipfile

import ci_evidence as evidence

NOW = datetime(2026, 11, 3, 8, tzinfo=timezone.utc)
WORKFLOW = '.github/workflows/verify.yml'


class FakeAPI:
    def __init__(self, run, receipt, tree):
        self.run, self.document, self.tree = run, receipt, tree
        self.calls = []
        self.associations = deepcopy(run.get('pull_requests', []))

    def get(self, path):
        self.calls.append(path)
        if path.startswith('/commits/') and '/pulls?' in path:
            return self.associations
        if '/git/commits/' in path:
            return {'tree': {'sha': self.tree}, 'parents': [{'sha': 'b' * 40}, {'sha': 'c' * 40}]}
        return {'workflow_runs': [self.run]}

    def receipt(self, run, artifact):
        return deepcopy(self.document)


class ReuseTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name) / 'source'
        self.root.mkdir()
        def git(*args):
            return subprocess.check_output(['git', '-C', str(self.root), *args], text=True,
                                           stderr=subprocess.DEVNULL).strip()
        git('init')
        git('config', 'user.name', 'Fixture')
        git('config', 'user.email', 'fixture@example.invalid')
        (self.root / 'source.txt').write_text('source\n')
        git('add', 'source.txt')
        git('commit', '-m', 'fixture')
        self.sha, self.tree = git('rev-parse', 'HEAD'), git('rev-parse', 'HEAD^{tree}')
        self.env = {'GITHUB_REPOSITORY': 'owner/repo', 'GITHUB_RUN_ID': '20',
                    'GITHUB_RUN_ATTEMPT': '1', 'GITHUB_SHA': self.sha,
                    'GITHUB_EVENT_NAME': 'push', 'GITHUB_REF': 'refs/heads/main',
                    'RUNNER_OS': 'Linux', 'RUNNER_ARCH': 'X64',
                    'RUNNER_ENVIRONMENT': 'github-hosted', 'ImageOS': 'ubuntu24', 'ImageVersion': '20261102',
                    'CI_EVIDENCE_CONTEXT': '{"policy":"current"}'}
        self.current = evidence.current_receipt(self.root, WORKFLOW, ['contracts', 'cli'], self.env, NOW)
        self.receipt = dict(self.current, run_id=10, event='pull_request', base_ref='main',
                            ref='refs/pull/7/merge', sha='a' * 40,
                            source_run=10, verified_at=(NOW - timedelta(hours=1)).isoformat())
        self.run = {'id': 10, 'run_attempt': 1, 'status': 'completed', 'conclusion': 'success',
                    'path': WORKFLOW, 'repository': {'full_name': 'owner/repo'},
                    'head_repository': {'full_name': 'owner/repo'}, 'event': 'pull_request',
                    'head_sha': 'c' * 40, 'updated_at': (NOW - timedelta(minutes=30)).isoformat(),
                    'pull_requests': [{'number': 7, 'base': {'sha': 'b' * 40, 'ref': 'main'},
                                       'head': {'sha': 'c' * 40}}]}
        pull = self.run['pull_requests'][0]
        pull.update(merged_at=(NOW - timedelta(minutes=20)).isoformat(), merge_commit_sha=self.sha)
        pull['head']['repo'] = {'full_name': 'owner/repo'}
        pull['base']['repo'] = {'full_name': 'owner/repo'}

    def lookup(self, *, exact=False, api=None):
        return evidence.lookup(self.root, WORKFLOW, 'ci-evidence-public', ['contracts', 'cli'], self.env,
                               exact_sha=exact, now=NOW, api=api or FakeAPI(self.run, self.receipt, self.tree))

    def test_same_tree_pr_reused_despite_different_commit_metadata(self):
        receipt, reused = self.lookup()
        self.assertTrue(reused)
        self.assertEqual(receipt['sha'], self.sha)
        self.assertEqual(receipt['tree'], self.tree)
        self.assertEqual(receipt['source_run'], 10)
        self.assertEqual(receipt['reused_from'], {'run_id': 10, 'run_attempt': 1, 'sha': 'a' * 40})
        self.assertEqual(receipt['verified_at'], self.receipt['verified_at'])

    def test_merged_pr_with_empty_run_associations_resolves_by_head_commit(self):
        api = FakeAPI(dict(self.run, pull_requests=[]), self.receipt, self.tree)
        pull = deepcopy(self.run['pull_requests'][0])
        pull['head']['repo'] = {'full_name': 'owner/repo'}
        pull['base']['repo'] = {'full_name': 'owner/repo'}
        pull['merged_at'] = (NOW - timedelta(minutes=20)).isoformat()
        api.associations = [pull]
        self.assertTrue(self.lookup(api=api)[1])
        self.assertIn('/commits/' + self.run['head_sha'] + '/pulls?per_page=100', api.calls)
        for change in ('fork', 'unmerged', 'wrong-base', 'missing-repo', 'wrong-landed-sha'):
            with self.subTest(change=change):
                candidate = deepcopy(pull)
                if change == 'fork':
                    candidate['head']['repo']['full_name'] = 'attacker/fork'
                elif change == 'unmerged':
                    candidate['merged_at'] = None
                elif change == 'wrong-base':
                    candidate['base']['ref'] = 'other'
                elif change == 'wrong-landed-sha':
                    candidate['merge_commit_sha'] = 'f' * 40
                else:
                    del candidate['head']['repo']
                api.associations = [candidate]
                self.assertFalse(self.lookup(api=api)[1])

    def test_embedded_association_cannot_bypass_merged_pr_server_proof(self):
        for change in ({'merged_at': None}, {'merge_commit_sha': 'f' * 40}):
            with self.subTest(change=change):
                api = FakeAPI(self.run, self.receipt, self.tree)
                api.associations[0].update(change)
                self.assertFalse(self.lookup(api=api)[1])
                self.assertTrue(any('/pulls?' in call for call in api.calls))

    def test_superset_coverage_can_be_reused(self):
        self.receipt['checks'].append('connector')
        self.assertTrue(self.lookup()[1])

    def test_missing_required_check_runs_fresh(self):
        self.receipt['checks'].remove('cli')
        self.assertFalse(self.lookup()[1])

    def test_changed_source_or_profile_context_runs_fresh(self):
        for key, value in [('tree', 'f' * 40), ('context', 'different'), ('workflow', '.github/workflows/other.yml')]:
            with self.subTest(key=key):
                api = FakeAPI(self.run, dict(self.receipt, **{key: value}), self.tree)
                self.assertFalse(self.lookup(api=api)[1])

    def test_runner_image_cpu_and_repository_configuration_are_bound(self):
        for key in ('ImageVersion', 'RUNNER_ARCH', 'RUNNER_ENVIRONMENT', 'CI_EVIDENCE_CONTEXT'):
            with self.subTest(key=key):
                changed = dict(self.env, **{key: '{} ' if key == 'CI_EVIDENCE_CONTEXT' else 'different'})
                current = evidence.context_identity(changed)
                self.assertNotEqual(current, self.current['context'])

    def test_python_runtime_patch_is_part_of_environment_identity(self):
        with mock.patch.object(evidence.sys, 'version', 'a different Python patch release'):
            self.assertNotEqual(evidence.context_identity(self.env), self.current['context'])

    def test_fork_wrong_workflow_or_failed_run_never_reused(self):
        changes = [{'head_repository': {'full_name': 'attacker/fork'}}, {'repository': {'full_name': 'other/repo'}},
                   {'path': '.github/workflows/other.yml'}, {'conclusion': 'failure'},
                   {'conclusion': 'cancelled'}, {'status': 'in_progress'}, {'run_attempt': 2},
                   {'event': 'pull_request_target'}, {'id': 20}]
        for change in changes:
            with self.subTest(change=change):
                self.assertFalse(self.lookup(api=FakeAPI(dict(self.run, **change), self.receipt, self.tree))[1])

    def test_missing_or_malformed_receipt_never_reused(self):
        documents = [None, [], {}, dict(self.receipt, schema_version=True),
                     dict(self.receipt, run_id='10'), dict(self.receipt, checks='contracts'),
                     dict(self.receipt, checks=[{}]), dict(self.receipt, sha='not-a-sha')]
        for document in documents:
            with self.subTest(document=document):
                self.assertFalse(self.lookup(api=FakeAPI(self.run, document, self.tree))[1])

    def test_pr_merge_parents_and_recorded_ref_must_match_server(self):
        changes = [{'head_sha': 'd' * 40},
                   {'pull_requests': [{'number': 7, 'base': {'sha': 'd' * 40}, 'head': {'sha': 'c' * 40}}]},
                   {'pull_requests': [{'number': 8, 'base': {'sha': 'b' * 40}, 'head': {'sha': 'c' * 40}}]},
                   {'pull_requests': []}]
        for change in changes:
            with self.subTest(change=change):
                self.assertFalse(self.lookup(api=FakeAPI(dict(self.run, **change), self.receipt, self.tree))[1])
        self.receipt['base_ref'] = 'other'
        self.assertFalse(self.lookup()[1])

    def test_explicit_fork_pull_metadata_cannot_contradict_run_repository(self):
        self.run['pull_requests'][0]['head']['repo'] = {'full_name': 'attacker/fork'}
        self.assertFalse(self.lookup()[1])

    def test_lookup_limits_optional_artifact_attempts(self):
        class BusyAPI(FakeAPI):
            attempts = 0
            def get(self, path):
                return {'workflow_runs': [dict(self.run, id=number) for number in range(100, 200)]}
            def receipt(self, run, artifact):
                self.attempts += 1
                raise evidence.EvidenceError('no receipt')
        api = BusyAPI(self.run, self.receipt, self.tree)
        self.assertFalse(self.lookup(api=api)[1])
        self.assertEqual(api.attempts, evidence.MAX_CANDIDATES)

    def test_server_commit_tree_is_verified_independently_of_receipt(self):
        self.assertFalse(self.lookup(api=FakeAPI(self.run, self.receipt, 'f' * 40))[1])

    def test_stale_original_evidence_cannot_be_refreshed_by_reuse(self):
        self.receipt['verified_at'] = (NOW - timedelta(hours=25)).isoformat()
        self.assertFalse(self.lookup()[1])

    def test_future_or_naive_or_invalid_timestamp_runs_fresh(self):
        for value in [(NOW + timedelta(seconds=1)).isoformat(), '2026-11-03T08:00:00', 'invalid']:
            with self.subTest(value=value):
                self.receipt['verified_at'] = value
                self.assertFalse(self.lookup()[1])

    def test_exact_release_rejects_pr_even_with_identical_source(self):
        self.env['GITHUB_REF'] = 'refs/tags/cli-v0.1.0'
        self.assertFalse(self.lookup(exact=True)[1])

    def main_evidence(self):
        self.run.update(event='push', head_branch='main', head_sha=self.sha)
        self.receipt.update(event='push', ref='refs/heads/main', base_ref='', sha=self.sha)

    def test_release_accepts_exact_main_successful_full_coverage(self):
        self.main_evidence()
        self.env['GITHUB_REF'] = 'refs/tags/cli-v0.1.0'
        self.assertTrue(self.lookup(exact=True)[1])

    def test_release_rejects_different_main_sha_or_non_main_branch(self):
        self.main_evidence()
        self.env['GITHUB_REF'] = 'refs/tags/cli-v0.1.0'
        self.run['head_sha'] = 'f' * 40
        self.assertFalse(self.lookup(exact=True)[1])
        self.run.update(head_sha=self.sha, head_branch='feature/change')
        self.assertFalse(self.lookup(exact=True)[1])

    def test_exact_main_green_with_only_affected_coverage_is_insufficient(self):
        self.main_evidence()
        self.receipt['checks'] = ['contracts']
        self.assertFalse(self.lookup(exact=True)[1])

    def test_pr_manual_and_non_main_push_always_test_fresh(self):
        for event, ref in [('pull_request', 'refs/pull/7/merge'),
                           ('workflow_dispatch', 'refs/heads/main'), ('push', 'refs/heads/feature')]:
            with self.subTest(event=event):
                self.env.update(GITHUB_EVENT_NAME=event, GITHUB_REF=ref)
                api = FakeAPI(self.run, self.receipt, self.tree)
                self.assertFalse(self.lookup(api=api)[1])
                self.assertEqual(api.calls, [])

    def test_api_failure_runs_fresh(self):
        class Offline:
            def get(self, path):
                raise OSError('offline')
        self.assertFalse(self.lookup(api=Offline())[1])

    def test_unsupported_or_corrupt_optional_archive_falls_back_to_tests(self):
        import zlib
        for error in (NotImplementedError('unsupported compression'), zlib.error('bad deflate')):
            api = FakeAPI(self.run, self.receipt, self.tree)
            def broken(run, artifact):
                raise error
            api.receipt = broken
            with self.subTest(error=type(error).__name__):
                self.assertFalse(self.lookup(api=api)[1])

    def test_current_checkout_must_match_workflow_sha_and_be_clean(self):
        self.env['GITHUB_SHA'] = 'f' * 40
        with self.assertRaises(evidence.EvidenceError):
            self.lookup()
        self.env['GITHUB_SHA'] = self.sha
        (self.root / 'source.txt').write_text('different inputs\n')
        with self.assertRaises(evidence.EvidenceError):
            self.lookup()

    def test_cli_writes_current_receipt_and_fresh_output_without_token(self):
        directory = Path(self.scratch.name)
        checks, output, receipt = directory / 'checks.json', directory / 'output', directory / 'receipt.json'
        checks.write_text('["cli", "contracts"]')
        env = dict(os.environ, **self.env, GITHUB_OUTPUT=str(output))
        env.pop('GH_TOKEN', None)
        env.pop('GITHUB_TOKEN', None)
        subprocess.run([sys.executable, str(Path(evidence.__file__).resolve()), 'lookup', '--workflow', WORKFLOW,
                        '--artifact', 'ci-evidence-public', '--checks', str(checks), '--output', str(receipt)],
                       cwd=self.root, env=env, check=True, capture_output=True, text=True)
        self.assertIn('reused=false\n', output.read_text())
        self.assertEqual(json.loads(receipt.read_text())['sha'], self.sha)


class ArchiveTests(unittest.TestCase):
    def artifact(self, entries, *, expired=False, size=200, digest=None):
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, 'w') as archive:
            for name, content in entries:
                archive.writestr(name, content)
        class API(evidence.GitHub):
            def get(self, path):
                return {'artifacts': [{'id': 1, 'name': 'proof-attempt-2', 'expired': expired, 'size_in_bytes': size, 'digest': digest}]}
            def read(self, path, **kwargs):
                return buffer.getvalue()
        return API('owner/repo', 'fixture').receipt({'id': 8, 'run_attempt': 2}, 'proof')

    def test_single_small_json_receipt_reads_without_extraction(self):
        self.assertEqual(self.artifact([('receipt.json', '{"proof":true}')]), {'proof': True})

    def test_mismatched_server_artifact_digest_is_rejected(self):
        with self.assertRaises(evidence.EvidenceError):
            self.artifact([('receipt.json', '{}')], digest='sha256:' + 'f' * 64)

    def test_other_attempt_does_not_match_current_run(self):
        api = evidence.GitHub('owner/repo', 'fixture')
        api.get = lambda path: {'artifacts': [{'name': 'proof-attempt-1'}]}
        with self.assertRaises(evidence.EvidenceError):
            api.receipt({'id': 8, 'run_attempt': 2}, 'proof')

    def test_expired_oversized_ambiguous_or_traversal_archives_are_rejected(self):
        cases = [([('../receipt.json', '{}')], {}),
                 ([('receipt.json', '{}'), ('other', '{}')], {}),
                 ([('receipt.json', 'x' * (evidence.MAX_RECEIPT_BYTES + 1))], {}),
                 ([('receipt.json', '{}')], {'expired': True}),
                 ([('receipt.json', '{}')], {'size': evidence.MAX_ARCHIVE_BYTES + 1})]
        for entries, kwargs in cases:
            with self.subTest(entries=[name for name, _ in entries], kwargs=kwargs):
                with self.assertRaises(evidence.EvidenceError):
                    self.artifact(entries, **kwargs)

    def test_cross_host_artifact_redirect_strips_token(self):
        request = Request('https://api.github.com/repos/o/r/actions/artifacts/1/zip',
                          headers={'Authorization': 'Bearer secret'})
        redirected = evidence.SafeRedirect().redirect_request(request, None, 302, '', {}, 'https://storage.example/zip')
        self.assertIsNone(redirected.get_header('Authorization'))
        same_host = evidence.SafeRedirect().redirect_request(request, None, 302, '', {}, 'https://api.github.com/other')
        self.assertEqual(same_host.get_header('Authorization'), 'Bearer secret')
        with self.assertRaises(evidence.EvidenceError):
            evidence.SafeRedirect().redirect_request(request, None, 302, '', {}, 'http://storage.example/zip')


if __name__ == '__main__':
    unittest.main()
