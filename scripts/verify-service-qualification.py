#!/usr/bin/env python3
"""Authenticate the matching-service release receipt before public delivery."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

SERVICE_REPOSITORY = 'raysofspace/enumeray'
ISSUER = 'https://token.actions.githubusercontent.com'


def verify_request(request, directory, run=subprocess.run):
    receipt = request.get('receipt')
    if not isinstance(receipt, str) or not isinstance(request.get('bundle'), dict):
        raise ValueError('A signed service qualification receipt is required')
    record = json.loads(receipt)
    if not isinstance(record, dict):
        raise ValueError('Invalid signed qualification receipt')
    if record.get('service_repository') != SERVICE_REPOSITORY:
        raise ValueError('Unrecognized service qualification authority')
    for field in ('public_revision', 'service_revision'):
        if not re.fullmatch(r'[0-9a-f]{40}', str(record.get(field, ''))):
            raise ValueError('A full immutable source revision is required: ' + field)
    version = record.get('version', '')
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?', str(version)):
        raise ValueError('Invalid release version')
    if not re.fullmatch(r'[0-9]+', str(record.get('qualification_run', ''))):
        raise ValueError('A matching-service qualification run is required')
    source = directory / 'service-qualification.json'
    bundle = directory / 'service-qualification.sigstore.json'
    source.write_text(receipt)
    bundle.write_text(json.dumps(request['bundle']))
    identity = ('https://github.com/' + SERVICE_REPOSITORY +
                '/.github/workflows/release.yml@refs/tags/binaries-v' + version)
    run(['cosign', 'verify-blob', '--bundle', str(bundle), '--certificate-identity', identity,
         '--certificate-oidc-issuer', ISSUER, str(source)], check=True)
    return {'revision': record['public_revision'], 'tag': 'binaries-v' + version}


def main():
    request = json.loads(os.environ['QUALIFIED_REQUEST'])
    with tempfile.TemporaryDirectory(prefix='enumeray-qualification-') as temporary:
        verified = verify_request(request, Path(temporary))
    with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
        for key, value in verified.items():
            output.write(key + '=' + value + '\n')


if __name__ == '__main__':
    main()
