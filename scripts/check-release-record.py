#!/usr/bin/env python3
"""Exercise generated metadata through the real updater without a network.

The supplied source-built CLI performs upgrades into a temporary installation.
Archive executables are shell fixtures, so this is contract evidence, not proof
that a published binary works on any platform.
"""
import argparse
import base64
from datetime import datetime, timedelta, timezone
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


def ed25519_sign(message, secret):
    """Small dependency-free Ed25519 signer for the offline fixture."""
    q = 2 ** 255 - 19
    l = 2 ** 252 + 27742317777372353535851937790883648493
    d = (-121665 * pow(121666, q - 2, q)) % q
    i = pow(2, (q - 1) // 4, q)
    by = (4 * pow(5, q - 2, q)) % q

    def add(p, r):
        x1, y1 = p
        x2, y2 = r
        t = d * x1 * x2 * y1 * y2 % q
        return ((x1 * y2 + x2 * y1) * pow(1 + t, q - 2, q) % q,
                (y1 * y2 + x1 * x2) * pow(1 - t, q - 2, q) % q)

    def multiply(p, n):
        result = (0, 1)
        while n:
            if n & 1:
                result = add(result, p)
            p = add(p, p)
            n >>= 1
        return result

    def encode(p):
        x, y = p
        return (y | ((x & 1) << 255)).to_bytes(32, 'little')

    h = hashlib.sha512(secret).digest()
    scalar = int.from_bytes(h[:32], 'little')
    scalar &= (1 << 254) - 8
    scalar |= 1 << 254
    base = (pow((by * by - 1) * pow(d * by * by + 1, q - 2, q) % q,
                (q + 3) // 8, q), by)
    if (base[0] * base[0] - (by * by - 1) *
            pow(d * by * by + 1, q - 2, q)) % q:
        base = ((base[0] * i) % q, by)
    if base[0] & 1:
        base = (q - base[0], by)
    public = encode(multiply(base, scalar))
    nonce = int.from_bytes(hashlib.sha512(h[32:] + message).digest(), 'little') % l
    r = encode(multiply(base, nonce))
    challenge = int.from_bytes(hashlib.sha512(r + public + message).digest(), 'little') % l
    return r + ((nonce + challenge * scalar) % l).to_bytes(32, 'little'), public


def sign_record(record, signature, keyring):
    """Create the same signed envelope shape used by the Rust test fixtures."""
    secret = bytes([7]) * 32
    record_bytes = record.read_bytes()
    _, public_key = ed25519_sign(b'', secret)
    keyring.write_text(json.dumps({
        'version': 'tapid-release-keyring-v1',
        'keys': [{
            'key_id': 'test-release-key',
            'algorithm': 'ed25519',
            'public_key': base64.b64encode(public_key).decode(),
            'fingerprint': 'sha256-' + hashlib.sha256(public_key).hexdigest(),
        }],
    }))
    now = datetime.now(timezone.utc)
    claims = {
        'schema': 'tapid-release-v1-signature',
        'created_at': (now - timedelta(minutes=1)).isoformat().replace('+00:00', 'Z'),
        'expires_at': (now + timedelta(days=1)).isoformat().replace('+00:00', 'Z'),
    }
    digest = 'sha256-' + hashlib.sha256(record_bytes).hexdigest()
    envelope = {
        'version': 'tapid-trust-envelope-v1',
        'subject': 'tapid-release-v1',
        'artifact_digest': digest,
        'claims': claims,
    }
    signing_payload = dict(envelope)
    signing_payload['signature_context'] = {
        'algorithm': 'ed25519', 'key_id': 'test-release-key'}
    payload = record.parent / '.signature-payload.json'
    raw_signature = record.parent / '.signature.raw'
    payload.write_text(json.dumps(signing_payload, ensure_ascii=False,
                                  separators=(',', ':'), sort_keys=True))
    raw_signature.write_bytes(ed25519_sign(payload.read_bytes(), secret)[0])
    envelope['signature'] = {
        'algorithm': 'ed25519',
        'key_id': 'test-release-key',
        'subject': envelope['subject'],
        'artifact_digest': digest,
        'value': base64.b64encode(raw_signature.read_bytes()).decode(),
    }
    signature.write_text(json.dumps(envelope, separators=(',', ':')))


def release(directory, version, provider, env, keyring):
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
    sign_record(record, directory / 'tapid-release-v1.tsv.sig', keyring)
    mapping = {DISCOVERY: str(record),
               DISCOVERY + '.sig': str(directory / 'tapid-release-v1.tsv.sig'),
               f'https://tapid.dev/releases/v1/v{version}.tsv': str(record),
               f'https://tapid.dev/releases/v1/v{version}.tsv.sig': str(directory / 'tapid-release-v1.tsv.sig')}

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
        installed.mkdir()
        destination = installed / 'tapid'
        keyring = directory / 'keyring.json'
        mapping, first_payload = release(
            directory / 'first', '1.2.3',
            'https://gitlab.example/tapid/releases/v1.2.3/downloads', env, keyring)
        (directory / 'mapping.json').write_text(json.dumps(mapping))
        command(['sh', str(ROOT / 'scripts/install.sh'), '--install-dir', str(installed)], env, directory)
        require(destination.read_bytes() == first_payload, 'installer changed executable bytes')
        require(command([str(destination), '--version'], env, directory).strip() == 'tapid 1.2.3',
                'installer did not install the generated release')
        # Exercise explicit-version discovery through the same generated record.
        command(['sh', str(ROOT / 'scripts/install.sh'), '--version', '1.2.3',
                 '--install-dir', str(directory / 'explicit')], env, directory)
        env['TAPID_RELEASE_KEYRING'] = str(keyring)
        destination.write_bytes(fixture_executable('1.2.2'))
        (installed / '.tapid-managed').write_text('tapid-managed-v1\n')
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
            'https://downloads.example.net/tapid/v1.2.4', env, keyring)
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
        require(state['verification'] == 'signature', 'updater reported incorrect verification provenance')
        require(state['last_known_good']['version'] == '1.2.4', 'recovery state did not follow migration')
        digest = state['last_known_good']['artifact_sha256']
        cache = installed / ('.tapid-release-artifact-' + digest)
        require(cache.is_file() and hashlib.sha256(cache.read_bytes()).hexdigest() == digest,
                'no valid cached release available for the fail-closed check')

        record = directory / 'second/tapid-release-v1.tsv'
        record.write_text(record.read_text().replace('tapid-release-v1', 'tapid-release-v99', 1))
        sign_record(record, directory / 'second/tapid-release-v1.tsv.sig', keyring)
        output = command(upgrade, env, directory, fails=True)
        require('invalid tapid-release-v1 record' in output, 'malformed metadata failed for an unrelated reason')
        require(snapshot(installed) == before, 'rejected metadata changed installation or recovery state')
        requests = (directory / 'requests').read_text().splitlines()
        require(requests.count(DISCOVERY) == 6, 'updater did not consistently use owned discovery')
        require(requests.count(DISCOVERY + '.sig') == 5, 'updater did not consistently fetch record signatures')
        require(any(url.startswith('https://gitlab.example/') for url in requests), 'first provider was unused')
        require(any(url.startswith('https://downloads.example.net/') for url in requests), 'second provider was unused')
        require(not any(urlsplit(url).hostname == 'github.com' for url in requests),
                'unexpected GitHub fallback')
        print('Generated signed release record passed upgrade, repeat, provider migration, and cached rejection checks.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True, help='source-built tapid executable')
    args = parser.parse_args()
    check(args.binary.resolve())


if __name__ == '__main__':
    main()
