"""Deterministic public release archives and their source/license identity."""
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import tarfile

REPOSITORY = 'https://github.com/raysofspace/enumeray-cli'
MINIMUM_PLATFORMS = {
    'x86_64-apple-darwin': {'os': 'macos', 'version': '14.0'},
    'aarch64-apple-darwin': {'os': 'macos', 'version': '14.0'},
    'x86_64-unknown-linux-gnu': {'os': 'linux', 'glibc': '2.28'},
    'aarch64-unknown-linux-gnu': {'os': 'linux', 'glibc': '2.28'},
}


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def source_identity(root, revision=None, require_clean=False):
    root = Path(root).resolve()
    identity = {'repository': REPOSITORY, 'revision': None, 'tree': None,
                'lockfile_sha256': sha256(root / 'Cargo.lock') if (root / 'Cargo.lock').is_file() else None,
                'clean': False}
    try:
        actual_root = subprocess.check_output(['git', 'rev-parse', '--show-toplevel'], cwd=root,
                                              text=True, stderr=subprocess.DEVNULL).strip()
        if Path(actual_root).resolve() != root:
            raise ValueError('Source must be the public repository root')
        head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
        tree = subprocess.check_output(['git', 'rev-parse', 'HEAD^{tree}'], cwd=root, text=True).strip()
        clean = not subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=all'],
                                            cwd=root, text=True).strip()
        identity.update(revision=head, tree=tree, clean=clean)
    except (subprocess.CalledProcessError, ValueError):
        if require_clean or revision is not None:
            raise ValueError('An exact clean public source checkout is required') from None
    if revision is not None and (not re.fullmatch(r'[0-9a-f]{40}', revision) or identity['revision'] != revision):
        raise ValueError('Requested source revision does not match the public checkout HEAD')
    if require_clean and (not identity['clean'] or not identity['lockfile_sha256']):
        raise ValueError('Release packaging requires a clean public checkout and Cargo.lock')
    return identity


def rust_toolchain(root):
    document = Path(root) / 'rust-toolchain.toml'
    match = re.search(r'^\s*channel\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"\s*$', document.read_text(), re.M)
    if not match:
        raise ValueError('Release source must pin an exact Rust toolchain version')
    return match[1]


def verify_toolchain(root, run=subprocess.check_output):
    expected = rust_toolchain(root)
    for tool in ('rustc', 'cargo'):
        reported = run([tool, '--version'], text=True).split()
        if len(reported) < 2 or reported[0] != tool or reported[1] != expected:
            raise ValueError('Release builds require the source-pinned ' + tool + ' ' + expected)
    return expected


def write_archive(destination, members):
    """Write (name, bytes-or-None, executable) members in a normalized tar/gzip."""
    destination = Path(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with destination.open('wb') as raw:
        with gzip.GzipFile(fileobj=raw, mode='wb', filename='', mtime=0, compresslevel=9) as compressed:
            with tarfile.open(fileobj=compressed, mode='w', format=tarfile.PAX_FORMAT) as archive:
                for name, data, executable in sorted(members, key=lambda member: member[0]):
                    info = tarfile.TarInfo(name)
                    info.uid = info.gid = info.mtime = 0
                    info.uname = info.gname = ''
                    info.mode = 0o755 if data is None or executable else 0o644
                    if data is None:
                        info.type = tarfile.DIRTYPE
                        archive.addfile(info)
                    else:
                        info.size = len(data)
                        archive.addfile(info, io.BytesIO(data))
    digest = sha256(destination)
    destination.with_name(destination.name + '.sha256').write_text(digest + '  ' + destination.name + '\n')
    return digest


def archive_directory(directory, destination):
    directory = Path(directory)
    members = [(directory.name, None, False)]
    for path in directory.rglob('*'):
        if path.is_symlink() or not (path.is_dir() or path.is_file()):
            raise ValueError('Release archives contain only regular files and directories: ' + str(path))
        members.append((directory.name + '/' + path.relative_to(directory).as_posix(),
                        None if path.is_dir() else path.read_bytes(), bool(path.stat().st_mode & 0o111)))
    return write_archive(destination, members)


def dependency_notices(metadata, package_name='enumeray-cli', strict=False):
    """Include license texts from each normal/build dependency, without host paths."""
    packages = {package['id']: package for package in metadata['packages'] if 'id' in package}
    roots = [package['id'] for package in metadata['packages'] if package['name'] == package_name and 'id' in package]
    nodes = {node['id']: node for node in (metadata.get('resolve') or {}).get('nodes', [])}
    selected, pending = set(), list(roots)
    while pending:
        identifier = pending.pop()
        if identifier in selected:
            continue
        selected.add(identifier)
        for dependency in nodes.get(identifier, {}).get('deps', []):
            if any(kind.get('kind') != 'dev' for kind in dependency.get('dep_kinds', [{'kind': None}])):
                pending.append(dependency['pkg'])
    if strict and (not roots or not nodes):
        raise ValueError('Complete Cargo dependency metadata is required for release license notices')
    notices = []
    for identifier in sorted(selected, key=lambda key: (packages[key]['name'], packages[key]['version'])):
        package = packages[identifier]
        base = Path(package['manifest_path']).parent
        candidates = {path for path in base.rglob('*') if path.is_file() and not path.is_symlink()
                      and re.match(r'^(LICENSE|LICENCE|COPYING|NOTICE|COPYRIGHT)(?:[._-]|$)', path.name, re.I)
                      and not any(part in ('target', '.git') for part in path.relative_to(base).parts)}
        if package.get('license_file'):
            explicit = (base / package['license_file']).resolve()
            if not explicit.is_relative_to(base.resolve()) or not explicit.is_file():
                raise ValueError('Dependency license file escapes its source: ' + package['name'])
            candidates.add(explicit)
        # Workspace packages inherit the project license outside their crate directories.
        if package.get('source') is None:
            for parent in (base, *base.parents):
                if (parent / 'Cargo.lock').is_file() and (parent / 'LICENSE').is_file():
                    candidates.add(parent / 'LICENSE')
                    break
        license_files = []
        for path in sorted(candidates, key=lambda path: str(path)):
            label = path.relative_to(base).as_posix() if path.is_relative_to(base) else 'PROJECT-LICENSE'
            license_files.append({'path': label, 'text': path.read_text(encoding='utf-8', errors='replace')})
        license_origin = None
        if not license_files:
            supplemental = Path(__file__).with_name('dependency-licenses')
            manifest = supplemental / 'sources.json'
            for entry in json.loads(manifest.read_text()) if manifest.is_file() else []:
                if package['name'] not in entry['packages'] or package['version'] != entry['version']:
                    continue
                vcs_file = base / '.cargo_vcs_info.json'
                vcs_revision = json.loads(vcs_file.read_text()).get('git', {}).get('sha1') if vcs_file.is_file() else None
                if (package.get('repository') != entry['repository'] or package.get('license') != entry['license']
                        or vcs_revision != entry['revision']):
                    raise ValueError('Supplemental license source identity mismatch: ' + package['name'])
                license_path = supplemental / entry['file']
                if not license_path.resolve().is_relative_to(supplemental.resolve()) or sha256(license_path) != entry['sha256']:
                    raise ValueError('Supplemental dependency license checksum failed')
                license_files.append({'path': 'UPSTREAM-LICENSE', 'text': license_path.read_text()})
                license_origin = entry['repository'] + '/blob/' + entry['revision'] + '/LICENSE'
                break
        if strict and (not package.get('license') or not license_files):
            raise ValueError('Missing dependency license declaration or text: ' + package['name'])
        notice = {'name': package['name'], 'version': package['version'],
                  'license': package.get('license'), 'source': package.get('source'),
                  'license_files': license_files}
        if license_origin:
            notice['license_origin'] = license_origin
        notices.append(notice)
    return {'schema': 1, 'dependencies': notices}


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')
