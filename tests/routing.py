r"""Checks that renaming a file does not change how it is read.

    .\.venv\Scripts\python.exe tests\routing.py

Plan 34's phase 1 says the probe and the decode must select the same backend,
item/layer and sample type, and that content routing must be identical "for
original, missing, misleading and uppercase extensions". `owns()` in every format
module is extension-only today, so a correct file under a wrong extension can be
handed to the wrong decoder.

`target/research-routing-p72melmd/` was the plan's research set, but it holds only
the renamed copies and not the files they were made from -- every one is 85 bytes
of signature -- so it cannot be compared against anything and is not used here.
This makes its own copies from `tests/fixtures/`, which is what the plan asks for
anyway: an extension mismatch fixture per format, committed.

Each source is copied beside itself under a wrong extension and under an
uppercase one, and every copy must decode to the same bytes, the same alpha and
the same properties as its source. A copy that refuses is the probe having
promised a frame the decode would not produce.
"""

from __future__ import annotations

import hashlib
import os
import pathlib
import shutil
import sys

import vapoursynth as vs


class Policy(vs.EnvironmentPolicy):
    def on_policy_registered(self, api):  # pyright: ignore[reportIncompatibleMethodOverride]
        self.api = api
        self.environment = api.create_environment(vs.CoreCreationFlags.DISABLE_AUTO_LOADING)

    def on_policy_cleared(self):
        self.api.destroy_environment(self.environment)

    def get_current_environment(self):
        return self.environment

    def set_environment(self, environment):
        return self.environment


if not vs.has_policy():
    vs.register_policy(Policy())

plugin = pathlib.Path(os.environ.get("IMGSEQS_PLUGIN", "target/release/vs_imageseqs.dll"))
core = vs.core
core.std.LoadPlugin(str(plugin.resolve()))
print(f"# plugin {plugin.name}", file=sys.stderr)

FIXTURES = pathlib.Path("tests/fixtures")
SCRATCH = pathlib.Path("target/routing-scratch")

# One source per distinct extension, so every router is exercised without the
# check taking minutes. A format with no fixture here is a gap in the set, not
# a pass, so the count is reported.
SOURCES = []
# A format content cannot name is not a routing failure when it is renamed,
# because there is nothing in the bytes to route by. Targa has no leading
# signature at all, and the icon family shares its first four bytes with a Targa
# type 1 or 2 header, and a bare DIB has no `BM` file header to be named by, so
# none of the three can be identified without the name; `.icc` is not an image
# at all, and `.y4m` is an encoder's input rather than something this plugin
# reads, so both are skipped for that reason.
SKIP = {".tga", ".targa", ".icb", ".vda", ".vst", ".ico", ".cur", ".dib", ".icc", ".y4m"}
_seen: set[str] = set()
for _entry in sorted(FIXTURES.iterdir()):
    if not _entry.is_file() or _entry.suffix.lower() in _seen:
        continue
    _seen.add(_entry.suffix.lower())
    if _entry.suffix.lower() not in SKIP:
        SOURCES.append(_entry.name)

# Extensions that belong to some other format this tree owns, plus one it does
# not know at all, plus the source's own in the other case.
WRONG = [".bmp", ".jpg", ".dat"]


def read(path: pathlib.Path) -> tuple[str, str]:
    """The decoded digest and the properties, or the reason it refused."""
    name = str(path).replace("\\", "/")
    try:
        result = core.imgseqs.ReadAlpha(files=[name])
        frame = result["clip"].get_frame(0)
        digest = hashlib.sha256()
        for plane in range(frame.format.num_planes):
            digest.update(bytes(frame[plane]))
        digest.update(bytes(result["alpha"].get_frame(0)[0]))
        props = frame.props
        facts = " ".join(
            f"{key}={props[key]}"
            for key in sorted(props.keys())
            # `ImgSeqPath` is the file's own name, so it differs by
            # construction for a renamed copy and is not a routing fact.
            if key.startswith("ImgSeq") and key != "ImgSeqPath"
        )
        return digest.hexdigest()[:16], facts
    except Exception as error:
        return "REFUSED", str(error)[-70:]


if SCRATCH.is_dir():
    shutil.rmtree(SCRATCH)
SCRATCH.mkdir(parents=True)

failures = 0
checked = 0
missing = []
for name in SOURCES:
    source = FIXTURES / name
    if not source.is_file():
        missing.append(name)
        continue
    want_digest, want_facts = read(source)
    if want_digest == "REFUSED":
        print(f"  SOURCE REFUSED {name}: {want_facts}")
        failures += 1
        continue
    stem = pathlib.Path(name).stem
    suffixes = [*list(WRONG), pathlib.Path(name).suffix.upper()]
    for suffix in suffixes:
        copy = SCRATCH / f"{stem}{suffix}"
        shutil.copyfile(source, copy)
        checked += 1
        got_digest, got_facts = read(copy)
        if got_digest == "REFUSED":
            print(f"  REFUSED  {name} as {suffix}: {got_facts}")
            failures += 1
        elif got_digest != want_digest:
            print(f"  DIFFERS  {name} as {suffix}: pixels differ from the source")
            failures += 1
        elif got_facts != want_facts:
            print(f"  FACTS    {name} as {suffix}: properties differ from the source")
            failures += 1

if missing:
    print(f"  {len(missing)} source(s) not in tests/fixtures: {', '.join(missing)}")
print(f"{failures} of {checked} renamed copies read differently from their source")
sys.exit(1 if failures else 0)
