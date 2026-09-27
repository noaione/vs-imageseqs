# 20 — Finish portable packaging and source rebuild validation

Status: proposed follow-up to [14](14-linux-wheel-distribution.md), based on
the current build scripts on 2026-09-27. Priority: high for source-build
consistency, medium for additional portability work.

## source archives and rebuilds

`tools/build-manylinux.sh` copies `tests/` into the relinking archive, but
`pyproject.toml` does not include `tests/` in the sdist. Running the manylinux
script from an unpacked sdist therefore lacks an input needed by its source
bundle step. The current source CI checks archive filenames, not this build
route. The relinking archive is generated, but a clean rebuild from it has not
been validated.

Choose an explicit source-artifact contract: include the validation fixtures
needed for rebuilding/testing, or make packaging independent of absent tests
and document how validation inputs are supplied. Then build from an unpacked
sdist and independently from the relinking archive, outside the checkout, with
fresh Cargo/native prefixes. Verify locked vendored dependencies and installed
output. Test the documented modified-library rebuild path as well as an
unchanged build. Identify which tooling still needs network access; vendored
Rust sources alone do not make the complete procedure offline.

## macOS dependencies and source provenance

The macOS job installs dav1d/libde265 with Homebrew and tests on that same
machine. Its dylib still depends on those system libraries. This is documented,
but does not demonstrate a portable ZIP or wheel on a clean Mac.

Evaluate bundling the existing shared libraries inside `imageseqs/lib/` with
relative install names, preserving the manifest's single plugin entry. Audit
the complete dependency closure, architecture and deployment target. Validate
the wheel and relocated ZIP on a separate host without the build-time Homebrew
paths. Update exact license texts and notices if the packaged versions change.
Choose tooling only after checking its current supported behavior.

Track exact Windows native sources, triplet/options and application build inputs
alongside their binaries, and test the documented rebuild/relink workflow.
This continues the repository's existing `THIRD_PARTY_NOTICES` requirements;
Linux's archive must not be described as covering other platforms.

## release metadata and repeatable staging

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

## acceptance

Treat these as separate patches with separate validation: source rebuilds first,
then macOS portability, source provenance, and release metadata/staging. Keep
the existing release gates and plugin-only layout. Add no new platform or codec
dependency merely to complete this follow-up.
