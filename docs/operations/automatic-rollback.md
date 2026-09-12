# Automatic fallback with current data

The trusted host calls `Supervisor::observe_failure_and_rollback` to persist the existing
failure classification and immediately consume an eligible rollback decision. It supplies the
fixed production process controller and staging-stop driver, not browser credentials. After a
supervisor reboot, `rollback_automatically` can consume an already committed decision without a
new observation. The host must continue collecting authoritative production observations while
the bridge API is unavailable and bind them to the current service invocation.

Only the activation journal's previous-good artifact is considered. Both installed bundles,
signatures, current security floors/revocations, signed old-new-old evidence and current format
versions are checked again. Evidence already accepted for this promotion does not age out of the
rollback window; its signature, identities and current trust policy remain mandatory. The current
configuration must match the promotion's recorded digest and pass the previous binary's offline
configuration check. This is an identity projection of the current compatible configuration;
changed or unsupported configurations require operator recovery. No old configuration is copied.

Before process effects, the private ledger persists one attempt and the failed digest's quarantine.
The process controller stops the entire production group, including queued restart jobs. The
supervisor rechecks database identity and performs bounded read-only schema/integrity validation,
including committed WAL. It then journals the switch, starts the verified previous binary against
that same database and configuration, awaits core readiness, and journals completion. Ordinary
service startup owns reconnect and uncertain-command reconciliation. No charger command interface,
external database connection, backup restore or migration reversal exists in this path.

Status includes the retained rollback trigger ID, failed digest, previous-good digest, step and
sanitized reason. Missing previous-good explicitly reports `no_previous_good`, including first
installation. Ineligible evidence/data/artifacts report `eligibility_rejected`; failure after the
attempt begins reports `process_failed`. All expose `recovery_required`. After interruption, an
`attempting` record also exposes recovery and is never automatically retried. A successful fallback
is idempotent across reboot; later internal fallback failure exposes recovery instead of another
switch. External outages remain insufficient to request a version change.

Quarantine and the incident remain durable after success, and release mutations are blocked until
explicit operator recovery. There is intentionally no public reset or re-promotion bypass. Pending
activation journal writes use its existing crash recovery; uncertain private-ledger writes block
all process effects. Backups and operational/export records are left in place.

The executable still needs the authoritative service-manager observation collector described in
[the failure policy](rollback-signal-policy.md). This change connects classification and rollback
at the trusted host boundary; it does not claim that merely starting the current IPC executable
enables unattended systemd observation collection.

Run `cargo test --locked -p uob-release-manager --lib rollback` for persisted trigger consumption,
post-promotion committed-record/cursor preservation, quarantine, failed/interrupted attempts,
reboot, configuration/data identity, security/evidence rejection and first-installation behavior.
The existing compatibility and activation suites cover format continuity and journal crash points.
