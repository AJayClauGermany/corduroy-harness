"""Credential routing tests: no model, subprocess, or real keychain access."""

import argparse
import contextlib
import io
import json
import os
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest import mock

import bench


class Credentials(unittest.TestCase):
    def config(self, **overrides):
        args = {
            "url": bench.DEFAULT_URL, "out": ".bench-runs/test-credentials",
            "timeout": 60, "workspace_name": "workspace", "user": "unused",
            "binary": "target/release/corduroy", "plugins": "plugins/target",
            "label": "haiku", "provider": "anthropic", "model_id": "claude-haiku-5-5",
            "api_key_env": None, "keychain_secret": None,
            "srt_path": None, "srt_path_env": None,
        }
        args.update(overrides)
        with mock.patch.object(bench.os, "geteuid", return_value=1000):
            return bench.Config(argparse.Namespace(**args))

    def main(self, *arguments, env=None):
        with (
            mock.patch.object(sys, "argv", ["bench.py", *arguments]),
            mock.patch.dict(os.environ, env or {}, clear=True),
            mock.patch.object(bench, "run_model") as run_model,
            mock.patch.object(bench, "collect", return_value=""),
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            result = bench.main()
        return result, run_model

    def test_keychain_trial_declares_secret_once_and_preserves_grants(self):
        config = self.config(keychain_secret="work-anthropic_2")
        with tempfile.TemporaryDirectory() as directory:
            config.out = Path(directory)
            with mock.patch.object(bench.subprocess, "Popen") as spawn:
                trial = bench.Trial(config, "read", 1)
                # configure() rewrites fragments too; it must not add secret tables.
                trial.configure(
                    old=f'kind = "openai-compatible"\nbase_url = "{bench.DEFAULT_URL}"',
                    new=f'kind = "openai-compatible"\nbase_url = "{bench.DEFAULT_URL}"\nmax_tokens = 120',
                )
                trial.configure(append=f'\n[providers.other]\nkind = "openai-compatible"\nbase_url = "{bench.DEFAULT_URL}"\n')
                generated = tomllib.loads(trial.agent_text)
                spawn.assert_not_called()
        base = tomllib.loads((bench.ROOT / "examples/local-model/corduroy.toml").read_text())
        self.assertEqual(generated["secrets"], {"work-anthropic_2": {"kind": "keychain"}})
        self.assertEqual(generated["providers"]["local"]["secret"], "work-anthropic_2")
        self.assertNotIn("api_key_env", generated["providers"]["local"])
        self.assertEqual(generated["kernels"], base["kernels"])
        self.assertEqual(generated["plugins"][0]["grants"], base["plugins"][0]["grants"])
        self.assertEqual(trial.agent_text.count("[secrets.work-anthropic_2]"), 1)

    def test_keychain_does_not_forward_environment_keys(self):
        config = self.config(keychain_secret="anthropic")
        with mock.patch.dict(os.environ, {
            "ANTHROPIC_API_KEY": "test-anthropic-canary", "OPENAI_API_KEY": "test-openai-canary",
        }, clear=True):
            for provider_key in (True, False):
                env = config.env(provider_key=provider_key)
                self.assertNotIn("ANTHROPIC_API_KEY", env)
                self.assertNotIn("OPENAI_API_KEY", env)
        self.assertIsNone(config.key_env)

    def test_keychain_does_not_change_srt_policy(self):
        policies = []
        kernels = []
        with tempfile.TemporaryDirectory() as directory:
            for secret in (None, "anthropic"):
                config = self.config(keychain_secret=secret, srt_path="/test/srt")
                config.out = Path(directory)
                trial = bench.Trial(config, "read", 1)
                policies.append(json.loads((trial.private / "srt.json").read_text()))
                kernels.append(tomllib.loads(trial.agent_text)["kernels"])
        self.assertEqual(policies[0], policies[1])
        self.assertEqual(kernels[0], kernels[1])
        self.assertEqual(policies[1]["network"]["allowedDomains"], [])
        self.assertEqual(policies[1]["network"]["deniedDomains"], ["*"])

    def test_environment_source_remains_supported(self):
        for name in (None, "WORK_API_KEY"):
            with self.subTest(name=name):
                config = self.config(api_key_env=name)
                variable = name or "ANTHROPIC_API_KEY"
                with mock.patch.dict(os.environ, {variable: "test-key-canary"}, clear=True):
                    self.assertEqual(config.env()[variable], "test-key-canary")
                    self.assertNotIn(variable, config.env(provider_key=False))
                hosted = config.hosted((bench.ROOT / "examples/local-model/corduroy.toml").read_text())
                self.assertEqual(tomllib.loads(hosted)["providers"]["local"]["api_key_env"], variable)
                self.assertNotIn("[secrets.", hosted)

    def test_local_model_is_unchanged(self):
        config = self.config(provider=None, model_id=None)
        base = (bench.ROOT / "examples/local-model/corduroy.toml").read_text()
        self.assertEqual(config.hosted(base), base)
        self.assertIsNone(config.key_env)

    def test_keychain_cli_needs_no_environment_key(self):
        result, run_model = self.main(
            "--provider", "anthropic", "--model-id", "claude-haiku-5-5",
            "--keychain-secret", "anthropic", "--only", "read", "--trials", "1",
        )
        self.assertEqual(result, 0)
        run_model.assert_called_once()
        self.assertEqual(run_model.call_args.args[0].keychain_secret, "anthropic")

    def test_environment_cli_still_requires_key(self):
        with self.assertRaises(SystemExit) as error:
            self.main("--provider", "anthropic", "--model-id", "claude-haiku-5-5")
        self.assertEqual(error.exception.code, 2)
        result, run_model = self.main(
            "--provider", "anthropic", "--model-id", "claude-haiku-5-5",
            "--api-key-env", "WORK_API_KEY", env={"WORK_API_KEY": "test-key-canary"},
        )
        self.assertEqual(result, 0)
        run_model.assert_called_once()

    def test_conflicting_sources_are_rejected(self):
        with self.assertRaises(SystemExit) as error:
            self.main(
                "--provider", "anthropic", "--model-id", "claude-haiku-5-5",
                "--keychain-secret", "anthropic", "--api-key-env", "ANTHROPIC_API_KEY",
            )
        self.assertEqual(error.exception.code, 2)

    def test_invalid_secret_names_are_rejected_before_sandbox_probe(self):
        for name in ("", "anthropic.work", "bad/name", 'bad"\n[secrets.other]', "caf\u00e9"):
            with self.subTest(name=name), mock.patch.object(bench, "check_srt") as probe:
                with self.assertRaises(SystemExit) as error:
                    self.main(
                        "--provider", "anthropic", "--model-id", "claude-haiku-5-5",
                        "--keychain-secret", name, "--srt",
                    )
                self.assertEqual(error.exception.code, 2)
                probe.assert_not_called()

    def test_keychain_requires_hosted_model(self):
        for arguments in ([], ["--models", "all"], ["--provider", "anthropic"]):
            with self.subTest(arguments=arguments), self.assertRaises(SystemExit) as error:
                self.main(*arguments, "--keychain-secret", "anthropic", "--list")
            self.assertEqual(error.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
