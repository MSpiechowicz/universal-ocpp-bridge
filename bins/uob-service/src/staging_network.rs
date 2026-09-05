//! Runtime proof of the packaged, root-owned staging network boundary.
use uob_contracts::Environment;

const ERROR: &str = "staging requires the isolated /run/netns/uob-staging loopback-only network";

pub(crate) fn verify(environment: Environment) -> Result<(), &'static str> {
    if environment != Environment::Staging {
        return Ok(());
    }
    verify_linux()
}

#[cfg(target_os = "linux")]
fn verify_linux() -> Result<(), &'static str> {
    use std::{fs, os::unix::fs::MetadataExt};
    let current = fs::metadata("/proc/self/ns/net").map_err(|_| ERROR)?;
    let expected = fs::metadata("/run/netns/uob-staging").map_err(|_| ERROR)?;
    let identity = |metadata: &fs::Metadata| (metadata.dev(), metadata.ino());
    if identity(&current) != identity(&expected) {
        return Err(ERROR);
    }
    // /proc/net follows this process's namespace; /sys/class/net need not do so.
    let devices = fs::read_to_string("/proc/net/dev").map_err(|_| ERROR)?;
    validate_devices(&devices)?;
    let ipv4 = fs::read_to_string("/proc/net/route").map_err(|_| ERROR)?;
    let ipv6 = fs::read_to_string("/proc/net/ipv6_route").map_err(|_| ERROR)?;
    validate_routes(&ipv4, &ipv6)
}

#[cfg(not(target_os = "linux"))]
fn verify_linux() -> Result<(), &'static str> {
    Err(ERROR)
}

fn validate_devices(devices: &str) -> Result<(), &'static str> {
    // Some kernels create inert fallback tunnel devices in *every* fresh namespace.
    // They have no underlying network or route; do not mistake them for admitted test uplinks.
    const KERNEL_FALLBACKS: &[&str] = &[
        "tunl0", "gre0", "gretap0", "erspan0", "ip_vti0", "ip6_vti0", "sit0", "ip6tnl0", "ip6gre0",
    ];
    let names = devices
        .lines()
        .skip(2)
        .map(|line| line.split_once(':').map(|(name, _)| name.trim()))
        .collect::<Vec<_>>();
    if names.contains(&Some("lo"))
        && names
            .iter()
            .all(|name| name.is_some_and(|name| name == "lo" || KERNEL_FALLBACKS.contains(&name)))
    {
        Ok(())
    } else {
        Err(ERROR)
    }
}

fn validate_routes(ipv4: &str, ipv6: &str) -> Result<(), &'static str> {
    if !ipv4
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
    fn an_uplink_or_unreadable_device_list_fails_closed() {
        assert!(validate_devices("header\nheader\n lo: 0\n").is_ok());
        for devices in ["", "header\nheader\n", "header\nheader\n lo: 0\n eth0: 0\n"] {
            assert!(validate_devices(devices).is_err());
        }
        assert!(validate_devices("header\nheader\n lo: 0\n tunl0: 0\n").is_ok());
        assert!(validate_routes("Iface Destination\n", "").is_ok());
        assert!(validate_routes("Iface Destination\ntunl0 00000000\n", "").is_err());
        assert!(validate_routes("Iface Destination\n", "route tunl0\n").is_err());
        assert!(validate_routes("", "").is_err());
        assert!(verify(Environment::Production).is_ok());
        assert!(verify(Environment::Demo).is_ok());
    }
}
