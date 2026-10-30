#!/usr/bin/env python3
"""Authenticate a private service receipt before public preparation or publication."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

SERVICE_REPOSITORY = 'raysofspace/enumeray'
PUBLIC_REPOSITORY = 'raysofspace/enumeray-cli'
ISSUER = 'https://token.actions.githubusercontent.com'

_spec = importlib.util.spec_from_file_location('release_artifacts', Path(__file__).with_name('release_artifacts.py'))
artifacts = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(artifacts)


def full_hash(value, length=64):
    return isinstance(value, str) and re.fullmatch('[0-9a-f]{%d}' % length, value) is not None


def validate_record(record, purpose):
    if not isinstance(record, dict) or record.get('schema_version') != 1:
        raise ValueError('Invalid qualification receipt schema')
    if purpose not in ('prepare', 'publish') or record.get('purpose') != purpose:
        raise ValueError('Qualification receipt has the wrong purpose')
    if (record.get('service_repository') != SERVICE_REPOSITORY or
            record.get('public_repository') != PUBLIC_REPOSITORY):
        raise ValueError('Unrecognized qualification authority or public repository')
    for field in ('public_revision', 'service_revision'):
        if not full_hash(record.get(field), 40):
            raise ValueError('A full immutable source revision is required: ' + field)
    version = record.get('version')
    artifacts.validate_version(version)
    if not isinstance(record.get('qualification_run'), str) or not re.fullmatch(r'[1-9][0-9]*', record['qualification_run']):
        raise ValueError('A matching-service qualification run is required')
    artifacts.validate_baseline(record.get('baseline'), version)
    if purpose == 'publish' and not full_hash(record.get('manifest_sha256')):
        raise ValueError('Publication requires the exact candidate manifest digest')
    return record


def verify_request(request, directory, run=subprocess.run, purpose='prepare'):
    if not isinstance(request, dict):
        raise ValueError('A signed service qualification receipt is required')
    receipt = request.get('receipt')
    if not isinstance(receipt, str) or not isinstance(request.get('bundle'), dict):
        raise ValueError('A signed service qualification receipt is required')
    record = validate_record(json.loads(receipt), purpose)
    directory.mkdir(parents=True, exist_ok=True)
    source = directory / ('service-' + purpose + '.json')
    bundle = directory / ('service-' + purpose + '.sigstore.json')
    source.write_text(receipt)
    bundle.write_text(json.dumps(request['bundle'], sort_keys=True) + '\n')
    service_tag = 'cli-v' + record['version']
    identity = 'https://github.com/' + SERVICE_REPOSITORY + '/.github/workflows/release.yml@refs/tags/' + service_tag
    run(['cosign', 'verify-blob', '--bundle', str(bundle), '--certificate-identity', identity,
         '--certificate-oidc-issuer', ISSUER,
         '--certificate-github-workflow-repository', SERVICE_REPOSITORY,
         '--certificate-github-workflow-ref', 'refs/tags/' + service_tag,
         '--certificate-github-workflow-sha', record['service_revision'],
         '--certificate-github-workflow-trigger', 'push', str(source)], check=True)
    return {**record, 'revision': record['public_revision'], 'tag': 'v' + record['version'],
            'receipt_sha256': hashlib.sha256(receipt.encode()).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--purpose', choices=('prepare', 'publish'), default='prepare')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    request = json.loads(os.environ['QUALIFIED_REQUEST'])
    with tempfile.TemporaryDirectory(prefix='enumeray-qualification-') as temporary:
        verified = verify_request(request, args.output or Path(temporary), purpose=args.purpose)
    if args.output:
        (args.output / 'identity.json').write_text(json.dumps(verified, sort_keys=True, indent=2) + '\n')
    if os.environ.get('GITHUB_OUTPUT'):
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            for key in ('revision', 'tag', 'purpose'):
                output.write(key + '=' + verified[key] + '\n')


if __name__ == '__main__':
    main()
