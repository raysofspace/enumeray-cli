#!/usr/bin/env python3
"""Archive one clean public commit, without Git history or parent service files."""
import argparse
import importlib.util
import json
from pathlib import Path
import subprocess

_spec = importlib.util.spec_from_file_location('release_artifacts', Path(__file__).with_name('release_artifacts.py'))
artifacts = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(artifacts)


def package_source(root, output, revision, version=None):
    root, output = Path(root).resolve(), Path(output).resolve()
    identity = artifacts.source_identity(root, revision, require_clean=True)
    toolchain = artifacts.rust_toolchain(root)
    targets = list(artifacts.MINIMUM_PLATFORMS)
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--locked', '--format-version', '1', '--filter-platform', targets[0]],
        cwd=root, text=True))
    actual_version = next(package['version'] for package in metadata['packages']
                          if package['name'] == 'enumeray-cli')
    if version is not None and version != actual_version:
        raise ValueError('Source archive version differs from the qualified package version')
    version = actual_version
    notices_by_package = {}
    for target in targets:
        target_metadata = metadata if target == targets[0] else json.loads(subprocess.check_output(
            ['cargo', 'metadata', '--locked', '--format-version', '1', '--filter-platform', target],
            cwd=root, text=True))
        for notice in artifacts.dependency_notices(target_metadata, strict=True)['dependencies']:
            notices_by_package[(notice['name'], notice['version'], notice['source'])] = notice
    notices = {'schema': 1, 'dependencies': [notices_by_package[key] for key in sorted(notices_by_package)]}
    # Read exact tracked blobs: export-ignore attributes and local Git configuration
    # must not silently change which build inputs a source release contains.
    tree_entries = subprocess.check_output(['git', 'ls-tree', '-rz', '--full-tree', revision], cwd=root)
    entries = []
    for entry in tree_entries.split(b'\0'):
        if not entry:
            continue
        header, name = entry.split(b'\t', 1)
        mode, kind, identifier = header.split()
        path = Path(name.decode('utf-8'))
        if (mode not in (b'100644', b'100755') or kind != b'blob' or path.is_absolute()
                or '..' in path.parts or '.git' in path.parts):
            raise ValueError('Public source releases contain only tracked regular files: ' + str(path))
        entries.append((mode, identifier, path.as_posix()))
    blobs = subprocess.check_output(['git', 'cat-file', '--batch'], cwd=root,
                                    input=b''.join(identifier + b'\n' for _, identifier, _ in entries))
    prefix = 'enumeray-' + version + '-source'
    members = [(prefix, None, False)]
    required = {'Cargo.toml', 'Cargo.lock', 'LICENSE', 'rust-toolchain.toml', 'crates/cli/Cargo.toml'}
    cursor = 0
    for mode, identifier, name in entries:
        header_end = blobs.index(b'\n', cursor)
        reported_id, kind, length = blobs[cursor:header_end].split()
        size = int(length)
        cursor = header_end + 1
        data = blobs[cursor:cursor + size]
        cursor += size + 1
        if reported_id != identifier or kind != b'blob' or len(data) != size:
            raise ValueError('Git returned an inconsistent source blob')
        if name in ('release-source.json', 'THIRD_PARTY_NOTICES.json'):
            raise ValueError('Tracked source conflicts with generated release identity files')
        required.discard(name)
        members.append((prefix + '/' + name, data, mode == b'100755'))
    if required:
        raise ValueError('Public source lacks required build/license files: ' + ', '.join(sorted(required)))
    members.extend([
        (prefix + '/release-source.json', (json.dumps(identity, indent=2, sort_keys=True) + '\n').encode(), False),
        (prefix + '/THIRD_PARTY_NOTICES.json', (json.dumps(notices, indent=2, sort_keys=True) + '\n').encode(), False),
    ])
    # Never emit a release while tracked build inputs changed during metadata collection.
    if artifacts.source_identity(root, revision, require_clean=True) != identity:
        raise ValueError('Public source identity changed during packaging')
    destination = output / (prefix + '.tar.gz')
    digest = artifacts.write_archive(destination, members)
    report = {'schema_version': 1, 'version': version, 'rust_toolchain': toolchain, 'archive': destination.name,
              'sha256': digest, 'source': identity}
    artifacts.write_json(output / (prefix + '.json'), report)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--revision', required=True, help='Exact clean public HEAD commit')
    parser.add_argument('--version')
    parser.add_argument('--source', type=Path, help='Public candidate source checkout; defaults to this repository')
    parser.add_argument('--output', type=Path, default=Path('dist'))
    args = parser.parse_args()
    try:
        root = args.source.resolve() if args.source else Path(__file__).resolve().parents[1]
        report = package_source(root, args.output, args.revision, args.version)
    except (ValueError, subprocess.CalledProcessError) as error:
        parser.error(str(error))
    print(json.dumps(report, indent=2, sort_keys=True))


if __name__ == '__main__':
    main()
