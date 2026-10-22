// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The request path allocates nothing (ADR-055 §11, Scalability): routing
//! and evaluating a request against a compiled table is measured with a
//! counting allocator. One test per binary, so nothing else allocates on
//! the measured thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::net::IpAddr;

use temps_workload_ingress::{
    AccessPolicy, CompiledRoutes, Decision, Endpoint, EndpointProtocol, EndpointTarget, HostClaim,
    HostClaimKind, IpRule, ProductKind, ProductRef, RateLimit, RequestContext,
};

struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `try_with`: the thread-local may be gone while a thread exits.
        let _ = COUNTING.try_with(|on| {
            if on.get() {
                let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Ctx;

impl RequestContext for Ctx {
    fn client_ip(&self) -> IpAddr {
        IpAddr::V4([192, 0, 2, 10].into())
    }
    fn country(&self) -> Option<[u8; 2]> {
        Some(*b"es")
    }
    fn has_password_grant(&self, _: i64) -> bool {
        true
    }
    fn has_login_grant(&self, _: i64, _: &str) -> bool {
        true
    }
    fn has_signed_link(&self, _: i64) -> bool {
        true
    }
    fn captcha_passed(&self, _: i64) -> bool {
        true
    }
    fn within_rate_limit(&self, _: i64, _: &RateLimit) -> bool {
        true
    }
}

#[test]
fn routing_and_evaluating_a_request_does_not_allocate() {
    let endpoints: Vec<Endpoint> = (0..200)
        .map(|i| Endpoint {
            id: i,
            owner: ProductRef {
                kind: ProductKind::Environment,
                id: i,
            },
            target: EndpointTarget::Workload {
                workload_id: i,
                revision: 1,
                port: 3000,
            },
            hosts: vec![
                HostClaim {
                    host: format!("app-{i}.example.test"),
                    kind: HostClaimKind::CustomDomain,
                    claim_id: i,
                },
                HostClaim {
                    host: format!("*.tenant-{i}.example.test"),
                    kind: HostClaimKind::CustomDomain,
                    claim_id: 1000 + i,
                },
            ],
            path_prefix: None,
            protocol: EndpointProtocol::Http,
            server_requires_tls: false,
            access: vec![
                AccessPolicy::RateLimit {
                    limit: RateLimit {
                        requests: 100,
                        per_secs: 1,
                    },
                },
                AccessPolicy::IpRules {
                    rules: vec![IpRule {
                        cidr: "192.0.2.0/24".into(),
                        allow: true,
                    }],
                },
                AccessPolicy::Geo {
                    allow: vec!["ES".into()],
                    deny: vec![],
                },
                AccessPolicy::TempsLogin {
                    permission: "projects:read".into(),
                },
                AccessPolicy::Password { password_ref: 1 },
                AccessPolicy::Captcha,
            ],
        })
        .collect();
    let routes = CompiledRoutes::compile(&endpoints).unwrap();
    let requests = [
        ("App-150.Example.TEST:443", "/api/users"),
        ("x.tenant-7.example.test.", "/"),
        ("missing.example.test", "/"),
    ];

    COUNTING.with(|on| on.set(true));
    let mut allowed = 0;
    for _ in 0..1000 {
        for (host, path) in requests {
            if let Some(endpoint) = routes.route(host, path) {
                if routes.evaluate(endpoint, &Ctx) == Decision::Allow {
                    allowed += 1;
                }
            }
        }
    }
    COUNTING.with(|on| on.set(false));

    assert_eq!(allowed, 2000);
    assert_eq!(ALLOCATIONS.with(Cell::get), 0, "the request path allocated");
}
