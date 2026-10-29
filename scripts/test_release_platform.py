"""Newer hosts and incompatible binary symbols cannot establish minimum-OS proof."""
import importlib.util
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('release_platform', Path(__file__).with_name('release_platform.py'))
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)


class PlatformAuditTests(unittest.TestCase):
    def linux(self, binary, libc='2.28', symbols='GLIBC_2.28 GLIBC_2.17', library='libc.so.6'):
        replies = [f'glibc {libc}\n', symbols, f' (NEEDED) Shared library: [{library}]\n']
        with mock.patch.object(audit.platform, 'system', return_value='Linux'), \
             mock.patch.object(audit.platform, 'machine', return_value='x86_64'):
            return audit.audit_platform(binary, 'x86_64-unknown-linux-gnu', {'os': 'linux', 'glibc': '2.28'},
                                        run=mock.Mock(side_effect=replies))

    def elf(self, directory):
        binary = Path(directory) / 'enumeray'
        binary.write_bytes(b'\x7fELF\x02\x01' + b'\0' * 12 + struct.pack('<H', 62))
        return binary

    def test_exact_native_glibc_runtime_and_compatible_symbols_establish_platform_proof(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.linux(self.elf(directory))
            self.assertTrue(result['platform_qualified'])
            self.assertEqual(result['binary_audit']['required_glibc'], '2.28')

    def test_newer_runtime_only_proves_a_newer_host_smoke(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertFalse(self.linux(self.elf(directory), libc='2.39')['platform_qualified'])

    def test_new_glibc_symbol_or_external_library_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = self.elf(directory)
            with self.assertRaisesRegex(ValueError, 'newer than'):
                self.linux(binary, symbols='GLIBC_2.34')
            with self.assertRaisesRegex(ValueError, 'non-system'):
                self.linux(binary, library='libssl.so.3')

    def test_cross_architecture_execution_cannot_be_native_proof(self):
        with mock.patch.object(audit.platform, 'system', return_value='Linux'), \
             mock.patch.object(audit.platform, 'machine', return_value='aarch64'):
            with self.assertRaisesRegex(ValueError, 'native execution'):
                audit.audit_platform(Path('/unused'), 'x86_64-unknown-linux-gnu', {'os': 'linux', 'glibc': '2.28'})

    def test_macos15_does_not_qualify_macos14_and_late_deployment_target_is_rejected(self):
        for runtime, minimum, expected in [('14.7.1', '14.0', True), ('15.5', '14.0', False)]:
            with self.subTest(runtime=runtime), \
                 mock.patch.object(audit.platform, 'system', return_value='Darwin'), \
                 mock.patch.object(audit.platform, 'machine', return_value='arm64'), \
                 mock.patch.object(audit.platform, 'mac_ver', return_value=(runtime, (), '')):
                replies = ['0\n', 'arm64\n', 'cmd LC_BUILD_VERSION\n minos ' + minimum,
                           'enumeray:\n /usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n']
                result = audit.audit_platform(Path('/unused'), 'aarch64-apple-darwin',
                                              {'os': 'macos', 'version': '14.0'},
                                              run=mock.Mock(side_effect=replies))
                self.assertEqual(result['platform_qualified'], expected)
        with mock.patch.object(audit.platform, 'system', return_value='Darwin'), \
             mock.patch.object(audit.platform, 'machine', return_value='arm64'), \
             mock.patch.object(audit.platform, 'mac_ver', return_value=('14.7', (), '')):
            with self.assertRaisesRegex(ValueError, 'deployment target'):
                audit.audit_platform(Path('/unused'), 'aarch64-apple-darwin',
                                     {'os': 'macos', 'version': '14.0'},
                                     run=mock.Mock(side_effect=['0', 'arm64', 'minos 15.0']))

    def test_rosetta_process_cannot_establish_native_intel_qualification(self):
        with mock.patch.object(audit.platform, 'system', return_value='Darwin'), \
             mock.patch.object(audit.platform, 'machine', return_value='x86_64'):
            runner = mock.Mock(return_value='1\n')
            with self.assertRaisesRegex(ValueError, 'Rosetta'):
                audit.audit_platform(Path('/unused'), 'x86_64-apple-darwin',
                                     {'os': 'macos', 'version': '14.0'}, run=runner)
            self.assertEqual(runner.call_args.args[0], ['sysctl', '-n', 'sysctl.proc_translated'])

    def test_only_real_intel_unknown_oid_can_bypass_absent_translation_flag(self):
        unknown = subprocess.CalledProcessError(1, 'sysctl', stderr="sysctl: unknown oid 'sysctl.proc_translated'")
        audit.require_untranslated_macos('x86_64', run=mock.Mock(side_effect=unknown))
        audit.require_untranslated_macos('arm64', run=mock.Mock(return_value='0\n'))
        with self.assertRaisesRegex(ValueError, 'Cannot establish'):
            audit.require_untranslated_macos('arm64', run=mock.Mock(side_effect=unknown))
        denied = subprocess.CalledProcessError(1, 'sysctl', stderr='sysctl: operation not permitted')
        with self.assertRaisesRegex(ValueError, 'Cannot establish'):
            audit.require_untranslated_macos('x86_64', run=mock.Mock(side_effect=denied))


if __name__ == '__main__':
    unittest.main()
