// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Container platform strings (`linux/amd64`, `linux/arm64`) and their
//! normalization.
//!
//! Architecture names arrive from three different sources that all spell the
//! same thing differently:
//!
//! - `docker info` / image inspect → `x86_64` or `aarch64` (Docker's `info`
//!   reports the *kernel* name) and `amd64` / `arm64` (image config).
//! - Rust's `std::env::consts::ARCH` → `x86_64` / `aarch64`.
//! - OCI platform strings → `amd64` / `arm64`.
//!
//! Everything that crosses a boundary (heartbeat payloads, the `nodes`
//! table, scheduling decisions, image tags) uses the OCI spelling produced by
//! [`normalize_platform`], so comparisons are string equality and never
//! "is `x86_64` the same as `amd64`?".

/// Canonical platform for the architecture this binary was compiled for.
///
/// This is the *build host's* architecture. Prefer [`normalize_platform`] fed
/// by `docker info` whenever a Docker daemon is reachable — the agent may talk
/// to a daemon on a different machine (or a different architecture, e.g. a
/// QEMU-emulated `docker:dind`), and what matters for placing containers is the
/// daemon's architecture, not the agent binary's.
pub fn native_platform() -> String {
    normalize_platform("linux", std::env::consts::ARCH)
}

/// Normalize an OS + architecture pair into an OCI platform string.
///
/// Unknown architectures are passed through lowercased rather than guessed at,
/// so an unexpected value surfaces as a visible mismatch (`linux/riscv64` !=
/// `linux/amd64`) instead of being silently coerced to amd64.
pub fn normalize_platform(os: &str, arch: &str) -> String {
    let os = match os.trim().to_ascii_lowercase().as_str() {
        "" => "linux".to_string(),
        other => other.to_string(),
    };
    format!("{}/{}", os, normalize_arch(arch))
}

/// Normalize an architecture name to its OCI spelling (`amd64`, `arm64`, ...).
pub fn normalize_arch(arch: &str) -> String {
    match arch.trim().to_ascii_lowercase().as_str() {
        "x86_64" | "amd64" | "x86-64" => "amd64".to_string(),
        "aarch64" | "arm64" | "armv8" | "armv8l" => "arm64".to_string(),
        // Docker reports ARM variants both as a suffixed name (`armv7l` from
        // `docker info`) and as a bare `arm` plus a separate variant field.
        // Bare `arm` means v7 in practice — it's what 32-bit ARM images
        // default to — while v6 must keep its variant or it would be treated
        // as v7 and rejected as unbuildable.
        "armv7l" | "armv7" | "arm" => "arm/v7".to_string(),
        "armv6l" | "armv6" => "arm/v6".to_string(),
        "i386" | "i686" | "x86" => "386".to_string(),
        other => other.to_string(),
    }
}

/// The architecture part of a platform string (`linux/arm64` → `arm64`).
///
/// Falls back to the whole input when there is no `/`, so a bare `arm64` also
/// works.
pub fn platform_arch(platform: &str) -> String {
    match platform.split_once('/') {
        Some((_, arch)) => normalize_arch(arch),
        None => normalize_arch(platform),
    }
}

/// Image-tag-safe suffix for a platform (`linux/arm/v7` → `arm-v7`).
///
/// Used to derive the per-architecture tags a multi-arch build produces
/// (`myapp:latest` for the native platform, `myapp:latest-arm64` for the rest).
/// Docker tags allow `[A-Za-z0-9_.-]`, so `/` must go.
pub fn platform_tag_suffix(platform: &str) -> String {
    platform_arch(platform).replace('/', "-")
}

/// Derive the image tag that carries `platform`, given the tag built for
/// `native_platform`.
///
/// The native platform keeps the plain tag so single-architecture clusters
/// (the overwhelming majority) see byte-identical behaviour to before.
pub fn tag_for_platform(base_tag: &str, platform: &str, native: &str) -> String {
    if platforms_match(platform, native) {
        base_tag.to_string()
    } else {
        format!("{}-{}", base_tag, platform_tag_suffix(platform))
    }
}

/// Compare two platform strings after normalization.
///
/// Tolerates the OS being omitted on either side (`arm64` vs `linux/arm64`)
/// and variant suffixes Docker sometimes appends (`linux/arm64/v8`).
pub fn platforms_match(a: &str, b: &str) -> bool {
    normalized_platform_key(a) == normalized_platform_key(b)
}

/// Architectures Temps will hand to `docker build --platform`.
///
/// Deliberately an allowlist. A node reports its architecture over the
/// network, and that value ends up as a build target for every deployment
/// eligible for that node — so an unrecognised value must not reach Docker,
/// where it fails the build for everyone. Contains the platforms Docker
/// publishes official images for.
const BUILDABLE_ARCHES: &[&str] = &[
    "amd64", "arm64", "arm/v7", "arm/v6", "386", "ppc64le", "s390x", "riscv64", "mips64le",
];

/// Whether we can ask a Docker daemon to build for this platform.
///
/// Unknown architectures are still *stored* and compared verbatim — a node
/// running something exotic simply never matches an image, which is the safe
/// outcome. This gate only decides whether we'd attempt a build for it.
pub fn is_buildable_platform(platform: &str) -> bool {
    let canonical = canonicalize_platform(platform);
    let Some((os, arch)) = canonical.split_once('/') else {
        return false;
    };
    // Docker builds Linux images; a `windows/amd64` node is not something this
    // build path can serve.
    os == "linux" && BUILDABLE_ARCHES.contains(&arch)
}

/// Canonical `os/arch` form of a single platform string.
///
/// Accepts everything the ecosystem spells differently — `arm64`,
/// `linux/aarch64`, `Linux/ARM64`, `linux/arm64/v8` — and returns the one form
/// the rest of the codebase compares and stores (`linux/arm64`).
pub fn canonicalize_platform(platform: &str) -> String {
    normalized_platform_key(platform)
}

/// Key used for platform equality: `os/arch` with any variant suffix dropped.
fn normalized_platform_key(platform: &str) -> String {
    let platform = platform.trim().to_ascii_lowercase();
    let mut parts = platform.splitn(2, '/');
    let first = parts.next().unwrap_or_default();
    match parts.next() {
        // "linux/arm64/v8" → os = linux, arch = arm64/v8 → keep arm/v7-style
        // variants but drop the redundant arm64/v8 spelling.
        Some(rest) => {
            let arch = normalize_arch(rest.strip_suffix("/v8").unwrap_or(rest));
            format!("{}/{}", first, arch)
        }
        // Bare architecture, e.g. "arm64" — assume linux, the only OS Temps
        // deploys containers on.
        None => format!("linux/{}", normalize_arch(first)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_arch_docker_info_spellings() {
        // `docker info` reports kernel names
        assert_eq!(normalize_arch("x86_64"), "amd64");
        assert_eq!(normalize_arch("aarch64"), "arm64");
        // image config / OCI spellings are already canonical
        assert_eq!(normalize_arch("amd64"), "amd64");
        assert_eq!(normalize_arch("arm64"), "arm64");
        // case and whitespace insensitive
        assert_eq!(normalize_arch(" X86_64 "), "amd64");
        assert_eq!(normalize_arch("ARM64"), "arm64");
    }

    /// ARMv6 workers report `armv6l` from `docker info`. Without this mapping
    /// the platform stays `linux/armv6l`, which isn't in the buildable set —
    /// so no image is ever built for the node and the scheduler drops it,
    /// despite `arm/v6` being advertised as supported.
    #[test]
    fn test_normalize_arch_arm_variants() {
        assert_eq!(normalize_arch("armv6l"), "arm/v6");
        assert_eq!(normalize_arch("armv6"), "arm/v6");
        assert_eq!(normalize_arch("armv7l"), "arm/v7");
        // Bare `arm` means v7, which is what 32-bit ARM images default to.
        assert_eq!(normalize_arch("arm"), "arm/v7");
        // Already-canonical variants survive untouched.
        assert_eq!(normalize_arch("arm/v6"), "arm/v6");
        assert_eq!(normalize_arch("arm/v7"), "arm/v7");

        assert!(is_buildable_platform("linux/armv6l"));
        assert!(!platforms_match("linux/armv6l", "linux/arm/v7"));
        assert!(platforms_match("linux/armv6l", "linux/arm/v6"));
    }

    #[test]
    fn test_normalize_arch_passes_through_unknown() {
        // Not guessed at — an unknown arch must stay visibly different from
        // amd64 so it fails a comparison instead of deploying the wrong image.
        assert_eq!(normalize_arch("riscv64"), "riscv64");
        assert_eq!(normalize_arch("s390x"), "s390x");
    }

    #[test]
    fn test_normalize_platform() {
        assert_eq!(normalize_platform("linux", "x86_64"), "linux/amd64");
        assert_eq!(normalize_platform("Linux", "aarch64"), "linux/arm64");
        // Docker's os_type is capitalized
        assert_eq!(normalize_platform("Linux", "arm64"), "linux/arm64");
        // empty OS defaults to linux
        assert_eq!(normalize_platform("", "x86_64"), "linux/amd64");
    }

    #[test]
    fn test_platform_arch() {
        assert_eq!(platform_arch("linux/arm64"), "arm64");
        assert_eq!(platform_arch("linux/x86_64"), "amd64");
        // bare architecture
        assert_eq!(platform_arch("arm64"), "arm64");
    }

    #[test]
    fn test_platforms_match() {
        assert!(platforms_match("linux/amd64", "linux/x86_64"));
        assert!(platforms_match("linux/arm64", "linux/aarch64"));
        // variant suffix Docker sometimes appends
        assert!(platforms_match("linux/arm64/v8", "linux/arm64"));
        // OS omitted on one side
        assert!(platforms_match("arm64", "linux/arm64"));

        assert!(!platforms_match("linux/amd64", "linux/arm64"));
        assert!(!platforms_match("linux/amd64", "linux/riscv64"));
    }

    #[test]
    fn test_platform_tag_suffix() {
        assert_eq!(platform_tag_suffix("linux/arm64"), "arm64");
        assert_eq!(platform_tag_suffix("linux/amd64"), "amd64");
        // no slashes may survive into a Docker tag
        assert!(!platform_tag_suffix("linux/arm/v7").contains('/'));
        assert_eq!(platform_tag_suffix("linux/arm/v7"), "arm-v7");
    }

    #[test]
    fn test_tag_for_platform_keeps_native_tag_untouched() {
        // Single-arch clusters must see exactly the tag they see today.
        assert_eq!(
            tag_for_platform("myapp:latest", "linux/amd64", "linux/amd64"),
            "myapp:latest"
        );
        // equivalent spelling still counts as native
        assert_eq!(
            tag_for_platform("myapp:latest", "linux/x86_64", "linux/amd64"),
            "myapp:latest"
        );
        assert_eq!(
            tag_for_platform("myapp:latest", "linux/arm64", "linux/amd64"),
            "myapp:latest-arm64"
        );
    }

    #[test]
    fn test_is_buildable_platform() {
        assert!(is_buildable_platform("linux/amd64"));
        assert!(is_buildable_platform("linux/arm64"));
        // Equivalent spellings resolve the same way.
        assert!(is_buildable_platform("linux/aarch64"));
        assert!(is_buildable_platform("arm64"));
        assert!(is_buildable_platform("linux/arm/v7"));

        // A node reporting this must never turn into `docker build --platform
        // linux/foo`, which would fail the build for every deployment eligible
        // for that node.
        assert!(!is_buildable_platform("linux/foo"));
        assert!(!is_buildable_platform("linux/riscv128"));
        assert!(!is_buildable_platform("windows/amd64"));
        assert!(!is_buildable_platform(""));
    }

    #[test]
    fn test_native_platform_is_a_known_platform() {
        let native = native_platform();
        assert!(
            native == "linux/amd64" || native == "linux/arm64",
            "unexpected native platform: {}",
            native
        );
    }
}
