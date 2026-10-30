#!/usr/bin/env python3
"""Stage immutable GitHub release candidates and publish only authorized bytes.

This tooling runs from the trusted dispatch workflow checkout. It never executes
candidate source or binaries. Public signing identifies that workflow's main ref,
which is separate from the immutable source revision selected by the service.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from urllib.parse import quote

PUBLIC_REPOSITORY = 'raysofspace/enumeray-cli'
PUBLIC_SIGNER = 'https://github.com/' + PUBLIC_REPOSITORY + '/.github/workflows/release.yml@refs/heads/main'
ISSUER = 'https://token.actions.githubusercontent.com'
MANIFEST = 'release-manifest.json'
TARGETS = {
    'x86_64-unknown-linux-gnu': {'minimum_glibc': '2.28'},
    'aarch64-unknown-linux-gnu': {'minimum_glibc': '2.28'},
    'x86_64-apple-darwin': {'minimum_macos': '15.0'},
    'aarch64-apple-darwin': {'minimum_macos': '15.0'},
}
RESERVED = {MANIFEST, MANIFEST + '.sigstore.json', 'service-prepare.json',
            'service-prepare.sigstore.json', 'service-publish.json', 'service-publish.sigstore.json'}
HOMEBREW_LIFECYCLE = ['clean-install-and-formula-test',
                      'same-candidate-revision-upgrade-and-formula-test',
                      'uninstall-preserving-private-state']
_spec = importlib.util.spec_from_file_location('service_qualification', Path(__file__).with_name('verify-service-qualification.py'))
qualification = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(qualification)
artifacts_policy = qualification.artifacts


class PendingCandidate(ValueError):
    """Preparation has not yet staged its signed completion manifest."""


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def canonical(record):
    return json.dumps(record, sort_keys=True, indent=2) + '\n'


def validate_release(release, version):
    if (release.get('tag_name') != 'v' + version
            or type(release.get('prerelease')) is not bool
            or release['prerelease'] != artifacts_policy.is_prerelease(version)):
        raise ValueError('GitHub release metadata differs from the authorized version')


def asset_name(value):
    if not isinstance(value, str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,199}', value):
        raise ValueError('Invalid release asset name')
    return value


def validate_homebrew(report, target, version, archive, digest):
    if 'apple' not in target:
        return
    evidence = report.get('channel_qualification', {}).get('homebrew')
    if (not isinstance(evidence, dict) or evidence.get('schema_version') != 1 or
            evidence.get('channel') != 'homebrew' or evidence.get('state') != 'passed' or
            evidence.get('minimum_platform_qualified') is not True or
            evidence.get('target') != target or evidence.get('version') != version or
            evidence.get('archive') != archive or evidence.get('sha256') != digest or
            evidence.get('lifecycle') != HOMEBREW_LIFECYCLE or
            not qualification.full_hash(evidence.get('binary_sha256'))):
        raise ValueError('Native Homebrew installation qualification failed: ' + target)
    runtime = evidence.get('runtime', {})
    expected_cpu = 'arm64' if target.startswith('aarch64-') else 'x86_64'
    if (not isinstance(runtime, dict) or runtime.get('system') != 'Darwin' or
            not isinstance(runtime.get('macos'), str) or not runtime['macos'].startswith('15.') or
            runtime.get('machine') != expected_cpu):
        raise ValueError('Homebrew qualification requires native macOS 15: ' + target)


def verify_public(path, bundle, run=subprocess.run, workflow_revision=None):
    command = ['cosign', 'verify-blob', '--bundle', str(bundle), '--certificate-identity', PUBLIC_SIGNER,
               '--certificate-oidc-issuer', ISSUER,
               '--certificate-github-workflow-repository', PUBLIC_REPOSITORY,
               '--certificate-github-workflow-ref', 'refs/heads/main',
               '--certificate-github-workflow-trigger', 'repository_dispatch']
    if workflow_revision:
        command.extend(['--certificate-github-workflow-sha', workflow_revision])
    run(command + [str(path)], check=True)


class GitHub:
    def __init__(self, repository=PUBLIC_REPOSITORY, run=subprocess.run):
        if repository != PUBLIC_REPOSITORY:
            raise ValueError('Unexpected public release repository')
        self.repository, self.run = repository, run
        self.prefix = 'repos/' + repository

    def api(self, suffix, method='GET', body=None, missing=False):
        command = ['gh', 'api', self.prefix + '/' + suffix, '--method', method]
        kwargs = {'capture_output': True, 'text': True}
        if body is not None:
            command.extend(['--input', '-'])
            kwargs['input'] = json.dumps(body)
        result = self.run(command, **kwargs)
        if result.returncode:
            if missing and 'HTTP 404' in result.stderr:
                return None
            raise ValueError('GitHub release request failed: ' + method + ' ' + suffix)
        return json.loads(result.stdout) if result.stdout.strip() else None

    def release(self, tag):
        return self.api('releases/tags/' + quote(tag, safe=''), missing=True)

    def assets(self, release):
        result, page = [], 1
        while True:
            rows = self.api('releases/%s/assets?per_page=100&page=%d' % (release['id'], page))
            result.extend(rows)
            if len(rows) < 100:
                break
            page += 1
        names = [entry['name'] for entry in result]
        if len(names) != len(set(names)):
            raise ValueError('Release has duplicate asset names')
        return {entry['name']: entry for entry in result}

    def revision(self, tag):
        entry = self.api('git/ref/tags/' + quote(tag, safe=''), missing=True)
        if entry is None:
            return None
        entry = entry['object']
        for _ in range(10):
            if entry['type'] == 'commit':
                return entry['sha']
            if entry['type'] != 'tag':
                break
            entry = self.api('git/tags/' + entry['sha'])['object']
        raise ValueError('Release tag does not resolve to a commit')

    def download(self, asset, path):
        with Path(path).open('wb') as output:
            result = self.run(['gh', 'api', self.prefix + '/releases/assets/' + str(asset['id']),
                               '--header', 'Accept: application/octet-stream'], stdout=output)
        if result.returncode:
            raise ValueError('Cannot download release asset ' + asset['name'])

    def ensure_release(self, identity):
        tag, revision = identity['tag'], identity['public_revision']
        existing = self.revision(tag)
        if existing is None:
            self.api('git/refs', 'POST', {'ref': 'refs/tags/' + tag, 'sha': revision})
        elif existing != revision:
            raise ValueError('Release version already names different public source')
        release = self.release(tag)
        if release is None:
            prerelease = artifacts_policy.is_prerelease(identity['version'])
            release = self.api('releases', 'POST', {'tag_name': tag, 'target_commitish': revision,
                'name': ('Alpha — ' if '-alpha' in identity['version'] else '') + tag,
                'draft': True, 'prerelease': prerelease, 'make_latest': 'false' if prerelease else 'legacy',
                'body': ('Alpha release. Commands and APIs may change; updates may require a fresh installation. '
                         'No in-place alpha upgrade compatibility is promised. Preserve existing state and matching binaries.\n\n'
                         if '-alpha' in identity['version'] else '')
                        + 'Unified Enumeray CLI, connector and compute runner. Publication requires matching-service artifact qualification.'})
        validate_release(release, identity['version'])
        return release

    def upload(self, release, path):
        path = Path(path)
        name = asset_name(path.name)
        existing = self.assets(release).get(name)
        if existing:
            if existing['state'] == 'starter' and existing['size'] == 0 and release['draft']:
                self.api('releases/assets/' + str(existing['id']), 'DELETE')
            else:
                with tempfile.TemporaryDirectory(prefix='enumeray-asset-') as temporary:
                    downloaded = Path(temporary) / name
                    self.download(existing, downloaded)
                    if sha256(downloaded) != sha256(path):
                        raise ValueError('Immutable release asset conflicts: ' + name)
                return
        if not release['draft']:
            raise ValueError('Cannot add missing assets to a published release')
        result = self.run(['gh', 'release', 'upload', release['tag_name'], str(path),
                           '--repo', self.repository], capture_output=True, text=True)
        if result.returncode:
            # A lost acknowledgement or concurrent identical upload is reconciled.
            existing = self.assets(release).get(name)
            if not existing or existing['state'] != 'uploaded':
                raise ValueError('Release upload failed: ' + name)
            with tempfile.TemporaryDirectory(prefix='enumeray-upload-') as temporary:
                downloaded = Path(temporary) / name
                self.download(existing, downloaded)
                if sha256(downloaded) != sha256(path):
                    raise ValueError('Immutable release asset conflicts: ' + name)


def validate_manifest(manifest, directory=None, identity=None, require_signatures=True):
    if not isinstance(manifest, dict) or manifest.get('schema_version') != 1:
        raise ValueError('Invalid release manifest schema')
    if manifest.get('public_repository') != PUBLIC_REPOSITORY:
        raise ValueError('Unexpected manifest repository')
    for field in ('public_revision', 'source_tree', 'builder_workflow_revision'):
        if not qualification.full_hash(manifest.get(field), 40):
            raise ValueError('Invalid manifest source identity: ' + field)
    for field in ('lockfile_sha256', 'prepare_receipt_sha256'):
        if not qualification.full_hash(manifest.get(field)):
            raise ValueError('Invalid manifest digest: ' + field)
    version = manifest.get('version')
    artifacts_policy.validate_version(version)
    if not isinstance(manifest.get('rust_toolchain'), str) or not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', manifest['rust_toolchain']):
        raise ValueError('Manifest requires the exact source Rust toolchain')
    if manifest.get('tag') != 'v' + version:
        raise ValueError('Manifest tag and version differ')
    artifacts_policy.validate_baseline(manifest.get('baseline'), version)
    entries = manifest.get('files')
    if not isinstance(entries, list) or not entries:
        raise ValueError('Manifest has no release files')
    files = {}
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError('Invalid manifest file')
        name = asset_name(entry.get('name'))
        if name in files or name in RESERVED or not qualification.full_hash(entry.get('sha256')):
            raise ValueError('Duplicate, reserved or invalid manifest file')
        if entry.get('kind') not in ('binary', 'source', 'checksum', 'signature', 'qualification', 'source-identity'):
            raise ValueError('Invalid manifest file kind')
        files[name] = entry
        if directory is not None:
            path = Path(directory) / name
            if path.is_symlink() or not path.is_file() or sha256(path) != entry['sha256']:
                raise ValueError('Release asset failed its manifest digest: ' + name)
    targets = manifest.get('targets')
    if not isinstance(targets, dict) or set(targets) != set(TARGETS):
        raise ValueError('Candidate requires all supported targets')
    expected_binaries = set()
    for target, minimum in TARGETS.items():
        report = targets[target]
        archive = 'enumeray-%s-%s.tar.gz' % (version, target)
        if not isinstance(report, dict) or report.get('archive') != archive or any(report.get(k) != v for k, v in minimum.items()):
            raise ValueError('Target qualification or minimum platform differs')
        if files.get(archive, {}).get('kind') != 'binary' or files[archive].get('target') != target:
            raise ValueError('Target archive is not bound to its target')
        qualification_name = report.get('qualification')
        if files.get(qualification_name, {}).get('kind') != 'qualification':
            raise ValueError('Target qualification report is missing')
        expected_binaries.add(archive)
    if {name for name, f in files.items() if f['kind'] == 'binary'} != expected_binaries:
        raise ValueError('Unexpected binary artifact')
    sources = [name for name, f in files.items() if f['kind'] == 'source']
    if sources != ['enumeray-' + version + '-source.tar.gz']:
        raise ValueError('Candidate requires its versioned source archive')
    expected_checksums = {name + '.sha256' for name in [*expected_binaries, *sources]}
    expected_signatures = {name + '.sigstore.json' for name in expected_checksums}
    checksums = {name for name, item in files.items() if item['kind'] == 'checksum'}
    signatures = {name for name, item in files.items() if item['kind'] == 'signature'}
    if checksums != expected_checksums or not signatures <= expected_signatures:
        raise ValueError('Candidate contains an unexpected checksum or signature')
    if require_signatures and signatures != expected_signatures:
        raise ValueError('Archive requires a signed checksum')
    expected_reports = {'qualification-' + target + '.json' for target in TARGETS}
    if ({name for name, item in files.items() if item['kind'] == 'qualification'} != expected_reports or
            {name for name, item in files.items() if item['kind'] == 'source-identity'} != {'enumeray-' + version + '-source.json'} or
            any(targets[target]['qualification'] != 'qualification-' + target + '.json' for target in TARGETS)):
        raise ValueError('Candidate qualification reports have unexpected identities')
    for archive in [*expected_binaries, *sources]:
        if (files.get(archive + '.sha256', {}).get('kind') != 'checksum' or
                (require_signatures and files.get(archive + '.sha256.sigstore.json', {}).get('kind') != 'signature')):
            raise ValueError('Archive requires a signed checksum')
        if directory is not None and (Path(directory) / (archive + '.sha256')).read_text() != files[archive]['sha256'] + '  ' + archive + '\n':
            raise ValueError('Archive checksum differs from qualified bytes')
    if directory is not None:
        source_name = 'enumeray-' + version + '-source.json'
        if files.get(source_name, {}).get('kind') != 'source-identity':
            raise ValueError('Source archive identity report is missing')
        source_record = json.loads((Path(directory) / source_name).read_text())
        source = {'repository': 'https://github.com/' + PUBLIC_REPOSITORY,
                  'revision': manifest['public_revision'], 'tree': manifest['source_tree'],
                  'lockfile_sha256': manifest['lockfile_sha256'], 'clean': True}
        if (source_record.get('schema_version') != 1 or source_record.get('version') != version or
                source_record.get('source') != source or source_record.get('archive') != sources[0] or
                source_record.get('rust_toolchain') != manifest['rust_toolchain'] or
                source_record.get('sha256') != files[sources[0]]['sha256']):
            raise ValueError('Versioned source report differs from its archive identity')
        for target, minimum in TARGETS.items():
            item = targets[target]
            report = json.loads((Path(directory) / item['qualification']).read_text())
            platform = {'os': 'macos', 'version': '15.0'} if 'apple' in target else {'os': 'linux', 'glibc': '2.28'}
            if (report.get('schema_version') != 1 or report.get('target') != target or
                    report.get('archive') != item['archive'] or report.get('sha256') != files[item['archive']]['sha256'] or
                    report.get('source') != source or report.get('minimum_platform') != platform or
                    report.get('platform_qualified') is not True):
                raise ValueError('Native minimum-platform qualification failed: ' + target)
            validate_homebrew(report, target, version, item['archive'], files[item['archive']]['sha256'])
    if identity:
        for field in ('version', 'public_revision', 'public_repository', 'baseline'):
            if manifest[field] != identity[field]:
                raise ValueError('Manifest conflicts with authorized ' + field)
        if identity['purpose'] == 'prepare' and manifest['prepare_receipt_sha256'] != identity['receipt_sha256']:
            raise ValueError('Candidate belongs to a different preparation authorization')
    return manifest


def fetch_candidate(client, tag, directory, run=subprocess.run, published=False):
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    release = client.release(tag)
    if not release:
        raise PendingCandidate('Candidate draft is not available yet')
    if published and release.get('draft'):
        raise ValueError('Channel delivery requires a published release')
    assets = client.assets(release)
    if MANIFEST not in assets or MANIFEST + '.sigstore.json' not in assets:
        if not release['draft']:
            raise ValueError('Published release lacks a complete signed manifest')
        raise PendingCandidate('Signed candidate manifest is not available yet')
    for name in (MANIFEST, MANIFEST + '.sigstore.json'):
        client.download(assets[name], directory / name)
    verify_public(directory / MANIFEST, directory / (MANIFEST + '.sigstore.json'), run)
    manifest = validate_manifest(json.loads((directory / MANIFEST).read_text()))
    if manifest['tag'] != tag or client.revision(tag) != manifest['public_revision']:
        raise ValueError('Release tag differs from signed candidate source')
    validate_release(release, manifest['version'])
    verify_public(directory / MANIFEST, directory / (MANIFEST + '.sigstore.json'), run,
                  workflow_revision=manifest['builder_workflow_revision'])
    for entry in manifest['files']:
        name = entry['name']
        if name not in assets or assets[name]['state'] != 'uploaded':
            raise ValueError('Complete signed candidate is missing asset: ' + name)
        client.download(assets[name], directory / name)
    validate_manifest(manifest, directory)
    for entry in manifest['files']:
        if entry['kind'] == 'checksum':
            checksum = directory / entry['name']
            verify_public(checksum, directory / (entry['name'] + '.sigstore.json'), run)
            expected_name = entry['name'].removesuffix('.sha256')
            if checksum.read_text() != sha256(directory / expected_name) + '  ' + expected_name + '\n':
                raise ValueError('Archive checksum record differs from candidate')
    for name in ('service-prepare.json', 'service-prepare.sigstore.json'):
        if name not in assets:
            raise ValueError('Candidate lost its preparation authorization')
        client.download(assets[name], directory / name)
    request = {'receipt': (directory / 'service-prepare.json').read_text(),
               'bundle': json.loads((directory / 'service-prepare.sigstore.json').read_text())}
    identity = qualification.verify_request(request, directory, run=run, purpose='prepare')
    validate_manifest(manifest, identity=identity)
    allowed = {entry['name'] for entry in manifest['files']} | RESERVED
    if set(assets) - allowed:
        raise ValueError('Release contains assets outside its immutable manifest')
    if not release['draft']:
        for name in ('service-publish.json', 'service-publish.sigstore.json'):
            if name not in assets:
                raise ValueError('Published release lacks its publication authorization')
            client.download(assets[name], directory / name)
        request = {'receipt': (directory / 'service-publish.json').read_text(),
                   'bundle': json.loads((directory / 'service-publish.sigstore.json').read_text())}
        approved = qualification.verify_request(request, directory, run=run, purpose='publish')
        if approved['manifest_sha256'] != sha256(directory / MANIFEST):
            raise ValueError('Published release authorization names a different manifest')
        if any(approved[field] != identity[field] for field in
               ('service_revision', 'public_revision', 'version', 'baseline')):
            raise ValueError('Published release source and baseline authorization differs')
    return manifest


def upload_service_evidence(client, release, evidence, purpose, run=subprocess.run):
    """Preserve the first valid signature bundle across an identical retry."""
    source = Path(evidence) / ('service-' + purpose + '.json')
    bundle = Path(evidence) / ('service-' + purpose + '.sigstore.json')
    assets = client.assets(release)
    existing = assets.get(bundle.name)
    if existing and existing['state'] == 'uploaded':
        with tempfile.TemporaryDirectory(prefix='enumeray-receipt-') as temporary:
            retained = Path(temporary) / bundle.name
            client.download(existing, retained)
            original = retained.read_bytes()
            request = {'receipt': source.read_text(), 'bundle': json.loads(original)}
            qualification.verify_request(request, Path(temporary), run=run, purpose=purpose)
            bundle.write_bytes(original)
    client.upload(release, source)
    client.upload(release, bundle)


def verify_evidence(identity, evidence, purpose, run):
    evidence = Path(evidence)
    request = {'receipt': (evidence / ('service-' + purpose + '.json')).read_text(),
               'bundle': json.loads((evidence / ('service-' + purpose + '.sigstore.json')).read_text())}
    verified = qualification.verify_request(request, evidence, run=run, purpose=purpose)
    if any(identity.get(field) != verified[field] for field in verified):
        raise ValueError('Saved identity differs from signed service authorization')
    return verified


def sign_file(client, release, source, bundle, run=subprocess.run):
    existing = client.assets(release).get(bundle.name)
    if existing and existing['state'] == 'uploaded':
        client.download(existing, bundle)
        verify_public(source, bundle, run)
    else:
        run(['cosign', 'sign-blob', '--yes', '--bundle', str(bundle), str(source)], check=True)


def build_manifest(identity, artifacts, workflow_revision, signed=True):
    artifacts = Path(artifacts)
    if not qualification.full_hash(workflow_revision, 40):
        raise ValueError('The trusted builder workflow revision is required')
    version = identity['version']
    source_archive = 'enumeray-' + version + '-source.tar.gz'
    source_record = json.loads((artifacts / ('enumeray-' + version + '-source.json')).read_text())
    source = source_record.get('source', source_record)
    if (source.get('revision') != identity['public_revision'] or not source.get('clean') or
            source.get('repository') != 'https://github.com/' + PUBLIC_REPOSITORY or
            not qualification.full_hash(source.get('tree'), 40) or
            not qualification.full_hash(source.get('lockfile_sha256'))):
        raise ValueError('Source archive identity differs from authorized public revision')
    targets = {}
    for target, minimum in TARGETS.items():
        name = 'qualification-' + target + '.json'
        report = json.loads((artifacts / name).read_text())
        archive = 'enumeray-%s-%s.tar.gz' % (version, target)
        platform = {'os': 'macos', 'version': '15.0'} if 'apple' in target else {'os': 'linux', 'glibc': '2.28'}
        if (report.get('schema_version') != 1 or report.get('target') != target or
                report.get('archive') != archive or report.get('sha256') != sha256(artifacts / archive) or
                report.get('source') != source or report.get('minimum_platform') != platform or
                report.get('platform_qualified') is not True):
            raise ValueError('Native minimum-platform qualification failed: ' + target)
        validate_homebrew(report, target, version, archive, report['sha256'])
        targets[target] = {**minimum, 'archive': archive, 'qualification': name}
    if not (artifacts / source_archive).is_file():
        raise ValueError('Versioned source archive is missing')
    files = []
    for path in sorted(artifacts.iterdir()):
        name = asset_name(path.name)
        if name in RESERVED:
            continue
        if path.is_symlink() or not path.is_file():
            raise ValueError('Candidate contains a non-regular asset')
        target = next((target for target in TARGETS if name == targets[target]['archive']), None)
        if target:
            kind = 'binary'
        elif name == source_archive:
            kind = 'source'
        elif name == 'enumeray-' + version + '-source.json':
            kind = 'source-identity'
        elif name in {item['qualification'] for item in targets.values()}:
            kind = 'qualification'
        elif name.endswith('.sha256'):
            kind = 'checksum'
        elif name.endswith('.sha256.sigstore.json'):
            kind = 'signature'
        else:
            raise ValueError('Unexpected candidate asset: ' + name)
        files.append({'name': name, 'sha256': sha256(path), 'kind': kind, **({'target': target} if target else {})})
    manifest = {'schema_version': 1, 'version': version, 'tag': identity['tag'],
        'public_repository': PUBLIC_REPOSITORY, 'public_revision': identity['public_revision'],
        'source_tree': source['tree'], 'lockfile_sha256': source['lockfile_sha256'],
        'builder_workflow_revision': workflow_revision, 'baseline': identity['baseline'],
        'rust_toolchain': source_record.get('rust_toolchain'),
        'prepare_receipt_sha256': identity['receipt_sha256'], 'files': files, 'targets': targets}
    return validate_manifest(manifest, artifacts, identity, require_signatures=signed)


def stage(client, identity, artifacts, workflow_revision, evidence, run=subprocess.run):
    verify_evidence(identity, evidence, 'prepare', run)
    try:
        with tempfile.TemporaryDirectory(prefix='enumeray-existing-') as temporary:
            existing = fetch_candidate(client, identity['tag'], Path(temporary), run)
            validate_manifest(existing, identity=identity)
            return existing
    except PendingCandidate:
        pass
    artifacts, evidence = Path(artifacts), Path(evidence)
    # Validate every source, report and checksum before granting public signatures.
    build_manifest(identity, artifacts, workflow_revision, signed=False)
    release = client.ensure_release(identity)
    for path in sorted(artifacts.glob('*.sha256')):
        sign_file(client, release, path, path.with_name(path.name + '.sigstore.json'), run)
    manifest = build_manifest(identity, artifacts, workflow_revision)
    (artifacts / MANIFEST).write_text(canonical(manifest))
    sign_file(client, release, artifacts / MANIFEST, artifacts / (MANIFEST + '.sigstore.json'), run)
    upload_service_evidence(client, release, evidence, 'prepare', run)
    for entry in manifest['files']:
        client.upload(release, artifacts / entry['name'])
    client.upload(release, artifacts / (MANIFEST + '.sigstore.json'))
    client.upload(release, artifacts / MANIFEST)  # Signed completion marker is last.
    return manifest


def publish(client, identity, directory, evidence, run=subprocess.run):
    verify_evidence(identity, evidence, 'publish', run)
    manifest = fetch_candidate(client, identity['tag'], directory, run)
    validate_manifest(manifest, identity=identity)
    if sha256(Path(directory) / MANIFEST) != identity['manifest_sha256']:
        raise ValueError('Publication authorization names a different candidate manifest')
    prepare = json.loads((Path(directory) / 'service-prepare.json').read_text())
    for field in ('service_revision', 'public_revision', 'version', 'baseline'):
        if prepare[field] != identity[field]:
            raise ValueError('Preparation and publication identities differ')
    release = client.release(identity['tag'])
    if release['draft']:
        upload_service_evidence(client, release, evidence, 'publish', run)
    else:
        assets = client.assets(release)
        for name in ('service-publish.json', 'service-publish.sigstore.json'):
            if name not in assets:
                raise ValueError('Published release lacks its publication authorization')
            client.download(assets[name], Path(directory) / name)
    if release['draft']:
        prerelease = artifacts_policy.is_prerelease(identity['version'])
        client.api('releases/' + str(release['id']), 'PATCH', {
            'draft': False, 'prerelease': prerelease, 'make_latest': 'false' if prerelease else 'legacy'})
    else:
        request = {'receipt': (Path(directory) / 'service-publish.json').read_text(),
                   'bundle': json.loads((Path(directory) / 'service-publish.sigstore.json').read_text())}
        retained = qualification.verify_request(request, Path(directory), run=run, purpose='publish')
        if any(retained[field] != identity[field] for field in
               ('manifest_sha256', 'service_revision', 'public_revision', 'version', 'baseline')):
            raise ValueError('Published release authorization conflicts')
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    authorize = commands.add_parser('authorize')
    authorize.add_argument('--purpose', choices=('prepare', 'publish'), required=True)
    authorize.add_argument('--output', type=Path, required=True)
    fetch = commands.add_parser('fetch')
    fetch.add_argument('--tag', required=True)
    fetch.add_argument('--directory', type=Path, required=True)
    fetch.add_argument('--published', action='store_true')
    ready = commands.add_parser('ready')
    ready.add_argument('--identity', type=Path, required=True)
    ready.add_argument('--directory', type=Path, required=True)
    for name in ('stage', 'publish'):
        command = commands.add_parser(name)
        command.add_argument('--identity', type=Path, required=True)
        command.add_argument('--evidence', type=Path, required=True)
        if name == 'stage':
            command.add_argument('--artifacts', type=Path, required=True)
            command.add_argument('--workflow-revision', required=True)
        else:
            command.add_argument('--directory', type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == 'authorize':
            result = qualification.verify_request(json.loads(os.environ['QUALIFIED_REQUEST']), args.output, purpose=args.purpose)
            (args.output / 'identity.json').write_text(canonical(result))
            if os.environ.get('GITHUB_OUTPUT'):
                with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
                    for key in ('revision', 'tag', 'purpose'):
                        output.write(key + '=' + result[key] + '\n')
        elif args.command == 'fetch':
            fetch_candidate(GitHub(), args.tag, args.directory, published=args.published)
        elif args.command == 'ready':
            identity = json.loads(args.identity.read_text())
            ready = False
            try:
                manifest = fetch_candidate(GitHub(), identity['tag'], args.directory)
                validate_manifest(manifest, identity=identity)
                ready = True
            except PendingCandidate:
                pass
            with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
                output.write('ready=' + str(ready).lower() + '\n')
        else:
            identity = json.loads(args.identity.read_text())
            if args.command == 'stage':
                stage(GitHub(), identity, args.artifacts, args.workflow_revision, args.evidence)
            else:
                publish(GitHub(), identity, args.directory, args.evidence)
        return 0
    except PendingCandidate as error:
        print(str(error), file=sys.stderr)
        return 3
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print('Release delivery failed: ' + str(error), file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
