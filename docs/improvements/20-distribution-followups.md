# 20 — Finish portable packaging and source rebuild validation

- status: partially implemented — **release metadata and repeatable staging**
  landed; **source archives and rebuilds** and **macOS dependencies and source
  provenance** are still proposed
- touches (landed slice): `tools/create-changelog.py`, `tools/build_output.py`
  (new), `tools/stage-native.py`, `tools/check-linux-wheel.py`,
  `tools/package-linux-wheel.py`, `tools/build-manylinux.sh`,
  `tests/check-packaging-tools.py` (new), `.github/workflows/build.yml`,
  `.gitignore`, `pyproject.toml`, `CHANGELOG.md`, `AGENTS.md`, `docs/HANDOFF.md`
  and the index row
- depends on: [14](14-linux-wheel-distribution.md), whose layout and release
  gates this keeps and whose `tools/` it hardens
- expected (landed slice): a release cannot be published from a tag whose
  version, `pyproject.toml`, `Cargo.toml` and changelog do not agree, and a
  second build in the same checkout starts from what it built rather than from
  what the last one left
- result: `create-changelog.py` is strict by default — the tag, both version
  files and a non-empty changelog section have to agree, and a section ends at
  the next heading rather than at the next *version* heading — with an explicit
  `--preview` for a version that is not released yet. `tools/build_output.py` is
  the one place that empties a build output of the artifacts a build writes, and
  the staging and checking tools use it instead of accumulating wheels. 46 new
  checks in `tests/check-packaging-tools.py`; nothing in the plugin, the wheel
  layout or the release gates changed.
- risk: low — release tooling and CI only

## source archives and rebuilds

**still proposed.** `tools/build-manylinux.sh` copies `tests/` into the
relinking archive, but `pyproject.toml` does not include `tests/` in the sdist.
Running the manylinux script from an unpacked sdist therefore lacks an input
needed by its source bundle step. The current source CI checks archive filenames,
not this build route. The relinking archive is generated, but a clean rebuild
from it has not been validated.

Choose an explicit source-artifact contract: include the validation fixtures
needed for rebuilding/testing, or make packaging independent of absent tests
and document how validation inputs are supplied. Then build from an unpacked
sdist and independently from the relinking archive, outside the checkout, with
fresh Cargo/native prefixes. Verify locked vendored dependencies and installed
output. Test the documented modified-library rebuild path as well as an
unchanged build. Identify which tooling still needs network access; vendored
Rust sources alone do not make the complete procedure offline.

This is the slice with the highest priority in this plan and it is **not**
landed. Its acceptance needs a fresh manylinux container, network access and a
build from an unpacked archive, none of which the machine this was written on
has, so it is the next one to do where those exist.

## macOS dependencies and source provenance

**still proposed.** The macOS job installs dav1d/libde265 with Homebrew and tests
on that same machine. Its dylib still depends on those system libraries. This is
documented, but does not demonstrate a portable ZIP or wheel on a clean Mac.

Evaluate bundling the existing shared libraries inside `imageseqs/lib/` with
relative install names, preserving the manifest's single plugin entry. Audit the
complete dependency closure, architecture and deployment target. Validate the
wheel and relocated ZIP on a separate host without the build-time Homebrew
paths. Update exact license texts and notices if the packaged versions change.
Choose tooling only after checking its current supported behavior.

Track exact Windows native sources, triplet/options and application build inputs
alongside their binaries, and test the documented rebuild/relink workflow. This
continues the repository's existing `THIRD_PARTY_NOTICES` requirements; Linux's
archive must not be described as covering other platforms.

## release metadata and repeatable staging

### problem and evidence

`tools/create-changelog.py` currently succeeds when the requested version has
no matching changelog section. It lists platform ZIPs but omits the new Linux
relinking archive. Add a strict release mode that checks tag, project/Cargo
version and changelog agreement, and tests Unicode text, missing sections and
the exact next-section boundary. Keep an explicit permissive preview mode if
useful. Read and write UTF-8 explicitly.

Packaging scripts reuse output directories, while wheel checkers require
exactly one wheel. Test two consecutive builds and a version change in the same
checkout. Use a fresh per-build staging directory, or reject nonempty output
with clear instructions; preserve user files rather than broadly deleting them.

### what it did, measured

**a release could be published with placeholder notes.** With
`VERSION=refs/tags/v0.2.0` the tool before this change wrote a
`CHANGELOG-GENERATED.md` whose body read "No changelog found for version 0.2.0"
and exited 0, and it never compared the tag with either version file. It now
compares the tag with `[project] version` in `pyproject.toml` and
`[package] version` in `Cargo.toml`, and requires a section for the version that
is not empty; every failure exits 1 and names both sides of the disagreement:

| run | before | after |
| --- | --- | --- |
| `v0.1.0`, everything agrees | notes | notes, and the section is the body |
| `v0.2.0`, tree still at 0.1.0 | placeholder notes, exit 0 | exit 1: "the tag names 0.2.0, but pyproject.toml states 0.1.0, Cargo.toml states 0.1.0" |
| `v2.0.0`, tree agrees, no section | placeholder notes, exit 0 | exit 1: "holds no section for 2.0.0" |
| `v2.0.0 --preview` | placeholder notes | placeholder notes, exit 0 |

**the section boundary was one heading kind too narrow.** The old loop stopped
at the next line starting `## [`, so a section followed by `## unreleased`
swallowed it. Any `## ` line ends a section now, which is checked both ways: a
following version's notes and a trailing `## unreleased` are both excluded.

**the notes are read and written as UTF-8** whatever the locale says, which the
old `read_text()`/`write_text()` did not ask for; the check writes a section
holding an em dash and Japanese text and reads the file back as bytes.

**the release listing was missing an asset it publishes.** The relinking archive
is copied into the release assets like every other `*.tar.gz`, and the table now
names it. The prose mentions the source distribution beside the wheel, which is
also attached.

**two builds in one checkout could not both succeed.** Reproduced on the staging
side, which needs no wheel to be built: with two wheels in `dist/` — what a
second build of a changed version leaves — `stage-native.py` failed with
"expected exactly one wheel" and `check-linux-wheel.py` with "expected exactly
one repaired wheel, found 2", and neither said which wheels it had found. On the
Linux side `target/manylinux/unrepaired`, `target/manylinux/repaired` and `dist`
accumulated the same way, so `auditwheel repair` and the checker were handed more
than one wheel on a repeat run.

`tools/build_output.py` is now the one place that answers this: `clear()` removes
the files a build writes — wheels and source archives by default, or a
`--pattern` — and nothing else, so a file a user keeps in the same directory
survives; `single()` reports the files it found rather than a count, and says how
to start over. `build-manylinux.sh` clears its three wheel directories before it
builds, the wheel and sdist CI steps clear `dist` first, and `stage-native.py`
replaces the entries the wheel writes, so a file a later wheel no longer carries
is not left behind from the previous build. `native/` and `source-bundle/` are
ignored now, because they are staged build outputs like `dist/`.

### deviations from the plan

- **the slice order is not the plan's.** The acceptance below asks for source
  rebuilds first. That slice's own validation — an unpacked sdist, the relinking
  archive, fresh Cargo and native prefixes, a clean container — cannot be run on
  the machine this was written on, so the slice that could be validated landed
  first. The other two are untouched rather than half done.
- **the clearing tool removes files rather than refusing a nonempty directory.**
  The plan offers either. Removing only the artifact kinds a build writes is what
  makes a repeat build work without an instruction to follow, and it is narrower
  than deleting the output directory, which is what the alternative has to avoid
  to preserve user files.
- **the version agreement is textual, not PEP 440.** The release job installs
  nothing, so `packaging` is not reliably importable there, and a tag is the
  maintainer's to write: the tool compares the tag with the two files and the
  changelog rather than validating the version's shape.

### validation

- `python tests/check-packaging-tools.py`: **46 checks pass**, covering the
  changelog matrix above, the section boundary in both directions, UTF-8, the
  two-build staging sequence including a version change, and what clearing leaves
  alone.
- the tool against this repository: `VERSION=refs/tags/v0.1.0 python
  tools/create-changelog.py --dry-run` generates the real 0.1.0 notes with the
  relinking archive listed; `v0.2.0` exits 1; `--preview` writes the placeholder
  and exits 0.
- `tools/build_output.py clear --pattern '*.whl' a b c` was run directly, which
  is the line `build-manylinux.sh` now has: the wheels go, a source archive
  beside them stays, and a directory that does not exist is not an error.
- the workflow YAML parses, and the new steps are in the `wheels`, `source` and
  `release` jobs; `release` now sets up its own Python rather than using the
  runner's `python3`.
- **not run here:** `bash -n tools/build-manylinux.sh` (the sandbox refuses the
  WSL bash service) and `python -m build` (sandbox), so the two-build CI sequence
  is exercised by the staging check rather than by a real wheel build.

### left over

- **the source-rebuild slice above**, which is the plan's first priority.
- **`native/` is replaced per entry, not per directory.** A wheel that stops
  writing a whole top-level entry leaves that entry behind, because removing a
  directory the wheel no longer mentions would be removing something the build
  cannot prove it owns. No platform's wheel changes its top-level layout.
- **the release job fetches the tag's subject and never uses it**
  (`version_subject`), which predates this slice and is left as it was.
- **`python -m build` does not clear `dist` itself.** The CI steps clear it
  before the build and the tools say how to start over, but a developer running
  `python -m build` twice locally still accumulates wheels until the next
  `stage-native.py` tells them to.

## acceptance

Treat these as separate patches with separate validation: source rebuilds first,
then macOS portability, source provenance, and release metadata/staging. Keep the
existing release gates and plugin-only layout. Add no new platform or codec
dependency merely to complete this follow-up.
