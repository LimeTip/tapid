import unittest
from datetime import datetime, timedelta, timezone

import release_identity


class ReleaseIdentityTests(unittest.TestCase):
    def test_accepts_canonical_zero_major_version_and_matching_tag(self):
        for version in ("0.0.0", "0.12.3"):
            with self.subTest(version=version):
                self.assertEqual(
                    release_identity.validate_version_tag(version, "v" + version),
                    (version, "v" + version),
                )

    def test_rejects_noncanonical_or_mismatched_version_tags(self):
        cases = (
            ("v0.1.2", "v0.1.2"),
            ("0.1", "v0.1"),
            ("0.1.2-alpha", "v0.1.2-alpha"),
            ("01.2.3", "v01.2.3"),
            ("1.2.3", "v1.2.3"),
            ("0.1.2", "v0.1.3"),
            ("0.1.2", "0.1.2"),
        )
        for version, tag in cases:
            with self.subTest(version=version, tag=tag):
                with self.assertRaisesRegex(ValueError, "canonical 0.x.y"):
                    release_identity.validate_version_tag(version, tag)

    def test_accepts_only_full_lowercase_hexadecimal_commits(self):
        commits = ("a" * 40, "0123456789abcdef" * 4)
        for commit in commits:
            with self.subTest(length=len(commit)):
                self.assertEqual(release_identity.validate_commit(commit), commit)

        invalid = ("a" * 39, "a" * 65, "A" * 40, "g" * 40, "0" * 39 + "-")
        for commit in invalid:
            with self.subTest(commit=commit):
                with self.assertRaisesRegex(ValueError, "40 to 64 lowercase hexadecimal"):
                    release_identity.validate_commit(commit)

    def test_derives_archives_in_the_fixed_six_target_order(self):
        self.assertEqual(
            release_identity.release_archives("0.12.3"),
            (
                ("aarch64-apple-darwin", "tapid-0.12.3-aarch64-apple-darwin.tar.gz"),
                ("aarch64-pc-windows-msvc", "tapid-0.12.3-aarch64-pc-windows-msvc.tar.gz"),
                ("aarch64-unknown-linux-gnu", "tapid-0.12.3-aarch64-unknown-linux-gnu.tar.gz"),
                ("x86_64-apple-darwin", "tapid-0.12.3-x86_64-apple-darwin.tar.gz"),
                ("x86_64-pc-windows-msvc", "tapid-0.12.3-x86_64-pc-windows-msvc.tar.gz"),
                ("x86_64-unknown-linux-gnu", "tapid-0.12.3-x86_64-unknown-linux-gnu.tar.gz"),
            ),
        )
    def test_derives_github_release_and_manifest_urls(self):
        self.assertEqual(
            release_identity.github_release_urls("LimeTip/tapid", "0.12.3", "v0.12.3"),
            {
                "base_url": "https://github.com/LimeTip/tapid/releases/download/v0.12.3",
                "manifest_endpoint": (
                    "https://github.com/LimeTip/tapid/releases/download/v0.12.3/"
                    "release-manifest.json"
                ),
            },
        )
    def test_derives_canonical_utc_timestamps_from_injected_clock(self):
        local = timezone(timedelta(hours=2))
        calls = []

        def clock():
            calls.append(True)
            return datetime(2026, 9, 1, 21, 15, 30, 999999, tzinfo=local)

        self.assertEqual(
            release_identity.freshness_timestamps(clock),
            {
                "created_at": "2026-09-01T19:15:30Z",
                "expires_at": "2026-10-01T19:15:30Z",
            },
        )
        self.assertEqual(calls, [True])
        self.assertEqual(release_identity.FRESHNESS_DURATION, timedelta(days=30))

    def test_rejects_a_clock_without_an_explicit_timezone(self):
        with self.assertRaisesRegex(ValueError, "timezone-aware"):
            release_identity.freshness_timestamps(
                lambda: datetime(2026, 9, 1, 19, 15, 30)
            )

    def test_canonical_json_and_digest_ignore_mapping_insertion_order(self):
        first = {
            "z": [{"b": 2, "a": 1}],
            "a": "å",
            "enabled": True,
        }
        second = {
            "enabled": True,
            "a": "å",
            "z": [{"a": 1, "b": 2}],
        }
        expected = '{"a":"å","enabled":true,"z":[{"a":1,"b":2}]}'.encode()

        self.assertEqual(release_identity.canonical_json(first), expected)
        self.assertEqual(release_identity.canonical_json(second), expected)
        self.assertEqual(
            release_identity.plan_digest(first),
            "sha256-7375f30bd0473f38c947c1d74ebf9162ef3a7de5698c1c51ac1ede707d4c50a2",
        )
        self.assertEqual(release_identity.plan_digest(first), release_identity.plan_digest(second))


if __name__ == "__main__":
    unittest.main()
