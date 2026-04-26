# Multi-Host Networking PoC

Prove that the design works on real Linux machines before iterating in Rust.

## What it does

Creates a Linux bridge + VXLAN tunnel + Docker bridge network on each of two
hosts so containers on either host can ping each other directly.

```
Host A (10.0.0.1)                          Host B (10.0.0.2)
┌──────────────────────────────┐            ┌──────────────────────────────┐
│  docker container c1         │            │  docker container c1         │
│      172.20.1.10             │            │      172.20.2.10             │
│       │                      │            │       │                      │
│   br-temps0 (172.20.1.1/24)  │            │   br-temps0 (172.20.2.1/24)  │
│       │                      │            │       │                      │
│   vxlan-temps0 (vni 42)──────┼────────────┼──vxlan-temps0 (vni 42)       │
│   route: 172.20.2.0/24       │            │   route: 172.20.1.0/24       │
│           dev vxlan-temps0   │            │           dev vxlan-temps0   │
└──────────────────────────────┘            └──────────────────────────────┘
```

## Prerequisites

- Two Linux hosts (any modern distro: Ubuntu 22.04+, Debian 12+, Rocky 9+)
- They can reach each other on **UDP/4789** (cloud firewalls / security groups)
- `iproute2`, `bridge` (from iproute2), `nftables`, `docker` installed
- Run as root or with `sudo`

## Run

On host A:

```bash
sudo PEER_UNDERLAY=10.0.0.2 PEER_CIDR=172.20.2.0/24 \
     LOCAL_CIDR=172.20.1.0/24 LOCAL_BRIDGE_IP=172.20.1.1 \
     ./node-up.sh
```

On host B:

```bash
sudo PEER_UNDERLAY=10.0.0.1 PEER_CIDR=172.20.1.0/24 \
     LOCAL_CIDR=172.20.2.0/24 LOCAL_BRIDGE_IP=172.20.2.1 \
     ./node-up.sh
```

Then start a container on each side:

```bash
# On host A
docker run -d --rm --name c1 --network temps0 --ip 172.20.1.10 nginx:alpine

# On host B
docker run -it --rm --name c1 --network temps0 --ip 172.20.2.10 alpine \
    sh -c "ping -c 3 172.20.1.10"
```

You should see ICMP replies. To tear down:

```bash
sudo ./node-down.sh
```

## What this validates

- Linux kernel supports VXLAN (it does, since 2012)
- Cloud underlay allows UDP/4789 between hosts
- MTU math is correct (1450 inside a 1500 underlay)
- Docker plays nicely with a pre-created bridge via `bridge.name` driver opt
- nftables FORWARD + MASQUERADE rules don't conflict with Docker's

If any of these fail, the failure is in the *environment*, not the design.
The Rust port (`crates/temps-network`) implements exactly this same flow.
