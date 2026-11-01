"""One assembly boundary for public examples in web and native archives."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
from urllib.parse import unquote

GUIDES = {
    "crates/cli/INSTALL.md": "INSTALL.md",
    "crates/cli/SETUP_GUIDE.md": "SETUP_GUIDE.md",
    "crates/cli/AUTHOR_GUIDE.md": "AUTHOR_GUIDE.md",
    "crates/cli/ANALYSIS_GUIDE.md": "ANALYSIS_GUIDE.md",
    "crates/cli/CLI_AUTOMATION.md": "CLI_AUTOMATION.md",
    "crates/connector/README.md": "CONNECTOR_GUIDE.md",
    "crates/connector/EXTENDING.md": "EXTENDING.md",
}
PROJECTS = ("examples/scalar_model", "examples/collections",
            "starter-kit/python-json", "starter-kit/python-files",
            "starter-kit/executable-json", "starter-kit/executable-files",
            "examples/http-contract")
STANDALONE_PROJECTS = ("examples/scalar_model", "examples/collections", "examples/http-contract")

def bundle_guide(source, destination, repository, guide_names, revision=None):
    """Keep packaged guides navigable without their original checkout layout."""
    prefix = "https://github.com/raysofspace/enumeray-cli/blob/main/"

    def rewrite(match):
        target, separator, fragment = match[1].partition("#")
        if target.startswith(prefix):
            resolved = repository / unquote(target[len(prefix):])
        elif re.match(r"[A-Za-z][A-Za-z0-9+.-]*:", target) or target.startswith(("/", "#")) or not target:
            return match[0]
        else:
            resolved = source.parent / unquote(target)
        resolved = resolved.resolve()
        if resolved in guide_names:
            url = guide_names[resolved]
        else:
            try:
                url = prefix.replace('/main/', '/' + revision + '/') if revision else prefix
                url += resolved.relative_to(repository.resolve()).as_posix()
            except ValueError:
                raise SystemExit(f"Guide link escapes the source repository: {source}: {target}")
        return "](" + url + (separator + fragment if separator else "") + ")"

    destination.write_text(re.sub(r"\]\(([^\s)]+)\)", rewrite, source.read_text()))


def copy_examples(root, destination, revision=None):
    """Release archives exclude even ignored local files from the example tree."""
    relative = Path('crates/cli/examples')
    if revision is None:
        copy_project(root / relative, destination, root=root)
        return
    destination.mkdir()
    files = subprocess.check_output(['git', 'ls-tree', '-rz', '--name-only', revision, '--', relative.as_posix()], cwd=root)
    for entry in files.split(b'\0'):
        if not entry:
            continue
        source = root / entry.decode('utf-8')
        if source.is_symlink() or not source.is_file():
            raise ValueError('Release examples must contain tracked regular files')
        target = destination / source.relative_to(root / relative)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)


def bundle_example_guides(root, destination, guide_names, revision=None):
    """Keep links relative to each relocated example, including top-level guides."""
    sources = root / 'crates/cli/examples'
    bundled = {**guide_names, (root / 'LICENSE').resolve(): 'LICENSE',
               sources.resolve(): 'examples'}
    for path in destination.rglob('*'):
        relative = path.relative_to(destination)
        bundled[(sources / relative).resolve()] = (Path('examples') / relative).as_posix()
    for target in destination.rglob('*.md'):
        source = sources / target.relative_to(destination)
        relative_names = {name: Path(os.path.relpath(destination.parent / output, target.parent)).as_posix()
                          for name, output in bundled.items()}
        bundle_guide(source, target, root, relative_names, revision=revision)
    return bundled


def copy_project(source, destination, revision=None, root=None):
    """Copy public source inventory, excluding ignored files and Python caches."""
    if source.is_symlink():
        raise ValueError('Example sources must be regular files and directories')
    if root is not None and (root / '.git').exists():
        relative = source.relative_to(root).as_posix()
        command = (['git', 'ls-tree', '-rz', '--name-only', revision, '--', relative] if revision else
                   ['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard', '--', relative])
        entries = subprocess.check_output(command, cwd=root)
        destination.mkdir(parents=True)
        for entry in entries.split(b'\0'):
            if entry:
                original = root / entry.decode('utf-8')
                if original.is_symlink() or not original.is_file():
                    raise ValueError('Examples must contain regular source files')
                target = destination / original.relative_to(source)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(original, target)
        return
    ignored = shutil.ignore_patterns('__pycache__', '*.pyc', '.DS_Store')
    for path in source.rglob('*'):
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            raise ValueError('Example sources must be regular files and directories')
    shutil.copytree(source, destination, ignore=ignored)


def starter_assets(root, kind, io):
    templates = root / 'crates/cli/starter-kit'
    assets = {name: (templates / name).read_bytes() for name in
              ('protocol.py', 'wrapper.py', 'model.py', 'test_protocol.py', 'test_starter.py', 'model.sbatch')}
    assets['implementation.py'] = (templates / ('python.py' if kind == 'python' else 'executable.py')).read_bytes()
    if kind == 'executable':
        assets['demo_solver.py'] = (templates / 'demo_solver.py').read_bytes()
    for guide in ('AUTHOR_GUIDE.md', 'ANALYSIS_GUIDE.md'):
        assets[guide] = (root / 'crates/cli' / guide).read_bytes()
    model = f'starter-{kind}-{io}'
    assets['README.md'] = (templates / 'README.md').read_text().replace('{{MODEL_ID}}', model).replace(
        '{{ANALYSIS_BINDINGS}}', ' --file dataset=fixtures/dataset.csv' if io == 'files' else '').encode()
    return assets


def audit_starter(root, project, kind, io):
    """A same-version stale CLI must not label old embedded assets as this source."""
    for name, expected in starter_assets(root, kind, io).items():
        actual = project / name
        if not actual.is_file() or actual.read_bytes() != expected:
            raise ValueError('Built CLI has stale starter assets: ' + name + '; rebuild it from the selected public source')


def assemble_examples(root, destination, binary, source, version, release=False):
    root, destination = Path(root).resolve(), Path(destination).resolve()
    guide_names = {(root / name).resolve(): output for name, output in GUIDES.items()}
    copy_examples(root, destination / 'examples', source['revision'] if release else None)
    bundled_names = bundle_example_guides(root, destination / 'examples', guide_names,
                                         revision=source['revision'])
    for relative in STANDALONE_PROJECTS:
        source_directory = root / relative
        if not source_directory.is_dir():
            raise ValueError('Missing standalone example: ' + relative)
        copy_project(source_directory, destination / relative,
                     source['revision'] if release else None, root=root)
        copied_directory = destination / relative
        # Map only the acquired inventory. Release assembly can exclude tracked-
        # revision omissions and ignored local files that still exist in source.
        bundled_names[source_directory.resolve()] = relative
        for path in copied_directory.rglob('*'):
            source_path = source_directory / path.relative_to(copied_directory)
            bundled_names[source_path.resolve()] = path.relative_to(destination).as_posix()
    support = root / 'examples/local-scalar-model.json'
    if support.is_file():
        shutil.copy2(support, destination / 'examples/local-scalar-model.json')
        bundled_names[support.resolve()] = 'examples/local-scalar-model.json'
    for relative in STANDALONE_PROJECTS:
        for target in (destination / relative).rglob('*.md'):
            source_file = root / relative / target.relative_to(destination / relative)
            names = {name: Path(os.path.relpath(destination / output, target.parent)).as_posix()
                     for name, output in bundled_names.items()}
            bundle_guide(source_file, target, root, names, revision=source['revision'])
    # Revisit package Markdown now that standalone sibling projects are mapped.
    for target in (destination / 'examples').rglob('*.md'):
        if any(target.is_relative_to(destination / project) for project in STANDALONE_PROJECTS):
            continue
        source_file = root / 'crates/cli/examples' / target.relative_to(destination / 'examples')
        names = {name: Path(os.path.relpath(destination / output, target.parent)).as_posix()
                 for name, output in bundled_names.items()}
        bundle_guide(source_file, target, root, names, revision=source['revision'])
    for guide, output in guide_names.items():
        # These guides are also embedded verbatim by model init.
        if output in ('AUTHOR_GUIDE.md', 'ANALYSIS_GUIDE.md'):
            shutil.copy2(guide, destination / output)
        else:
            bundle_guide(guide, destination / output, root, bundled_names, revision=source['revision'])
    shutil.copy2(root / 'LICENSE', destination / 'LICENSE')
    shutil.copy2(root / 'scripts/verify-example-bundle.py', destination / 'verify-examples.py')
    starters = destination / 'starter-kit'
    starters.mkdir()
    for kind in ('python', 'executable'):
        for io in ('json', 'files'):
            subprocess.run([str(Path(binary).resolve()), 'model', 'init', f'starter-{kind}-{io}',
                            '--directory', str(starters / f'{kind}-{io}'), '--kind', kind,
                            '--io', io, '--driver', 'slurm'], check=True, stdout=subprocess.DEVNULL)
            audit_starter(root, starters / f'{kind}-{io}', kind, io)
    files = {}
    for base in ('examples', 'starter-kit'):
        for path in sorted((destination / base).rglob('*')):
            if path.is_file():
                files[path.relative_to(destination).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    for output in (*GUIDES.values(), 'LICENSE', 'verify-examples.py'):
        files[output] = hashlib.sha256((destination / output).read_bytes()).hexdigest()
    manifest = {'format_version': 1, 'version': version, 'source': source,
                'projects': list(PROJECTS), 'files': dict(sorted(files.items()))}
    (destination / 'examples-manifest.json').write_text(json.dumps(manifest, indent=2, sort_keys=True) + '\n')
    return manifest
