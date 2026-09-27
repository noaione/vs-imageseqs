"""Generate the release notes for one tag from ``CHANGELOG.md``.

The release workflow runs this with the tag in ``VERSION``:

    VERSION=refs/tags/v0.2.0 python3 tools/create-changelog.py

The notes are written to ``CHANGELOG-GENERATED.md``. The run is strict by
default: the tag, the project version in ``pyproject.toml``, the crate version in
``Cargo.toml`` and the changelog all have to agree, and a version the changelog
does not hold is an error rather than a release published with placeholder notes.
``--preview`` turns those checks off, which is what a version that is not
released yet needs.

Every file is read and written as UTF-8 whatever the locale says, because a
changelog holds prose rather than a machine's byte range.
"""

from __future__ import annotations

import argparse
import os
import tomllib
from pathlib import Path

ROOT_DIR = Path(__file__).parent.parent.absolute()

CHANGELOG = "CHANGELOG.md"
GENERATED = "CHANGELOG-GENERATED.md"

INNER_DESC = """The following release notes are automatically generated.

For the complete changelog, visit [here](https://github.com/noaione/vs-imageseqs/blob/master/CHANGELOG.md).
If you encounter any problems, please report them on the [issues](https://github.com/noaione/vs-imageseqs/issues/new/choose) page.

## Changelog
"""  # ruff: ignore[line-too-long]

OUTER_DESC = """
---

Following are the files included in this release:
| platform | file |
| --- | --- |
| windows x64 | `windows-x86_64-plugin.zip` |
| linux x64 | `linux-x86_64-plugin.zip` |
| linux x64, relinking sources | `linux-relink-source.tar.gz` |
| macos arm64 | `macos-arm64-plugin.zip` |

There is also an attached wheel, mirrored to here from PyPI, and a source
distribution to build from.

Please make sure to download the correct file for your system.

"""


def version_of(tag: str) -> str:
    """The version a git tag names, from ``refs/tags/v1.2.3`` or ``v1.2.3``.

    The leading ``v`` is optional, because the tag is the maintainer's to write
    and the changelog heading is what the version has to match.
    """
    if tag.startswith("refs/"):
        parts = tag.split("/")
        if len(parts) != 3 or parts[:2] != ["refs", "tags"]:
            raise ValueError(f"a tag is refs/tags/<name>, not {tag}")
        name = parts[2]
    elif "/" in tag:
        raise ValueError(f"a tag is refs/tags/<name> or <name>, not {tag}")
    else:
        name = tag
    version = name.removeprefix("v")
    if not version:
        raise ValueError(f"the tag {tag} names no version")
    return version


def stated_version(path: Path, table: str) -> str:
    """The version one table of a TOML file states."""
    try:
        with path.open("rb") as handle:
            document = tomllib.load(handle)
    except FileNotFoundError:
        raise SystemExit(f"{path} is missing") from None
    except tomllib.TOMLDecodeError as error:
        raise SystemExit(f"{path} is not valid TOML: {error}") from None
    version = document.get(table, {}).get("version")
    if not isinstance(version, str):
        raise SystemExit(f"{path} states no [{table}] version")
    return version


def check_versions(root: Path, version: str) -> None:
    """The tag and the two version files have to agree.

    The changelog half of the agreement is `release_section`: a version that is
    not in the file at all is what a release with placeholder notes looks like,
    and that is caught where the section is read.
    """
    stated = {
        "pyproject.toml": stated_version(root / "pyproject.toml", "project"),
        "Cargo.toml": stated_version(root / "Cargo.toml", "package"),
    }
    mismatched = [f"{name} states {found}" for name, found in stated.items() if found != version]
    if mismatched:
        raise SystemExit(f"the tag names {version}, but {', '.join(mismatched)}")


def release_section(lines: list[str], version: str) -> list[str] | None:
    """The changelog lines of one version's section, without its heading.

    A section opens at the ``## [version]`` heading and closes at the next line
    that opens any section, not only another version: ``## unreleased`` is one
    too, so a section is not swallowed by whatever is written after it.
    """
    start = None
    for index, line in enumerate(lines):
        if start is None:
            if line.startswith(f"## [{version}]"):
                start = index + 1
            continue
        if line.startswith("## "):
            return lines[start:index]
    return None if start is None else lines[start:]


def notes(section: list[str] | None, version: str) -> str:
    """The release body of one section, or the placeholder a preview writes."""
    body = "\n".join(section).strip() if section is not None else ""
    if not body:
        return f"{INNER_DESC}\n\nNo changelog found for version {version}"
    return f"{INNER_DESC}\n{body}"


def main() -> None:
    parser = argparse.ArgumentParser(description="Generate the release notes for one tag from ``CHANGELOG.md``.")
    parser.add_argument(
        "--root",
        type=Path,
        default=ROOT_DIR,
        help="the tree holding the changelog and the version files (default: the repository)",
    )
    parser.add_argument(
        "--tag",
        default=os.getenv("VERSION"),
        help="the tag to generate for, as refs/tags/v1.2.3 or v1.2.3 (default: $VERSION)",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=None,
        help=f"where to write the notes (default: {GENERATED} in the root)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the notes instead of writing them",
    )
    parser.add_argument(
        "--preview",
        action="store_true",
        help="do not check the tag, the versions and the changelog against each other",
    )
    args = parser.parse_args()

    if not args.tag:
        parser.error("no tag to generate for: pass --tag or set VERSION")
    try:
        version = version_of(args.tag)
    except ValueError as error:
        parser.error(str(error))

    if not args.preview:
        check_versions(args.root, version)

    changelog = args.root / CHANGELOG
    if not changelog.is_file():
        raise SystemExit(f"{changelog} is missing")
    lines = changelog.read_text(encoding="utf-8").splitlines()
    section = release_section(lines, version)
    if (section is None or not "\n".join(section).strip()) and not args.preview:
        raise SystemExit(
            f"{changelog} holds no section for {version}; "
            f"add a '## [{version}]' heading or pass --preview"
        )

    document = notes(section, version) + "\n" + OUTER_DESC
    if attest_url := os.getenv("ATTEST_URL"):
        document += f"---\n\n**Attestation URL**: {attest_url}\n"

    if args.dry_run:
        print(document)
        return
    output = args.output or args.root / GENERATED
    output.write_text(document, encoding="utf-8")
    print(f"wrote {output}")


if __name__ == "__main__":
    main()
