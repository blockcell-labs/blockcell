#!/usr/bin/env python3
"""Exercise the actual release shell and installer with local, inert fixtures."""

from __future__ import annotations

import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[1]
VERSION = "v9.8.7"
ASSET = f"blockcell-{VERSION}-linux-amd64.tar.gz"
PREVIOUS = b"previous-installed-binary\n"
REPLACEMENT = b"#!/bin/sh\n# inert fixture: never execute this file\n"


class InstallerFixture:
    def __init__(self, root: Path, mode: str = "ok", repair_lookup: bool = False):
        self.root = root
        self.mode = mode
        self.bin = root / "bin"
        self.bin.mkdir()
        self.target = root / "install"
        self.target.mkdir()
        self.binary = self.target / "blockcell"
        self.binary.write_bytes(PREVIOUS)
        self.log = root / "commands.jsonl"
        archive = root / ASSET
        with tarfile.open(archive, "w:gz") as output:
            info = tarfile.TarInfo(
                "dist/blockcell" if mode == "candidate_copy_failure" else "blockcell"
            )
            info.size = len(REPLACEMENT)
            info.mode = 0o755
            output.addfile(info, io.BytesIO(REPLACEMENT))
            if mode == "archive_link":
                info = tarfile.TarInfo("unused-link")
                info.type = tarfile.SYMTYPE
                info.linkname = "blockcell"
                output.addfile(info)
        sha = hashlib.sha256(archive.read_bytes()).hexdigest()
        if mode == "bad_checksum":
            sha = "0" * 64
        checksums = "" if mode == "missing_checksum" else f"{sha}  {ASSET}\n"
        (root / "checksums.txt").write_text(checksums)
        self.env = os.environ.copy()
        self.env.update(
            PATH=f"{self.bin}:/usr/bin:/bin",
            BLOCKCELL_VERSION=VERSION,
            BLOCKCELL_INSTALL_DIR=str(self.target),
            ENTRYPOINT_ROOT=str(root),
            ENTRYPOINT_MODE=mode,
            ENTRYPOINT_REPAIR_LOOKUP=str(int(repair_lookup)),
        )
        self._make_commands()

    def _command(self, name: str, code: str):
        prefix = f"#!{sys.executable}\n" + textwrap.dedent("""\
            import json, os, pathlib, shutil, subprocess, sys
            root = pathlib.Path(os.environ['ENTRYPOINT_ROOT'])
            mode = os.environ['ENTRYPOINT_MODE']
            args = sys.argv[1:]
            with (root / 'commands.jsonl').open('a') as log:
                log.write(json.dumps([pathlib.Path(sys.argv[0]).name, *args]) + '\\n')
            """)
        path = self.bin / name
        path.write_text(prefix + textwrap.dedent(code))
        path.chmod(0o755)

    def _make_commands(self):
        self._command("uname", "print('Linux' if args == ['-s'] else 'x86_64')\n")
        self._command("curl", f"""\
            url = next(arg for arg in args if arg.startswith('https://'))
            if 'api.github.com/' in url:
                if mode == 'metadata_failure':
                    sys.exit(22)
                if mode == 'metadata_invalid':
                    print('{{not-json}}')
                elif mode == 'no_assets':
                    print('{{"assets":[]}}')
                else:
                    separators = (',', ':') if os.environ['ENTRYPOINT_REPAIR_LOOKUP'] == '1' else None
                    print(json.dumps({{'assets': [{{'browser_download_url':
                        'https://example.invalid/releases/{VERSION}/{ASSET}'}}]}}, separators=separators))
            else:
                destination = pathlib.Path(args[args.index('-o') + 1])
                if not destination.is_relative_to(root):
                    sys.exit('mock refuses output outside its temporary root')
                source = root / ('checksums.txt' if url.endswith('/checksums.txt') else '{ASSET}')
                shutil.copyfile(source, destination)
            """)
        self._command("grep", f"""\
            if mode == 'grep_failure' and '-Eo' in args:
                sys.exit(2)
            if (os.environ['ENTRYPOINT_REPAIR_LOOKUP'] == '1'
                    and len(args) == 2 and args[0] == '-F' and args[1].startswith('-')):
                args.insert(1, '--')
            os.execv({shutil.which('grep')!r}, ['grep', *args])
            """)
        self._command("sed", f"""\
            if mode == 'sed_failure' and any('browser_download_url' in arg for arg in args):
                sys.exit(2)
            os.execv({shutil.which('sed')!r}, ['sed', *args])
            """)
        self._command("tar", f"""\
            if mode == 'archive_listing_failure' and '-tzf' in args:
                sys.exit(73)
            if mode == 'archive_verbose_failure' and '-tvzf' in args:
                subprocess.run([{shutil.which('tar')!r}, *args], check=True)
                sys.exit(74)
            if mode == 'extract_failure' and '-xzf' in args:
                subprocess.run([{shutil.which('tar')!r}, *args], check=True)
                sys.exit(75)
            os.execv({shutil.which('tar')!r}, ['tar', *args])
            """)
        self._command("mktemp", f"""\
            if '-d' in args:
                if mode == 'temp_dir_failure':
                    sys.exit(76)
                args = ['-d', str(root / 'download.XXXXXX')]
            elif mode == 'temp_file_failure':
                sys.exit(76)
            os.execv({shutil.which('mktemp')!r}, ['mktemp', *args])
            """)
        self._command("cp", f"""\
            if mode == 'candidate_copy_failure' and '/dist/' in args[0]:
                pathlib.Path(args[-1]).write_bytes(b'partial copy')
                sys.exit(77)
            if mode == 'install_copy_failure':
                pathlib.Path(args[-1]).write_bytes(b'partial copy')
                sys.exit(77)
            os.execv({shutil.which('cp')!r}, ['cp', *args])
            """)
        for command, failure in [("mkdir", "mkdir_failure"), ("chmod", "chmod_failure"), ("mv", "move_failure")]:
            real = shutil.which(command)
            self._command(command, f"""\
                if mode == {failure!r}:
                    sys.exit(78)
                os.execv({real!r}, [{command!r}, *args])
                """)
        for command in ["git", "npm", "cargo", "sudo", "apt-get", "brew"]:
            self._command(command, "sys.exit(91)\n")

    def run(self, method: str) -> subprocess.CompletedProcess[str]:
        env = self.env | {"BLOCKCELL_INSTALL_METHOD": method}
        return subprocess.run(
            ["/bin/sh", str(ROOT / "install.sh")], env=env,
            capture_output=True, text=True, timeout=30,
        )

    def commands(self) -> list[list[str]]:
        return [json.loads(line) for line in self.log.read_text().splitlines()]


class InstallerEntrypointTests(unittest.TestCase):
    def test_release_and_auto_install_success_without_source_fallback(self):
        for method in ("release", "auto"):
            with self.subTest(method=method), tempfile.TemporaryDirectory() as temporary:
                fixture = InstallerFixture(Path(temporary))
                result = fixture.run(method)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(fixture.binary.read_bytes(), REPLACEMENT)
                self.assertTrue(os.access(fixture.binary, os.X_OK))
                self.assertFalse(any(cmd[0] in ("git", "npm", "cargo") for cmd in fixture.commands()))
                self.assertEqual(list(fixture.target.iterdir()), [fixture.binary])
                self.assertFalse(list(fixture.root.glob("download.*")))

    def test_integrity_and_commit_failures_preserve_existing_binary(self):
        failures = {
            "bad_checksum": ("SHA-256 checksum mismatch", False),
            "missing_checksum": ("No valid SHA-256 checksum", False),
            "archive_link": ("unsupported links", False),
            "archive_listing_failure": ("Failed to inspect release archive", False),
            "archive_verbose_failure": ("Failed to inspect release archive", False),
            "extract_failure": ("Failed to extract release archive", True),
            "candidate_copy_failure": ("Release install failed", True),
            "mkdir_failure": ("Release install failed", True),
            "temp_file_failure": ("Release install failed", True),
            "install_copy_failure": ("Release install failed", True),
            "chmod_failure": ("Release install failed", True),
            "move_failure": ("Release install failed", True),
        }
        for method in ("release", "auto"):
            for failure, (message, extracted) in failures.items():
                with self.subTest(method=method, failure=failure), tempfile.TemporaryDirectory() as temporary:
                    # Simulate only the prerequisite grep fix when running old
                    # code: these regressions must reach the integrity checks.
                    fixture = InstallerFixture(Path(temporary), failure, repair_lookup=True)
                    result = fixture.run(method)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    if method == "auto" and message == "Release install failed":
                        message = "refusing source fallback"
                    self.assertIn(message, result.stdout + result.stderr)
                    self.assertEqual(fixture.binary.read_bytes(), PREVIOUS)
                    commands = fixture.commands()
                    self.assertTrue(any(cmd[0] == "curl" and "-o" in cmd and cmd[-1].endswith("checksums.txt") for cmd in commands))
                    self.assertEqual(any(cmd[:2] == ["tar", "-xzf"] for cmd in commands), extracted)
                    self.assertFalse(any(cmd[0] in ("git", "npm", "cargo") for cmd in commands))
                    self.assertEqual(list(fixture.target.iterdir()), [fixture.binary])
                    self.assertFalse(list(fixture.root.glob("download.*")))

    def test_metadata_or_parser_failure_does_not_fallback(self):
        for failure in ("metadata_failure", "metadata_invalid", "grep_failure", "sed_failure", "temp_dir_failure"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temporary:
                fixture = InstallerFixture(Path(temporary), failure, repair_lookup=True)
                result = fixture.run("auto")
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(fixture.binary.read_bytes(), PREVIOUS)
                self.assertFalse(any(cmd[0] in ("git", "npm", "cargo") for cmd in fixture.commands()))

    def test_valid_empty_assets_allows_source_fallback(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = InstallerFixture(Path(temporary), "no_assets")
            result = fixture.run("auto")
            # The harmless git stub rejects the source build after recording it.
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("falling back to source", result.stdout)
            self.assertTrue(any(cmd[0] == "git" for cmd in fixture.commands()))
            self.assertEqual(fixture.binary.read_bytes(), PREVIOUS)


class ManifestEntrypointTests(unittest.TestCase):
    files = {
        "macos/aarch64": f"blockcell-{VERSION}-darwin-arm64.updater",
        "macos/x86_64": f"blockcell-{VERSION}-darwin-amd64.updater",
        "linux/x86_64": f"blockcell-{VERSION}-linux-amd64.updater",
        "windows/x86_64": f"blockcell-{VERSION}-windows-amd64.exe",
    }

    def run_manifest(self, names: list[str], malformed_sha: bool = False):
        source = (ROOT / ".github/workflows/release.yml").read_text()
        step = source.split("      - name: Generate updater manifest.json", 1)[1]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1].split("\n      - name:", 1)[0])
        script = script.replace("${{ github.repository }}", "blockcell-labs/blockcell")
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary)
            checksums = "".join(f"{'bad' if malformed_sha else format(index, '064x')}  {name}\n" for index, name in enumerate(names, 1))
            (path / "checksums.txt").write_text(checksums)
            env = os.environ.copy() | {"TAG": VERSION, "UPDATER_PRIVATE_KEY": "", "UPDATER_PUBLIC_KEY": ""}
            result = subprocess.run(
                ["/bin/bash", "-e", "-o", "pipefail", "-c", script], cwd=path,
                env=env, text=True, capture_output=True, timeout=10,
            )
            manifest = json.loads((path / "manifest.json").read_text()) if (path / "manifest.json").exists() else None
            return result, manifest

    def test_all_four_platforms_have_correct_urls_and_checksums(self):
        names = [*self.files.values(), f"blockcell-{VERSION}-linux-amd64.tar.gz"]
        result, manifest = self.run_manifest(names)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIsNotNone(manifest)
        artifacts = manifest["artifacts"]
        self.assertEqual(len(artifacts), 4)
        self.assertEqual({item["os"] + "/" + item["arch"] for item in artifacts}, set(self.files))
        for index, item in enumerate(artifacts, 1):
            target = item["os"] + "/" + item["arch"]
            self.assertEqual(item["url"], f"https://github.com/blockcell-labs/blockcell/releases/download/{VERSION}/{self.files[target]}")
            self.assertEqual(item["sha256"], format(index, "064x"))

    def test_empty_missing_duplicate_or_invalid_assets_fail_before_manifest(self):
        names = list(self.files.values())
        cases = [
            ([], False),
            (names[:-1], False),
            (names + names[:1], False),
            (names[:-1] + names[:1], False),
            (names, True),
        ]
        for files, malformed_sha in cases:
            with self.subTest(files=files, malformed_sha=malformed_sha):
                result, manifest = self.run_manifest(files, malformed_sha)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIsNone(manifest)


if __name__ == "__main__":
    unittest.main()
