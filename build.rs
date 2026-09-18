//! Finds the native library the plugin links against.
//!
//! webp is decoded by libwebp instead of by the pure rust decoder in `image`
//! (see `src/formats/webp.rs`), so the library has to be located at build time.
//! Windows builds take it from the vcpkg tree the other native dependencies
//! already come from; everywhere else it is located with `pkg-config`.
//!
//! On the `pkg-config` platforms the static archives are preferred, so a plugin
//! built there does not need the distribution's `libwebp` at run time. The link
//! flags are emitted here rather than by `pkg_config::Config::probe`, which
//! keeps a library shared whenever its archive sits under a system prefix such
//! as `/usr/lib`: that is where distributions install `libwebp.a`. A library
//! whose archive is missing keeps the shared form, so a machine that only has
//! the shared library still links.

/// Oldest libwebp the entry points in `src/formats/webp.rs` are written against.
#[cfg(not(windows))]
const MINIMUM_VERSION: &str = "1.2.0";

/// Libraries the platform itself provides. They stay shared: a second copy of
/// them beside the one the loader already uses is not what this build wants,
/// and not every system ships an archive for them (macos has no `libm.a`).
#[cfg(not(windows))]
const SYSTEM_LIBRARIES: &[&str] = &["c", "m", "pthread", "dl", "rt", "gcc_s", "stdc++"];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    vcpkg::find_package("libwebp").expect(
        "libwebp is not installed for the target triplet; run the vcpkg install in AGENTS.md",
    );

    #[cfg(not(windows))]
    link_unix_libwebp();
}

/// Links libwebp through the paths and libraries `pkg-config` reports.
///
/// Asking for the static flags also names the packages libwebp's archive itself
/// needs, such as libsharpyuv, so a distribution that installs `libwebp.pc` but
/// not those packages fails here whichever flags are asked for: `pkg-config`
/// resolves the same dependency graph either way and reports the missing
/// package.
#[cfg(not(windows))]
fn link_unix_libwebp() {
    // `cargo_metadata(false)` suppresses the link flags the crate would emit
    // while its environment tracking stays on, so a changed `PKG_CONFIG_PATH`
    // still rebuilds.
    let library = pkg_config::Config::new()
        .cargo_metadata(false)
        .atleast_version(MINIMUM_VERSION)
        .statik(true)
        .probe("libwebp")
        .expect("libwebp development files are not installed, see the build section of README.md");

    for directory in &library.link_paths {
        println!("cargo:rustc-link-search=native={}", directory.display());
    }

    let mut webp_is_static = false;
    for name in library_names(&library) {
        let archive = !SYSTEM_LIBRARIES.contains(&name) && archive_exists(&library, name);
        if name == "webp" {
            webp_is_static = archive;
        }
        if archive {
            println!("cargo:rustc-link-lib=static={name}");
        } else {
            println!("cargo:rustc-link-lib={name}");
        }
    }

    if !webp_is_static {
        println!(
            "cargo:warning=libwebp is linked from the shared library and will be needed at run time; install the development package (libwebp-dev on debian and ubuntu, webp on macos with homebrew) to link the archive instead"
        );
    }
}

/// Names of the libraries to link, in the order `pkg-config` reported them and
/// without the repeats a dependency can introduce.
#[cfg(not(windows))]
fn library_names(library: &pkg_config::Library) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    for name in &library.libs {
        if !names.contains(&name.as_str()) {
            names.push(name);
        }
    }
    names
}

/// Whether `name` has an archive among the directories `pkg-config` reported.
#[cfg(not(windows))]
fn archive_exists(library: &pkg_config::Library, name: &str) -> bool {
    library
        .link_paths
        .iter()
        .any(|directory| directory.join(format!("lib{name}.a")).is_file())
}
