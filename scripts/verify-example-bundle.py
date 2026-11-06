#!/usr/bin/env python3
"""Verify and run the exact supplied examples without a source checkout.

Requires Python 3.9+ only. Tests run in disposable copies, preserving the bundle.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tempfile

PROJECTS = ('examples/scalar_model', 'examples/collections',
            'starter-kit/python-json', 'starter-kit/python-files',
            'starter-kit/executable-json', 'starter-kit/executable-files',
            'examples/http-contract', 'examples/scientific')
HTTP_PROJECT = 'examples/http-contract'


def verify_public_content(root):
    """Published guides/examples must not advertise retired or private material."""
    private = re.compile(r'https?://github\.com/raysofspace/enumeray(?=[/#?\s)"\'>]|$)', re.IGNORECASE)
    for path in Path(root).rglob('*'):
        if path.is_file() and path.suffix.lower() in ('.md', '.html', '.json'):
            # Top-level compatibility/source manifests can retain machine identity;
            # authored guides and example JSON are the public presentation boundary.
            if path.suffix.lower() == '.json' and path.parent == Path(root):
                continue
            content = path.read_text(encoding='utf-8')
            if re.search('euhforia', content, re.IGNORECASE) or private.search(content):
                raise ValueError('Retired or private source advertised in public material: ' + path.name)


def verify_manifest(root):
    root = Path(root).resolve()
    manifest = json.loads((root / 'examples-manifest.json').read_text())
    if manifest.get('format_version') != 1 or manifest.get('projects') != list(PROJECTS):
        raise ValueError('Unsupported example manifest or project inventory')
    files = manifest.get('files')
    if not isinstance(files, dict) or not files:
        raise ValueError('Missing example file hashes')
    for name, digest in files.items():
        relative = PurePosixPath(name)
        if (relative.is_absolute() or '..' in relative.parts or str(relative) != name
                or not relative.parts or not isinstance(digest, str)
                or not re.fullmatch('[0-9a-f]{64}', digest)):
            raise ValueError('Invalid example file inventory')
        path = root / name
        if any((root / Path(*relative.parts[:index])).is_symlink()
               for index in range(1, len(relative.parts) + 1)) or not path.is_file():
            raise ValueError('Missing or linked example file: ' + name)
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise ValueError('Example checksum mismatch: ' + name)
    actual = set()
    for name in ('examples', 'starter-kit'):
        for path in (root / name).rglob('*'):
            if path.is_symlink() or not (path.is_file() or path.is_dir()):
                raise ValueError('Linked or special example file')
            if path.is_file():
                actual.add(path.relative_to(root).as_posix())
    expected = {name for name in files if name.startswith(('examples/', 'starter-kit/'))}
    if actual != expected:
        raise ValueError('Example tree differs from its manifest')
    for project in PROJECTS:
        if project == HTTP_PROJECT:
            if not (root / project / 'collection-identity.py').is_file() or not (root / project / 'digest-vectors/vectors.json').is_file():
                raise ValueError('Missing executable HTTP identity vectors: ' + project)
            continue
        if not any((root / project).glob('test_*.py')):
            raise ValueError('Missing executable tests: ' + project)
    verify_public_content(root)
    return manifest


def verify_examples(root):
    root = Path(root).resolve()
    manifest = verify_manifest(root)
    with tempfile.TemporaryDirectory(prefix='enumeray-example-tests-') as temporary:
        fresh = Path(temporary)
        environment = {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'HOME': str(fresh),
                       'PYTHONDONTWRITEBYTECODE': '1'}
        for name in PROJECTS:
            project = fresh / name
            shutil.copytree(root / name, project)
            if name == HTTP_PROJECT:
                result = subprocess.run([sys.executable, '-B', 'collection-identity.py',
                                         '--verify-vectors', 'digest-vectors'], cwd=project,
                                        env=environment, check=True, capture_output=True, text=True)
                if json.loads(result.stdout).get('verified_vectors') != 4:
                    raise ValueError('HTTP identity verification must exercise all four published vectors')
                continue
            # Numerical solver qualification uses the separately locked scientific
            # runtime; ordinary native packages need no scientific dependencies.
            pattern = 'test_runtime.py' if name == 'examples/scientific' else 'test_*.py'
            subprocess.run([sys.executable, '-B', '-m', 'unittest', 'discover', '-s', '.',
                            '-p', pattern], cwd=project, env=environment, check=True)
    return {'format_version': 1, 'source': manifest['source'],
            'projects': list(PROJECTS), 'file_count': len(manifest['files'])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parent)
    parser.add_argument('--manifest-only', action='store_true')
    args = parser.parse_args()
    if args.manifest_only:
        result = verify_manifest(args.root)
    else:
        result = verify_examples(args.root)
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == '__main__':
    main()
