"""Offline runner regressions; fixture binaries are not release evidence."""
import importlib.util
from pathlib import Path
import tempfile
import subprocess
import sys
import json
import unittest
import shutil

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('doc_examples', ROOT / 'scripts/check-doc-examples.py')


class RunnerTests(unittest.TestCase):
    def test_public_installer_workflow_cannot_access_or_push_private_website(self):
        workflow = (ROOT / '.github/workflows/website-installer-sync.yml').read_text()
        self.assertNotIn('repository: LimeTip/tapid-web', workflow)
        self.assertNotIn('TAPID_WEB_SYNC_TOKEN', workflow)
        self.assertNotIn('git push', workflow)
        self.assertNotIn('sync-website:', workflow)
        self.assertIn('sh -n scripts/install.sh', workflow)
        self.assertIn('PowerShell syntax', workflow)

    def load(self):
        self.assertTrue(Path(SPEC.origin).is_file(), 'missing executable example runner')
        module = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(module)
        return module

    def fixture_run(self, commands, body='exit 0', **kwargs):
        runner = self.load()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / 'fixture'
            binary.write_text('#!/bin/sh\nif [ "$1" = --version ]; then echo "tapid 1.2.3"; exit; fi\n' + body + '\n')
            binary.chmod(0o755)
            script = root / 'example.sh'
            script.write_text(commands)
            return runner.run_example(script, binary, kwargs.pop('expected_digest', runner.digest(binary)), kwargs.pop('expected_version', 'tapid 1.2.3'), **kwargs)

    def test_wrong_digest_rejected_before_commands(self):
        report = self.fixture_run('tapid init\n', expected_digest='0' * 64)
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(report['failure_class'], 'provenance')
        self.assertEqual(report['commands'], [])

    def test_version_mismatch_retains_observed_probe(self):
        report = self.fixture_run('tapid init\n', expected_version='tapid 9.9.9')
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(report.get('version_probe'), {
            'exit_code': 0, 'output': 'tapid 1.2.3\n', 'failure_class': None})

    def test_wrong_version_rejected_before_commands(self):
        report = self.fixture_run('tapid init\n', expected_version='tapid 9.9.9')
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(report['failure_class'], 'provenance')
        self.assertEqual(report['commands'], [])

    def test_changed_frozen_lockfile_fails(self):
        report = self.fixture_run('tapid install\ntapid install --offline --frozen\n',
                                  'printf "%s" "$*" > tapid.lock')
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(report['failure_class'], 'assertion')

    def test_path_substitution_and_shell_syntax_rejected_before_execution(self):
        for command in ['PATH=/bin tapid init', 'cd ../escape', 'cd demo extra',
                        'tapid init; true', '/bin/true', 'mkdir -p /tmp/escape']:
            with self.subTest(command=command):
                report = self.fixture_run('mkdir demo\n' + command + '\n')
                self.assertEqual(report['status'], 'failed')
                self.assertEqual(report['failure_class'], 'contract')
                self.assertEqual(report['commands'], [])

    def test_process_timeout_is_bounded_failure(self):
        report = self.fixture_run('tapid init\n', 'sleep 10', timeout=0.5)
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(report['failure_class'], 'timeout')
        self.assertIsNone(report['commands'][-1]['exit_code'])

    def test_canonical_quickstart_keeps_existing_directory_prerequisite(self):
        path = ROOT / 'docs/examples/quickstart.sh'
        self.assertTrue(path.is_file())
        self.assertEqual(path.read_text(), 'mkdir demo\ncd demo\ntapid init\ntapid i is-char\ntapid install --offline --frozen\n')

    def test_binary_replacement_is_not_silent_success(self):
        report = self.fixture_run('tapid init\n', 'printf "changed" > "$0"')
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(report['failure_class'], 'provenance')

    def test_cargo_artifact_must_be_unique_inside_dedicated_target(self):
        runner = self.load()
        self.assertTrue(hasattr(runner, 'cargo_executable'), 'missing artifact-bound Cargo selection')
        import json
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp)
            binary = target / 'tapid'
            binary.write_text('artifact')
            artifact = {'reason': 'compiler-artifact', 'target': {'name': 'tapid', 'kind': ['bin']}, 'executable': str(binary)}
            self.assertEqual(runner.cargo_executable(json.dumps(artifact), target), binary.resolve())
            artifact['executable'] = '/unrelated/tapid'
            with self.assertRaises(ValueError):
                runner.cargo_executable(json.dumps(artifact), target)
            with self.assertRaises(ValueError):
                runner.cargo_executable('', target)

    def test_missing_manifest_assertion_fails_even_when_command_exits_zero(self):
        report = self.fixture_run('tapid init\n', assertions=['manifest'])
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(report['failure_class'], 'assertion')

    def test_empty_example_cannot_pass(self):
        report = self.fixture_run('# nothing executed\n')
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(report['failure_class'], 'contract')

    def test_io_error_returns_report_instead_of_losing_evidence(self):
        runner = self.load()
        report = runner.run_example(Path('/missing-doc-example'), Path('/missing-binary'), '0' * 64, 'tapid 1.2.3')
        self.assertEqual(report['status'], 'failed')
        self.assertIn('error', report)

    def test_oversized_example_is_rejected_before_execution(self):
        report = self.fixture_run('#' + 'x' * 32768 + '\ntapid init\n')
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(report['commands'], [])

    def test_cli_unknown_example_emits_failed_report(self):
        with tempfile.TemporaryDirectory() as tmp:
            report = Path(tmp) / 'report.json'
            result = subprocess.run([sys.executable, SPEC.origin, '--lane', 'source', '--example', 'unknown', '--report', str(report)], capture_output=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(json.loads(report.read_text())['status'], 'failed')

    def test_explicit_upgrade_target_allows_only_expected_transition(self):
        import hashlib
        import shlex
        replacement = '#!/bin/sh\necho "tapid 2.0.0"\n'
        body = 'test -f "$(dirname "$0")/.tapid-managed" || exit 1\nprintf %s ' + shlex.quote(replacement) + ' > "$0"'
        report = self.fixture_run('tapid upgrade\ntapid --version\n', body,
                                  upgrade_target={'sha256': hashlib.sha256(replacement.encode()).hexdigest(), 'version': 'tapid 2.0.0'})
        self.assertEqual(report['status'], 'passed', report)
        self.assertEqual(report['binary_after']['version'], 'tapid 2.0.0')

    def test_upgrade_provenance_requires_persisted_state(self):
        report = self.fixture_run('tapid init\n', assertions=['upgrade-provenance'])
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(report['failure_class'], 'assertion')

    def test_required_ci_runs_offline_contract_and_preserves_live_separation(self):
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        self.assertIn('python3 -m unittest discover -s tests -p test_doc_examples.py', ci)
        self.assertIn('--lane source --example init --example upgrade-help', ci)
        self.assertNotIn('--allow-network', ci)

    def test_public_smoke_uses_published_binary_not_source_build(self):
        workflow = (ROOT / '.github/workflows/release-public-smoke.yml').read_text()
        self.assertTrue('--lane published' in workflow)
        self.assertTrue('--example quickstart' in workflow)
        self.assertNotIn('cargo build', workflow)

    @unittest.skipUnless(shutil.which('pwsh'), 'PowerShell runtime not installed')
    def test_native_powershell_quickstart_executes_maintained_file(self):
        runner = self.load()
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            binary = base / 'tapid.exe'
            binary.write_text('''#!/bin/sh
if [ "$1" = --version ]; then echo "tapid 1.2.3"; exit; fi
if [ "$1" = init ]; then printf '{"dependencies":{"is-char":"1"}}' > package.json; fi
if [ "$1" = i ]; then mkdir -p node_modules/is-char; printf '{"name":"is-char"}' > node_modules/is-char/package.json; printf lock > tapid.lock; fi
''')
            binary.chmod(0o755)
            report = base / 'report.json'
            result = subprocess.run(['pwsh', '-NoProfile', '-File', str(ROOT / 'scripts/check-doc-examples.ps1'), '-Binary', str(binary), '-ExpectedSha256', runner.digest(binary), '-ExpectedVersion', 'tapid 1.2.3', '-ReleaseTag', 'v1.2.3', '-ReportPath', str(report)], capture_output=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            evidence = json.loads(report.read_text())
            self.assertEqual(evidence['status'], 'passed', evidence)
            self.assertEqual(len(evidence['commands']), 5)

    @unittest.skipUnless(shutil.which('pwsh'), 'PowerShell runtime not installed')
    def test_native_powershell_runner_rejects_wrong_digest_with_report(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            binary = base / 'tapid.exe'
            binary.write_text('#!/bin/sh\necho "tapid 1.2.3"\n')
            binary.chmod(0o755)
            report = base / 'report.json'
            result = subprocess.run(['pwsh', '-NoProfile', '-File', str(ROOT / 'scripts/check-doc-examples.ps1'), '-Binary', str(binary), '-ExpectedSha256', '0' * 64, '-ExpectedVersion', 'tapid 1.2.3', '-ReleaseTag', 'v1.2.3', '-ReportPath', str(report)], capture_output=True, timeout=30)
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(report.is_file(), 'native runner must persist failure evidence')
            self.assertEqual(json.loads(report.read_text())['failure_class'], 'provenance')

    def test_released_upgrade_absence_is_an_explicit_negative_contract(self):
        report = self.fixture_run('tapid upgrade --help\n', "printf \"error: unrecognized subcommand 'upgrade'\\n\"; exit 2", expected_exit=2, expected_output="unrecognized subcommand 'upgrade'")
        self.assertEqual(report['status'], 'passed', report)
        self.assertEqual(report['commands'][0]['exit_code'], 2)

    def test_failed_cd_stops_without_implicit_directory_creation(self):
        report = self.fixture_run('cd demo\ntapid init\n')
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(len(report['commands']), 1)
        self.assertEqual(report['commands'][0]['exit_code'], 1)

    def test_missing_mkdir_does_not_get_inserted(self):
        report = self.fixture_run('tapid init demo\n', '[ -d "$2" ] || exit 1')
        self.assertEqual(report['status'], 'failed')

    def test_output_limit_stops_noisy_child(self):
        report = self.fixture_run('tapid init\n', 'while :; do printf abcdefghijklmnopqrstuvwxyz; done', output_limit=128)
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(report['failure_class'], 'output-limit')
        self.assertLessEqual(len(report['commands'][0]['output']), 128)

    def test_success_records_each_literal_command(self):
        runner = self.load()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / 'fixture'
            binary.write_text('#!/bin/sh\nif [ "$1" = --version ]; then echo "tapid 1.2.3"; exit; fi\n[ -d "$2" ] || exit 1\nprintf \'{"name":"demo"}\\n\' > "$2/package.json"\n')
            binary.chmod(0o755)
            script = root / 'init.sh'
            script.write_text('mkdir demo\ntapid init demo\ncd demo\n')
            report = runner.run_example(script, binary, runner.digest(binary), 'tapid 1.2.3')
            self.assertEqual(report['status'], 'passed')
            self.assertEqual([c['command'] for c in report['commands']], script.read_text().splitlines())
            self.assertEqual([c['exit_code'] for c in report['commands']], [0, 0, 0])


if __name__ == '__main__':
    unittest.main()
