"""Build output directories, and what a second build does to them.

Every build here writes into a directory the previous one left behind: ``dist/``
accumulates one wheel per version, ``target/manylinux/unrepaired`` accumulates the
wheel auditwheel is handed, and the checkers beside them require exactly one
wheel. A build therefore empties its own output of the artifacts it is about to
write, and only of those, so a file a user keeps beside them survives; a check
that needs one file names what else is there rather than reporting a count.

The file name has an underscore rather than the dashes its siblings use because
this one is imported by the other tools as well as run from the shell.
"""

from __future__ import annotations

import argparse
from collections.abc import Sequence
from pathlib import Path

# The kinds of file this repository's builds write. Everything else a build
# output directory holds is somebody else's.
ARTIFACTS = ("*.whl", "*.tar.gz")


def clear(directory: Path, patterns: Sequence[str] = ARTIFACTS) -> list[Path]:
    """Removes the build artifacts `directory` holds, and reports which.

    Only files matching `patterns` are removed, so a directory that also holds
    something a user put there keeps it. A directory that does not exist, or
    holds no such file, is left as it is.
    """
    removed = []
    for pattern in patterns:
        for path in sorted(directory.glob(pattern)):
            if path.is_file():
                path.unlink()
                removed.append(path)
    return removed


def single(directory: Path, pattern: str, description: str) -> Path:
    """The one file `pattern` matches in `directory`.

    A build that ran twice without clearing up leaves more than one, which is
    what this names: "expected exactly one wheel, found 2: a-0.1.0.whl,
    b-0.2.0.whl" is a fixable message and "expected exactly one wheel" is not.
    """
    matches = sorted(path for path in directory.glob(pattern) if path.is_file())
    if len(matches) != 1:
        found = ", ".join(path.name for path in matches) or "nothing"
        # There is nothing to clear when the directory is empty, so the hint is
        # only useful once there is more than one file to choose between.
        hint = (
            f"\nrun 'python tools/build_output.py clear {directory}' to start over"
            if matches
            else ""
        )
        raise SystemExit(
            f"expected exactly one {description} in {directory}, "
            f"found {len(matches)}: {found}{hint}"
        )
    return matches[0]


def main() -> None:
    parser = argparse.ArgumentParser(description="Build output directories, and what a second build does to them.")
    parser.add_argument("command", choices=["clear"])
    parser.add_argument("directories", type=Path, nargs="+")
    parser.add_argument(
        "--pattern",
        action="append",
        default=None,
        help="only remove these files, repeatable (default: wheels and source archives)",
    )
    args = parser.parse_args()
    patterns = tuple(args.pattern) if args.pattern else ARTIFACTS
    for directory in args.directories:
        removed = clear(directory, patterns)
        for path in removed:
            print(f"removed {path}")
        print(f"cleared {len(removed)} file(s) from {directory}")


if __name__ == "__main__":
    main()
