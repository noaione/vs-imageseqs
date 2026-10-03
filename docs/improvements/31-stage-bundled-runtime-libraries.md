# 31 - stage bundled runtime libraries

status: implemented, 2026-10-03. Packaging-tool regression checks pass locally
on Windows using macOS and Linux wheel layouts. Remote CI and native macOS/Linux
execution were not rerun in this session.

## failure and fix

macOS CI finished building its wheel but failed in `tools/stage-native.py`:

```text
unexpected plugin library in the wheel: vapoursynth/plugins/imageseqs/lib/libdav1d.7.dylib
```

`plugin_libraries` checked every `.dll`, `.so` or `.dylib` anywhere below
`imageseqs/`. It therefore treated a bundled codec in `imageseqs/lib/` as a
plugin and required its filename to match the manifest's plugin stem.

The check now examines only libraries whose immediate parent is the manifest's
`imageseqs/` directory. The baseline and CPU variants must still match the
manifest stem, unexpected sibling libraries are still refused, and a matching
filename nested inside `lib/` cannot stand in for the required baseline plugin.
The existing staging code continues to copy the whole plugin directory,
including its bundled runtime libraries and legal files.

Linux's current versioned codec names end in `.so.7` or `.so.0`, so their final
suffix already escaped the old `.so` check. An unversioned `.so` dependency
would trigger the same bug. Tests cover both forms. Dependency allowlists and
loader-path checks remain in the existing platform wheel checkers.

There is no change to frames, formats, properties, arguments, wheel contents,
native linkage or licenses. Under the repository's changelog rule this tooling
repair is recorded here and in the index, without a changelog entry.

## validation

The old 55-check packaging suite passed before edits, showing the missing
coverage. The 19 new checks reproduced the macOS error and the unversioned Linux
case before changing the tool. They also exposed the misplaced-plugin case.
After the fix, all 74 checks in `tests/check-packaging-tools.py` pass:

- macOS's baseline dylib and versioned dav1d/libde265 dependencies stage.
- Linux's baseline, AVX2 and AVX512 plugins plus versioned and unversioned
  dependencies stage.
- Every supplied manifest, plugin, dependency and license is byte-identical
  after staging.
- A baseline only inside `lib/` is refused, as is an unexpected library beside
  the manifest.
- Existing Windows staging, repeated-build cleanup and release metadata checks
  continue to pass.

Python AST and `git diff HEAD --check` pass. The full native validator passed
all 580 checks before the release build, again after that build, and after the
tooling edits, with no warnings or failing checks. Release builds pass before
and after. The DLL SHA-256 is identical throughout:
`74f9f2c5322621e125d5ee56d239a30d767239f74b22c724a93df6ace5b1e031`.

The [BENCH.md](../BENCH.md) harness ran before and after on all 35
`sandbox/png` files with `--imgseqs-only --reps 3 --extra --prefetch 16`,
explicitly loading `target/release/vs_imageseqs.dll`:

| configuration | before frame totals (s) | after frame totals (s) |
| --- | --- | --- |
| default | 0.508, 0.385, 0.374 | 0.543, 0.379, 0.397 |
| `prefetch=0` | 1.077, 0.890, 0.789 | 1.321, 0.980, 1.135 |
| `prefetch=16` | 0.282, 0.282, 0.259 | 0.388, 0.316, 0.300 |

These are measurements of the same binary, so batch differences do not show a
decoder regression caused by this Python staging fix. No speed or memory
improvement is claimed. Future decoder changes still require paired baseline
comparisons and investigation of significant regressions, as described in
[26](26-remove-image-rs.md).

Logs are retained under ignored `target/stage-native-*.log`: `before` and
`after` for packaging, `reproduction` for the old tool with new cases,
`validator-{before,release-before,after}`, `build-{before,after}` and
`bench-{before,after}`. Native wheel rebuilding is not required for a change
that alters neither wheel contents nor legal files.
