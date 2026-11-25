"""Tests for the sn PGO build and benchmark programs."""

from __future__ import annotations

import tempfile
import unittest
from argparse import Namespace
from pathlib import Path
from unittest.mock import patch

import benchmark_pgo
import build_pgo
from pgo_common import (
    held_out_workloads,
    prepare_fixture,
    training_workloads,
)


class BuildPgoTests(unittest.TestCase):
    def test_native_and_linux_musl_targets_can_run(self) -> None:
        self.assertTrue(build_pgo.target_runs_on_host("aarch64-apple-darwin", "aarch64-apple-darwin"))
        self.assertTrue(build_pgo.target_runs_on_host("x86_64-unknown-linux-musl", "x86_64-unknown-linux-gnu"))
        self.assertFalse(build_pgo.target_runs_on_host("aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"))

    def test_hot_count_parser_requires_positive_95th_percentile(self) -> None:
        summary = "123 functions with count >= 41 account for 95% of the total counts."
        self.assertEqual(build_pgo.parse_hot_count(summary), 41)
        with self.assertRaises(RuntimeError):
            build_pgo.parse_hot_count("no detailed summary")
        with self.assertRaises(RuntimeError):
            build_pgo.parse_hot_count("1 functions with count >= 0 account for 95% of the total counts.")

    def test_optimized_flags_keep_existing_linker_flags(self) -> None:
        profile = Path("profiles") / "sn.profdata"
        flags = build_pgo.optimized_rustflags("-C linker=rust-lld", profile, 27)
        self.assertIn("-C linker=rust-lld", flags)
        self.assertIn(f"-Cprofile-use={profile}", flags)
        self.assertIn("--profile-summary-hot-count=27", flags)

    def test_github_environment_is_appended(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "github-env"
            path.write_text("EXISTING=1\n", encoding="utf-8")
            build_pgo.write_github_environment(path, "-Cprofile-use=profile")
            self.assertEqual(
                path.read_text(encoding="utf-8"),
                "EXISTING=1\nRUSTFLAGS=-Cprofile-use=profile\n",
            )


class BenchmarkTests(unittest.TestCase):
    def test_fixture_does_not_forward_credentials(self) -> None:
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.dict(
                "os.environ",
                {
                    "SN_PASSWORD": "private",
                    "SN_PROFILE": "work",
                    "GH_TOKEN": "private",
                    "PATH": "/usr/bin",
                },
                clear=True,
            ),
        ):
            _, environment = prepare_fixture(Path(temporary), "http://127.0.0.1:1234")
        self.assertEqual(environment["SN_PASSWORD"], "pgo-password")
        self.assertNotIn("SN_PROFILE", environment)
        self.assertNotIn("GH_TOKEN", environment)
        self.assertEqual(environment["PATH"], "/usr/bin")
        self.assertEqual(environment["SN_INSTANCE"], "http://127.0.0.1:1234")

    def test_workloads_have_unique_names(self) -> None:
        names = [w.name for w in (*training_workloads(), *held_out_workloads())]
        self.assertEqual(len(names), len(set(names)))

    def test_summary_and_change(self) -> None:
        summary = benchmark_pgo.summarize([1, 2, 3, 4, 5])
        self.assertEqual(summary.median_ns, 3)
        self.assertEqual(summary.p95_ns, 5)
        self.assertAlmostEqual(benchmark_pgo.percent_change(100, 90), -10.0)

    def test_target_directory_resolves_matched_binaries(self) -> None:
        arguments = Namespace(
            target_dir=Path("target/pgo"),
            target="x86_64-pc-windows-msvc",
            baseline=None,
            pgo=None,
        )
        baseline, pgo = benchmark_pgo.resolve_binaries(arguments)
        root = arguments.target_dir.resolve()
        target = arguments.target
        self.assertEqual(
            baseline,
            root / "baseline" / target / "dist" / "sn.exe",
        )
        self.assertEqual(
            pgo,
            root / "optimized" / target / "dist" / "sn.exe",
        )


if __name__ == "__main__":
    unittest.main()
