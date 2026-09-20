from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

from hatchling.builders.hooks.plugin.interface import BuildHookInterface
from packaging import tags

PLUGIN_FILENAME_BY_PLATFORM = {
    "win32": "vs_imageseqs.dll",
    "darwin": "libvs_imageseqs.dylib",
}


def plugin_filename(environment: dict[str, str]) -> str:
    target = environment.get("CARGO_BUILD_TARGET", "").lower()
    if "windows" in target or "mingw" in target:
        return "vs_imageseqs.dll"
    if "darwin" in target or "apple" in target:
        return "libvs_imageseqs.dylib"
    return PLUGIN_FILENAME_BY_PLATFORM.get(sys.platform, "libvs_imageseqs.so")


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


def build_plugin(root: Path, environment: dict[str, str]) -> Path:
    cargo = environment.get("CARGO", "cargo")
    try:
        subprocess.run(
            [cargo, "build", "--release", "--locked"],
            cwd=root,
            env=environment,
            check=True,
        )
    except FileNotFoundError as error:
        raise RuntimeError("Cargo is required to build the VapourSynth plugin") from error
    except subprocess.CalledProcessError as error:
        raise RuntimeError("Cargo failed while building the VapourSynth plugin") from error

    artifact = release_directory(root, environment) / plugin_filename(environment)
    if not artifact.is_file():
        raise RuntimeError(f"Cargo completed but did not produce {artifact}")
    return artifact


def wheel_platform_tag() -> str:
    platform_tags = tuple(tags.platform_tags())
    if sys.platform == "linux":
        # packaging 26.3 puts the generic linux tag before the manylinux and
        # musllinux tags. PyPI does not accept that generic tag for uploads.
        for platform in platform_tags:
            if platform.startswith(("manylinux_", "musllinux_")):
                return platform
        raise RuntimeError("could not determine a manylinux or musllinux tag")
    return platform_tags[0]


# Do not subscript ``BuildHookInterface``: hatchling 1.27-1.32.2 declare it
# with one type parameter and 1.32.3 added a second one, so any fixed
# subscript makes the hook unloadable for the other releases. The plain class
# is accepted by every version and the hook never needs the specialization.
class NativePluginHook(BuildHookInterface):  # type: ignore[type-arg]
    """Build the Cargo plugin and place it in VapourSynth's plugin tree."""

    plugin_directory = Path("vapoursynth") / "plugins"

    def initialize(self, version: str, build_data: dict[str, object]) -> None:
        root = Path(self.root)
        environment = cargo_environment(root)
        artifact = build_plugin(root, environment)

        destination_directory = root / self.plugin_directory
        destination_directory.mkdir(parents=True, exist_ok=True)
        staged_plugin = destination_directory / artifact.name
        shutil.copy2(artifact, staged_plugin)

        force_include = build_data.setdefault("force_include", {})
        if not isinstance(force_include, dict):
            raise TypeError("Hatch build data force_include must be a mapping")
        force_include[str(staged_plugin)] = str(
            self.plugin_directory / artifact.name
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
        build_data["tag"] = f"py3-none-{wheel_platform_tag()}"

    def finalize(
        self,
        version: str,
        build_data: dict[str, object],
        artifact_path: str,
    ) -> None:
        del version, build_data, artifact_path
        shutil.rmtree(
            Path(self.root) / self.plugin_directory.parent,
            ignore_errors=True,
        )
