//! The one link argument this crate's own build has to make.
//!
//! Nothing is located here any more. Every native library the plugin uses
//! arrives through a crate whose own build script finds it -- dav1d, libde265
//! and libheif among them -- and the webp decoder is the `wpd` crate, whose
//! assembly its own build script drives. What is left is a property of this
//! crate's own link on ELF, which no other build script can be asked for.

/// Resolves the references the plugin makes to its own definitions to the
/// definitions it links in, rather than to whatever the dynamic linker finds
/// first when it loads the plugin.
///
/// An ELF shared object needs this because of how `wpd` reaches its gamma
/// tables. Its x86 and x86-64 decode routines are NASM sources, and a
/// rip-relative load is what they use (`R_X86_64_PC32`). rustc writes a
/// cdylib's version script from every `#[no_mangle]` symbol in the crate graph,
/// so the two tables `wpd` exports are exported here too, which makes them
/// preemptible; a preemptible symbol is not something that relocation can name
/// from a shared object, and both link editors refuse the link with
/// `relocation R_X86_64_PC32 cannot be used against symbol ...; recompile with
/// -fPIC`. Binding the references locally is what the relocation needs, and it
/// is what a plugin wants anyway: its own copy of a library is the one its own
/// code should call. `-Bsymbolic-functions` does not cover this, because the
/// symbols are data.
///
/// Neither other format has the problem. A PE image resolves a relative
/// reference to a definition in its own module, and Mach-O binds a definition
/// to the image that holds it unless the link asks for interposition, so the
/// apple link needs no flag and gets none.
#[cfg(all(unix, not(target_vendor = "apple")))]
fn bind_definitions_locally() {
    // Named for the cdylib, so the flag reaches the shared object and no
    // other link: an executable defines symbols its own code can already
    // reach.
    println!("cargo:rustc-link-arg-cdylib=-Wl,-Bsymbolic");
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // The cfg is the function's own, so only an ELF link reaches it.
    #[cfg(all(unix, not(target_vendor = "apple")))]
    bind_definitions_locally();
}
