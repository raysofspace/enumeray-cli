"""Immutable two-phase delivery, source separation and replay regressions."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('release_delivery', Path(__file__).with_name('release_delivery.py'))
delivery = importlib.util.module_from_spec(spec)
spec.loader.exec_module(delivery)


def identity(purpose='prepare'):
    record = {'schema_version': 1, 'purpose': purpose, 'version': '0.1.0',
              'public_repository': delivery.PUBLIC_REPOSITORY, 'public_revision': 'a' * 40,
              'service_repository': 'raysofspace/enumeray', 'service_revision': 'b' * 40,
              'baseline': {'id': 'persistent-v1', 'sha256': 'c' * 64},
              'qualification_run': '123', 'tag': 'v0.1.0', 'revision': 'a' * 40}
    receipt = json.dumps(record)
    import hashlib
    record['receipt_sha256'] = hashlib.sha256(receipt.encode()).hexdigest()
    return record, receipt


def artifacts(root):
    record, receipt = identity()
    source = {'repository': 'https://github.com/' + delivery.PUBLIC_REPOSITORY,
              'revision': record['public_revision'], 'tree': 'd' * 40,
              'lockfile_sha256': 'e' * 64, 'clean': True}
    (root / 'service-prepare.json').write_text(receipt)
    (root / 'service-prepare.sigstore.json').write_text('{}')
    for target in delivery.TARGETS:
        archive = 'enumeray-0.1.0-' + target + '.tar.gz'
        (root / archive).write_bytes(target.encode())
        minimum = {'os': 'macos', 'version': '14.0'} if 'apple' in target else {'os': 'linux', 'glibc': '2.28'}
        report = {'schema_version': 1, 'target': target, 'archive': archive,
                  'sha256': delivery.sha256(root / archive), 'source': source,
                  'minimum_platform': minimum, 'platform_qualified': True}
        if 'apple' in target:
            report['channel_qualification'] = {'homebrew': {
                'schema_version': 1, 'channel': 'homebrew', 'target': target,
                'version': '0.1.0', 'archive': archive, 'sha256': report['sha256'],
                'state': 'passed', 'minimum_platform_qualified': True,
                'binary_sha256': '0' * 64, 'lifecycle': delivery.HOMEBREW_LIFECYCLE,
                'runtime': {'system': 'Darwin', 'macos': '14.7.1',
                            'machine': 'arm64' if target.startswith('aarch64-') else 'x86_64'}}}
        (root / ('qualification-' + target + '.json')).write_text(json.dumps(report))
    (root / 'enumeray-0.1.0-source.tar.gz').write_bytes(b'source')
    (root / 'enumeray-0.1.0-source.json').write_text(json.dumps({'schema_version': 1, 'version': '0.1.0', 'rust_toolchain': '1.98.1', 'source': source, 'archive': 'enumeray-0.1.0-source.tar.gz', 'sha256': delivery.sha256(root / 'enumeray-0.1.0-source.tar.gz')}))
    for archive in root.glob('*.tar.gz'):
        checksum = archive.with_name(archive.name + '.sha256')
        checksum.write_text(delivery.sha256(archive) + '  ' + archive.name + '\n')
        checksum.with_name(checksum.name + '.sigstore.json').write_text('{}')
    manifest = delivery.build_manifest(record, root, 'f' * 40)
    (root / delivery.MANIFEST).write_text(delivery.canonical(manifest))
    (root / (delivery.MANIFEST + '.sigstore.json')).write_text('{}')
    return record, manifest


class MemoryGitHub:
    def __init__(self, root, draft=True):
        self.files = {path.name: path.read_bytes() for path in root.iterdir() if path.is_file()}
        self.record = {'id': 1, 'draft': draft, 'tag_name': 'v0.1.0'}
        self.calls = []

    def release(self, tag):
        return self.record

    def revision(self, tag):
        return 'a' * 40

    def assets(self, release):
        return {name: {'id': name, 'name': name, 'state': 'uploaded', 'size': len(data)} for name, data in self.files.items()}

    def download(self, asset, path):
        Path(path).write_bytes(self.files[asset['name']])

    def upload(self, release, path):
        data = Path(path).read_bytes()
        if path.name in self.files and self.files[path.name] != data:
            raise ValueError('Immutable release asset conflicts')
        self.files[path.name] = data
        self.calls.append(('upload', path.name))

    def api(self, suffix, method='GET', body=None, **kwargs):
        self.calls.append((method, suffix))
        if method == 'PATCH':
            self.record['draft'] = body['draft']
            return self.record
        raise AssertionError((suffix, method))


class ManifestTests(unittest.TestCase):
    def test_public_release_tag_is_distinct_from_private_or_legacy_trigger(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record, manifest = artifacts(root)
            self.assertEqual(manifest['tag'], 'v0.1.0')
            delivery.validate_manifest(manifest, root, record)
            for tag in ('cli-v0.1.0', 'binaries-v0.1.0', 'v0.2.0'):
                modified = {**manifest, 'tag': tag}
                with self.subTest(tag=tag), self.assertRaisesRegex(ValueError, 'tag and version'):
                    delivery.validate_manifest(modified, root)

    def test_default_branch_workflow_is_distinct_from_selected_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record, manifest = artifacts(root)
            self.assertNotEqual(manifest['builder_workflow_revision'], manifest['public_revision'])
            verifier = mock.Mock()
            delivery.verify_public(root / delivery.MANIFEST, root / (delivery.MANIFEST + '.sigstore.json'),
                                   verifier, workflow_revision=manifest['builder_workflow_revision'])
            command = verifier.call_args.args[0]
            self.assertEqual(command[command.index('--certificate-github-workflow-sha') + 1], 'f' * 40)
            self.assertIn(delivery.PUBLIC_SIGNER, command)
            self.assertIn('refs/heads/main', command)
            delivery.validate_manifest(manifest, root, record)

    def test_manifest_requires_all_targets_digests_and_unique_safe_names(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _, manifest = artifacts(root)
            for change in ('missing-target', 'traversal', 'duplicate', 'wrong-hash', 'wrong-baseline'):
                modified = copy.deepcopy(manifest)
                if change == 'missing-target':
                    modified['targets'].pop(next(iter(modified['targets'])))
                elif change == 'traversal':
                    modified['files'][0]['name'] = '../escape'
                elif change == 'duplicate':
                    modified['files'].append(modified['files'][0])
                elif change == 'wrong-hash':
                    modified['files'][0]['sha256'] = '0' * 64
                else:
                    modified['baseline'] = {'id': 'candidate', 'sha256': 'c' * 64}
                with self.subTest(change=change), self.assertRaises(ValueError):
                    delivery.validate_manifest(modified, root)

    def test_newer_runtime_smoke_cannot_establish_minimum_platform_qualification(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record, _ = artifacts(root)
            report = next(root.glob('qualification-*.json'))
            changed = json.loads(report.read_text())
            changed['platform_qualified'] = False
            report.write_text(json.dumps(changed))
            with self.assertRaisesRegex(ValueError, 'minimum-platform'):
                delivery.build_manifest(record, root, 'f' * 40)

    def test_fetch_rejects_modified_or_incomplete_completed_candidate(self):
        with tempfile.TemporaryDirectory() as temporary, tempfile.TemporaryDirectory() as destination:
            root = Path(temporary)
            record, manifest = artifacts(root)
            client = MemoryGitHub(root)
            result = delivery.fetch_candidate(client, record['tag'], Path(destination), run=mock.Mock())
            self.assertEqual(result, manifest)
            archive = manifest['targets']['x86_64-unknown-linux-gnu']['archive']
            client.files[archive] = b'tampered'
            with self.assertRaisesRegex(ValueError, 'manifest digest'):
                delivery.fetch_candidate(client, record['tag'], Path(destination), run=mock.Mock())
            client.files.pop(archive)
            with self.assertRaisesRegex(ValueError, 'missing asset'):
                delivery.fetch_candidate(client, record['tag'], Path(destination), run=mock.Mock())
            client.files.pop(delivery.MANIFEST)
            with self.assertRaises(delivery.PendingCandidate):
                delivery.fetch_candidate(client, record['tag'], Path(destination), run=mock.Mock())

    def test_complete_prepare_replay_reuses_original_manifest_without_signing(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record, manifest = artifacts(root)
            client = MemoryGitHub(root)
            client.ensure_release = lambda identity: client.record
            verifier = mock.Mock()
            result = delivery.stage(client, record, root, '1' * 40, root, run=verifier)
            self.assertEqual(result, manifest)
            self.assertEqual(client.calls, [])
            self.assertTrue(all(call.args[0][:2] == ['cosign', 'verify-blob'] for call in verifier.call_args_list))

    def test_fresh_stage_publishes_completion_marker_after_all_signed_payloads(self):
        with tempfile.TemporaryDirectory() as temporary, tempfile.TemporaryDirectory() as fetched:
            root = Path(temporary)
            record, _ = artifacts(root)
            client = MemoryGitHub(root)
            client.files = {}
            client.record = None

            def ensure_release(record):
                client.record = {'id': 1, 'draft': True, 'tag_name': record['tag']}
                return client.record

            def signer(command, **kwargs):
                if command[:2] == ['cosign', 'sign-blob']:
                    Path(command[command.index('--bundle') + 1]).write_text('{"signature":"fixture"}')
                return subprocess.CompletedProcess(command, 0)

            client.ensure_release = ensure_release
            result = delivery.stage(client, record, root, 'f' * 40, root, run=signer)
            self.assertEqual(client.calls[-1], ('upload', delivery.MANIFEST))
            self.assertEqual(delivery.fetch_candidate(client, record['tag'], Path(fetched), run=signer), result)

    def test_failed_platform_report_is_rejected_before_public_signatures_or_mutation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record, _ = artifacts(root)
            report = next(root.glob('qualification-*.json'))
            modified = json.loads(report.read_text())
            modified['platform_qualified'] = False
            report.write_text(json.dumps(modified))
            client = MemoryGitHub(root)
            client.record = None
            client.ensure_release = mock.Mock()
            verifier = mock.Mock()
            with self.assertRaisesRegex(ValueError, 'minimum-platform'):
                delivery.stage(client, record, root, 'f' * 40, root, run=verifier)
            client.ensure_release.assert_not_called()
            self.assertTrue(all(call.args[0][:2] == ['cosign', 'verify-blob'] for call in verifier.call_args_list))

    def test_unrelated_checksum_cannot_acquire_a_public_workflow_signature(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record, _ = artifacts(root)
            (root / 'unrelated.sha256').write_text('0' * 64 + '  unrelated\n')
            with self.assertRaisesRegex(ValueError, 'unexpected checksum'):
                delivery.build_manifest(record, root, 'f' * 40, signed=False)

    def test_macos_requires_same_archive_successful_native_homebrew_lifecycle(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record, _ = artifacts(root)
            report_file = root / 'qualification-aarch64-apple-darwin.json'
            original = json.loads(report_file.read_text())
            for change in ('missing', 'failed', 'newer-os', 'wrong-archive', 'incomplete-upgrade'):
                report = copy.deepcopy(original)
                homebrew = report['channel_qualification']['homebrew']
                if change == 'missing':
                    report.pop('channel_qualification')
                elif change == 'failed':
                    homebrew['state'] = 'failed'
                elif change == 'newer-os':
                    homebrew['runtime']['macos'] = '15.0'
                elif change == 'wrong-archive':
                    homebrew['sha256'] = '0' * 64
                else:
                    homebrew['lifecycle'] = homebrew['lifecycle'][:1]
                report_file.write_text(json.dumps(report))
                with self.subTest(change=change), self.assertRaisesRegex(ValueError, 'Homebrew'):
                    delivery.build_manifest(record, root, 'f' * 40, signed=False)

    def test_channel_fetch_rejects_draft_and_premature_publication(self):
        with tempfile.TemporaryDirectory() as temporary, tempfile.TemporaryDirectory() as destination:
            root = Path(temporary)
            record, _ = artifacts(root)
            client = MemoryGitHub(root)
            with self.assertRaisesRegex(ValueError, 'published stable'):
                delivery.fetch_candidate(client, record['tag'], Path(destination), run=mock.Mock(), published=True)
            client.record['draft'] = False
            with self.assertRaisesRegex(ValueError, 'publication authorization'):
                delivery.fetch_candidate(client, record['tag'], Path(destination), run=mock.Mock(), published=True)

    def test_saved_identity_cannot_bypass_signed_source_or_purpose(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record, _ = artifacts(root)
            record['public_revision'] = '0' * 40
            with self.assertRaisesRegex(ValueError, 'Saved identity'):
                delivery.verify_evidence(record, root, 'prepare', mock.Mock())

    def test_publish_binds_manifest_and_source_and_never_rebuilds(self):
        with tempfile.TemporaryDirectory() as temporary, tempfile.TemporaryDirectory() as destination:
            root = Path(temporary)
            record, _ = artifacts(root)
            publication, _ = identity('publish')
            publication['manifest_sha256'] = delivery.sha256(root / delivery.MANIFEST)
            receipt = json.dumps({key: value for key, value in publication.items() if key != 'receipt_sha256'})
            (root / 'service-publish.json').write_text(receipt)
            publication['receipt_sha256'] = delivery.sha256(root / 'service-publish.json')
            (root / 'service-publish.sigstore.json').write_text('{}')
            client = MemoryGitHub(root)
            verifier = mock.Mock()
            delivery.publish(client, publication, Path(destination), root, run=verifier)
            self.assertFalse(client.record['draft'])
            self.assertEqual([call for call in client.calls if call[0] == 'PATCH'], [('PATCH', 'releases/1')])
            delivery.publish(client, publication, Path(destination), root, run=verifier)
            self.assertEqual(len([call for call in client.calls if call[0] == 'PATCH']), 1)
            self.assertTrue(all(call.args[0][:2] == ['cosign', 'verify-blob'] for call in verifier.call_args_list))
            publication['manifest_sha256'] = '0' * 64
            (root / 'service-publish.json').write_text(json.dumps({key: value for key, value in publication.items() if key != 'receipt_sha256'}))
            publication['receipt_sha256'] = delivery.sha256(root / 'service-publish.json')
            with self.assertRaisesRegex(ValueError, 'different candidate'):
                delivery.publish(client, publication, Path(destination), root, run=verifier)


class GitHubTests(unittest.TestCase):
    def test_annotated_tags_are_peeled_to_the_original_commit(self):
        client = delivery.GitHub()
        client.api = mock.Mock(side_effect=[{'object': {'type': 'tag', 'sha': 'c' * 40}},
                                          {'object': {'type': 'commit', 'sha': 'a' * 40}}])
        self.assertEqual(client.revision('v0.1.0'), 'a' * 40)

    def test_upload_reuses_identical_and_rejects_conflicting_completed_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / 'archive.tar.gz'
            path.write_bytes(b'original')
            client = delivery.GitHub(run=mock.Mock())
            client.assets = mock.Mock(return_value={path.name: {'id': 1, 'name': path.name, 'state': 'uploaded', 'size': 8}})
            client.download = lambda asset, output: output.write_bytes(b'original')
            release = {'draft': True, 'tag_name': 'v0.1.0'}
            client.upload(release, path)
            client.run.assert_not_called()
            path.write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError, 'conflicts'):
                client.upload(release, path)
            client.run.assert_not_called()

    def test_incomplete_starter_can_retry_but_published_missing_asset_cannot(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'archive.tar.gz'
            path.write_bytes(b'original')
            client = delivery.GitHub(run=mock.Mock(return_value=subprocess.CompletedProcess([], 0, '', '')))
            client.api = mock.Mock()
            client.assets = mock.Mock(return_value={path.name: {'id': 1, 'state': 'starter', 'size': 0}})
            client.upload({'draft': True, 'tag_name': 'v0.1.0'}, path)
            client.api.assert_called_once_with('releases/assets/1', 'DELETE')
            self.assertNotIn('--clobber', client.run.call_args.args[0])
            client.assets.return_value = {}
            with self.assertRaisesRegex(ValueError, 'published release'):
                client.upload({'draft': False}, path)


class WorkflowTests(unittest.TestCase):
    def test_source_version_strips_only_the_public_tag_prefix(self):
        workflow = (Path(__file__).parents[1] / '.github/workflows/release.yml').read_text()
        self.assertIn('--version "${RELEASE_TAG#v}"', workflow)
        self.assertNotIn('${RELEASE_TAG#cli-v}', workflow)

    def test_dispatch_queue_serializes_a_version_without_cancelling_other_versions(self):
        workflow = (Path(__file__).parents[1] / '.github/workflows/release.yml').read_text()
        concurrency = workflow.split('\nconcurrency:\n', 1)[1].split('\njobs:', 1)[0]
        self.assertIn('group: binary-release-delivery-${{ fromJSON(github.event.client_payload.receipt).version }}', concurrency)
        self.assertIn('cancel-in-progress: false', concurrency)
        # A purpose/action key would permit prepare and publish to race each other;
        # the default dispatch ref would collapse every release into one queue.
        self.assertNotIn('github.event.action', concurrency)
        self.assertNotIn('github.ref', concurrency)
        self.assertIn('authorize --purpose "$PURPOSE"', workflow)

    def test_linux_runner_and_docker_architecture_must_both_match_without_emulation(self):
        workflow = (Path(__file__).parents[1] / '.github/workflows/release.yml').read_text()
        guard = workflow.split('          # Check the runner and Docker host', 1)[1].split('          # Provision a reviewed', 1)[0]
        guard = '\n'.join(line[10:] for line in guard.splitlines()[1:])
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name, output in (('docker', 'TEST_DOCKER_CPU'), ('uname', 'TEST_RUNNER_CPU')):
                path = root / name
                path.write_text('#!/bin/sh\nprintf "%s\\n" "$' + output + '"\n')
                path.chmod(0o755)
            for target, runner, machine, daemon, expected in (
                    ('x86_64-unknown-linux-gnu', 'X64', 'x86_64', 'amd64', 0),
                    ('aarch64-unknown-linux-gnu', 'ARM64', 'aarch64', 'arm64', 0),
                    ('x86_64-unknown-linux-gnu', 'X64', 'x86_64', 'arm64', 1),
                    ('aarch64-unknown-linux-gnu', 'X64', 'aarch64', 'aarch64', 1)):
                env = {**os.environ, 'PATH': str(root) + ':/usr/bin:/bin', 'TARGET': target,
                       'RUNNER_ARCH': runner, 'TEST_RUNNER_CPU': machine, 'TEST_DOCKER_CPU': daemon}
                result = subprocess.run(['/bin/bash', '-euc', guard], env=env, capture_output=True, text=True)
                with self.subTest(target=target, runner=runner, daemon=daemon):
                    self.assertEqual(result.returncode, expected, result.stderr)

    def test_dispatch_build_and_publication_privileges_remain_separate(self):
        workflow = (Path(__file__).parents[1] / '.github/workflows/release.yml').read_text()
        self.assertIn('types: [qualified-release, qualified-publication]', workflow)
        build = workflow.split('\n  build:', 1)[1].split('\n  source:', 1)[0]
        self.assertNotIn('contents: write', build)
        self.assertNotIn('id-token: write', build)
        self.assertNotIn('secrets.', build)
        self.assertIn('ref: ${{ needs.authorize.outputs.revision }}', build)
        for name, following in (('stage', 'publish'), ('publish', 'channels')):
            privileged = workflow.split('\n  ' + name + ':', 1)[1].split('\n  ' + following + ':', 1)[0]
            self.assertIn('ref: ${{ github.sha }}', privileged)
            self.assertNotIn('ref: ${{ needs.authorize.outputs.revision }}', privileged)
            self.assertNotIn('cargo build', privileged)
        publication = workflow.split('\n  publish:', 1)[1].split('\n  channels:', 1)[0]
        self.assertNotIn('id-token: write', publication)
        self.assertNotIn('sign-blob', publication)


if __name__ == '__main__':
    unittest.main()
