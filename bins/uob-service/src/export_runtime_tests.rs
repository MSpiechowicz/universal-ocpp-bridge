use super::LifecycleConfiguration;

#[test]
fn shutdown_deadlines_are_finite_and_validated_offline() {
    for seconds in [0, 301, u64::MAX] {
        assert!(
            LifecycleConfiguration {
                shutdown_timeout_seconds: seconds
            }
            .validate()
            .is_none()
        );
    }
    assert_eq!(
        LifecycleConfiguration::default()
            .validate()
            .unwrap()
            .as_secs(),
        20
    );
}
