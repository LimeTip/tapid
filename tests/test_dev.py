"""Exercise build cache selection with real temporary Git worktrees."""

import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest


spec = importlib.util.spec_from_file_location(
    "dev", Path(__file__).resolve().parents[1] / "scripts" / "dev.py"
)
dev = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dev)


class DevelopmentCacheTests(unittest.TestCase):
    def test_worktrees_share_cache_but_unrelated_repositories_do_not(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo = root / "repo"
            other = root / "other"
            for path in [repo, other]:
                subprocess.run(["git", "init", "--quiet", str(path)], check=True)
            worktree = root / "worktree"
            subprocess.run(
                ["git", "worktree", "add", "--quiet", "--orphan", "-b", "fixture", str(worktree)],
                cwd=repo, check=True,
            )
            main_env = dev.cargo_environment(repo, {})
            self.assertEqual(main_env, dev.cargo_environment(worktree, {}))
            self.assertNotEqual(main_env, dev.cargo_environment(other, {}))
            self.assertEqual(Path(main_env["CARGO_TARGET_DIR"]), repo.resolve() / ".git" / "target" / "dev")

    def test_explicit_target_and_other_environment_are_preserved(self):
        env = {"CARGO_TARGET_DIR": "custom-target", "RUSTFLAGS": "-Dwarnings"}
        self.assertEqual(dev.cargo_environment(Path("missing-repository"), env), env)

    def test_sibling_bare_repositories_keep_separate_worktree_caches(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            caches = []
            for name in ["tapid", "other"]:
                repo = root / f"{name}.git"
                subprocess.run(["git", "init", "--quiet", "--bare", str(repo)], check=True)
                worktrees = [root / f"{name}-first", root / f"{name}-second"]
                for index, worktree in enumerate(worktrees):
                    subprocess.run(
                        ["git", "--git-dir", str(repo), "worktree", "add", "--quiet",
                         "--orphan", "-b", f"fixture-{index}", str(worktree)],
                        check=True,
                    )
                first = dev.cargo_environment(worktrees[0], {})["CARGO_TARGET_DIR"]
                second = dev.cargo_environment(worktrees[1], {})["CARGO_TARGET_DIR"]
                self.assertEqual(first, second)
                caches.append(first)
            self.assertNotEqual(caches[0], caches[1])

    def test_nested_git_directories_keep_distinct_repository_caches(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            shared = root / "shared.git"
            subprocess.run(["git", "init", "--quiet", "--bare", str(shared)], check=True)
            # A separate conventional repository can coexist with this bare Git directory.
            subprocess.run(["git", "init", "--quiet", str(shared)], check=True)
            bare_worktree = root / "bare-worktree"
            subprocess.run(
                ["git", "--git-dir", str(shared), "worktree", "add", "--quiet",
                 "--orphan", "-b", "fixture", str(bare_worktree)],
                check=True,
            )
            bare_cache = dev.cargo_environment(bare_worktree, {})["CARGO_TARGET_DIR"]
            conventional_cache = dev.cargo_environment(shared, {})["CARGO_TARGET_DIR"]
            self.assertNotEqual(bare_cache, conventional_cache)


if __name__ == "__main__":
    unittest.main()
