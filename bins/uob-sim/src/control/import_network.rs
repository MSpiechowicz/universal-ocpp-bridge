//! Imports can dispatch only inside the administrator-owned, loopback-only test namespace.
//! Kept simulator-owned: the simulator never links the service implementation.
const ERROR: &str = "import_requires_isolated_staging_network";

#[cfg(target_os = "linux")]
pub(super) fn verify() -> Result<(), &'static str> {
    use std::{fs, os::unix::fs::MetadataExt};
    let current = fs::metadata("/proc/self/ns/net").map_err(|_| ERROR)?;
    let expected = fs::metadata("/run/netns/uob-staging").map_err(|_| ERROR)?;
    if (current.dev(), current.ino()) != (expected.dev(), expected.ino()) {
        return Err(ERROR);
    }
    validate(
        &fs::read_to_string("/proc/net/dev").map_err(|_| ERROR)?,
        &fs::read_to_string("/proc/net/route").map_err(|_| ERROR)?,
        &fs::read_to_string("/proc/net/ipv6_route").map_err(|_| ERROR)?,
    )
}

#[cfg(not(target_os = "linux"))]
pub(super) fn verify() -> Result<(), &'static str> {
    Err(ERROR)
}

fn validate(devices: &str, ipv4: &str, ipv6: &str) -> Result<(), &'static str> {
    const INERT: &[&str] = &[
        "tunl0", "gre0", "gretap0", "erspan0", "ip_vti0", "ip6_vti0", "sit0", "ip6tnl0", "ip6gre0",
    ];
    let names: Vec<_> = devices
        .lines()
        .skip(2)
        .map(|line| line.split_once(':').map(|(name, _)| name.trim()))
        .collect();
    if !names.contains(&Some("lo"))
        || names
            .iter()
            .any(|name| !name.is_some_and(|name| name == "lo" || INERT.contains(&name)))
        || !ipv4
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("Iface"))
        || ipv4
            .lines()
            .skip(1)
            .any(|line| line.split_whitespace().next() != Some("lo"))
        || ipv6
            .lines()
            .any(|line| line.split_whitespace().last() != Some("lo"))
    {
        return Err(ERROR);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uplinks_routes_and_malformed_observations_fail_closed() {
        let devices = "header\nheader\nlo: 0\n";
        assert!(validate(devices, "Iface Destination\n", "").is_ok());
        assert!(validate("", "Iface\n", "").is_err());
        assert!(validate("header\nheader\nlo: 0\neth0: 0\n", "Iface\n", "").is_err());
        assert!(validate(devices, "Iface\neth0 00000000\n", "").is_err());
        assert!(validate(devices, "Iface\n", "route eth0\n").is_err());
    }
}
