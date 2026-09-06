#!/usr/bin/env python3
"""Execute the small, literal Unix documentation command vocabulary."""
import argparse
import hashlib
import platform
import sys
import tomllib
import json
import os
import re
from pathlib import Path
import shlex
import shutil
import subprocess
import selectors
import signal
import time
import tempfile


def cargo_executable(output, target):
    """Use Cargo artifact messages, never a conventional/stale target path."""
    artifacts = set()
    for line in output.splitlines():
        if not line.startswith('{'):
            continue
        item = json.loads(line)
        if (item.get('reason') == 'compiler-artifact' and
                item.get('target', {}).get('name') == 'tapid' and
                'bin' in item.get('target', {}).get('kind', []) and item.get('executable')):
            path = Path(item['executable']).resolve()
            if not path.is_relative_to(Path(target).resolve()) or not path.is_file():
                raise ValueError('Cargo executable is outside dedicated target or missing')
            artifacts.add(path)
    if len(artifacts) != 1:
        raise ValueError('Cargo did not identify exactly one Tapid executable')
    return artifacts.pop()


def read_bounded(path, limit=1048576):
    with open(path, 'rb') as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError('file exceeds size limit: ' + str(path))
    return data


def digest(path):
    with open(path, 'rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def bounded_process(args, *, cwd, env, timeout=120, output_limit=65536):
    """Bound combined output and process-group lifetime (Unix, not a sandbox)."""
    chunks = bytearray()
    failure = None
    with subprocess.Popen(args, cwd=cwd, env=env, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT, start_new_session=True) as child:
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(child.stdout, selectors.EVENT_READ)
                deadline = time.monotonic() + timeout
                while selector.get_map():
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        failure = 'timeout'
                        break
                    for key, _ in selector.select(min(remaining, 0.1)):
                        data = os.read(key.fd, min(8192, output_limit + 1 - len(chunks)))
                        if not data:
                            selector.unregister(key.fileobj)
                            continue
                        chunks.extend(data)
                        if len(chunks) > output_limit:
                            failure = 'output-limit'
                            break
                    if failure:
                        break
                if not failure:
                    try:
                        child.wait(timeout=max(0.001, deadline - time.monotonic()))
                    except subprocess.TimeoutExpired:
                        failure = 'timeout'
        finally:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait(timeout=5)
    return {'exit_code': None if failure else child.returncode,
            'output': bytes(chunks[:output_limit]).decode('utf-8', errors='replace'),
            'failure_class': failure}


def run_example(script, binary, expected_digest, expected_version, **options):
    report = {'schema_version': 1, 'example': Path(script).stem, 'status': 'failed',
              'commands': [], 'assertions': []}
    try:
        return execute_example(report, script, binary, expected_digest, expected_version, **options)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        report.update(status='failed', failure_class='execution', error=str(error)[:2000])
        return report


def execute_example(report, script, binary, expected_digest, expected_version, *, timeout=120, output_limit=65536, assertions=(), upgrade_target=None, expected_exit=0, expected_output=None):
    report.update(status='passed')
    text = read_bounded(script, 32768).decode('utf-8')
    if '\r' in text or '\0' in text:
        raise ValueError('examples require LF text without NUL')
    report['script_sha256'] = digest(script)
    commands = [line for line in text.splitlines() if line.strip() and not line.startswith('#')]
    allowed = re.compile(r'(?:mkdir [a-z][a-z0-9-]*|cd [a-z][a-z0-9-]*|tapid (?:init(?: [a-z][a-z0-9-]*)?|(?:i|install)(?: is-char)?(?: --offline)?(?: --frozen)?|--version|upgrade(?: --dry-run| --help)?))')
    if not commands or len(commands) > 16 or any(not allowed.fullmatch(line) for line in commands):
        report.update(status='failed', failure_class='contract', error='unsupported command vocabulary')
        return report
    if expected_exit and commands != ['tapid upgrade --help']:
        raise ValueError('negative outcome only supported for upgrade capability probe')
    if 'tapid upgrade' in commands and not upgrade_target:
        raise ValueError('upgrade requires an explicit expected target digest and version')
    with tempfile.TemporaryDirectory(prefix='tapid-doc-example-') as tmp:
        root = Path(tmp)
        home = root / 'home'
        home.mkdir()
        bindir = root / 'bin'
        bindir.mkdir()
        installed = bindir / 'tapid'
        shutil.copy2(binary, installed)
        if upgrade_target:
            # This copy is owned by the disposable harness, never the user's install.
            (bindir / '.tapid-managed').write_text('tapid-managed-v1\n')
        if digest(installed) != expected_digest:
            report.update(status='failed', failure_class='provenance', error='binary digest mismatch')
            return report
        env = {'HOME': str(home), 'PATH': f'{bindir}:/usr/bin:/bin',
               'XDG_CACHE_HOME': str(home / '.cache'), 'TMPDIR': tmp,
               'LANG': 'C', 'SHELL': '/bin/sh'}
        version = bounded_process([str(installed), '--version'], cwd=root, env=env, timeout=10, output_limit=output_limit)
        report['version_probe'] = version
        if version['failure_class'] or version['exit_code'] or version['output'].strip() != expected_version:
            report.update(status='failed', failure_class='provenance', error='binary version mismatch')
            return report
        cwd = root / 'project'
        cwd.mkdir()
        for command in commands:
            if not command.strip() or command.startswith('#'):
                continue
            args = shlex.split(command)
            frozen = args[0] == 'tapid' and '--frozen' in args
            lock = cwd / 'tapid.lock'
            before = read_bounded(lock, 8388608) if frozen and lock.is_file() else None
            if args[0] == 'cd':
                target = cwd / args[1]
                code = 0 if target.is_dir() else 1
                output = ''
                if code == 0:
                    cwd = target
            else:
                result = bounded_process(args, cwd=cwd, env=env, timeout=timeout, output_limit=output_limit)
                code, output = result['exit_code'], result['output']
                if result['failure_class']:
                    report['commands'].append({'command': command, **result})
                    report.update(status='failed', failure_class=result['failure_class'])
                    break
            report['commands'].append({'command': command, 'exit_code': code, 'output': output})
            if command == 'tapid upgrade' and code == 0:
                expected_digest = upgrade_target['sha256']
                expected_version = upgrade_target['version']
                if digest(installed) != expected_digest:
                    report.update(status='failed', failure_class='provenance', error='upgrade target digest mismatch')
                    break
                after = bounded_process([str(installed), '--version'], cwd=cwd, env=env, timeout=timeout, output_limit=output_limit)
                if after['failure_class'] or after['exit_code'] or after['output'].strip() != expected_version:
                    report.update(status='failed', failure_class='provenance', error='upgrade target version mismatch')
                    break
                report['binary_after'] = {'sha256': digest(installed), 'version': after['output'].strip()}
            if digest(installed) != expected_digest or Path(shutil.which('tapid', path=env['PATH']) or '').resolve() != installed.resolve():
                report.update(status='failed', failure_class='provenance', error='executable changed')
                break
            if code != expected_exit or (expected_output and expected_output not in output):
                report.update(status='failed', failure_class='command')
                break
            if frozen and (before is None or not lock.is_file() or read_bounded(lock, 8388608) != before):
                report.update(status='failed', failure_class='assertion', error='frozen lockfile changed or missing')
                break
        if report['status'] == 'passed':
            for assertion in assertions:
                if assertion == 'manifest':
                    passed = (cwd / 'package.json').is_file()
                elif assertion == 'is-char-installed':
                    manifest = json.loads(read_bounded(cwd / 'package.json'))
                    package = json.loads(read_bounded(cwd / 'node_modules/is-char/package.json'))
                    passed = ('is-char' in manifest.get('dependencies', {}) and
                              package.get('name') == 'is-char' and (cwd / 'tapid.lock').is_file())
                elif assertion == 'upgrade-provenance':
                    state_path = bindir / '.tapid-release-state.json'
                    state = json.loads(read_bounded(state_path)) if state_path.is_file() else {}
                    report['upgrade_state'] = state
                    passed = (state.get('schema') == 'tapid-release-state-v2' and
                              state.get('verification') in ['signature', 'checksum'] and
                              state.get('last_known_good', {}).get('version') == expected_version.removeprefix('tapid ') and
                              bool(re.fullmatch('[a-f0-9]{64}', state.get('last_known_good', {}).get('artifact_sha256', ''))))
                elif assertion == 'frozen-lockfile-unchanged':
                    passed = any('--frozen' in item['command'] and item['exit_code'] == 0 for item in report['commands'])
                else:
                    raise ValueError('unknown assertion: ' + assertion)
                report['assertions'].append({'id': assertion, 'passed': passed})
                if not passed:
                    report.update(status='failed', failure_class='assertion', error=assertion)
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--lane', choices=['source', 'published'], required=True)
    parser.add_argument('--example', action='append', required=True)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--expected-sha256')
    parser.add_argument('--expected-version', help='Exact output, e.g. tapid 0.0.9')
    parser.add_argument('--release-tag')
    parser.add_argument('--release-source-sha')
    parser.add_argument('--allow-network', action='store_true')
    parser.add_argument('--upgrade-target-sha256')
    parser.add_argument('--upgrade-target-version')
    args = parser.parse_args(argv)
    root = Path(__file__).resolve().parents[1]
    report = {'schema_version': 1, 'lane': args.lane, 'source_sha': None,
              'release_tag': args.release_tag, 'release_source_sha': args.release_source_sha, 'platform': sys.platform,
              'machine': platform.machine(), 'status': 'failed', 'examples': []}
    try:
        inventory = json.loads(read_bounded(root / 'docs/examples/contracts.json'))
        if inventory['schema_version'] != 1:
            raise ValueError('unsupported contract schema')
        entries = {entry['id']: entry for entry in inventory['examples']}
        if len(entries) != len(inventory['examples']):
            raise ValueError('duplicate example ID')
        upgrade_target = None
        if args.upgrade_target_sha256 or args.upgrade_target_version:
            if not re.fullmatch('[a-f0-9]{64}', args.upgrade_target_sha256 or '') or not re.fullmatch(r'tapid \d+\.\d+\.\d+', args.upgrade_target_version or ''):
                raise ValueError('upgrade target requires exact digest and version')
            upgrade_target = {'sha256': args.upgrade_target_sha256, 'version': args.upgrade_target_version}
        selected = []
        for name in args.example:
            if name not in entries:
                raise ValueError('unknown example: ' + name)
            entry = entries[name]
            if args.lane not in entry['lanes'] or sys.platform not in entry['platforms']:
                raise ValueError('example unavailable for lane/platform: ' + name)
            if entry['network'] and not args.allow_network:
                raise ValueError('network example requires --allow-network: ' + name)
            path = (root / entry['file']).resolve()
            if not path.is_relative_to(root / 'docs/examples') or path.suffix != '.sh':
                raise ValueError('invalid example path')
            selected.append((entry, path))
        source = bounded_process(['git', 'rev-parse', 'HEAD'], cwd=root, env=os.environ, timeout=10)
        if source['failure_class'] or source['exit_code']:
            raise ValueError('cannot identify source revision')
        report['source_sha'] = source['output'].strip()
        dirty = bounded_process(['git', 'status', '--porcelain', '--untracked-files=all'], cwd=root, env=os.environ, timeout=10)
        if dirty['failure_class'] or dirty['exit_code']:
            raise ValueError('cannot identify source tree state')
        report['source_dirty'] = bool(dirty['output'])
        with tempfile.TemporaryDirectory(prefix='tapid-doc-build-') as target:
            if args.lane == 'source':
                if args.binary or args.expected_sha256 or args.expected_version:
                    raise ValueError('source lane builds its own artifact; binary overrides prohibited')
                build = bounded_process(['cargo', 'build', '--locked', '--bin', 'tapid',
                                         '--message-format=json-render-diagnostics', '--target-dir', target],
                                        cwd=root, env=os.environ, timeout=600, output_limit=8388608)
                if build['failure_class'] or build['exit_code']:
                    report['build'] = build
                    raise ValueError('source build failed; no fallback executable')
                binary = cargo_executable(build['output'], target)
                expected_digest = digest(binary)
                manifest = tomllib.loads(read_bounded(root / 'crates/tapid-cli/Cargo.toml').decode())
                expected_version = 'tapid ' + manifest['package']['version']
            else:
                if not args.binary or not re.fullmatch('[a-f0-9]{64}', args.expected_sha256 or ''):
                    raise ValueError('published lane requires binary and expected SHA-256')
                if not re.fullmatch(r'v\d+\.\d+\.\d+', args.release_tag or ''):
                    raise ValueError('published lane requires an exact stable release tag')
                if args.expected_version != 'tapid ' + args.release_tag[1:]:
                    raise ValueError('release tag and expected version disagree')
                binary = args.binary.resolve(strict=True)
                expected_digest = args.expected_sha256
                expected_version = args.expected_version
            report['binary'] = {'path': str(binary), 'sha256': expected_digest, 'version': expected_version}
            for entry, script in selected:
                outcome = {'exit_code': 0}
                if args.lane == 'published' and 'release_expectations' in entry:
                    if args.release_tag not in entry['release_expectations']:
                        raise ValueError('release capability expectation needs review: ' + args.release_tag)
                    outcome = entry['release_expectations'][args.release_tag]
                result = run_example(script, binary, expected_digest, expected_version,
                                     assertions=entry['assertions'], upgrade_target=upgrade_target,
                                     expected_exit=outcome['exit_code'], expected_output=outcome.get('output_contains'))
                report['examples'].append(result)
                if result['status'] != 'passed':
                    break
            report['status'] = 'passed' if (len(report['examples']) == len(selected) and
                                           all(e['status'] == 'passed' for e in report['examples'])) else 'failed'
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        report.update(status='failed', failure_class='execution', error=str(error)[:2000])
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'status': report['status'], 'report': str(args.report)}))
    return 0 if report['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
