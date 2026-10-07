"""Temporary Stage 6 proof regressions; remove with the pilot."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'scripts/ci-freshness-pilot.py'


class PilotTests(unittest.TestCase):
    def load(self):
        self.assertTrue(SCRIPT.exists(), 'freshness pilot helper is missing')
        spec = importlib.util.spec_from_file_location('pilot', SCRIPT)
        module = importlib.util.module_from_spec(spec)
        assert spec is not None and spec.loader is not None
        spec.loader.exec_module(module)
        return module

    def test_patch_changes_only_license_literal(self):
        module = self.load()
        original = b'prefix\n.write_all(b"Copyright 2026 LimeTip AB.\\n\\n")\nsuffix\n'
        changed = module.mark_license(original, 'TAPID_FRESHNESS_TEST')
        self.assertEqual(changed.replace(b'TAPID_FRESHNESS_TEST\\n', b''), original)
        self.assertNotEqual(changed, original)


    def test_invalid_marker_rejected(self):
        with self.assertRaises(ValueError):
            self.load().mark_license(b'Copyright 2026 LimeTip AB.\\n\\n', 'unsafe"')

    def test_missing_anchor_rejected(self):
        with self.assertRaises(ValueError):
            self.load().mark_license(b'no anchor', 'TAPID_TEST')

    def test_nonzero_cargo_cannot_pass(self):
        import os
        import sys
        with tempfile.TemporaryDirectory() as root:
            log = Path(root) / 'cargo.log'
            with self.assertRaises(subprocess.CalledProcessError):
                self.load().run([sys.executable, '-c', 'raise SystemExit(7)'], log, Path(root), os.environ.copy())
            self.assertIn('EXIT_STATUS=7', log.read_text())

    def test_invalid_source_rejected_before_cargo(self):
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaises(ValueError):
                self.load().validate_source(Path(root))

    def test_digest_closes_file_before_cleanup(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / 'bytes'
            path.write_bytes(b'test')
            from unittest.mock import Mock
            stream = path.open('rb')
            source = Mock()
            source.open.return_value = stream
            try:
                self.load().sha(source)
                self.assertTrue(stream.closed)
            finally:
                stream.close()

    def test_failed_install_has_no_success_receipt_and_cleans_home(self):
        from unittest.mock import patch
        module = self.load()
        seen = []
        def fail_cargo(command, log, cwd, env):
            if 'install' in command:
                seen.append(Path(env['HOME']))
                raise subprocess.CalledProcessError(9, command)
            return ''
        with tempfile.TemporaryDirectory() as root:
            evidence = Path(root) / 'evidence'
            with patch.object(module.shutil, 'copytree'), patch.object(module, 'run', side_effect=fail_cargo):
                with self.assertRaises(subprocess.CalledProcessError):
                    module.prove(SCRIPT.parents[1], evidence)
            self.assertFalse((evidence / 'results.json').exists())
            self.assertIn('NOT_COMPLETED', (evidence / 'identity.json').read_text())
            self.assertEqual(len(seen), 1)
            self.assertFalse(seen[0].exists())

    def test_timeout_kills_real_descendant(self):
        import os
        import sys
        import time
        from unittest.mock import patch
        module = self.load()
        real_run = subprocess.run
        def alive(pid):
            if os.name == 'nt':
                import ctypes
                kernel = ctypes.WinDLL('kernel32', use_last_error=True)
                kernel.OpenProcess.restype = ctypes.c_void_p
                handle = kernel.OpenProcess(0x100000, False, pid)
                if not handle:
                    return False
                try:
                    kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
                    return kernel.WaitForSingleObject(handle, 0) == 258
                finally:
                    kernel.CloseHandle.argtypes = [ctypes.c_void_p]
                    kernel.CloseHandle(handle)
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return False
            # An exited orphan may remain a zombie until the host reaps it.
            stat = Path('/proc') / str(pid) / 'stat'
            return not (stat.exists() and stat.read_text().split(') ')[1].startswith('Z'))
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            pidfile = root / 'child.pid'
            held = root / 'held'
            child = ('import os,time; from pathlib import Path; '
                     f'f=open({str(held)!r}, "w"); '
                     f'Path({str(pidfile)!r}).write_text(str(os.getpid())); '
                     'time.sleep(20)')
            parent = ('import subprocess,sys,time; '
                      f'subprocess.Popen([sys.executable,"-c",{child!r}]); time.sleep(20)')
            pid = None
            try:
                with patch.object(module, 'COMMAND_TIMEOUT', 0.8):
                    with self.assertRaises(subprocess.TimeoutExpired):
                        module.run([sys.executable, '-c', parent], root / 'timeout.log', root, os.environ.copy())
                self.assertTrue(pidfile.exists(), 'real descendant never started')
                pid = int(pidfile.read_text())
                deadline = time.monotonic() + 3
                while alive(pid) and time.monotonic() < deadline:
                    time.sleep(0.05)
                self.assertFalse(alive(pid), 'real descendant survived timeout')
                # Windows cannot remove this file while the sleeping child holds it.
                held.unlink()
            finally:
                if pid is not None and alive(pid):
                    if os.name == 'nt':
                        real_run(['taskkill', '/PID', str(pid), '/T', '/F'], timeout=5, capture_output=True)
                    else:
                        import signal
                        os.kill(pid, signal.SIGKILL)

    def test_every_executable_probe_uses_supervisor(self):
        import ast
        tree = ast.parse(SCRIPT.read_text())
        raw = []
        for function in (n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef)):
            for node in ast.walk(function):
                if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and \
                        isinstance(node.func.value, ast.Name) and node.func.value.id == 'subprocess' and \
                        node.func.attr in ('run', 'check_output', 'Popen'):
                    raw.append((function.name, node.func.attr))
        self.assertEqual(raw, [('execute', 'Popen'), ('execute', 'run')])

    def test_windows_tree_command_is_bounded_and_precedes_wait(self):
        from unittest.mock import Mock, patch
        module = self.load()
        proc = Mock(pid=123)
        proc.wait.side_effect = [subprocess.TimeoutExpired(['tool'], 30), 1]
        events = []
        proc.wait.side_effect = lambda **kwargs: events.append(('wait', kwargs)) or (
            (_ for _ in ()).throw(subprocess.TimeoutExpired(['tool'], 30)) if len(events) == 1 else 1)
        def kill(command, **kwargs):
            events.append(('kill', command, kwargs))
        with tempfile.TemporaryFile() as output, \
                patch.object(module.os, 'name', 'nt'), \
                patch.object(module.subprocess, 'Popen', return_value=proc) as spawn, \
                patch.object(module.subprocess, 'run', side_effect=kill):
            with self.assertRaises(subprocess.TimeoutExpired):
                module.execute(['tool'], '.', {}, output, output, 30)
        self.assertFalse(spawn.call_args.kwargs['start_new_session'])
        self.assertEqual(events[1][1], ['taskkill', '/PID', '123', '/T', '/F'])
        self.assertEqual(events[1][2]['timeout'], 5)
        self.assertTrue(events[1][2]['check'])
        self.assertEqual(events[2], ('wait', {'timeout': 5}))
        proc.kill.assert_not_called()

    def test_windows_cleanup_failure_is_bounded_and_fails_closed(self):
        from unittest.mock import Mock, patch
        module = self.load()
        proc = Mock(pid=456)
        proc.wait.side_effect = [subprocess.TimeoutExpired(['tool'], 30), 1]
        with tempfile.TemporaryFile() as output, \
                patch.object(module.os, 'name', 'nt'), \
                patch.object(module.subprocess, 'Popen', return_value=proc), \
                patch.object(module.subprocess, 'run', side_effect=subprocess.TimeoutExpired(['taskkill'], 5)):
            with self.assertRaises(subprocess.TimeoutExpired) as failure:
                module.execute(['tool'], '.', {}, output, output, 30)
        self.assertEqual(failure.exception.cmd, ['taskkill'])
        proc.kill.assert_called_once_with()
        self.assertEqual(proc.wait.call_args.kwargs, {'timeout': 5})

    def test_capture_probe_timeout_has_bounded_error_receipt(self):
        import sys
        from unittest.mock import patch
        module = self.load()
        with tempfile.TemporaryDirectory() as root, patch.object(module, 'PROBE_TIMEOUT', 0.2):
            root = Path(root)
            with self.assertRaises(subprocess.TimeoutExpired) as failure:
                module.capture([sys.executable, '-c',
                                'import sys,time; sys.stderr.write("x"*100000); sys.stderr.flush(); time.sleep(20)'],
                               stdout_path=root / 'stdout', stderr_path=root / 'stderr')
            self.assertEqual(len(failure.exception.stderr), 65536)
            self.assertEqual((root / 'stderr').stat().st_size, 65536)

    def test_timeout_log_is_not_a_success_receipt(self):
        import os
        import sys
        from unittest.mock import patch
        module = self.load()
        with tempfile.TemporaryDirectory() as root, patch.object(module, 'COMMAND_TIMEOUT', 0.2):
            log = Path(root) / 'timeout.log'
            with self.assertRaises(subprocess.TimeoutExpired):
                module.run([sys.executable, '-c', 'import time; time.sleep(20)'], log, root, os.environ.copy())
            self.assertIn('FAILURE=TimeoutExpired', log.read_text())
            self.assertIn('ELAPSED_SECONDS=', log.read_text())
            self.assertNotIn('EXIT_STATUS=0', log.read_text())

    def test_stale_output_rejected(self):
        with self.assertRaises(AssertionError):
            self.load().assert_changed('tapid 1', 'tapid 1', 'baseline', 'baseline', 'a', 'a', 'TAPID_TEST')


if __name__ == '__main__':
    unittest.main()
