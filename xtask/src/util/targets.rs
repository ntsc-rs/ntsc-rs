//! Contains information on the various build targets how their build artifacts are named.

use std::error::Error;

#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    /// Cargo target triple for this target
    pub target_triple: &'static str,
    /// OpenFX architecture string for this target
    /// https://openfx.readthedocs.io/en/master/Reference/ofxPackaging.html#installation-directory-hierarchy
    pub ofx_architecture: &'static str,
    /// File extension for a dynamic library on this platform, excluding the leading dot
    pub library_extension: &'static str,
    /// Prefix for the library output filename. Platform-dependant; thanks Cargo!
    /// On Unix, it's "lib", so e.g. "foo" becomes "libfoo.so" or "libfoo.dylib". On Windows, there's no prefix, so it
    /// would just be "foo.dll".
    pub library_prefix: &'static str,
}

impl Target {
    pub fn from_triple(triple: &str) -> Result<&'static Self, Box<dyn Error>> {
        Ok(TARGETS
            .iter()
            .find(|candidate_target| candidate_target.target_triple == triple)
            .ok_or_else(|| format!("Your target \"{}\" is not supported", triple))?)
    }
}

// "Supported" target triples
pub const TARGETS: &[Target] = &[
    Target {
        target_triple: "x86_64-unknown-linux-gnu",
        ofx_architecture: "Linux-x86-64",
        library_extension: "so",
        library_prefix: "lib",
    },
    Target {
        target_triple: "i686-unknown-linux-gnu",
        ofx_architecture: "Linux-x86",
        library_extension: "so",
        library_prefix: "lib",
    },
    Target {
        target_triple: "x86_64-pc-windows-msvc",
        ofx_architecture: "Win64",
        library_extension: "dll",
        library_prefix: "",
    },
    Target {
        target_triple: "i686-pc-windows-msvc",
        ofx_architecture: "Win32",
        library_extension: "dll",
        library_prefix: "",
    },
    Target {
        target_triple: "x86_64-apple-darwin",
        ofx_architecture: "MacOS",
        library_extension: "dylib",
        library_prefix: "lib",
    },
    Target {
        target_triple: "aarch64-apple-darwin",
        ofx_architecture: "MacOS",
        library_extension: "dylib",
        library_prefix: "lib",
    },
];

pub const LINUX_X86_64: &Target = &TARGETS[0];
pub const LINUX_X86: &Target = &TARGETS[1];
pub const WINDOWS_X86_64: &Target = &TARGETS[2];
pub const WINDOWS_I686: &Target = &TARGETS[3];
pub const MACOS_X86_64: &Target = &TARGETS[4];
pub const MACOS_AARCH64: &Target = &TARGETS[5];
