"""Checks the packaging tools against the ways a release build goes wrong.

Run from the repository root with any Python 3.12 or later:

    python tests/check-packaging-tools.py

The build hook and packaging tools are covered against release failures:

- ``tools/create-changelog.py``: a tag, ``pyproject.toml``, ``Cargo.toml`` and a
  changelog section that do not agree, a section that is missing or empty, the
  boundary at the next section, and non-ASCII prose that has to survive the
  round trip. The tool is run as a subprocess against a tree built here, so the
  repository's own changelog is not read.
- ``tools/stage-native.py``: two consecutive stagings in one checkout, including
  a version change, where the second wheel no longer carries a file the first one
  did, plus macOS/Linux bundles whose runtime dependencies live below ``lib/``.
- ``tools/build_output.py``: what a build clears from its own output directory,
  and what it leaves alone.
- ``hatch_build.py``: CPU flags reach target dependencies while Cargo's host
  build scripts and proc macros remain runnable on the build machine.

The scratch tree is ``target/check-packaging-tools`` rather than the system
temporary directory, because the repository's ignored build tree is the one
place a build here is always allowed to write; set ``IMGSEQS_CHECK_TMP`` to put
it somewhere else. It is removed on success and kept on failure.
"""

from __future__ import annotations

import importlib.util
import os
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path
from types import ModuleType
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
TOOLS = ROOT / "tools"
DEFAULT_SCRATCH = ROOT / "target" / "check-packaging-tools"
SCRATCH = Path(os.environ.get("IMGSEQS_CHECK_TMP", str(DEFAULT_SCRATCH)))

sys.path.insert(0, str(TOOLS))

# the tools directory is only importable once it is on the path
import build_output  # pyright: ignore[reportMissingImports] # ruff: ignore[module-import-not-at-top-of-file]

FAILURES: list[str] = []


def check(condition: bool, message: str) -> None:
    if condition:
        print(f"ok - {message}")
    else:
        print(f"FAIL - {message}")
        FAILURES.append(message)


def run(tool: str, *arguments: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
    """Runs one packaging tool and reports what it printed."""
    environment = dict(os.environ)
    environment.pop("VERSION", None)
    environment.pop("ATTEST_URL", None)
    if env:
        environment.update(env)
    return subprocess.run(
        [sys.executable, str(TOOLS / tool), *arguments],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=environment,
    )


def message_of(result: subprocess.CompletedProcess) -> str:
    return f"{result.stdout.strip()} {result.stderr.strip()}".strip()


CHANGELOG = """# changelog

## unreleased

- a note that is not released yet

## [9.9.9] - 2026-02-01

### fixed
- a section before the one under test

## [1.2.3] - 2026-01-01

### fixed
- a cafe note with an em dash \u2014 and \u65e5\u672c\u8a9e text

### build
- a subsection line that belongs to the section

## [3.0.0] - 2025-06-01

## [0.1.0] - 2025-01-01

- the oldest section

## unreleased

- a trailing note that is not released either
"""

PYPROJECT = """[project]
name = "vapoursynth-imageseqs"
version = "{version}"
"""

CARGO = """[package]
name = "vs-imageseqs"
version = "{version}"
"""


def release_tree(version: str = "1.2.3") -> Path:
    """A tree holding a changelog and the two version files."""
    tree = SCRATCH / "tree"
    tree.mkdir(parents=True, exist_ok=True)
    (tree / "CHANGELOG.md").write_text(CHANGELOG, encoding="utf-8")
    (tree / "pyproject.toml").write_text(PYPROJECT.format(version=version), encoding="utf-8")
    (tree / "Cargo.toml").write_text(CARGO.format(version=version), encoding="utf-8")
    return tree


def check_changelog() -> None:
    tree = release_tree()

    # The tag, the two version files and the changelog agree, so the notes are
    # the section itself: the heading is replaced by the preamble, and the
    # section ends where the next one opens rather than at the end of the file.
    result = run("create-changelog.py", "--root", str(tree), "--tag", "refs/tags/v1.2.3", "--dry-run")
    notes = message_of(result)
    check(result.returncode == 0, f"changelog: an agreeing release generates notes ({notes[:120]})")
    check(
        "a cafe note with an em dash \u2014 and \u65e5\u672c\u8a9e text" in notes,
        "changelog: the section's prose is the notes"
    )
    check(
        "a subsection line that belongs to the section" in notes,
        "changelog: a subsection stays in its section"
    )
    check("the oldest section" not in notes, "changelog: the next version's section is not included")
    check("a note that is not released yet" not in notes, "changelog: the section before it is not included")
    check(
        "a trailing note that is not released either" not in notes,
        "changelog: a trailing '## unreleased' ends a section"
    )
    check("a section before the one under test" not in notes, "changelog: another version's notes are not included")
    check("linux-relink-source.tar.gz" in notes, "changelog: the release lists the Linux relinking archive")

    # The same run through the environment the workflow uses.
    result = run(
        "create-changelog.py",
        "--root",
        str(tree),
        "--dry-run",
        env={"VERSION": "refs/tags/v1.2.3"},
    )
    check(result.returncode == 0, "changelog: the tag can come from VERSION")

    # A version the changelog does not hold is a release with no notes.
    release_tree("2.0.0")
    result = run("create-changelog.py", "--root", str(tree), "--tag", "refs/tags/v2.0.0")
    check(result.returncode != 0, "changelog: a version with no section is an error")
    check("holds no section for 2.0.0" in message_of(result), "changelog: and says which version it wanted")

    # An empty section is the same failure: a heading with nothing under it.
    result = run("create-changelog.py", "--root", str(tree), "--tag", "refs/tags/v3.0.0")
    check(result.returncode != 0, "changelog: a section with no notes is an error")

    # The two version files have to state what the tag says.
    release_tree("1.2.3")
    result = run("create-changelog.py", "--root", str(tree), "--tag", "refs/tags/v9.9.9")
    check(result.returncode != 0, "changelog: a tag the version files disagree with is an error")
    check(
        "the tag names 9.9.9" in message_of(result) and "pyproject.toml states 1.2.3" in message_of(result),
        f"changelog: and names both sides ({message_of(result)[:120]})",
    )

    (tree / "Cargo.toml").write_text(CARGO.format(version="1.2.2"), encoding="utf-8")
    result = run("create-changelog.py", "--root", str(tree), "--tag", "refs/tags/v1.2.3")
    check(result.returncode != 0, "changelog: a crate version the tag disagrees with is an error")
    check("Cargo.toml states 1.2.2" in message_of(result), "changelog: and names the crate version")

    # A preview is what a version that is not released yet needs.
    release_tree("2.0.0")
    result = run("create-changelog.py", "--root", str(tree), "--tag", "refs/tags/v2.0.0", "--preview", "--dry-run")
    check(result.returncode == 0, "changelog: a preview generates notes for an unreleased version")
    check("No changelog found for version 2.0.0" in message_of(result), "changelog: and says the section is missing")

    # Writing is UTF-8 whatever the locale says, which is what the prose needs.
    release_tree("1.2.3")
    output = SCRATCH / "notes.md"
    result = run(
        "create-changelog.py",
        "--root",
        str(tree),
        "--tag",
        "refs/tags/v1.2.3",
        "--output",
        str(output),
    )
    check(result.returncode == 0 and output.is_file(), "changelog: the notes are written where asked")
    written = output.read_bytes()
    check(written.decode("utf-8").count("\u65e5\u672c\u8a9e") == 1, "changelog: the written notes are UTF-8")
    check(any(byte > 0x7F for byte in written), "changelog: and hold the non-ASCII prose as itself")

    # A tag that is not a tag, and no tag at all.
    result = run("create-changelog.py", "--root", str(tree), "--tag", "refs/heads/main")
    check(result.returncode != 0, "changelog: a branch ref is not a tag")
    result = run("create-changelog.py", "--root", str(tree))
    check(result.returncode != 0, "changelog: no tag is an error rather than an empty release")


def wheel(
    directory: Path,
    version: str,
    *,
    extra: bool,
    baseline: bool = True,
    plugins: tuple[str, ...] = (),
) -> Path:
    """A wheel holding what the staging tool reads, and nothing else.

    ``baseline`` writes the library the manifest names and ``plugins`` writes
    whatever else belongs in the plugin's directory, so a caller can build the
    wheel a CPU variant set produces and the wheels a broken one would.
    """
    path = directory / f"vapoursynth_imageseqs-{version}-py3-none-win_amd64.whl"
    prefix = "vapoursynth/plugins/imageseqs/"
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr(prefix + "manifest.vs", "[VapourSynth Manifest V1]\nvs_imageseqs\n")
        if baseline:
            archive.writestr(prefix + "vs_imageseqs.dll", b"the plugin")
        for name in plugins:
            archive.writestr(prefix + name, b"the plugin")
        if extra:
            archive.writestr(prefix + "only-in-the-first-wheel.txt", b"stale")
        archive.writestr("LICENSE", f"the license of {version}\n")
        archive.writestr("THIRD_PARTY_NOTICES", b"notices")
        archive.writestr("LICENSES/dav1d-COPYING.txt", b"bsd")
    return path


def check_staging() -> None:
    dist = SCRATCH / "dist"
    native = SCRATCH / "native"
    dist.mkdir(parents=True, exist_ok=True)
    native.mkdir(parents=True, exist_ok=True)
    (dist / "notes.txt").write_text("mine", encoding="utf-8")
    (native / "keep.txt").write_text("mine", encoding="utf-8")

    first = wheel(dist, "0.1.0", extra=True)
    result = run("stage-native.py", str(dist), str(native))
    check(result.returncode == 0, f"staging: the first build stages its wheel ({message_of(result)[:120]})")
    staged = native / "imageseqs"
    check((staged / "vs_imageseqs.dll").is_file(), "staging: the plugin is staged")
    check((staged / "manifest.vs").is_file(), "staging: the manifest is staged")
    check((native / "LICENSES/dav1d-COPYING.txt").is_file(), "staging: the legal files are staged")
    check((staged / "only-in-the-first-wheel.txt").is_file(), "staging: every file of the wheel is staged")

    # The second build of a new version, in the same checkout: the checkers
    # require one wheel, and this names the two rather than reporting a count.
    second = wheel(dist, "0.2.0", extra=False)
    result = run("stage-native.py", str(dist), str(native))
    check(result.returncode != 0, "staging: a second wheel in the same directory is an error")
    check(
        first.name in message_of(result) and second.name in message_of(result),
        f"staging: and the error names both wheels ({message_of(result)[:160]})",
    )
    check("build_output.py clear" in message_of(result), "staging: and says how to start over")

    # What a build clears is its own output, not everything beside it. It runs
    # before the build writes, so it clears every artifact the directory holds,
    # which is what makes the second wheel above impossible to begin with.
    removed = build_output.clear(dist)
    check(
        {path.name for path in removed} == {first.name, second.name},
        f"staging: clearing removes every wheel in the output ({[path.name for path in removed]})",
    )
    check((dist / "notes.txt").is_file(), "staging: and leaves a file it did not write")
    check(build_output.clear(dist) == [], "staging: clearing twice removes nothing the second time")

    # The next build writes its own artifact into the output it just cleared.
    second = wheel(dist, "0.2.0", extra=False)
    result = run("stage-native.py", str(dist), str(native))
    check(result.returncode == 0, f"staging: the new version stages after clearing ({message_of(result)[:120]})")
    check(not (staged / "only-in-the-first-wheel.txt").exists(), "staging: a file the new wheel dropped is gone")
    check(
        (native / "LICENSE"
    ).read_text(encoding="utf-8") == "the license of 0.2.0\n", "staging: and a changed file is replaced")
    check((native / "keep.txt").is_file(), "staging: a file the wheel does not write survives")

    # The tool can be run from the shell, which is how the Linux build uses it.
    shell = SCRATCH / "shell-clear"
    shell.mkdir(parents=True, exist_ok=True)
    wheel(shell, "0.1.0", extra=False)
    result = run("build_output.py", "clear", str(shell))
    check(result.returncode == 0, "staging: the clearing tool runs from the shell")
    check(
        "cleared 1 file(s)" in message_of(result), f"staging: and reports what it cleared ({message_of(result)[:120]})"
    )


def check_clearing() -> None:
    directory = SCRATCH / "clear"
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "a-0.1.0-py3-none-any.whl").write_bytes(b"wheel")
    (directory / "b-0.2.0-py3-none-any.whl").write_bytes(b"wheel")
    (directory / "c-0.1.0.tar.gz").write_bytes(b"source")
    (directory / "keep.txt").write_text("mine", encoding="utf-8")
    (directory / "subdir").mkdir(exist_ok=True)
    (directory / "subdir" / "d-0.1.0.whl").write_bytes(b"wheel")

    removed = build_output.clear(directory, ("*.whl",))
    check(len(removed) == 2, f"clearing: a pattern clears only what it matches ({len(removed)})")
    check((directory / "c-0.1.0.tar.gz").is_file(), "clearing: a source archive survives a wheel-only clear")
    check((directory / "keep.txt").is_file(), "clearing: and so does a file that is not a build output")
    check((directory / "subdir" / "d-0.1.0.whl").is_file(), "clearing: and so does a wheel in a subdirectory")

    removed = build_output.clear(directory)
    check(
        [path.name for path in removed] == ["c-0.1.0.tar.gz"],
        "clearing: the default patterns cover wheels and source archives",
    )

    # A directory that does not exist is not an error: the first build has none.
    check(build_output.clear(SCRATCH / "nowhere") == [], "clearing: a directory that does not exist is left alone")


def check_variants() -> None:
    """One library per CPU level, and the wheels that would lie about them.

    A wheel carries the baseline build plus a variant for each x86-64 level the
    build produces; VapourSynth picks between them by the host CPU, so the
    bundle has to carry all of them and the manifest has to name the stem
    alone. Both halves of that are what this checks.
    """
    dist = SCRATCH / "variant-dist"
    native = SCRATCH / "variant-native"
    dist.mkdir(parents=True, exist_ok=True)
    native.mkdir(parents=True, exist_ok=True)
    staged = native / "imageseqs"
    names = ("vs_imageseqs.dll", "vs_imageseqs.avx2.dll", "vs_imageseqs.avx512.dll")

    wheel(dist, "0.3.0", extra=False, plugins=names[1:])
    result = run("stage-native.py", str(dist), str(native))
    check(result.returncode == 0, f"variants: a variant wheel stages ({message_of(result)[:120]})")
    for name in names:
        check((staged / name).is_file(), f"variants: {name} is staged")
    check(
        (staged / "manifest.vs").read_text(encoding="utf-8")
        == "[VapourSynth Manifest V1]\nvs_imageseqs\n",
        "variants: the manifest names the stem and no variant",
    )
    # A library that is neither the stem nor a variant of it is not something
    # the manifest can have promised.
    # Staging it would put a file in the install tree no VapourSynth can name.
    build_output.clear(dist)
    stray = "vs_imageseqs_bad.dll"
    wheel(dist, "0.4.0", extra=False, plugins=(stray,))
    result = run("stage-native.py", str(dist), str(native))
    check(result.returncode != 0, "variants: an unexpected plugin library is an error")
    check(stray in message_of(result), f"variants: and the error names it ({message_of(result)[:120]})")
    # A manifest naming a stem the wheel does not hold is a wheel with no
    # plugin, however many variants of something else it carries.
    build_output.clear(dist)
    wheel(dist, "0.5.0", extra=False, baseline=False, plugins=(names[1],))
    result = run("stage-native.py", str(dist), str(native))
    check(result.returncode != 0, "variants: a manifest the wheel does not satisfy is an error")
    missing = "which the wheel does not hold"
    check(missing in message_of(result), f"variants: and says so ({message_of(result)[:120]})")


def check_bundled_staging() -> None:
    """Plugin validation must distinguish bundled codecs from manifest siblings."""
    prefix = "vapoursynth/plugins/imageseqs/"
    stem = "libvs_imageseqs"
    cases = (
        ("macos", "macosx_12_0_arm64", ".dylib", ("",),
         ("lib/libdav1d.7.dylib", "lib/libde265.0.2.1.dylib")),
        ("linux", "manylinux_2_28_x86_64", ".so", ("", ".avx2", ".avx512"),
         ("lib/libdav1d-test.so.7", "lib/libde265-test.so.0", "lib/libdav1d-unversioned.so")),
    )
    for platform, tag, extension, variants, dependencies in cases:
        dist = SCRATCH / f"{platform}-dist"
        native = SCRATCH / f"{platform}-native"
        dist.mkdir(parents=True, exist_ok=True)
        archive_path = dist / f"vapoursynth_imageseqs-0.3.0-py3-none-{tag}.whl"
        manifest = f"[VapourSynth Manifest V1]\n{stem}\n".encode()
        metadata = {prefix + "manifest.vs": manifest, "LICENSE": b"license"}
        bundled = {prefix + name: name.encode() for name in dependencies}
        plugins = {prefix + f"{stem}{variant}{extension}": variant.encode() or b"baseline"
                   for variant in variants}

        def write_archive(contents: dict[str, bytes]) -> None:
            with zipfile.ZipFile(archive_path, "w") as archive:
                for name, content in contents.items():
                    archive.writestr(name, content)

        write_archive({**metadata, **bundled, **plugins})
        result = run("stage-native.py", str(dist), str(native))
        check(result.returncode == 0,
              f"{platform}: bundled dependencies stage ({message_of(result)[:160]})")
        for name, content in {**metadata, **bundled, **plugins}.items():
            staged = native / name.removeprefix("vapoursynth/plugins/")
            check(staged.is_file() and staged.read_bytes() == content,
                  f"{platform}: {staged.relative_to(native)} is preserved byte for byte")

        # A matching filename inside lib/ cannot satisfy the manifest: the
        # core looks beside the manifest, not in the dependency directory.
        write_archive({**metadata, prefix + f"lib/{stem}{extension}": b"misplaced plugin"})
        result = run("stage-native.py", str(dist), str(native))
        check(result.returncode != 0 and "which the wheel does not hold" in message_of(result),
              f"{platform}: a nested plugin cannot replace the manifest's baseline")

        stray = f"other_plugin{extension}"
        write_archive({**metadata, **bundled, **plugins, prefix + stray: b"unexpected plugin"})
        result = run("stage-native.py", str(dist), str(native))
        check(result.returncode != 0 and stray in message_of(result),
              f"{platform}: an unexpected manifest sibling is still rejected")


def check_build_targets() -> None:
    """Check the actual hook without requiring Hatchling, Rust or a wheel."""
    # Only the hook's third-party imports are stubbed. Its build command,
    # environment and artifact lookup run unchanged, with Cargo simulated.
    names = (
        "hatchling", "hatchling.builders", "hatchling.builders.hooks",
        "hatchling.builders.hooks.plugin", "hatchling.builders.hooks.plugin.interface",
        "packaging", "packaging.tags",
    )
    imports = {name: ModuleType(name) for name in names}
    imports[names[4]].BuildHookInterface = object  # pyright: ignore[reportAttributeAccessIssue]
    imports["packaging"].tags = imports["packaging.tags"]  # pyright: ignore[reportAttributeAccessIssue]
    spec = importlib.util.spec_from_file_location("imageseqs_build_hook_check", ROOT / "hatch_build.py")
    assert spec is not None and spec.loader is not None
    hook = importlib.util.module_from_spec(spec)
    with patch.dict(sys.modules, imports):
        spec.loader.exec_module(hook)

    root = SCRATCH / "build-hook"
    root.mkdir()
    for host in ("x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"):
        for variant in hook.variants({"CARGO_BUILD_TARGET": host}):
            for explicit_target in (False, True):
                environment = {"CARGO": "selected-cargo", "RUSTC": "selected-rustc",
                               "CARGO_TARGET_DIR": "cargo-output", "RUSTFLAGS": "--cfg existing"}
                if explicit_target:
                    environment["CARGO_BUILD_TARGET"] = host
                original = dict(environment)
                effective = dict(environment)
                if variant.target_cpu:
                    effective["CARGO_BUILD_TARGET"] = host
                artifact = hook.release_directory(root, effective) / hook.base_plugin_filename(effective)
                artifact.parent.mkdir(parents=True, exist_ok=True)
                artifact.write_bytes(b"built plugin")
                with patch.object(hook.subprocess, "run") as compiler:
                    compiler.return_value.stdout = f"rustc test\nhost: {host}\n"
                    result = hook.build_plugin(root, environment, variant)
                calls = compiler.call_args_list
                command = ["selected-cargo", "build", "--release", "--locked"]
                if variant.target_cpu:
                    command.extend(["--target", host])
                label = f"build: {host} {variant.target_cpu or 'baseline'} target={explicit_target}"
                check(calls[-1].args[0] == command, f"{label} selects the correct Cargo target")
                expected_flags = "--cfg existing"
                if variant.target_cpu:
                    expected_flags += f" -C target-cpu={variant.target_cpu}"
                check(calls[-1].kwargs["env"]["RUSTFLAGS"] == expected_flags,
                      f"{label} preserves caller flags and selects the CPU level")
                discovery = bool(variant.target_cpu and not explicit_target)
                check(len(calls) == 1 + discovery
                      and (not discovery or calls[0].args[0] == ["selected-rustc", "--version", "--verbose"]),
                      f"{label} discovers the host only when needed")
                check(result == artifact and environment == original,
                      f"{label} finds the artifact without changing the caller environment")

    for existing in ("", "--cfg\x1fexisting"):
        environment = {"CARGO_BUILD_TARGET": "x86_64-unknown-linux-gnu",
                       "RUSTFLAGS": "ignored lower-priority flags", "CARGO_ENCODED_RUSTFLAGS": existing}
        artifact = hook.release_directory(root, environment) / hook.base_plugin_filename(environment)
        artifact.parent.mkdir(parents=True, exist_ok=True)
        artifact.write_bytes(b"encoded flags plugin")
        with patch.object(hook.subprocess, "run") as compiler:
            hook.build_plugin(root, environment, hook.Variant(".avx512", "x86-64-v4"))
        expected = (existing + "\x1f" if existing else "") + "-C\x1ftarget-cpu=x86-64-v4"
        check(compiler.call_args.kwargs["env"]["CARGO_ENCODED_RUSTFLAGS"] == expected,
              f"build: encoded flags {existing!r} retain precedence and receive the CPU level")

    with patch.object(hook.subprocess, "run") as compiler:
        compiler.return_value.stdout = "rustc test without a host\n"
        try:
            hook.build_plugin(root, {}, hook.Variant(".avx512", "x86-64-v4"))
        except RuntimeError as error:
            check("host target" in str(error) and compiler.call_count == 1,
                  "build: missing host information stops before an unsafe Cargo invocation")
        else:
            check(False, "build: missing host information must be refused")


def prepare_scratch() -> None:
    """Makes the scratch tree empty, without clearing anyone else's.

    The default is this repository's own build tree, which this check may clear.
    A tree named through `IMGSEQS_CHECK_TMP` is only cleared when it is empty,
    because it is somebody else's directory and the point of the option is to
    move this check rather than to give it a directory to delete.
    """
    if SCRATCH.exists():
        if SCRATCH == DEFAULT_SCRATCH:
            shutil.rmtree(SCRATCH)
        elif any(SCRATCH.iterdir()):
            raise SystemExit(
                f"{SCRATCH} is not empty; point IMGSEQS_CHECK_TMP at a directory "
                f"this check may clear, or remove it yourself"
            )
    SCRATCH.mkdir(parents=True, exist_ok=True)


def main() -> None:
    prepare_scratch()
    try:
        for test in (check_changelog, check_staging, check_variants, check_bundled_staging,
                     check_clearing, check_build_targets):
            print(f"--- {test.__name__}")
            test()
    finally:
        if not FAILURES:
            shutil.rmtree(SCRATCH, ignore_errors=True)

    if FAILURES:
        print(f"\n{len(FAILURES)} check(s) failed; {SCRATCH} is kept")
        raise SystemExit(1)
    print("\nall checks passed")


if __name__ == "__main__":
    main()
