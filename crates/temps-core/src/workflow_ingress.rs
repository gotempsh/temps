// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Workflow executors are called directly by the orchestrator, never through
//! public ingress. Keep this policy identical at the edge and origin.

const PREFIX: &[u8] = b"/.well-known/temps/workflows";
const LIMIT: usize = 8192;

fn reserved(path: &[u8]) -> bool {
    path.windows(PREFIX.len()).enumerate().any(|(i, value)| {
        value.eq_ignore_ascii_case(PREFIX) && path.get(i + PREFIX.len()).is_none_or(|c| *c == b'/')
    })
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

/// Deny executor paths, including aliases normalized by common app routers.
/// Ordinary paths allocate nothing. Unusual paths use a fixed 8 KiB scratch
/// buffer; excessively long or deeply encoded unusual paths fail closed.
/// This is only a public-ingress guard: SDK signature verification remains
/// mandatory on the direct replica endpoint too.
pub fn blocks_workflow_executor(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or_default().as_bytes();
    if reserved(path) {
        return true;
    }
    if !path.iter().any(|b| matches!(b, b'%' | b'\\' | b';'))
        && !path.windows(2).any(|w| w == b"//")
        && !path.windows(3).any(|w| w == b"/./")
        && !path.windows(4).any(|w| w == b"/../")
    {
        return false;
    }
    if path.len() > LIMIT {
        return true;
    }
    let mut scratch = [0u8; LIMIT];
    scratch[..path.len()].copy_from_slice(path);
    let mut len = path.len();
    for pass in 0..8 {
        let mut read = 0;
        let mut write = 0;
        let mut decoded = false;
        while read < len {
            if scratch[read] == b'%' && read + 2 < len {
                if let (Some(a), Some(b)) = (hex(scratch[read + 1]), hex(scratch[read + 2])) {
                    scratch[write] = a * 16 + b;
                    read += 3;
                    write += 1;
                    decoded = true;
                    continue;
                }
            }
            scratch[write] = scratch[read];
            read += 1;
            write += 1;
        }
        len = write;
        if !decoded {
            break;
        }
        if pass == 7 {
            return true;
        }
    }
    for byte in &mut scratch[..len] {
        if *byte == b'\\' {
            *byte = b'/';
        }
    }
    if reserved(&scratch[..len]) {
        return true;
    }
    if scratch.first() != Some(&b'/') {
        return true;
    }
    // Normalize segments in place, including matrix parameters. Prefixes are
    // checked anywhere because path routes may strip a mount prefix.
    let mut read = 0;
    let mut write = 0;
    while read < len {
        while read < len && scratch[read] == b'/' {
            read += 1;
        }
        let start = read;
        while read < len && scratch[read] != b'/' {
            read += 1;
        }
        let end = (start..read).find(|i| scratch[*i] == b';').unwrap_or(read);
        let segment = &scratch[start..end];
        if segment.is_empty() || segment == b"." {
            continue;
        }
        if segment == b".." {
            while write > 0 {
                write -= 1;
                if scratch[write] == b'/' {
                    break;
                }
            }
            continue;
        }
        scratch[write] = b'/';
        write += 1;
        scratch.copy_within(start..end, write);
        write += end - start;
    }
    reserved(&scratch[..write])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_executor_ingress_blocks_executor_aliases() {
        for path in [
            "/.well-known/temps/workflows",
            "/.well-known/temps/workflows/invoke",
            "/.well-known/temps/workflows/manifest?x=1",
            "/mount/.well-known/temps/workflows/invoke",
            "/%2ewell-known/temps%2fworkflows/invoke",
            "/%252ewell-known/temps/workflows/invoke",
            "//.well-known//temps/./workflows/invoke",
            "/.well-known/other/../temps/workflows/invoke",
            "/.well-known;v=1/temps/workflows/invoke",
            "/.well-known%5ctemps%5cworkflows/invoke",
            "/.WELL-KNOWN/TEMPS/WORKFLOWS/invoke",
        ] {
            assert!(blocks_workflow_executor(path), "{path}");
        }
    }

    #[test]
    fn workflow_executor_ingress_bounds_normalization() {
        assert!(blocks_workflow_executor(&format!(
            "/{}%2f",
            "a".repeat(LIMIT)
        )));
        let mut path = "/.well-known/temps/workflows/invoke".to_string();
        for _ in 0..10 {
            path = path.replace('%', "%25").replace('.', "%2e");
        }
        assert!(blocks_workflow_executor(&path));
        for len in [0, 1, LIMIT - 1, LIMIT, LIMIT + 1] {
            let path = format!("/{}", "a".repeat(len));
            assert!(!blocks_workflow_executor(&path));
        }
    }

    #[test]
    fn workflow_executor_ingress_preserves_public_app_and_callback_routes() {
        for path in [
            "/",
            "/api/workflows/v1/runs",
            "/api/projects/1/environments/2/workflows",
            "/.well-known/acme-challenge/token",
            "/.well-known/security.txt",
            "/.well-known/temps/workflows-other",
            "/videos/render",
            "/files/a%20b",
            "/100%25",
        ] {
            assert!(!blocks_workflow_executor(path), "{path}");
        }
    }
}
