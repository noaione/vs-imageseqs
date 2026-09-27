import argparse
import os
import sys
from pathlib import Path

ROOT_DIR = Path(__file__).parent.parent.absolute()

parser = argparse.ArgumentParser(description="Generate changelog for the release")
parser.add_argument(
    "--dry-run",
    action="store_true",
    help="Generate the changelog but don't write it to the file",
)
args = parser.parse_args()

CHANGELOG_FILE = ROOT_DIR / "CHANGELOG.md"
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
| macos arm64 | `macos-arm64-plugin.zip` |

There is also an attached wheels file, mirrored to here. This is the same as the one from PyPI.

Please make sure to download the correct file for your system.

"""

# ref/tags/v1.0.0
GIT_TAGS = os.getenv("VERSION")
if not GIT_TAGS:
    raise ValueError("No git tags found")

# v1.0.0
if not GIT_TAGS.startswith("refs/tags/"):
    raise ValueError("Invalid git tag format")

VERSION = GIT_TAGS.split("/")[-1]
VERSION = VERSION.removeprefix("v")

EXTRACTED_CHANGELOG = ""
START = False
for line in CHANGELOG_FILE.read_text().splitlines():
    if line.startswith("## [") and START:
        break
    if line.startswith(f"## [{VERSION}]"):
        line = INNER_DESC
        START = True

    if START:
        EXTRACTED_CHANGELOG += line + "\n"

EXTRACTED_CHANGELOG = EXTRACTED_CHANGELOG.strip()

# Write into CHANGELOG-GENERATED.md
if not EXTRACTED_CHANGELOG:
    EXTRACTED_CHANGELOG = f"{INNER_DESC}\n\nNo changelog found for version {VERSION}"
EXTRACTED_CHANGELOG += "\n" + OUTER_DESC

if attest_url := os.getenv("ATTEST_URL"):
    EXTRACTED_CHANGELOG += f"---\n\n**Attestation URL**: {attest_url}\n"

if args.dry_run:
    print(EXTRACTED_CHANGELOG)
    sys.exit(0)
CHANGELOG_GENERATED_FILE = ROOT_DIR / "CHANGELOG-GENERATED.md"
CHANGELOG_GENERATED_FILE.write_text(EXTRACTED_CHANGELOG)
