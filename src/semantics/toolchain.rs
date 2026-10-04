//! The build facts of the Lean toolchain that native programs report:
//! `Lean.githash`, `System.Platform.target` and `Lean.version.specialDesc`.
//! Native Lean takes them from its own build, not from the program: the
//! githash from the runtime library (`src/runtime/platform.cpp`, `githash.h`),
//! the target and the special description from the toolchain's
//! `include/lean/version.h`, through `lean.h`'s inline functions. A translated
//! program reports the values of the Lean 4.34.0 toolchain it was translated
//! from, so each is a constant here; the target triple is the one the
//! program is built for, as native Lean's is the one its toolchain was built
//! for.
//!
//! The values are those native Lean 4.34.0 reports on the pinned host
//! (aarch64 Linux, the official `leanprover/lean4:v4.34.0` release),
//! recorded by the rows in `tests/cases/toolchain/toolchain.rows.toml`:
//! - `GITHASH` is the commit of the `v4.34.0` tag, the same in every
//!   official 4.34.0 build;
//! - `SPECIAL_DESC` is empty in a release build (a nightly has
//!   `nightly-YYYY-MM-DD`);
//! - `PLATFORM_TARGET` is the target triple of the toolchain's own build,
//!   which differs from platform to platform. Here it is put together from
//!   the build target's `cfg` (arch-vendor-os-env), so a program built for
//!   another platform reports that platform's triple, as native Lean built
//!   there does. Only the aarch64 Linux value is recorded from native Lean;
//!   the others follow the LLVM triples and are not checked against a native
//!   build (`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`,
//!   `x86_64-pc-windows-msvc`); a part the list below does not know is
//!   `unknown`.
//!
//! The other version queries (`Lean.version.major`, `minor`, `patch`,
//! `isRelease`) are numbers and a flag in `lean.h` that a translator emits
//! inline.
//!
//! Source: new, from the values the native rows record.

/// `Lean.getGithash` (`lean_get_githash`, `src/runtime/platform.cpp`): the
/// commit Lean 4.34.0 was built from (`LEAN_GITHASH`).
///
/// Source: new, the native value (`git rev-parse v4.34.0` gives the same).
pub const GITHASH: &str = "293d5d0c0c3f3dded4688b3ccd6a33939ac5102b";

/// One part of the target triple, chosen by the build target's `cfg`: a
/// macro `$name!()` that expands to a string literal, so that `concat!`
/// can join the parts at compile time.
macro_rules! cfg_part {
    ($name:ident: $key:ident { $($v:literal => $s:literal),* $(,)? } else $default:literal) => {
        $( #[cfg($key = $v)] macro_rules! $name { () => { $s }; } )*
        #[cfg(not(any($($key = $v),*)))] macro_rules! $name { () => { $default }; }
    };
}

cfg_part!(triple_arch: target_arch {
    "aarch64" => "aarch64", "x86_64" => "x86_64", "riscv64" => "riscv64",
    "loongarch64" => "loongarch64", "s390x" => "s390x",
} else "unknown");
cfg_part!(triple_vendor: target_vendor {
    "unknown" => "unknown", "apple" => "apple", "pc" => "pc",
} else "unknown");
cfg_part!(triple_os: target_os {
    "linux" => "linux", "macos" => "darwin", "windows" => "windows", "freebsd" => "freebsd",
} else "unknown");
cfg_part!(triple_env: target_env {
    "gnu" => "-gnu", "musl" => "-musl", "msvc" => "-msvc",
} else "");

/// `System.Platform.getTarget` (`lean_system_platform_target`, `lean.h`):
/// the LLVM target triple of the toolchain's build (`LEAN_PLATFORM_TARGET`
/// in `include/lean/version.h`), here the triple of the target this crate
/// is built for, from its `cfg`: `aarch64-unknown-linux-gnu` on the pinned
/// host, as native Lean reports there (see the module doc).
///
/// Source: new, the native value on aarch64 Linux, generalized by `cfg`.
pub const PLATFORM_TARGET: &str = concat!(
    triple_arch!(),
    "-",
    triple_vendor!(),
    "-",
    triple_os!(),
    triple_env!()
);

/// `Lean.version.getSpecialDesc` (`lean_version_get_special_desc`,
/// `lean.h`): the additional version text (`LEAN_SPECIAL_VERSION_DESC`),
/// empty for the 4.34.0 release.
///
/// Source: new, the native value.
pub const SPECIAL_DESC: &str = "";
