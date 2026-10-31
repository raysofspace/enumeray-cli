#!/usr/bin/env python3
"""Standalone client verification; service integration is qualified by the service repo."""
import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CATALOGUE = ROOT / 'scripts/test-strategy.json'


def load_catalogue():
    return json.loads(CATALOGUE.read_text())


def ignored_cases(source):
    for match in re.finditer(r'#\[ignore(?:\s*=\s*"[^"\n]*")?\]', source.read_text()):
        following = re.search(r'\b(?:async\s+)?fn\s+(\w+)\s*\(', source.read_text()[match.end():])
        if not following:
            raise ValueError('Cannot identify ignored case in ' + str(source))
        yield following.group(1)


def validate(catalogue, root=ROOT):
    if catalogue.get('version') != 1:
        raise ValueError('Unsupported verification catalogue version')
    checks = catalogue['checks']
    for profile, selected in catalogue['profiles'].items():
        if not selected or any(name not in checks for name in selected):
            raise ValueError('Invalid profile: ' + profile)
    if any(checks[name].get('explicit') for name in catalogue['profiles']['full']):
        raise ValueError('Full ordinary verification includes an explicit backend fixture')
    for name, check in checks.items():
        for command in check['commands'] + check.get('cleanup', []):
            if not isinstance(command, list) or not command or not all(isinstance(x, str) for x in command):
                raise ValueError('Invalid command in check ' + name)
    discovered = {p.relative_to(root).as_posix()
                  for folder in ('scripts', 'examples', 'crates')
                  for p in (root / folder).rglob('test_*.py')
                  if '__pycache__' not in p.parts and 'target' not in p.parts}
    discovered.update(p.relative_to(root).as_posix() for p in (root / 'crates').glob('*/tests/*.rs'))
    discovered.add('scripts/qualify-binary-archives.py')
    discovered.add('scripts/qualify-homebrew.py')
    if discovered != set(catalogue['suites']):
        raise ValueError('Unclassified or obsolete suites: ' + ', '.join(sorted(discovered ^ set(catalogue['suites']))))
    for source, owner in catalogue['suites'].items():
        if not (root / source).is_file() or owner not in checks:
            raise ValueError('Invalid suite owner: ' + source)
        if source.endswith('.rs'):
            target = Path(source).stem
            package = 'enumeray-' + Path(source).parts[1]
            if not any(command[:2] == ['cargo', 'test'] and ('--all-targets' in command or target in command)
                       and (package in command or '--workspace' in command)
                       for command in checks[owner]['commands']):
                raise ValueError('Suite owner does not execute test: ' + source)
    actual_ignored = {p.relative_to(root).as_posix() + '::' + name
                      for p in (root / 'crates').rglob('*.rs') for name in ignored_cases(p)}
    if actual_ignored != set(catalogue['ignored']):
        raise ValueError('Unclassified or obsolete ignored cases: ' + ', '.join(sorted(actual_ignored ^ set(catalogue['ignored']))))
    for source, entry in catalogue['ignored'].items():
        if entry['check'] not in checks or not checks[entry['check']].get('explicit'):
            raise ValueError('Ignored case must require an explicit fixture: ' + source)
        runner = root / entry['runner']
        if not runner.is_file() or source.split('::')[-1] not in runner.read_text() or '--ignored' not in runner.read_text():
            raise ValueError('Ignored case runner does not execute case: ' + source)
    return len(discovered), len(actual_ignored)


def independent(root=ROOT):
    result = subprocess.check_output(['cargo', 'tree', '--locked', '--edges', 'normal', '--prefix', 'none',
                                      '-p', 'enumeray-cli', '-p', 'enumeray-client', '-p', 'enumeray-connector'],
                                     cwd=root, text=True)
    names = {line.split()[0] for line in result.splitlines() if line.strip()}
    blocked = sorted(name for name in names if name in ('enumeray-api', 'axum', 'sqlx', 'aws-config',
                     'aws-types', 'aws-credential-types', 'aws-runtime', 'aws-sigv4') or
                     name.startswith(('axum-', 'sqlx-', 'aws-sdk-', 'aws-smithy-')))
    if blocked:
        raise ValueError('Client depends on server libraries: ' + ', '.join(blocked))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', nargs='?', default='static')
    parser.add_argument('names', nargs='*')
    parser.add_argument('--area', action='append', default=[])
    parser.add_argument('--json', action='store_true')
    args = parser.parse_args(argv)
    try:
        if args.command == 'independent':
            independent()
            return 0
        catalogue = load_catalogue()
        suites, ignored = validate(catalogue)
        if args.command in ('validate', 'inventory'):
            print('Validated %d suites and %d explicit ignored cases.' % (suites, ignored))
            return 0
        profiles = args.names + args.area if args.command in ('area', 'plan') else [args.command]
        if not profiles:
            profiles = ['full']
        selected = list(dict.fromkeys(name for profile in profiles for name in catalogue['profiles'][profile]))
        if args.command == 'plan':
            if args.json:
                print(json.dumps({'profiles': profiles, 'checks': selected}, indent=2))
            else:
                for name in selected:
                    print(name + ': ' + '; '.join(' '.join(c) for c in catalogue['checks'][name]['commands']))
            return 0
        for name in selected:
            check = catalogue['checks'][name]
            for tool in check.get('tools', []) + check.get('platform_tools', {}).get(sys.platform, []):
                if not shutil.which(tool):
                    raise ValueError('Missing prerequisite: ' + tool)
            result = 0
            try:
                for command in check['commands']:
                    print('+ ' + ' '.join(command), flush=True)
                    result = subprocess.run(command, cwd=ROOT, check=False).returncode
                    if result:
                        break
            finally:
                for command in check.get('cleanup', []):
                    cleanup = subprocess.run(command, cwd=ROOT, check=False).returncode
                    result = result or cleanup
            if result:
                return result
        return 0
    except (KeyError, ValueError, OSError, subprocess.CalledProcessError) as error:
        print('ERROR: ' + str(error), file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
