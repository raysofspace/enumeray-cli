"""Audit binary deployment bounds and distinguish native minimum-OS execution."""
import platform
import re
import struct
import subprocess


def version_tuple(value):
    parts = tuple(int(part) for part in value.split('.'))
    return parts + (0,) * max(0, 3 - len(parts))


def require_untranslated_macos(machine, run=subprocess.check_output):
    """An x86 process under Rosetta must not establish native Intel evidence."""
    try:
        translated = run(['sysctl', '-n', 'sysctl.proc_translated'], text=True,
                         stderr=subprocess.PIPE).strip()
    except subprocess.CalledProcessError as error:
        diagnostic = error.stderr or ''
        if isinstance(diagnostic, bytes):
            diagnostic = diagnostic.decode('utf-8', errors='replace')
        # This OID does not exist on Intel Macs; other sysctl failures cannot
        # establish native execution and must remain visible.
        if machine == 'x86_64' and 'unknown oid' in diagnostic.lower():
            return
        raise ValueError('Cannot establish untranslated native macOS execution') from error
    if translated == '1':
        raise ValueError('Rosetta execution cannot qualify the native target CPU')
    if translated != '0':
        raise ValueError('Cannot establish untranslated native macOS execution')


def audit_platform(binary, target, minimum, run=subprocess.check_output):
    expected_machine = 'arm64' if target.startswith('aarch64-') else 'x86_64'
    actual_machine = {'aarch64': 'arm64', 'AMD64': 'x86_64'}.get(platform.machine(), platform.machine())
    expected_system = 'Darwin' if minimum['os'] == 'macos' else 'Linux'
    runtime = {'system': platform.system(), 'machine': actual_machine,
               'native': platform.system() == expected_system and actual_machine == expected_machine}
    if not runtime['native']:
        raise ValueError('Qualification requires native execution on the target operating system and CPU')
    if minimum['os'] == 'linux':
        with binary.open('rb') as executable:
            header = executable.read(20)
        expected_elf_machine = 183 if expected_machine == 'arm64' else 62
        if (header[:6] != b'\x7fELF\x02\x01' or len(header) < 20
                or struct.unpack('<H', header[18:20])[0] != expected_elf_machine):
            raise ValueError('Archive binary is not the expected native 64-bit Linux executable')
        libc = run(['getconf', 'GNU_LIBC_VERSION'], text=True).strip()
        if not re.fullmatch(r'glibc [0-9]+\.[0-9]+', libc):
            raise ValueError('Cannot establish the native glibc runtime version')
        runtime['glibc'] = libc.split()[1]
        versions = run(['readelf', '--version-info', str(binary)], text=True)
        required = sorted({match for match in re.findall(r'\bGLIBC_([0-9]+\.[0-9]+(?:\.[0-9]+)?)\b', versions)},
                          key=version_tuple)
        if not required or any(version_tuple(value) > version_tuple(minimum['glibc']) for value in required):
            raise ValueError('Binary requires glibc symbols newer than the supported minimum')
        dynamic = run(['readelf', '--dynamic', str(binary)], text=True)
        dependencies = sorted(re.findall(r'\(NEEDED\).*?\[([^\]]+)\]', dynamic))
        allowed = {'libc.so.6', 'libm.so.6', 'libgcc_s.so.1', 'libpthread.so.0', 'libdl.so.2',
                   'librt.so.1', 'libutil.so.1', 'libresolv.so.2', 'ld-linux-x86-64.so.2', 'ld-linux-aarch64.so.1'}
        if not dependencies or set(dependencies) - allowed:
            raise ValueError('Binary has unsupported non-system dynamic library dependencies')
        binary_audit = {'required_glibc': required[-1], 'dynamic_libraries': dependencies}
        qualified = runtime['glibc'] == minimum['glibc']
    else:
        require_untranslated_macos(actual_machine, run=run)
        runtime['translated'] = False
        runtime['version'] = platform.mac_ver()[0]
        architectures = run(['lipo', '-archs', str(binary)], text=True).split()
        if architectures != [expected_machine]:
            raise ValueError('Archive binary is not the expected native macOS executable')
        commands = run(['otool', '-l', str(binary)], text=True)
        versions = re.findall(r'\bminos\s+([0-9.]+)', commands)
        versions += re.findall(r'LC_VERSION_MIN_MACOSX\s+cmdsize\s+\d+\s+version\s+([0-9.]+)', commands)
        if not versions or any(version_tuple(value) > version_tuple(minimum['version']) for value in versions):
            raise ValueError('Binary deployment target exceeds the supported macOS minimum')
        linked = run(['otool', '-L', str(binary)], text=True).splitlines()[1:]
        dependencies = sorted(line.strip().split(' (', 1)[0] for line in linked if line.strip())
        if not dependencies or any(not name.startswith(('/usr/lib/', '/System/Library/')) for name in dependencies):
            raise ValueError('Binary links to a library outside the macOS system runtime')
        binary_audit = {'minimum_macos': versions[0], 'dynamic_libraries': dependencies}
        qualified = bool(runtime['version']) and version_tuple(runtime['version'])[:1] == version_tuple(minimum['version'])[:1]
    return {'runtime': runtime, 'binary_audit': binary_audit, 'platform_qualified': qualified}
