use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Connect,
    Boot,
    Authorize,
    Status,
    StartTransaction,
    MeterValues,
    StopTransaction,
    AwaitRemoteStart,
    AwaitRemoteStop,
    TargetOffline,
    TargetOnline,
    ReconcileCommand,
    Heartbeat,
    Wait,
    Disconnect,
    CsmsOffline,
    CsmsReconnect,
    OfflineStart,
    OfflineStop,
    AssertLocalAuthorization,
    AwaitLocalAuthorization,
    AwaitReboot,
    DelayLocalReply,
    DropLocalReply,
    AssertReservation,
    AwaitReservation,
    AssertFirmware,
    AwaitFirmware,
    AssertDiagnostics,
    AwaitDiagnostics,
}

impl ActionKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Boot => "boot",
            Self::Authorize => "authorize",
            Self::Status => "status",
            Self::StartTransaction => "start_transaction",
            Self::MeterValues => "meter_values",
            Self::StopTransaction => "stop_transaction",
            Self::AwaitRemoteStart => "await_remote_start",
            Self::AwaitRemoteStop => "await_remote_stop",
            Self::TargetOffline => "target_offline",
            Self::TargetOnline => "target_online",
            Self::ReconcileCommand => "reconcile_command",
            Self::Heartbeat => "heartbeat",
            Self::Wait => "wait",
            Self::Disconnect => "disconnect",
            Self::CsmsOffline => "csms_offline",
            Self::CsmsReconnect => "csms_reconnect",
            Self::OfflineStart => "offline_start",
            Self::OfflineStop => "offline_stop",
            Self::AssertLocalAuthorization => "assert_local_authorization",
            Self::AwaitLocalAuthorization => "await_local_authorization",
            Self::AwaitReboot => "await_reboot",
            Self::DelayLocalReply => "delay_local_reply",
            Self::DropLocalReply => "drop_local_reply",
            Self::AssertReservation => "assert_reservation",
            Self::AwaitReservation => "await_reservation",
            Self::AssertFirmware => "assert_firmware",
            Self::AwaitFirmware => "await_firmware",
            Self::AssertDiagnostics => "assert_diagnostics",
            Self::AwaitDiagnostics => "await_diagnostics",
        }
    }

    #[must_use]
    pub const fn event(self) -> &'static str {
        match self {
            Self::Connect => "connected",
            Self::Boot => "boot_result",
            Self::Authorize => "authorization_result",
            Self::Status => "status_result",
            Self::StartTransaction => "transaction_started",
            Self::MeterValues => "meter_values_result",
            Self::StopTransaction => "transaction_stopped",
            Self::AwaitRemoteStart => "remote_start_received",
            Self::AwaitRemoteStop => "remote_stop_received",
            Self::TargetOffline => "target_unavailable",
            Self::TargetOnline => "target_reconnected",
            Self::ReconcileCommand => "command_reconciled",
            Self::Heartbeat => "heartbeat_result",
            Self::Wait => "delay_elapsed",
            Self::Disconnect => "disconnected",
            Self::CsmsOffline => "csms_disconnected",
            Self::CsmsReconnect => "csms_reconnected",
            Self::OfflineStart => "offline_start_result",
            Self::OfflineStop => "offline_stop_result",
            Self::AssertLocalAuthorization | Self::AwaitLocalAuthorization => {
                "local_authorization_observed"
            }
            Self::AwaitReboot => "reboot_observed",
            Self::DelayLocalReply | Self::DropLocalReply => "native_fault_armed",
            Self::AssertReservation | Self::AwaitReservation => "reservation_observed",
            Self::AssertFirmware | Self::AwaitFirmware => "firmware_observed",
            Self::AssertDiagnostics | Self::AwaitDiagnostics => "diagnostics_observed",
        }
    }

    #[must_use]
    pub const fn message_name(self) -> Option<&'static str> {
        match self {
            Self::Boot => Some("BootNotification"),
            Self::Authorize => Some("Authorize"),
            Self::Status => Some("StatusNotification"),
            Self::StartTransaction => Some("StartTransaction"),
            Self::MeterValues => Some("MeterValues"),
            Self::StopTransaction => Some("StopTransaction"),
            Self::Heartbeat => Some("Heartbeat"),
            Self::AwaitRemoteStart => Some("RemoteStartTransaction"),
            Self::AwaitRemoteStop => Some("RemoteStopTransaction"),
            Self::Connect
            | Self::TargetOffline
            | Self::TargetOnline
            | Self::ReconcileCommand
            | Self::Wait
            | Self::CsmsOffline
            | Self::CsmsReconnect
            | Self::OfflineStart
            | Self::OfflineStop
            | Self::AssertLocalAuthorization
            | Self::AwaitLocalAuthorization
            | Self::AwaitReboot
            | Self::DelayLocalReply
            | Self::DropLocalReply
            | Self::AssertReservation
            | Self::AwaitReservation
            | Self::AssertFirmware
            | Self::AwaitFirmware
            | Self::AssertDiagnostics
            | Self::AwaitDiagnostics
            | Self::Disconnect => None,
        }
    }

    #[must_use]
    pub fn accepts_message(self, message: &str) -> bool {
        match self {
            Self::StartTransaction => matches!(message, "StartTransaction" | "TransactionEvent"),
            Self::MeterValues => matches!(message, "MeterValues" | "TransactionEvent"),
            Self::StopTransaction => matches!(message, "StopTransaction" | "TransactionEvent"),
            _ => self.message_name() == Some(message),
        }
    }
}
