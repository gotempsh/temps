// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Kernel integration test for the mesh interface lifecycle.
//!
//! Brings up a real `temps-wg0` and checks the state the kernel reports. It
//! exists to pin one property: the mesh path needs `CAP_NET_ADMIN` and
//! nothing else — no `ip` binary on `PATH` and no euid 0. Run it as a
//! non-root uid whose only capability is `CAP_NET_ADMIN` (file capability on
//! the test binary, `--cap-drop ALL --cap-add NET_ADMIN` on the container)
//! with an empty `PATH`, and set `TEMPS_IT_EXPECT_NO_IP=1` so the test also
//! asserts that environment is what it claims to be.
//!
//! Gated like the `temps-network` kernel tests: `--features
//! integration_kernel`, Linux only, and `TEMPS_RUN_DIND_TESTS=1` so an
//! accidental local run never creates interfaces on a developer's host.

#![cfg(all(feature = "integration_kernel", target_os = "linux"))]

use std::net::Ipv4Addr;

use temps_wireguard::mesh::{
    ensure_interface, interface_state, MeshInterface, MeshKey, MESH_INTERFACE,
};

fn enabled() -> bool {
    std::env::var("TEMPS_RUN_DIND_TESTS").as_deref() == Ok("1")
}

fn path_has_ip() -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("ip").is_file()))
}

#[test]
fn mesh_interface_is_managed_without_an_ip_binary_or_root() {
    if !enabled() {
        eprintln!("skipped: TEMPS_RUN_DIND_TESTS=1 not set");
        return;
    }
    if std::env::var("TEMPS_IT_EXPECT_NO_IP").as_deref() == Ok("1") {
        assert!(
            !path_has_ip(),
            "`ip` is reachable on PATH; this run would not prove anything"
        );
    }

    let dir = std::env::temp_dir().join(format!("temps-it-mesh-{}", std::process::id()));
    let key = MeshKey::load_or_create(&dir).expect("mesh key");
    let wanted = MeshInterface {
        address: Ipv4Addr::new(10, 201, 0, 1),
        prefix_len: 16,
        listen_port: 51899,
        mtu: 1380,
    };

    // Fresh interface: created, addressed and brought up.
    assert!(ensure_interface(&wanted, &key).expect("create"));
    let state = interface_state()
        .expect("read state")
        .unwrap_or_else(|| panic!("{MESH_INTERFACE} missing after ensure_interface"));
    assert!(state.up, "interface is not up: {state:?}");
    assert_eq!(state.addresses, vec!["10.201.0.1/16".to_string()]);
    assert_eq!(state.mtu, 1380);
    assert_eq!(state.listen_port, 51899);

    // A settled interface is left alone: nothing changed, still up.
    assert!(!ensure_interface(&wanted, &key).expect("settled"));
    assert!(interface_state().unwrap().unwrap().up);

    // An MTU change is applied in place and reported as a change.
    let smaller = MeshInterface {
        mtu: 1300,
        ..wanted.clone()
    };
    assert!(ensure_interface(&smaller, &key).expect("mtu change"));
    let state = interface_state().unwrap().unwrap();
    assert_eq!(state.mtu, 1300);
    assert!(state.up);
    assert_eq!(state.addresses, vec!["10.201.0.1/16".to_string()]);

    let _ = std::fs::remove_dir_all(&dir);
}
