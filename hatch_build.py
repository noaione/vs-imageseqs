from __future__ import annotations

import hashlib
import os
import platform
import shutil
import subprocess
import sys
import sysconfig
from pathlib import Path
from typing import NamedTuple

from hatchling.builders.hooks.plugin.interface import BuildHookInterface
from packaging import tags

PLUGIN_FILENAME_BY_PLATFORM = {
    "win32": "vs_imageseqs.dll",
    "darwin": "libvs_imageseqs.dylib",
}

#: x86-64 microarchitecture levels. One row of [`variants`] per level, and a
#: level only means something on x86-64: naming one for another target makes
#: rustc warn that the processor is not recognized and ignore it, so an arm64
#: build passes no flag at all.
X86_64_V3 = "x86-64-v3"
X86_64_V4 = "x86-64-v4"


class Variant(NamedTuple):
    """One CPU optimization level of the plugin."""

    #: Suffix between the plugin's stem and its extension, empty for the
    #: baseline build. VapourSynth appends this itself when the host CPU
    #: supports the level, so a variant is never named in the manifest.
    suffix: str
    #: Value for cargo's `-C target-cpu`, or `None` to pass no flag at all,
    #: which is what makes the baseline build run anywhere.
    target_cpu: str | None


def base_plugin_filename(environment: dict[str, str]) -> str:
    """The name cargo writes for every variant of the library."""
    target = environment.get("CARGO_BUILD_TARGET", "").lower()
    if "windows" in target or "mingw" in target:
        return "vs_imageseqs.dll"
    if "darwin" in target or "apple" in target:
        return "libvs_imageseqs.dylib"
    return PLUGIN_FILENAME_BY_PLATFORM.get(sys.platform, "libvs_imageseqs.so")


def plugin_stem(environment: dict[str, str]) -> str:
    """The stem the manifest names, which is what a variant suffix follows."""
    return base_plugin_filename(environment).rsplit(".", 1)[0]


def plugin_extension(environment: dict[str, str]) -> str:
    """The platform's library extension, leading dot included."""
    return f".{base_plugin_filename(environment).rsplit('.', 1)[1]}"


def plugin_filename(environment: dict[str, str], variant: Variant) -> str:
    """The name one variant is staged as."""
    return f"{plugin_stem(environment)}{variant.suffix}{plugin_extension(environment)}"


def target_triple(environment: dict[str, str]) -> str:
    return environment.get("CARGO_BUILD_TARGET", "").lower()


def is_x86_64(environment: dict[str, str]) -> bool:
    """Whether this build targets x86-64, the one target with levels to name."""
    triple = target_triple(environment)
    if triple:
        return "x86_64" in triple or "amd64" in triple
    return sysconfig.get_platform().startswith(("win-amd64", "linux-x86_64"))


def variants(environment: dict[str, str]) -> list[Variant]:
    """The builds this host ships, baseline first.

    The baseline names no level, so it runs wherever the crate builds. The
    wider ones are only loaded by a host whose CPU has the level, which is
    what makes carrying them safe; the measurements are in
    `docs/improvements/23-cpu-variant-avx2.md` and
    `docs/improvements/24-cpu-variant-avx512.md`.
    """
    if not is_x86_64(environment):
        return [Variant("", None)]
    return [
        Variant("", None),
        Variant(".avx2", X86_64_V3),
        Variant(".avx512", X86_64_V4),
    ]


def manifest(environment: dict[str, str]) -> str:
    """The `manifest.vs` for this plugin.

    It names the plugin's stem alone: VapourSynth appends the variant suffix
    itself, looking for `<stem>.<variant><extension>`, so no variant is named
    here.
    """
    return f"[VapourSynth Manifest V1]\n{plugin_stem(environment)}\n"


def release_directory(root: Path, environment: dict[str, str]) -> Path:
    target_dir = Path(environment.get("CARGO_TARGET_DIR", root / "target"))
    if not target_dir.is_absolute():
        target_dir = root / target_dir

    cargo_target = environment.get("CARGO_BUILD_TARGET")
    if cargo_target:
        target_dir /= cargo_target
    return target_dir / "release"


def cargo_environment(root: Path) -> dict[str, str]:
    environment = os.environ.copy()

    if sys.platform == "darwin":
        # Keep the embedded libheif build independent of optional codecs found
        # on the build host. The content-addressed path makes Cargo rerun the
        # dependency build script when these options change. Respect an
        # explicit toolchain selected by callers.
        toolchain = root / "tools" / "macos-libheif-toolchain.cmake"
        toolchain_hash = hashlib.sha256(toolchain.read_bytes()).hexdigest()[:16]
        generated_toolchain = (
            root / "target" / "macos-native" / "toolchains" / f"{toolchain_hash}.cmake"
        )
        generated_toolchain.parent.mkdir(parents=True, exist_ok=True)
        if not generated_toolchain.is_file():
            shutil.copy2(toolchain, generated_toolchain)
        environment.setdefault(
            "CMAKE_TOOLCHAIN_FILE",
            str(generated_toolchain),
        )

    # cargo-vcpkg exposes the repository-local installed packages through
    # this generated vcpkg root. Respect an explicit VCPKG_ROOT when the
    # caller has selected another installation.
    local_vcpkg_root = root / "target" / "vcpkg-root"
    if sys.platform == "win32" and "VCPKG_ROOT" not in environment and local_vcpkg_root.is_dir():
        environment["VCPKG_ROOT"] = str(local_vcpkg_root)
        environment.setdefault("VCPKGRS_TRIPLET", "x64-windows-static-md")

        # dav1d-sys discovers dav1d through pkg-config rather than vcpkg-rs.
        # Put the repository-local pkg-config directory first so a globally
        # configured vcpkg installation cannot leak a different triplet or
        # version into the wheel.
        triplet = environment["VCPKGRS_TRIPLET"]
        pkgconfig = local_vcpkg_root / "installed" / triplet / "lib" / "pkgconfig"
        if pkgconfig.is_dir():
            existing_pkgconfig = environment.get("PKG_CONFIG_PATH")
            environment["PKG_CONFIG_PATH"] = os.pathsep.join(
                part for part in (str(pkgconfig), existing_pkgconfig) if part
            )

    return environment


def build_plugin(root: Path, environment: dict[str, str], variant: Variant) -> Path:
    """Builds one CPU variant and answers the artifact cargo wrote.

    Cargo writes the same file name for every variant, so the answer is the
    baseline's name whichever variant was asked for; the caller stages it under
    the variant's own name.
    """
    cargo = environment.get("CARGO", "cargo")
    build_environment = dict(environment)
    if variant.target_cpu is not None:
        # RUSTFLAGS is part of cargo's fingerprint, so a variant recompiles the
        # whole dependency graph rather than only this crate.
        existing = build_environment.get("RUSTFLAGS", "").strip()
        flag = f"-C target-cpu={variant.target_cpu}"
        build_environment["RUSTFLAGS"] = f"{existing} {flag}".strip()
    try:
        subprocess.run(
            [cargo, "build", "--release", "--locked"],
            cwd=root,
            env=build_environment,
            check=True,
        )
    except FileNotFoundError as error:
        raise RuntimeError("Cargo is required to build the VapourSynth plugin") from error
    except subprocess.CalledProcessError as error:
        level = variant.target_cpu or "the baseline"
        raise RuntimeError(f"Cargo failed while building {level}") from error

    artifact = release_directory(root, environment) / base_plugin_filename(environment)
    if not artifact.is_file():
        raise RuntimeError(f"Cargo completed but did not produce {artifact}")
    return artifact


def macos_architecture(environment: dict[str, str]) -> str:
    target = environment.get("CARGO_BUILD_TARGET", "").lower()
    if "aarch64" in target or "arm64" in target:
        return "arm64"
    if "x86_64" in target:
        return "x86_64"

    machine = platform.machine().lower()
    if machine in {"arm64", "aarch64"}:
        return "arm64"
    if machine == "x86_64":
        return "x86_64"
    raise ValueError(f"unsupported macOS wheel architecture: {machine or 'unknown'}")


def wheel_platform_tag(environment: dict[str, str]) -> str:
    if sys.platform == "linux":
        # A build host's supported tags do not certify the plugin's ABI or
        # external libraries. Only auditwheel may give Linux release wheels
        # their manylinux tag, after checking and bundling those dependencies.
        return sysconfig.get_platform().replace("-", "_").replace(".", "_")
    if sys.platform == "darwin" and environment.get("MACOSX_DEPLOYMENT_TARGET"):
        architecture = macos_architecture(environment)
        version = environment["MACOSX_DEPLOYMENT_TARGET"].split(".", maxsplit=1)
        if len(version) != 2 or not all(part.isdigit() for part in version):
            raise ValueError("MACOSX_DEPLOYMENT_TARGET must be a major.minor version")
        return f"macosx_{version[0]}_{version[1]}_{architecture}"
    return next(tags.platform_tags())


# Do not subscript ``BuildHookInterface``: hatchling 1.27-1.32.2 declare it
# with one type parameter and 1.32.3 added a second one, so any fixed
# subscript makes the hook unloadable for the other releases. The plain class
# is accepted by every version and the hook never needs the specialization.
class NativePluginHook(BuildHookInterface):  # type: ignore[type-arg]
    """Build the Cargo plugin and place it in VapourSynth's plugin tree."""

    plugin_directory = Path("vapoursynth") / "plugins" / "imageseqs"

    def initialize(self, version: str, build_data: dict[str, object]) -> None:
        del version
        root = Path(self.root)
        environment = cargo_environment(root)

        force_include = build_data.setdefault("force_include", {})
        if not isinstance(force_include, dict):
            raise TypeError("Hatch build data force_include must be a mapping")

        destination_directory = root / self.plugin_directory
        destination_directory.mkdir(parents=True, exist_ok=True)

        built = variants(environment)
        for variant in built:
            artifact = build_plugin(root, environment, variant)
            staged_plugin = destination_directory / plugin_filename(environment, variant)
            shutil.copy2(artifact, staged_plugin)
            force_include[str(staged_plugin)] = str(
                self.plugin_directory / staged_plugin.name
            )

        # Cargo writes one name for every variant, so the release directory is
        # left holding whichever was built last. Put the baseline back: it is
        # what a plain `cargo build` produces and what the benchmarks and the
        # Linux build's own readelf check read from that path.
        baseline = destination_directory / plugin_filename(environment, built[0])
        shutil.copy2(
            baseline,
            release_directory(root, environment) / base_plugin_filename(environment),
        )

        manifest_path = destination_directory / "manifest.vs"
        manifest_path.write_text(manifest(environment), encoding="utf-8", newline="\n")
        force_include[str(manifest_path)] = str(
            self.plugin_directory / manifest_path.name
        )

        # Keep the license and attribution files beside the native artifact in
        # every wheel. Hatch's normal package selection does not include
        # repository-level files or arbitrary license directories.
        force_include[str(root / "LICENSE")] = "LICENSE"
        force_include[str(root / "THIRD_PARTY_NOTICES")] = "THIRD_PARTY_NOTICES"
        for license_file in (root / "LICENSES").rglob("*"):
            if license_file.is_file():
                relative = license_file.relative_to(root).as_posix()
                force_include[str(license_file)] = relative

        # The wheel contains a native plugin, so it must not be tagged as a
        # universal pure-Python wheel.
        build_data["pure_python"] = False
        build_data["tag"] = f"py3-none-{wheel_platform_tag(environment)}"

    def finalize(
        self,
        version: str,
        build_data: dict[str, object],
        artifact_path: str,
    ) -> None:
        del version, build_data, artifact_path
        shutil.rmtree(
            Path(self.root) / "vapoursynth",
            ignore_errors=True,
        )
