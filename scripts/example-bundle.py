#!/usr/bin/env python3
"""Assemble the versioned offline examples using the native archive inventory.

This build command needs a matching built CLI; the resulting examples need only
Python 3.9+ to execute their local tests. It never publishes a release.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


artifacts = load('release_artifacts')
bundle = load('example_bundle')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--source', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--revision', help='Expected full public source HEAD')
    parser.add_argument('--release', action='store_true', help='Require a clean exact public checkout')
    parser.add_argument('--verify', action='store_true', help='Run extracted standalone conformance tests')
    args = parser.parse_args()
    root, binary = args.source.resolve(), args.binary.resolve()
    source = artifacts.source_identity(root, args.revision, require_clean=args.release)
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'], cwd=root, text=True))
    version = next(package['version'] for package in metadata['packages'] if package['name'] == 'enumeray-cli')
    artifacts.validate_version(version)
    expected = f'enumeray {version}'
    if subprocess.check_output([str(binary), '--version'], text=True).strip() != expected:
        parser.error('Built CLI version does not match the source')
    suffix = (source['revision'] or 'working-tree')[:12]
    name = f'enumeray-examples-{version}-{suffix}'
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    archive = output / (name + '.tar.gz')
    with tempfile.TemporaryDirectory(prefix='enumeray-examples-') as temporary:
        contents = Path(temporary) / name
        contents.mkdir()
        manifest = bundle.assemble_examples(root, contents, binary, source, version, release=args.release)
        if args.release and artifacts.source_identity(root, args.revision, require_clean=True) != source:
            raise ValueError('Public source identity changed during example assembly')
        digest = artifacts.archive_directory(contents, archive)
        if args.verify:
            # Verify bytes read back from the produced tar, not the assembly tree.
            import tarfile
            extracted = Path(temporary) / 'extracted'
            extracted.mkdir()
            with tarfile.open(archive) as tar:
                # archive_directory admits only regular files/directories under a fixed prefix.
                tar.extractall(extracted)
            subprocess.run([__import__('sys').executable, '-B',
                            str(extracted / name / 'verify-examples.py'), '--root', str(extracted / name)],
                           check=True, stdout=subprocess.DEVNULL)
    print(json.dumps({'archive': str(archive), 'checksum': str(archive) + '.sha256',
                      'sha256': digest, 'root': name, 'manifest': manifest}, sort_keys=True))


if __name__ == '__main__':
    main()
