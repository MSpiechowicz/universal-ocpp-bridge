//! Private-file authenticated OCPP peers for the real-daemon browser acceptance test.
//! Usage: `charging_browser_peer` <charging-port> <alpha-secret-path> <bravo-secret-path>
//! Stdin phases: disconnect, reconnect, prepare-controls, counts, start-a/b, stop-a/b,
//! availability-a/b, stop. Only fixed phase labels and bounded counters go to stdout.
use std::{env, fs, path::Path, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

#[path = "charging_browser_peer/control.rs"]
mod control;
use control::run;

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type Result<T, E = &'static str> = std::result::Result<T, E>;

fn request(
    port: u16,
    station: &str,
    protocol: &str,
    secret: &[u8],
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
    let mut request = format!("ws://127.0.0.1:{port}/ocpp/{station}")
        .into_client_request()
        .map_err(|_| "invalid peer destination")?;
    let credential = STANDARD.encode([station.as_bytes(), b":", secret].concat());
    request.headers_mut().insert(
        "Authorization",
        format!("Basic {credential}")
            .parse()
            .map_err(|_| "invalid peer credential")?,
    );
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        protocol.parse().map_err(|_| "invalid peer protocol")?,
    );
    Ok(request)
}

async fn connect(port: u16, station: &str, protocol: &str, secret: &[u8]) -> Result<Socket> {
    tokio::time::timeout(
        Duration::from_secs(5),
        connect_async(request(port, station, protocol, secret)?),
    )
    .await
    .map_err(|_| "peer handshake timed out")?
    .map(|(socket, _)| socket)
    .map_err(|_| "peer handshake denied")
}

// Boot and the original read-only seed run before either station is eligible for remote control.
async fn seed_call(socket: &mut Socket, id: &str, action: &str, payload: Value) -> Result<Value> {
    let message = json!([2, id, action, payload]);
    tokio::time::timeout(
        Duration::from_secs(5),
        socket.send(Message::Text(message.to_string().into())),
    )
    .await
    .map_err(|_| "peer send timed out")?
    .map_err(|_| "peer send failed")?;
    let reply = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .map_err(|_| "peer response timed out")?
        .ok_or("peer closed unexpectedly")?
        .map_err(|_| "peer response failed")?;
    let Message::Text(text) = reply else {
        return Err("unexpected peer response kind");
    };
    let response: Value = serde_json::from_str(&text).map_err(|_| "invalid peer response")?;
    if response[0] != 3 || response[1] != id {
        return Err("peer call rejected or mismatched");
    }
    Ok(response[2].clone())
}

async fn boot_alpha(socket: &mut Socket, id: &str) -> Result<()> {
    if seed_call(
        socket,
        id,
        "BootNotification",
        json!({"chargePointVendor":"BrowserVendorA","chargePointModel":"BrowserModelA"}),
    )
    .await?["status"]
        != "Accepted"
    {
        return Err("1.6 boot not accepted");
    }
    Ok(())
}

async fn seed_observations(alpha: &mut Socket, bravo: &mut Socket) -> Result<i64> {
    let stamp = timestamp()?;
    let previous = (time::OffsetDateTime::now_utc() - time::Duration::seconds(5))
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| "invalid older observation time")?;
    seed_call(
        alpha,
        "a-controller",
        "StatusNotification",
        json!({"connectorId":0,"status":"Unavailable","errorCode":"NoError","timestamp":stamp}),
    )
    .await?;
    seed_call(
        alpha,
        "a-status",
        "StatusNotification",
        json!({"connectorId":1,"status":"Available","errorCode":"NoError","timestamp":stamp}),
    )
    .await?;
    seed_call(
        alpha,
        "a-old-status",
        "StatusNotification",
        json!({"connectorId":1,"status":"Faulted","errorCode":"OtherError","timestamp":previous}),
    )
    .await?;
    seed_call(
        bravo,
        "b-status",
        "StatusNotification",
        json!({"timestamp":stamp,"connectorStatus":"Occupied","evseId":1,"connectorId":1}),
    )
    .await?;
    seed_call(
        bravo,
        "b-status-2",
        "StatusNotification",
        json!({"timestamp":stamp,"connectorStatus":"Available","evseId":2,"connectorId":1}),
    )
    .await?;
    let start = seed_call(
        alpha,
        "a-start",
        "StartTransaction",
        json!({"connectorId":1,"idTag":"BROWSER-A","meterStart":0,"timestamp":stamp}),
    )
    .await?;
    if start["idTagInfo"]["status"] != "Invalid" {
        return Err("1.6 authorization unexpectedly granted");
    }
    let seed_transaction = start["transactionId"]
        .as_i64()
        .ok_or("missing seed transaction")?;
    for (id, evse) in [("b-start", 1), ("b-start-2", 2)] {
        let started = seed_call(bravo, id, "TransactionEvent", json!({"eventType":"Started","timestamp":stamp,"triggerReason":"CablePluggedIn","seqNo":0,"transactionInfo":{"transactionId":format!("browser-bravo-tx-{evse}")},"evse":{"id":evse,"connectorId":1}})).await?;
        if started["idTokenInfo"]["status"] != "Invalid" {
            return Err("2.0.1 authorization unexpectedly granted");
        }
    }
    seed_call(
        alpha,
        "a-controller-meter",
        "MeterValues",
        json!({"connectorId":0,"meterValue":[{"timestamp":stamp,"sampledValue":[{"value":"0"}]}]}),
    )
    .await?;
    seed_call(alpha, "a-meter", "MeterValues", json!({"connectorId":1,"meterValue":[{"timestamp":stamp,"sampledValue":[{"value":"12.5"},{"value":"not-a-number","measurand":"Power.Active.Import"}]}]})).await?;
    seed_call(
        bravo,
        "b-meter",
        "MeterValues",
        json!({"evseId":1,"meterValue":[{"timestamp":stamp,"sampledValue":[{"value":7.5}]}]}),
    )
    .await?;
    seed_call(
        bravo,
        "b-meter-2",
        "MeterValues",
        json!({"evseId":2,"meterValue":[{"timestamp":stamp,"sampledValue":[{"value":0.0}]}]}),
    )
    .await?;
    Ok(seed_transaction)
}

fn timestamp() -> Result<String> {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| "invalid observation time")
}

#[tokio::main]
async fn main() {
    let args: Vec<_> = env::args_os().collect();
    let result = async {
        if args.len() != 4 {
            return Err("expected port and two secret file paths");
        }
        let port = args[1]
            .to_str()
            .ok_or("invalid port")?
            .parse()
            .map_err(|_| "invalid port")?;
        let a = fs::read(Path::new(&args[2])).map_err(|_| "cannot read station credential")?;
        let b = fs::read(Path::new(&args[3])).map_err(|_| "cannot read station credential")?;
        run(port, &a, &b).await
    }
    .await;
    if let Err(category) = result {
        eprintln!("charging browser peer: {category}");
        std::process::exit(1);
    }
}
