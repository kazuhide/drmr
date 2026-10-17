//! `ipaddress.ip_address(…).is_global` (the IANA special-purpose tables of `CPython` 3.11.15, the
//! reference version; 3.13's are the same — `tests/fixtures/hls_source/is_global.json` is identical from both): the
//! fetcher refuses a host that resolves to any address that is not globally reachable unless
//! the policy allows private networks (the DNS-rebinding and metadata-endpoint guard).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const V4_PRIVATE: &[(u32, u8)] = &[
    (0x0000_0000, 8),  // 0.0.0.0/8
    (0x0A00_0000, 8),  // 10.0.0.0/8
    (0x7F00_0000, 8),  // 127.0.0.0/8
    (0xA9FE_0000, 16), // 169.254.0.0/16
    (0xAC10_0000, 12), // 172.16.0.0/12
    (0xC000_0000, 24), // 192.0.0.0/24
    (0xC000_00AA, 31), // 192.0.0.170/31
    (0xC000_0200, 24), // 192.0.2.0/24
    (0xC0A8_0000, 16), // 192.168.0.0/16
    (0xC612_0000, 15), // 198.18.0.0/15
    (0xC633_6400, 24), // 198.51.100.0/24
    (0xCB00_7100, 24), // 203.0.113.0/24
    (0xF000_0000, 4),  // 240.0.0.0/4
    (0xFFFF_FFFF, 32), // 255.255.255.255/32
];
const V4_PRIVATE_EXCEPTIONS: &[(u32, u8)] = &[
    (0xC000_0009, 32), // 192.0.0.9/32
    (0xC000_000A, 32), // 192.0.0.10/32
];
const V4_SHARED: (u32, u8) = (0x6440_0000, 10); // 100.64.0.0/10: neither private nor global

const V6_PRIVATE: &[(u128, u8)] = &[
    (1, 128),                                         // ::1/128
    (0, 128),                                         // ::/128
    (0xFFFF_0000_0000, 96),                           // ::ffff:0.0.0.0/96
    (0x0064_FF9B_0001_u128 << 80, 48),                // 64:ff9b:1::/48
    (0x0100_u128 << 112, 64),                         // 100::/64
    (0x2001_u128 << 112, 23),                         // 2001::/23
    ((0x2001_u128 << 112) | (0x0DB8_u128 << 96), 32), // 2001:db8::/32
    (0x2002_u128 << 112, 16),                         // 2002::/16
    (0x3FFF_u128 << 112, 20),                         // 3fff::/20
    (0xFC00_u128 << 112, 7),                          // fc00::/7
    (0xFE80_u128 << 112, 10),                         // fe80::/10
];
const V6_PRIVATE_EXCEPTIONS: &[(u128, u8)] = &[
    ((0x2001_u128 << 112) | (0x0001_u128 << 96) | 1, 128), // 2001:1::1/128
    ((0x2001_u128 << 112) | (0x0001_u128 << 96) | 2, 128), // 2001:1::2/128
    ((0x2001_u128 << 112) | (0x0003_u128 << 96), 32),      // 2001:3::/32
    (
        (0x2001_u128 << 112) | (0x0004_u128 << 96) | (0x0112_u128 << 80),
        48,
    ), // 2001:4:112::/48
    ((0x2001_u128 << 112) | (0x0020_u128 << 96), 28),      // 2001:20::/28
    ((0x2001_u128 << 112) | (0x0030_u128 << 96), 28),      // 2001:30::/28
];

fn in_v4(address: u32, (network, prefix): (u32, u8)) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    };
    address & mask == network & mask
}

fn in_v6(address: u128, (network, prefix): (u128, u8)) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - u32::from(prefix))
    };
    address & mask == network & mask
}

fn v4_is_private(address: Ipv4Addr) -> bool {
    let value = u32::from(address);
    V4_PRIVATE.iter().any(|net| in_v4(value, *net))
        && V4_PRIVATE_EXCEPTIONS.iter().all(|net| !in_v4(value, *net))
}

fn v4_is_global(address: Ipv4Addr) -> bool {
    !in_v4(u32::from(address), V4_SHARED) && !v4_is_private(address)
}

fn v6_is_global(address: Ipv6Addr) -> bool {
    if let Some(mapped) = ipv4_mapped(address) {
        return v4_is_global(mapped);
    }
    let value = u128::from(address);
    let private = V6_PRIVATE.iter().any(|net| in_v6(value, *net))
        && V6_PRIVATE_EXCEPTIONS.iter().all(|net| !in_v6(value, *net));
    !private
}

/// `IPv6Address.ipv4_mapped`: the IPv4 address of `::ffff:a.b.c.d`.
fn ipv4_mapped(address: Ipv6Addr) -> Option<Ipv4Addr> {
    let value = u128::from(address);
    (value >> 32 == 0xFFFF)
        .then(|| Ipv4Addr::from(u32::try_from(value & 0xFFFF_FFFF).expect("32 bits")))
}

/// `ipaddress.ip_address(address).is_global`.
pub fn is_global(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => v4_is_global(v4),
        IpAddr::V6(v6) => v6_is_global(v6),
    }
}
