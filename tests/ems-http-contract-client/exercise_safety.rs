use super::{Demo, Error, Result};

pub(super) fn require_protocol_coverage(demo: &Demo) -> Result<()> {
    let mut ocpp16 = false;
    let mut ocpp201 = false;
    for scenario in &demo.scenario {
        if scenario.command_station.is_none() {
            continue;
        }
        match scenario.protocol.as_str() {
            "ocpp16" => ocpp16 = true,
            "ocpp201" => ocpp201 = true,
            _ => return Err(Error("unknown scenario protocol")),
        }
    }
    if !ocpp16 || !ocpp201 {
        return Err(Error("both protocol command scenarios required"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{Http, run_with_deadline};
    use std::{io::ErrorKind, net::TcpListener, time::Duration};

    #[test]
    fn active_remote_requires_explicit_permission_but_read_only_and_loopback_do_not() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        assert!(Http::new("https://ems.example:9080").is_ok());
        assert!(Http::new_for_exercise("https://ems.example:9080", false).is_err());
        assert!(Http::new_for_exercise("https://ems.example:9080", true).is_ok());
        assert!(Http::new_for_exercise("http://ems.example:9080", true).is_err());
        for base in [
            "http://localhost:9080",
            "http://127.0.0.1:9080",
            "http://[::1]:9080",
        ] {
            assert!(Http::new_for_exercise(base, false).is_ok(), "{base}");
        }
    }

    #[tokio::test]
    async fn incomplete_command_coverage_is_rejected_without_network_io() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        for (missing, present_without_command) in
            [("ocpp16", false), ("ocpp201", false), ("ocpp201", true)]
        {
            let mut demo: super::super::super::Demo =
                toml::from_str(include_str!("demo.toml")).unwrap();
            if present_without_command {
                demo.scenario
                    .iter_mut()
                    .find(|scenario| scenario.protocol == missing)
                    .unwrap()
                    .command_station = None;
            } else {
                demo.scenario
                    .retain(|scenario| scenario.protocol != missing);
            }
            let error = run_with_deadline(
                &base,
                "reader",
                "operator",
                &demo,
                false,
                Duration::from_millis(500),
            )
            .await
            .err()
            .expect("incomplete command coverage must fail");
            assert_eq!(
                error.to_string(),
                "both protocol command scenarios required"
            );
            assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        }
    }
}
