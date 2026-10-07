#!/usr/bin/env python3
"""TEMPORARY Stage 6 Cargo-install seam proof, not remote installer acceptance.
Remove this script, its tests and CI steps after retaining native receipts.
Adapted from plans/tapid-ci-evidence/stage6-freshness/prove_freshness.py.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import time
import tomllib

LICENSE = 'crates/tapid-cli/src/commands/license.rs'


def mark_license(original, marker):
    if not re.fullmatch(r'TAPID_[A-Z0-9_]+', marker):
        raise ValueError('invalid marker')
    needle = b'Copyright 2026 LimeTip AB.\\n\\n'
    if original.count(needle) != 1:
        raise ValueError('missing or ambiguous license marker anchor')
    return original.replace(needle, marker.encode('ascii') + b'\\n' + needle)


def validate_source(source):
    for name in ('Cargo.toml', 'Cargo.lock', 'crates/tapid-cli/Cargo.toml', LICENSE):
        path = source / name
        if not path.is_file() or path.is_symlink():
            raise ValueError('invalid source: ' + name)
    manifest = tomllib.loads((source / 'crates/tapid-cli/Cargo.toml').read_text())
    if manifest['package']['name'] != 'tapid':
        raise ValueError('unexpected package')
    return manifest['package']['version']


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def snapshot(source):
    return {p.relative_to(source).as_posix(): sha(p)
            for p in sorted(source.rglob('*')) if p.is_file()}


COMMAND_TIMEOUT = 900
PROBE_TIMEOUT = 30
CLEANUP_TIMEOUT = 5


def execute(command, cwd, env, stdout, stderr, timeout):
    """Finite wait with native tree termination; output goes to files, not pipes."""
    proc = subprocess.Popen([str(c) for c in command], cwd=cwd, env=env,
                            stdout=stdout, stderr=stderr,
                            start_new_session=os.name != 'nt')
    try:
        code = proc.wait(timeout=timeout)
    except BaseException:
        try:
            if os.name == 'nt':
                # taskkill walks descendants while the direct parent still exists.
                try:
                    subprocess.run(['taskkill', '/PID', str(proc.pid), '/T', '/F'],
                                   stdout=stderr, stderr=stderr, timeout=CLEANUP_TIMEOUT, check=True)
                except BaseException:
                    # Reap the direct process even when tree cleanup fails; never
                    # suppress that failure or publish a successful proof receipt.
                    proc.kill()
                    raise
            else:
                import signal
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        finally:
            proc.wait(timeout=CLEANUP_TIMEOUT)
        raise
    if code:
        raise subprocess.CalledProcessError(code, command)
    return code


def run(command, log, cwd, env):
    start = time.monotonic()
    with log.open('w', encoding='utf-8') as output:
        output.write('$ ' + subprocess.list2cmdline([str(c) for c in command]) + '\n')
        output.flush()
        try:
            code = execute(command, cwd, env, output, output, COMMAND_TIMEOUT)
        except BaseException as error:
            output.write(f'\nFAILURE={type(error).__name__}: {error}\n')
            if isinstance(error, subprocess.CalledProcessError):
                output.write(f'EXIT_STATUS={error.returncode}\n')
            raise
        else:
            output.write(f'\nEXIT_STATUS={code}\n')
        finally:
            output.write(f'ELAPSED_SECONDS={time.monotonic()-start:.3f}\n')
    return log.read_text(encoding='utf-8', errors='replace')


def capture(command, cwd=None, env=None, text=False, stdout_path=None, stderr_path=None):
    # Files avoid an inherited pipe keeping error collection alive after timeout.
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
        failed = True
        try:
            execute(command, cwd, env, output, errors, PROBE_TIMEOUT)
            failed = False
        except (subprocess.TimeoutExpired, subprocess.CalledProcessError) as error:
            output.seek(0)
            errors.seek(0)
            error.output = output.read(65536)
            error.stderr = errors.read(65536)
            raise
        finally:
            if stdout_path is not None:
                output.seek(0)
                with stdout_path.open('wb') as destination:
                    if failed:
                        destination.write(output.read(65536))
                    else:
                        shutil.copyfileobj(output, destination)
            if stderr_path is not None:
                errors.seek(0)
                with stderr_path.open('wb') as destination:
                    if failed:
                        destination.write(errors.read(65536))
                    else:
                        shutil.copyfileobj(errors, destination)
        output.seek(0)
        result = output.read()
    return result.decode() if text else result


def assert_changed(version_a, version_b, output_a, output_b, digest_a, digest_b, marker):
    assert version_a == version_b, 'version changed'
    assert marker not in output_a, 'baseline contaminated'
    assert marker in output_b, 'changed-source marker missing'
    assert digest_a != digest_b, 'stale executable digest'


def prove(repo, evidence):
    # Exclusive evidence creation prevents stale success receipts on failed reruns.
    evidence.mkdir(parents=True, exist_ok=False)
    head = capture(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip()
    before = capture(['git', 'status', '--porcelain=v1'], cwd=repo)
    tools = {name: capture(['rustup', 'which', name], text=True).strip()
             for name in ('cargo', 'rustc', 'rustdoc')}
    cargo_home = Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo'))
    marker = 'TAPID_FRESHNESS_' + head.upper() + '_' + os.environ.get('GITHUB_RUN_ATTEMPT', 'LOCAL')
    receipt = dict(head=head, marker=marker, platform=platform.platform(), machine=platform.machine(),
                   repository=os.environ.get('GITHUB_REPOSITORY'), run_id=os.environ.get('GITHUB_RUN_ID'),
                   run_attempt=os.environ.get('GITHUB_RUN_ATTEMPT'), tools=tools, assertions='NOT_COMPLETED')
    (evidence / 'identity.json').write_text(json.dumps(receipt, indent=2))
    # New private target: never write to checkout target/debug or Windows handoff bytes.
    with tempfile.TemporaryDirectory(prefix='tapid-freshness-') as directory:
        work = Path(directory)
        source = work / 'source'
        source.mkdir()
        for name in ('home', 'tmp', 'cargo-home'):
            (work / name).mkdir()
        env = os.environ.copy()
        env.update(HOME=str(work / 'home'), USERPROFILE=str(work / 'home'),
                   XDG_CACHE_HOME=str(work / 'home'), LOCALAPPDATA=str(work / 'home'),
                   TMPDIR=str(work / 'tmp'), TEMP=str(work / 'tmp'), TMP=str(work / 'tmp'),
                   CARGO_HOME=str(work / 'cargo-home'), CARGO_TARGET_DIR=str(work / 'target'),
                   CARGO_NET_OFFLINE='true', RUSTC=tools['rustc'], RUSTDOC=tools['rustdoc'],
                   CARGO_TERM_COLOR='never')
        for name in ('registry', 'git'):
            if (cargo_home / name).exists():
                shutil.copytree(cargo_home / name, work / 'cargo-home' / name)
        # Only trusted checkout HEAD is archived; no clone, remote ref or downloaded code.
        archive = evidence / 'source.tar'
        with archive.open('wb') as output, (evidence / 'archive.stderr').open('wb') as errors:
            execute(['git', 'archive', '--format=tar', head], repo, None, output, errors, PROBE_TIMEOUT)
        with tarfile.open(archive) as contents:
            for member in contents.getmembers():
                path = Path(member.name)
                if path.is_absolute() or '..' in path.parts or not (member.isfile() or member.isdir()):
                    raise ValueError('unsupported archive member: ' + member.name)
            contents.extractall(source, filter='data')
        version = validate_source(source)
        a = snapshot(source)
        run([tools['rustc'], '-Vv'], evidence / 'rustc.log', source, env)
        run([tools['cargo'], '--version'], evidence / 'cargo.log', source, env)
        binaries = []
        outputs = []
        versions = []
        digests = []
        dependencies = None
        for label in ('A', 'B'):
            root = work / ('root-' + label)
            assert not root.exists(), 'install root is not fresh'
            log = run([tools['cargo'], 'install', '--path', source / 'crates/tapid-cli',
                       '--locked', '--root', root], evidence / ('install-' + label + '.log'), source, env)
            binary = root / 'bin' / ('tapid.exe' if os.name == 'nt' else 'tapid')
            binaries.append(binary)
            # Separate output capture keeps assertion receipts free of command prefixes.
            def output(argument):
                return capture([str(binary), argument], cwd=work, env=env, text=True,
                               stdout_path=evidence / (label + '-' + argument.strip('-') + '.stdout'),
                               stderr_path=evidence / (label + '-' + argument.strip('-') + '.stderr'))
            versions.append(output('--version'))
            outputs.append(output('license'))
            digests.append(sha(binary))
            assert versions[-1].strip() == 'tapid ' + version
            current_deps = {p.name: sha(p) for p in (work / 'target/release/deps').iterdir()
                            if p.is_file() and p.suffix in ('.rlib', '.rmeta') and not p.name.startswith('libtapid')}
            if label == 'A':
                assert marker not in outputs[0]
                dependencies = current_deps
                original = (source / LICENSE).read_bytes()
                (source / LICENSE).write_bytes(mark_license(original, marker))
                b = snapshot(source)
                assert set(a) == set(b)
                assert [name for name in a if a[name] != b[name]] == [LICENSE]
                (evidence / 'source-A.json').write_text(json.dumps(a, sort_keys=True))
                (evidence / 'source-B.json').write_text(json.dumps(b, sort_keys=True))
            else:
                assert snapshot(source) == b, 'install mutated source'
                assert dependencies and current_deps == dependencies, 'dependency bytes changed'
                compiled = re.findall(r'^\s*Compiling (\S+) v', log, re.MULTILINE)
                assert compiled == ['tapid'], 'expected only Tapid compilation: ' + repr(compiled)
        assert_changed(*versions, *outputs, *digests, marker)
        old_output = capture([str(binaries[0]), 'license'], cwd=work, env=env, text=True)
        assert old_output == outputs[0] and sha(binaries[0]) == digests[0]
        for label, binary in zip(('A', 'B'), binaries):
            run([tools['cargo'], 'uninstall', 'tapid', '--root', binary.parent.parent],
                evidence / ('uninstall-' + label + '.log'), source, env)
            assert not binary.exists()
        receipt.update(version=version, binary_digests=digests, dependency_artifacts=len(dependencies),
                       source_archive_sha256=sha(archive), only_changed_file=LICENSE, assertions='PASS')
    assert not work.exists(), 'temporary experiment cleanup failed'
    assert capture(['git', 'status', '--porcelain=v1'], cwd=repo) == before
    assert capture(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip() == head
    receipt['cleanup'] = 'PASS'
    (evidence / 'results.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence', required=True, type=Path)
    args = parser.parse_args()
    prove(Path(__file__).resolve().parents[1], args.evidence.resolve())
