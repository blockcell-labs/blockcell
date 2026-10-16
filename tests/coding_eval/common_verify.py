"""Shared acceptance runner for the coding-eval tasks.

Every task's ``verify.py`` is a thin shim over this module. Two guards keep the
acceptance result honest, because the workspace under verification is fully
controlled by the agent being evaluated:

1. **Fixture tests are restored before discovery.** The pristine fixture's
   ``test_*.py`` files are copied back over the workspace, so an agent cannot
   make a task pass by deleting, emptying or weakening the shipped tests.
2. **Discovery must actually run a test.** ``unittest discover`` prints
   ``Ran 0 tests`` and exits 0 when it finds nothing, which previously let any
   task pass with zero verification. A run that discovers no test is refused
   with a distinct exit code.

The implementation files themselves are deliberately *not* restored — changing
those is the whole point of the task.
"""

from __future__ import annotations

import pathlib
import re
import shutil
import subprocess
import sys

# Distinct from 1 (= "tests ran and failed") so a refused run is diagnosable.
GUARD_FAILURE_EXIT = 2

# unittest's summary line, e.g. "Ran 1 test in 0.000s" / "Ran 12 tests in 0.1s".
_TESTS_RUN_RE = re.compile(r"^Ran (\d+) tests? in ", re.MULTILINE)


def parse_tests_run(output: str) -> int:
    """Number of tests unittest reported running, or -1 when no summary line."""
    matches = _TESTS_RUN_RE.findall(output)
    return int(matches[-1]) if matches else -1


def fixture_repo_for(shim_path: pathlib.Path) -> pathlib.Path:
    """Pristine fixture repo, derived from the task's ``verify.py`` location."""
    return shim_path.resolve().parent / "repo"


def restore_fixture_tests(fixture_repo: pathlib.Path, workspace: pathlib.Path) -> int:
    """Copy the fixture's ``test_*.py`` files back over the workspace.

    Returns the number of files that were missing or had been modified.
    """
    if not fixture_repo.is_dir():
        return 0
    restored = 0
    for source in sorted(fixture_repo.rglob("test_*.py")):
        target = workspace / source.relative_to(fixture_repo)
        target.parent.mkdir(parents=True, exist_ok=True)
        if not target.exists() or target.read_bytes() != source.read_bytes():
            shutil.copyfile(source, target)
            restored += 1
    return restored


def run_discovery(workspace: pathlib.Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            sys.executable,
            "-B",
            "-m",
            "unittest",
            "discover",
            "-s",
            str(workspace),
            "-p",
            "test_*.py",
            "-v",
        ],
        cwd=workspace,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )


def main() -> int:
    workspace = pathlib.Path(sys.argv[1]).resolve()
    restored = restore_fixture_tests(fixture_repo_for(pathlib.Path(sys.argv[0])), workspace)

    completed = run_discovery(workspace)
    sys.stdout.write(completed.stdout)
    if restored:
        print(
            f"acceptance guard: restored {restored} fixture test file(s) from the pristine fixture",
            file=sys.stderr,
        )

    tests_run = parse_tests_run(completed.stdout)
    if tests_run < 1:
        print(
            "acceptance guard: unittest discovered no tests "
            f"(Ran {tests_run} tests) — refusing to certify this task as passed",
            file=sys.stderr,
        )
        return GUARD_FAILURE_EXIT
    return completed.returncode


if __name__ == "__main__":
    raise SystemExit(main())
