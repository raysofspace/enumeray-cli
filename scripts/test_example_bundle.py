"""The shipped inventory and tests must operate on the acquired bytes."""
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


bundle = load('example_bundle', 'example_bundle.py')
verifier = load('verify_examples', 'verify-example-bundle.py')
artifacts = load('release_artifacts', 'release_artifacts.py')


class ExampleBundleTests(unittest.TestCase):
    def assemble(self, root, destination):
        for relative in bundle.GUIDES:
            guide = root / relative
            guide.parent.mkdir(parents=True, exist_ok=True)
            guide.write_text('A standalone guide.\n')
        examples = root / 'crates/cli/examples'
        examples.mkdir()
        (examples / 'README.md').write_text('[guide](../AUTHOR_GUIDE.md)\n')
        for name in ('scalar_model', 'collections'):
            project = root / 'examples' / name
            project.mkdir(parents=True)
            (project / 'value.py').write_text('ANSWER = 42\n')
            (project / 'test_fixture.py').write_text(
                'import unittest\nimport value\nclass Fixture(unittest.TestCase):\n'
                ' def test_local_import(self):\n  self.assertEqual(value.ANSWER, 42)\n')
        shutil.copytree(Path(__file__).resolve().parents[1] / 'examples/http-contract',
                        root / 'examples/http-contract', ignore=shutil.ignore_patterns('__pycache__', '*.pyc'))
        (root / 'examples/http-contract/README.md').write_text(
            '[workflow](HTTP_WORKFLOWS.md)\n[inputs](digest-vectors/)\n[automation](../../crates/cli/CLI_AUTOMATION.md)\n')
        (root / 'LICENSE').write_text('License\n')
        (root / 'scripts').mkdir()
        shutil.copyfile(Path(__file__).with_name('verify-example-bundle.py'), root / 'scripts/verify-example-bundle.py')
        templates = root / 'crates/cli/starter-kit'
        templates.mkdir()
        for name in ('protocol.py', 'wrapper.py', 'model.py', 'test_protocol.py', 'test_starter.py',
                     'model.sbatch', 'python.py', 'executable.py', 'demo_solver.py', 'README.md'):
            (templates / name).write_text('# embedded asset ' + name + '\n')
        destination.mkdir()

        def generate(command, **kwargs):
            project = Path(command[command.index('--directory') + 1])
            shutil.copytree(root / 'examples/scalar_model', project)
            kind = command[command.index('--kind') + 1]
            io = command[command.index('--io') + 1]
            for name, contents in bundle.starter_assets(root, kind, io).items():
                (project / name).write_bytes(contents)

        with mock.patch.object(bundle.subprocess, 'run', side_effect=generate):
            return bundle.assemble_examples(root, destination, root / 'enumeray',
                                            {'revision': 'a' * 40, 'clean': False}, '0.1.0')

    def test_exported_tests_run_without_the_source_tree(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'
            root.mkdir()
            destination = Path(temporary) / 'bundle'
            manifest = self.assemble(root, destination)
            shutil.rmtree(root)
            self.assertEqual(verifier.verify_examples(destination)['file_count'], len(manifest['files']))
            self.assertFalse(any(destination.rglob('__pycache__')))
            self.assertIn('[guide](../AUTHOR_GUIDE.md)', (destination / 'examples/README.md').read_text())
            self.assertIn('examples/http-contract', manifest['projects'])
            for name in ('raw-http.py', 'collection-identity.py', 'digest-vectors/vectors.json', 'files/nested/café.txt'):
                self.assertIn('examples/http-contract/' + name, manifest['files'])
            readme = (destination / 'examples/http-contract/README.md').read_text()
            self.assertIn('[workflow](HTTP_WORKFLOWS.md)', readme)
            self.assertIn('[inputs](digest-vectors)', readme)
            self.assertIn('[automation](../../CLI_AUTOMATION.md)', readme)

    def test_http_vectors_run_offline_and_reject_a_stale_digest_input(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'
            root.mkdir()
            destination = Path(temporary) / 'bundle'
            manifest = self.assemble(root, destination)
            # Keep the inventory internally consistent so qualification must
            # catch the scientific-identity mismatch by executing the vectors.
            relative = 'examples/http-contract/digest-vectors/mixed-requested.bytes.json'
            (destination / relative).write_bytes(b'[]')
            manifest['files'][relative] = hashlib.sha256(b'[]').hexdigest()
            (destination / 'examples-manifest.json').write_text(json.dumps(manifest))
            shutil.rmtree(root)
            with self.assertRaises(subprocess.CalledProcessError):
                verifier.verify_examples(destination)

    def test_same_version_stale_embedded_protocol_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'
            root.mkdir()
            destination = Path(temporary) / 'bundle'
            self.assemble(root, destination)
            project = destination / 'starter-kit/python-json'
            (project / 'protocol.py').write_text('# old same-version protocol\n')
            with self.assertRaisesRegex(ValueError, 'stale starter assets: protocol.py'):
                bundle.audit_starter(root, project, 'python', 'json')

    def test_manifest_rejects_modified_extra_and_linked_code_before_tests(self):
        for failure in ('changed', 'extra', 'link'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary) / 'source'
                root.mkdir()
                destination = Path(temporary) / 'bundle'
                self.assemble(root, destination)
                code = destination / 'examples/scalar_model/value.py'
                if failure == 'changed':
                    code.write_text('ANSWER = 0\n')
                elif failure == 'extra':
                    (code.parent / 'unlisted.py').write_text('')
                else:
                    code.unlink()
                    code.symlink_to(root / 'examples/scalar_model/value.py')
                with mock.patch.object(verifier.subprocess, 'run') as run:
                    with self.assertRaises(ValueError):
                        verifier.verify_examples(destination)
                    run.assert_not_called()

    def test_manifest_rejects_traversal(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'
            root.mkdir()
            destination = Path(temporary) / 'bundle'
            manifest = self.assemble(root, destination)
            manifest['files']['../outside'] = 'a' * 64
            (destination / 'examples-manifest.json').write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, 'inventory'):
                verifier.verify_manifest(destination)

    def test_examples_archive_is_deterministic_and_carries_exact_manifest_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'
            root.mkdir()
            destination = Path(temporary) / 'bundle'
            self.assemble(root, destination)
            archive = Path(temporary) / 'bundle.tar.gz'
            artifacts.archive_directory(destination, archive)
            original = archive.read_bytes()
            artifacts.archive_directory(destination, archive)
            self.assertEqual(original, archive.read_bytes())
            self.assertIn(hashlib.sha256(original).hexdigest(), Path(str(archive) + '.sha256').read_text())
            import tarfile
            with tarfile.open(archive) as tar:
                self.assertEqual(tar.extractfile('bundle/examples-manifest.json').read(),
                                 (destination / 'examples-manifest.json').read_bytes())

    def test_development_inventory_excludes_git_ignored_local_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            (root / '.gitignore').write_text('*.secret\n__pycache__/\n')
            source = root / 'examples'
            source.mkdir()
            (source / 'wrapper.py').write_text('# current untracked source\n')
            (source / 'credential.secret').write_text('local private bytes\n')
            (source / '__pycache__').mkdir()
            (source / '__pycache__/wrapper.pyc').write_bytes(b'cache')
            bundle.copy_project(source, root / 'export', root=root)
            self.assertEqual([path.name for path in (root / 'export').iterdir()], ['wrapper.py'])

    def test_source_links_are_rejected_and_python_caches_are_excluded(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'source'
            source.mkdir()
            (source / 'module.py').write_text('')
            (source / '__pycache__').mkdir()
            (source / '__pycache__/module.pyc').write_bytes(b'cache')
            bundle.copy_project(source, root / 'copy')
            self.assertFalse((root / 'copy/__pycache__').exists())
            (source / 'link').symlink_to(source / 'module.py')
            with self.assertRaisesRegex(ValueError, 'regular files'):
                bundle.copy_project(source, root / 'linked-copy')


if __name__ == '__main__':
    unittest.main()
