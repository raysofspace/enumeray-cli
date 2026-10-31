"""Packaged operational help must work without the repository's directory tree."""
import importlib.util
import contextlib
import hashlib
import io
import json
import shutil
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('package_binaries', Path(__file__).with_name('package-binaries.py'))
packaging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packaging)


class BundleGuideTests(unittest.TestCase):
    def test_nested_example_links_follow_the_relocated_archive_layout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'
            examples = root / 'crates/cli/examples'
            examples.mkdir(parents=True)
            (examples / 'nested').mkdir()
            (examples / 'README.md').write_text('[connector](../../connector/README.md#single-user-development)\n'
                                               '[nested](nested/README.md#usage)\n')
            (examples / 'nested/README.md').write_text('[installation](../../INSTALL.md#upgrade-maintenance)\n'
                                                      '[source](../../../../Cargo.toml)\n')
            destination = Path(temporary) / 'archive/examples'
            shutil.copytree(examples, destination)
            guides = {(root / 'crates/connector/README.md').resolve(): 'CONNECTOR_GUIDE.md',
                      (root / 'crates/cli/INSTALL.md').resolve(): 'INSTALL.md'}
            bundled = packaging.bundle_example_guides(root, destination, guides, revision='a' * 40)
            self.assertEqual((destination / 'README.md').read_text(),
                             '[connector](../CONNECTOR_GUIDE.md#single-user-development)\n'
                             '[nested](nested/README.md#usage)\n')
            self.assertIn('[installation](../../INSTALL.md#upgrade-maintenance)',
                          (destination / 'nested/README.md').read_text())
            self.assertIn('/blob/' + 'a' * 40 + '/Cargo.toml',
                          (destination / 'nested/README.md').read_text())
            self.assertEqual(bundled[(examples / 'README.md').resolve()], 'examples/README.md')

    def test_bundled_links_stay_local_and_other_repository_links_keep_fragments(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'crates/connector/EXTENDING.md'
            source.parent.mkdir(parents=True)
            source.write_text('[operation](README.md#enrollment)\n'
                              '[test](tests/README.md#fixture)\n'
                              '[install](https://github.com/raysofspace/enumeray-cli/blob/main/crates/cli/INSTALL.md#login)\n'
                              '[external](https://example.org/help#topic)\n'
                              '[same](#local)\n')
            output = root / 'bundle.md'
            packaging.bundle_guide(source, output, root, {
                (source.parent / 'README.md').resolve(): 'CONNECTOR_GUIDE.md',
                (root / 'crates/cli/INSTALL.md').resolve(): 'INSTALL.md',
            })
            rendered = output.read_text()
            self.assertIn('](CONNECTOR_GUIDE.md#enrollment)', rendered)
            self.assertIn('](INSTALL.md#login)', rendered)
            self.assertIn('](https://github.com/raysofspace/enumeray-cli/blob/main/crates/connector/tests/README.md#fixture)', rendered)
            self.assertIn('](https://example.org/help#topic)', rendered)
            self.assertIn('](#local)', rendered)
            self.assertIn('](README.md#enrollment)', source.read_text())

    def test_invalid_source_layout_cannot_emit_checkout_escaping_links(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'guide.md'
            source.write_text('[bad](../outside.md)')
            with self.assertRaisesRegex(SystemExit, 'escapes the source repository'):
                packaging.bundle_guide(source, root / 'output.md', root, {})


class UnifiedPackageTests(unittest.TestCase):
    def package(self, root, tree='enumeray-cli v0.1.0\nenumeray-connector v0.1.0\n', target_directory=None):
        api = root
        (api / 'scripts').mkdir(parents=True, exist_ok=True)
        (api / 'LICENSE').write_text('Apache License, Version 2.0\n')
        for name in ('INSTALL.md', 'SETUP_GUIDE.md', 'AUTHOR_GUIDE.md', 'ANALYSIS_GUIDE.md'):
            path = api / 'crates/cli' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('Standalone guide\n')
        for name in ('README.md', 'EXTENDING.md'):
            path = api / 'crates/connector' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('Standalone guide\n')
        (api / 'crates/cli/examples').mkdir(exist_ok=True)
        target_directory = target_directory or api / 'target'
        binary = target_directory / 'test-target/release/enumeray'
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_text('#!/bin/sh\nexit 0\n')
        binary.chmod(0o755)
        metadata = json.dumps({'packages': [{'name': 'enumeray-cli', 'version': '0.1.0'}],
                               'target_directory': str(target_directory)})
        with mock.patch.object(packaging, '__file__', str(api / 'scripts/package-binaries.py')), \
             mock.patch.object(packaging.subprocess, 'check_output', side_effect=[metadata, tree]), \
             mock.patch.object(packaging.artifacts, 'source_identity', return_value={
                 'repository': packaging.artifacts.REPOSITORY, 'revision': None, 'tree': None,
                 'lockfile_sha256': None, 'clean': False}), \
             mock.patch('sys.argv', ['package-binaries.py', '--target', 'test-target', '--output', str(root / 'dist')]), \
             contextlib.redirect_stdout(io.StringIO()):
            packaging.main()

    def test_one_archive_contains_one_executable_and_all_capabilities(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.package(root)
            archives = list((root / 'dist').glob('*.tar.gz'))
            self.assertEqual([p.name for p in archives], ['enumeray-0.1.0-test-target.tar.gz'])
            archive = archives[0]
            checksum, name = archive.with_suffix('.gz.sha256').read_text().split()
            self.assertEqual(name, archive.name)
            self.assertEqual(checksum, hashlib.sha256(archive.read_bytes()).hexdigest())
            with tarfile.open(archive) as tar:
                executables = [m for m in tar.getmembers() if m.isfile() and m.mode & 0o111]
                self.assertEqual([Path(m.name).name for m in executables], ['enumeray'])
                prefix = archive.name.removesuffix('.tar.gz')
                compatibility = json.load(tar.extractfile(prefix + '/compatibility.json'))
                self.assertEqual(compatibility['capabilities'], ['cli', 'connector', 'compute_runner'])
                self.assertFalse(compatibility['requires_database'])
                self.assertTrue(compatibility['requires_openssh_for_ssh'])
                self.assertEqual(compatibility['source']['repository'], packaging.artifacts.REPOSITORY)
                for guide in ('LICENSE', 'THIRD_PARTY_NOTICES.json', 'INSTALL.md', 'CONNECTOR_GUIDE.md',
                              'EXTENDING.md', 'AUTHOR_GUIDE.md', 'ANALYSIS_GUIDE.md', 'SETUP_GUIDE.md'):
                    self.assertTrue(tar.getmember(prefix + '/' + guide).isfile())

    def test_repeated_packaging_normalizes_mtimes_ownership_and_gzip_header(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.package(root)
            archive = next((root / 'dist').glob('*.tar.gz'))
            original = archive.read_bytes()
            self.package(root)
            self.assertEqual(archive.read_bytes(), original)
            with tarfile.open(archive) as tar:
                self.assertTrue(all(member.mtime == member.uid == member.gid == 0 for member in tar))

    def test_server_dependency_is_rejected_before_packaging(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaisesRegex(SystemExit, 'server libraries: sqlx-core'):
                self.package(root, 'enumeray-cli v0.1.0\nsqlx-core v0.9.0\n')
            self.assertEqual(list((root / 'dist').glob('*.tar.gz')), [])

    def test_custom_cargo_target_directory_is_used_without_a_workspace_target(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'checkout'
            target = Path(temporary) / 'cargo-cache'
            self.package(root, target_directory=target)
            self.assertFalse((root / 'target').exists())
            self.assertEqual(len(list((root / 'dist').glob('*.tar.gz'))), 1)

    def test_qualification_rejects_a_second_archive(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ('enumeray', 'enumeray-connector'):
                (root / f'{name}-0.1.0-test-target.tar.gz').touch()
            result = subprocess.run([sys.executable, str(Path(__file__).with_name('qualify-binary-archives.py')),
                                     '--archives', str(root), '--target', 'test-target'], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('expected one unified Enumeray archive', result.stderr)


if __name__ == '__main__':
    unittest.main()
