use std::{process::Command, time::Duration};

pub struct BrokerPause {
    container: String,
    paused: bool,
}
impl BrokerPause {
    pub fn from_runner() -> Self {
        let container = std::env::var("UOB_MQTT_BROKER_CONTAINER")
            .expect("runner-owned broker container missing");
        let inspect = Command::new("docker")
            .arg("inspect")
            .arg("--format")
            .arg("{{.Name}}")
            .arg(&container)
            .output()
            .expect("inspect runner-owned broker");
        assert!(
            inspect.status.success()
                && String::from_utf8_lossy(&inspect.stdout)
                    .trim()
                    .starts_with("/uob-ems-mqtt-"),
            "refusing to interrupt unrelated broker"
        );
        Self {
            container,
            paused: false,
        }
    }
    pub async fn interrupt(mut self) {
        let result = tokio::process::Command::new("docker")
            .arg("pause")
            .arg(&self.container)
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        assert!(
            result.status.success(),
            "runner-owned broker pause failed: {:?}",
            result.status
        );
        self.paused = true;
        tokio::time::sleep(Duration::from_secs(35)).await;
        let result = tokio::process::Command::new("docker")
            .arg("unpause")
            .arg(&self.container)
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        assert!(
            result.status.success(),
            "runner-owned broker resume failed: {:?}",
            result.status
        );
        self.paused = false;
    }
}
impl Drop for BrokerPause {
    fn drop(&mut self) {
        if self.paused {
            let status = Command::new("docker")
                .arg("unpause")
                .arg(&self.container)
                .status();
            if !status.is_ok_and(|status| status.success()) {
                eprintln!("failed to restore paused test broker; runner teardown must remove it");
            }
        }
    }
}
