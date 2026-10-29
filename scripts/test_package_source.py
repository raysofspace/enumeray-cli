"""Source releases bind exact clean public inputs and carry dependency licenses."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('package_source', Path(__file__).with_name('package-source.py'))
source = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source)


class SourcePackageTests(unittest.TestCase):
    def checkout(self, parent):
        root = parent / 'public'
        root.mkdir()
        for name, value in {
            'Cargo.toml': '[workspace]\nmembers = ["crates/cli"]\n',
            'Cargo.lock': 'version = 4\n',
            'LICENSE': 'Apache License, Version 2.0\n',
            'rust-toolchain.toml': '[toolchain]\nchannel = "1.98.1"\n',
            'crates/cli/Cargo.toml': '[package]\nname="enumeray-cli"\nversion="0.1.0"\n',
            'crates/cli/src/main.rs': 'fn main() {}\n',
            '.gitignore': 'ignored-secret\n',
            '.gitattributes': 'crates/cli/src/main.rs export-ignore\n',
        }.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(value)
        subprocess.run(['git', 'init', '-q', str(root)], check=True)
        subprocess.run(['git', 'add', '.'], cwd=root, check=True)
        subprocess.run(['git', '-c', 'user.name=Release fixture', '-c', 'user.email=fixture@example.org',
                        'commit', '-qm', 'Public fixture'], cwd=root, check=True)
        revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
        metadata = {'packages': [{'id': 'cli', 'name': 'enumeray-cli', 'version': '0.1.0',
                                 'source': None, 'license': 'Apache-2.0',
                                 'manifest_path': str(root / 'crates/cli/Cargo.toml')}],
                    'resolve': {'nodes': [{'id': 'cli', 'deps': []}]}}
        return root, revision, metadata

    def test_reproducible_archive_excludes_parent_history_and_ignored_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            root, revision, metadata = self.checkout(parent)
            (parent / 'private-service-secret').write_text('private')
            (root / 'ignored-secret').write_text('ignored')
            with mock.patch.object(source.subprocess, 'check_output', wraps=subprocess.check_output) as run:
                run.side_effect = lambda argv, **kwargs: (json.dumps(metadata) if argv[0] == 'cargo'
                                                          else subprocess.run(argv, **kwargs, stdout=subprocess.PIPE,
                                                                              check=True).stdout)
                first = source.package_source(root, parent / 'first', revision, '0.1.0')
                second = source.package_source(root, parent / 'second', revision, '0.1.0')
            archive = parent / 'first' / first['archive']
            self.assertEqual(first, second)
            self.assertEqual(archive.read_bytes(), (parent / 'second' / second['archive']).read_bytes())
            self.assertEqual(first['source']['revision'], revision)
            self.assertEqual(first['rust_toolchain'], '1.98.1')
            self.assertTrue(first['source']['clean'])
            with tarfile.open(archive) as tar:
                names = tar.getnames()
                self.assertTrue(any(name.endswith('crates/cli/src/main.rs') for name in names))
                self.assertTrue(any(name.endswith('THIRD_PARTY_NOTICES.json') for name in names))
                self.assertFalse(any('.git/' in name or 'secret' in name for name in names))
                identity = json.load(tar.extractfile('enumeray-0.1.0-source/release-source.json'))
                self.assertEqual(identity, first['source'])

    def test_dirty_or_wrong_revision_is_rejected_before_dependency_collection(self):
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            root, revision, _ = self.checkout(parent)
            with self.assertRaisesRegex(ValueError, 'does not match'):
                source.package_source(root, parent / 'dist', 'a' * 40)
            (root / 'Cargo.lock').write_text('changed')
            with self.assertRaisesRegex(ValueError, 'clean public checkout'):
                source.package_source(root, parent / 'dist', revision)
            self.assertFalse((parent / 'dist').exists())

    def test_dependency_notices_exclude_dev_only_dependencies_and_host_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            packages = []
            for name in ('enumeray-cli', 'normal-dependency', 'dev-dependency'):
                base = root / name
                base.mkdir()
                (base / 'LICENSE').write_text(name + ' license')
                packages.append({'id': name, 'name': name, 'version': '1.0.0', 'license': 'MIT',
                                 'manifest_path': str(base / 'Cargo.toml'), 'source': 'registry+https://crates.io'})
            metadata = {'packages': packages, 'resolve': {'nodes': [{
                'id': 'enumeray-cli', 'deps': [
                    {'pkg': 'normal-dependency', 'dep_kinds': [{'kind': None}]},
                    {'pkg': 'dev-dependency', 'dep_kinds': [{'kind': 'dev'}]},
                ]}]}}
            notices = source.artifacts.dependency_notices(metadata, strict=True)
            self.assertEqual({entry['name'] for entry in notices['dependencies']},
                             {'enumeray-cli', 'normal-dependency'})
            self.assertNotIn(str(root), json.dumps(notices))
            (root / 'normal-dependency/LICENSE').unlink()
            with self.assertRaisesRegex(ValueError, 'Missing dependency license'):
                source.artifacts.dependency_notices(metadata, strict=True)

    def test_supplemental_license_requires_matching_package_repository_and_vcs_revision(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            package = {'id': 'uuid-simd', 'name': 'uuid-simd', 'version': '0.8.0', 'license': 'MIT',
                       'repository': 'https://github.com/Nugine/simd', 'source': 'registry+https://crates.io',
                       'manifest_path': str(base / 'Cargo.toml')}
            metadata = {'packages': [package], 'resolve': {'nodes': [{'id': 'uuid-simd', 'deps': []}]}}
            with self.assertRaisesRegex(ValueError, 'source identity mismatch'):
                source.artifacts.dependency_notices(metadata, package_name='uuid-simd', strict=True)
            (base / '.cargo_vcs_info.json').write_text(json.dumps({
                'git': {'sha1': 'd74c030d9dc4f3cae02146d1f497ff62726ef09a'}}))
            notice = source.artifacts.dependency_notices(metadata, package_name='uuid-simd', strict=True)['dependencies'][0]
            self.assertIn('Copyright (c) 2021 Nugine', notice['license_files'][0]['text'])
            self.assertTrue(notice['license_origin'].endswith('/d74c030d9dc4f3cae02146d1f497ff62726ef09a/LICENSE'))

    def test_binary_packaging_rechecks_tracked_inputs_before_emitting_archive(self):
        binary_spec = importlib.util.spec_from_file_location('binary_package', Path(__file__).with_name('package-binaries.py'))
        binary_package = importlib.util.module_from_spec(binary_spec)
        binary_spec.loader.exec_module(binary_package)
        with tempfile.TemporaryDirectory() as temporary:
            root, revision, _ = self.checkout(Path(temporary))
            original = source.artifacts.source_identity(root, revision, require_clean=True)
            (root / 'crates/cli/src/main.rs').write_text('fn main() { panic!("changed"); }')
            with self.assertRaisesRegex(ValueError, 'clean public checkout'):
                binary_package.assert_source_unchanged(root, revision, original)

    def test_release_build_rejects_host_compiler_that_ignores_the_pinned_toolchain(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, _, _ = self.checkout(Path(temporary))
            with self.assertRaisesRegex(ValueError, 'rustc 1.98.1'):
                source.artifacts.verify_toolchain(root, run=mock.Mock(return_value='rustc 1.99.1 (host)'))
            self.assertEqual(source.artifacts.verify_toolchain(root, run=mock.Mock(side_effect=[
                'rustc 1.98.1 (pinned)', 'cargo 1.98.1 (pinned)'])), '1.98.1')


if __name__ == '__main__':
    unittest.main()
