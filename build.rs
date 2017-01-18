extern crate gcc;
#[macro_use] extern crate lazy_static;
extern crate regex;
extern crate semver;

use regex::Regex;
use semver::Version;
use std::env;
use std::ffi::OsStr;
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::process::Command;

lazy_static!{
    /// LLVM version used by this version of the crate.
    static ref CRATE_VERSION: Version = {
        let crate_version = Version::parse(env!("CARGO_PKG_VERSION"))
            .expect("Crate version is somehow not valid semver");
        Version {
            major: crate_version.major / 10,
            minor: crate_version.major % 10,
            .. crate_version
        }
    };

    /// Filesystem path to an llvm-config binary for the correct version.
    static ref LLVM_CONFIG_PATH: PathBuf = {
        // Try llvm-config via PATH first.
        if let Some(name) = locate_system_llvm_config() {
            return name.into();
        } else {
            println!("Didn't find usable system-wide LLVM.");
        }

        // Did the user give us a binary path to use? If yes, try
        // to use that and fail if it doesn't work.
        let binary_prefix_var = format!("LLVM_SYS_{}_PREFIX",
                                        env!("CARGO_PKG_VERSION_MAJOR"));
        if let Some(path) = env::var_os(&binary_prefix_var) {
            let mut pb: PathBuf = path.into();
            pb.push("bin");
            pb.push("llvm-config");

            let ver = llvm_version(&pb)
                .expect(&format!("Failed to execute {:?}", &pb));
            if is_compatible_llvm(&ver) {
                return pb;
            } else {
                panic!("LLVM binaries specified by {} are the wrong version.
                        (Found {}, need {}.)", binary_prefix_var, ver, *CRATE_VERSION);
            }
        } else {
            println!("{} not set, not using precompiled binaries", binary_prefix_var);
        }

        // No binaries. Offer to download and compile a blessed version,
        // but only with positive confirmation. It's a fairly large download,
        // takes a while and could get wiped out by a `cargo clean` so it's
        // useful if doing an install but is in general not recommended.
        let autobuild_var = format!("LLVM_SYS_{}_AUTOBUILD",
                                    env!("CARGO_PKG_VERSION_MAJOR"));
        match env::var_os(&autobuild_var) {
            Some(ref x) => {
                if x == "YES" {
                    unimplemented!();
                } else {
                    println!("{} must be exactly \"YES\" to enable autobuild (is {:?})",
                             autobuild_var, x);
                }
            }
            None => {
                println!("{} not set, will not automatically compile LLVM",
                         autobuild_var);
            }
        }

        panic!("Could not find a compatible version of LLVM");
    };
}

/// Try to find a system-wide version of llvm-config that is compatible with
/// this crate.
///
/// Returns None on failure.
fn locate_system_llvm_config() -> Option<&'static str> {
    match llvm_version("llvm-config") {
        Ok(ref version) if is_compatible_llvm(version) => {
            // Compatible version found. Nice.
            return Some("llvm-config");
        },
        Ok(version) => {
            // Version mismatch. Will try further searches, but warn that
            // we're not using the system one.
            println!("Found LLVM version {} on PATH, but need {}.",
                     version, *CRATE_VERSION);
        },
        Err(ref e) if e.kind() == ErrorKind::NotFound => {
            // Looks like we failed to execute any llvm-config. Keep
            // searching.
        },
        // Some other error, probably a weird failure. Give up.
        Err(e) => panic!("Failed to search PATH for llvm-config: {}", e),
    }

    None
}

/// Check whether the given version of LLVM is blacklisted,
/// returning `Some(reason)` if it is.
fn is_blacklisted_llvm(llvm_version: &Version) -> Option<&'static str> {
    static BLACKLIST: &'static [(u64, u64, u64, &'static str)] = &[];

    let blacklist_var = format!("LLVM_SYS_{}_IGNORE_BLACKLIST",
                                env!("CARGO_PKG_VERSION_MAJOR"));
    if let Some(x) = env::var_os(&blacklist_var) {
        if &x == "YES" {
            println!("cargo:warning=Ignoring blacklist entry for LLVM {}", llvm_version);
            return None;
        } else {
            println!("cargo:warning={} is set but not exactly \"YES\"; blacklist is still honored.",
                     &blacklist_var);
        }
    }

    for &(major, minor, patch, reason) in BLACKLIST.iter() {
        let bad_version = Version {
            major: major, minor: minor, patch: patch,
            pre: vec![], build: vec![],
        };

        if &bad_version == llvm_version {
            return Some(reason);
        }
    }
    None
}
/// Check whether the given LLVM version is compatible with this version of
/// the crate.
fn is_compatible_llvm(llvm_version: &Version) -> bool {
    if let Some(reason) = is_blacklisted_llvm(llvm_version) {
        println!("Found LLVM {}, which is blacklisted: {}", llvm_version, reason);
        return false;
    }

    let strict = env::var_os(format!("LLVM_SYS_{}_STRICT_VERSIONING",
                                     env!("CARGO_PKG_VERSION_MAJOR"))).is_some()
        || cfg!(feature="strict-versioning");
    if strict {
        llvm_version.major == CRATE_VERSION.major &&
            llvm_version.minor == CRATE_VERSION.minor
    } else {
        llvm_version.major >= CRATE_VERSION.major &&
            llvm_version.minor >= CRATE_VERSION.minor
    }
}

/// Get the output from running `llvm-config` with the given argument.
///
/// Lazily searches for or compiles LLVM as configured by the environment
/// variables.
fn llvm_config<S: AsRef<str>>(arg: S) -> String {
    llvm_config_ex(&*LLVM_CONFIG_PATH, arg)
        .expect("Surprising failure from llvm-config")
}

/// Invoke the specified binary as llvm-config.
///
/// Explicit version of the `llvm_config` function that bubbles errors
/// up.
fn llvm_config_ex<S: AsRef<OsStr>, T: AsRef<str>>(binary: S, arg: T)
        -> io::Result<String> {
    let args: Vec<&str> = arg.as_ref().split(' ').collect();
    Command::new(binary)
        .args(args.as_ref())
        .output()
        .map(|output| String::from_utf8(output.stdout)
            .expect("Output from llvm-config was not valid UTF-8"))
}

/// Get the LLVM version using llvm-config.
fn llvm_version<S: AsRef<OsStr>>(binary: S) -> io::Result<Version> {
    let version_str = try!(llvm_config_ex(binary.as_ref(), "--version"));

    // LLVM isn't really semver and uses version suffixes to build
    // version strings like '3.8.0svn', so limit what we try to parse
    // to only the numeric bits.
    let re = Regex::new(r"^(?P<major>\d+)\.(?P<minor>\d+)\.(?P<patch>\d+)")
        .unwrap();
    let (start, end) = re.find(&version_str)
        .expect("Could not determine LLVM version from llvm-config.");

    Ok(Version::parse(&version_str[start..end]).unwrap())
}

#[cfg(target_env = "msvc")]
fn lib_name(name: &str) -> &str {
    assert!(name.ends_with(".lib"));
    &name[..name.len() - 4]
}

#[cfg(not(target_env = "msvc"))]
fn lib_name(name: &str) -> &str {
    assert!(name.starts_with("-l"));
    &name[2..]
}

fn add_lib<S: AsRef<str>>(kind: &'static str, name: S) {
    println!("cargo:rustc-link-lib={}={}", kind, name.as_ref());
}

fn add_libs<S: AsRef<str>>(kind: &'static str, flag: S) {
    for name in llvm_config(flag).split_whitespace().map(lib_name) {
        add_lib(kind, name);
    }
}

fn main() {
    let (lib_type, link_mode_flag) = if env::var("CARGO_FEATURE_FORCE_DYNAMIC_LINKING").is_ok() {
        if env::var("CARGO_FEATURE_FORCE_STATIC_LINKING").is_ok() {
            panic!("Features force-dynamic-linking and force-static-linking are mutually exclusive");
        }
        ("dylib", "--link-shared")
    } else if env::var("CARGO_FEATURE_FORCE_STATIC_LINKING").is_ok() {
        ("static", "--link-static")
    } else {
        match llvm_config("--shared-mode").trim() {
            "shared" => ("dylib", "--link-shared"),
            "static" => ("static", "--link-static"),
            mode => {
                println!("cargo:warning=Unrecognized result from 'llvm-config --shared-mode': '{}'. Static linking is assumed", mode);
                ("static", "--link-static")
            },
        }
    };

    add_libs("dylib", "--system-libs");

    println!("cargo:rustc-link-search=native={}", llvm_config("--libdir"));

    if cfg!(target_env = "msvc") {
        // --libs returns absolute paths when
        // LLVM was built on Windows with MSVC
        add_libs(lib_type, format!("{} --libnames", link_mode_flag));
    } else {
        add_libs(lib_type, format!("{} --libs", link_mode_flag));
        // Determine which C++ standard library to use: LLVM's or GCC's.
        // This breaks the link step on Windows with MSVC.
        add_lib("dylib", if llvm_config("--cxxflags").contains("stdlib=libc++") {
            "c++"
        } else {
            "stdc++"
        });
    }

    // Build the extra wrapper functions.
    std::env::set_var("CFLAGS", llvm_config("--cflags").trim());
    gcc::compile_library("libtargetwrappers.a", &["wrappers/target.c"]);
}
