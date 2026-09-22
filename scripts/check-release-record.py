#!/usr/bin/env python3
"""Exercise generated metadata through the real installer and updater without a network.

The supplied source-built CLI performs upgrades into a temporary installation.
Archive executables are shell fixtures, so this is contract evidence, not proof
that a published binary works on any platform.
"""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tarfile
import tempfile
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]
DISCOVERY = 'https://tapid.dev/releases/v1/latest.tsv'
TARGETS = (
    'aarch64-apple-darwin', 'aarch64-pc-windows-msvc',
    'aarch64-unknown-linux-gnu', 'x86_64-apple-darwin',
    'x86_64-pc-windows-msvc', 'x86_64-unknown-linux-gnu',
)


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def command(args, env, cwd, *, fails=False):
    result = subprocess.run(args, env=env, cwd=cwd, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60)
    require(len(result.stdout) <= 65536, 'command output exceeded 64 KiB')
    require((result.returncode != 0) == fails,
            f'unexpected exit {result.returncode}: {args[0]}\n{result.stdout}')
    return result.stdout


def fixture_executable(version):
    return f"#!/bin/sh\nprintf 'tapid {version}\\n'\n".encode()


def release(directory, version, provider, env):
    directory.mkdir()
    payload = fixture_executable(version)
    for target in TARGETS:
        filename = f'tapid-{version}-{target}.tar.gz'
        with tarfile.open(directory / filename, 'w:gz', format=tarfile.USTAR_FORMAT) as archive:
            member = tarfile.TarInfo('tapid.exe' if 'windows' in target else 'tapid')
            member.mode = 0o755
            member.size = len(payload)
            archive.addfile(member, io.BytesIO(payload))
    command(['node', '--experimental-strip-types', str(ROOT / 'tools/release/release.ts'),
             'metadata', str(directory), version, provider], env, directory)
    record = directory / 'tapid-release-v1.tsv'
    mapping = {DISCOVERY: str(record),
               f'https://tapid.dev/releases/v1/v{version}.tsv': str(record)}
    for path in directory.glob('*.tar.gz'):
        mapping[f'{provider}/{path.name}'] = str(path)
    return mapping, payload


def snapshot(directory):
    return {path.name: path.read_bytes() for path in directory.iterdir() if path.is_file()}


def check(binary):
    require(platform.system() in ('Linux', 'Darwin'), 'this integration check requires Unix')
    require(binary.is_file(), f'missing source-built CLI: {binary}')
    with tempfile.TemporaryDirectory(prefix='tapid-release-contract-') as temporary:
        directory = Path(temporary)
        transport = directory / 'transport'
        transport.mkdir()
        home = directory / 'home'
        home.mkdir()
        # No inherited TAPID configuration or credentials. The only curl in PATH
        # is this exact URL-to-local-file transport; an unknown URL fails closed.
        env = {'PATH': str(transport) + os.pathsep + os.environ['PATH'],
               'HOME': str(home), 'TMPDIR': temporary, 'LC_ALL': 'C',
               'RELEASE_FIXTURE': temporary}
        curl = transport / 'curl'
        curl.write_text(f'#!{sys.executable}\n' + '''import json, os, pathlib, sys
root = pathlib.Path(os.environ['RELEASE_FIXTURE'])
args = sys.argv[1:]
urls = [arg for arg in args if arg.startswith('https://')]
if len(urls) != 1:
    sys.exit(22)
url = urls[0]
with (root / 'requests').open('a') as log:
    log.write(url + '\\n')
mapping = json.loads((root / 'mapping.json').read_text())
if url not in mapping:
    sys.exit(22)
body = pathlib.Path(mapping[url]).read_bytes()
if '--max-filesize' in args and len(body) > int(args[args.index('--max-filesize') + 1]):
    sys.exit(63)
if '-o' in args:
    pathlib.Path(args[args.index('-o') + 1]).write_bytes(body)
else:
    sys.stdout.buffer.write(body)
''')
        curl.chmod(0o755)
        installed = directory / 'installed'
        destination = installed / 'tapid'
        mapping, first_payload = release(
            directory / 'first', '1.2.3',
            'https://gitlab.example/tapid/releases/v1.2.3/downloads', env)
        (directory / 'mapping.json').write_text(json.dumps(mapping))
        command(['sh', str(ROOT / 'scripts/install.sh'), '--install-dir', str(installed)], env, directory)
        require(destination.read_bytes() == first_payload, 'installer changed executable bytes')
        require(command([str(destination), '--version'], env, directory).strip() == 'tapid 1.2.3',
                'installer did not install the generated release')
        # Exercise explicit-version discovery through the same generated record.
        command(['sh', str(ROOT / 'scripts/install.sh'), '--version', '1.2.3',
                 '--install-dir', str(directory / 'explicit')], env, directory)
        destination.write_bytes(fixture_executable('1.2.2'))
        upgrade = [str(binary), 'upgrade', '--destination', str(destination)]
        output = command(upgrade, env, directory)
        require('Upgraded Tapid to 1.2.3' in output, 'first upgrade did not report the generated version')
        require(destination.read_bytes() == first_payload, 'updater and installer disagree on artifact bytes')
        before = snapshot(installed)
        output = command(upgrade, env, directory)
        require('Tapid 1.2.3 is already up to date' in output, 'repeat upgrade did not report a no-op')
        require(snapshot(installed) == before, 'repeat upgrade changed installation or recovery state')

        # Keep discovery fixed while changing both the release version and host.
        mapping, second_payload = release(
            directory / 'second', '1.2.4',
            'https://downloads.example.net/tapid/v1.2.4', env)
        (directory / 'mapping.json').write_text(json.dumps(mapping))
        output = command(upgrade, env, directory)
        require('Upgraded Tapid to 1.2.4' in output, 'provider migration did not upgrade')
        require(destination.read_bytes() == second_payload, 'provider migration installed incorrect bytes')
        require(command([str(destination), '--version'], env, directory).strip() == 'tapid 1.2.4',
                'migrated installation reports the wrong version')
        before = snapshot(installed)
        output = command(upgrade, env, directory)
        require('Tapid 1.2.4 is already up to date' in output, 'repeat after migration did not report a no-op')
        require(snapshot(installed) == before, 'repeat after migration changed installation state')
        state = json.loads((installed / '.tapid-release-state.json').read_text())
        require(state['verification'] == 'checksum', 'updater reported incorrect verification provenance')
        require(state['last_known_good']['version'] == '1.2.4', 'recovery state did not follow migration')
        digest = state['last_known_good']['artifact_sha256']
        cache = installed / ('.tapid-release-artifact-' + digest)
        require(cache.is_file() and hashlib.sha256(cache.read_bytes()).hexdigest() == digest,
                'no valid cached release available for the fail-closed check')

        record = directory / 'second/tapid-release-v1.tsv'
        record.write_text(record.read_text().replace('tapid-release-v1', 'tapid-release-v99', 1))
        output = command(upgrade, env, directory, fails=True)
        require('invalid tapid-release-v1 record' in output, 'malformed metadata failed for an unrelated reason')
        require(snapshot(installed) == before, 'rejected metadata changed installation or recovery state')
        requests = (directory / 'requests').read_text().splitlines()
        require(requests.count(DISCOVERY) == 6, 'installer/updater did not consistently use owned discovery')
        require('https://tapid.dev/releases/v1/v1.2.3.tsv' in requests, 'explicit installer bypassed versioned discovery')
        require(any(url.startswith('https://gitlab.example/') for url in requests), 'first provider was unused')
        require(any(url.startswith('https://downloads.example.net/') for url in requests), 'second provider was unused')
        require(not any(urlsplit(url).hostname == 'github.com' for url in requests),
                'unexpected GitHub fallback')
        print('Generated release record passed installer, upgrade, repeat, provider migration, and cached rejection checks.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True, help='source-built tapid executable')
    args = parser.parse_args()
    check(args.binary.resolve())


if __name__ == '__main__':
    main()
