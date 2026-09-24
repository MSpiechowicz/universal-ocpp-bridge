//! Private-file authenticated OCPP peers for the real-daemon browser acceptance test.
//! Usage: `charging_browser_peer` <charging-port> <alpha-secret-path> <bravo-secret-path>
//! Commands on stdin: disconnect, reconnect, stop. Only phase names go to stdout.
use std::{env, fs, path::Path, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
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

async fn call(socket: &mut Socket, id: &str, action: &str, payload: Value) -> Result<Value> {
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
    if call(
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

async fn seed_observations(alpha: &mut Socket, bravo: &mut Socket) -> Result<()> {
    let observed = time::OffsetDateTime::now_utc();
    let stamp = observed
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| "invalid observation time")?;
    let previous = (observed - time::Duration::seconds(5))
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| "invalid older observation time")?;
    call(
        alpha,
        "a-controller",
        "StatusNotification",
        json!({"connectorId":0,"status":"Unavailable","errorCode":"NoError","timestamp":stamp}),
    )
    .await?;
    call(
        alpha,
        "a-status",
        "StatusNotification",
        json!({"connectorId":1,"status":"Available","errorCode":"NoError","timestamp":stamp}),
    )
    .await?;
    call(
        alpha,
        "a-old-status",
        "StatusNotification",
        json!({"connectorId":1,"status":"Faulted","errorCode":"OtherError","timestamp":previous}),
    )
    .await?;
    call(
        bravo,
        "b-status",
        "StatusNotification",
        json!({"timestamp":stamp,"connectorStatus":"Occupied","evseId":1,"connectorId":1}),
    )
    .await?;
    call(
        bravo,
        "b-status-2",
        "StatusNotification",
        json!({"timestamp":stamp,"connectorStatus":"Available","evseId":2,"connectorId":1}),
    )
    .await?;
    let start = call(
        alpha,
        "a-start",
        "StartTransaction",
        json!({"connectorId":1,"idTag":"BROWSER-A","meterStart":0,"timestamp":stamp}),
    )
    .await?;
    if start["idTagInfo"]["status"] != "Invalid" {
        return Err("1.6 authorization unexpectedly granted");
    }
    for (id, evse) in [("b-start", 1), ("b-start-2", 2)] {
        let started = call(bravo, id, "TransactionEvent", json!({"eventType":"Started","timestamp":stamp,"triggerReason":"CablePluggedIn","seqNo":0,"transactionInfo":{"transactionId":format!("browser-bravo-tx-{evse}")},"evse":{"id":evse,"connectorId":1}})).await?;
        if started["idTokenInfo"]["status"] != "Invalid" {
            return Err("2.0.1 authorization unexpectedly granted");
        }
    }
    call(
        alpha,
        "a-controller-meter",
        "MeterValues",
        json!({"connectorId":0,"meterValue":[{"timestamp":stamp,"sampledValue":[{"value":"0"}]}]}),
    )
    .await?;
    call(alpha, "a-meter", "MeterValues", json!({"connectorId":1,"meterValue":[{"timestamp":stamp,"sampledValue":[{"value":"12.5"},{"value":"not-a-number","measurand":"Power.Active.Import"}]}]})).await?;
    call(
        bravo,
        "b-meter",
        "MeterValues",
        json!({"evseId":1,"meterValue":[{"timestamp":stamp,"sampledValue":[{"value":7.5}]}]}),
    )
    .await?;
    call(
        bravo,
        "b-meter-2",
        "MeterValues",
        json!({"evseId":2,"meterValue":[{"timestamp":stamp,"sampledValue":[{"value":0.0}]}]}),
    )
    .await?;
    Ok(())
}

async fn run(port: u16, a: &[u8], b: &[u8]) -> Result<()> {
    if connect(port, "station-a", "ocpp1.6", b).await.is_ok()
        || connect(port, "station-a", "ocpp2.0.1", a).await.is_ok()
    {
        return Err("invalid station peer authenticated");
    }
    let mut alpha = connect(port, "station-a", "ocpp1.6", a).await?;
    let mut bravo = connect(port, "station-b", "ocpp2.0.1", b).await?;
    boot_alpha(&mut alpha, "a-boot").await?;
    if call(&mut bravo, "b-boot", "BootNotification", json!({"chargingStation":{"vendorName":"BrowserVendorB","model":"BrowserModelB"},"reason":"PowerUp"})).await?["status"] != "Accepted" {
        return Err("2.0.1 boot not accepted");
    }
    seed_observations(&mut alpha, &mut bravo).await?;
    println!("ready");
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(command) = lines.next_line().await.map_err(|_| "peer control failed")? {
        match command.as_str() {
            "disconnect" => {
                alpha.close(None).await.map_err(|_| "peer close failed")?;
                println!("disconnected");
            }
            "reconnect" => {
                alpha = connect(port, "station-a", "ocpp1.6", a).await?;
                boot_alpha(&mut alpha, "a-reboot").await?;
                call(
                    &mut alpha,
                    "a-restored",
                    "StatusNotification",
                    json!({"connectorId":1,"status":"Available","errorCode":"NoError"}),
                )
                .await?;
                println!("reconnected");
            }
            "stop" => {
                let _ = alpha.close(None).await;
                let _ = bravo.close(None).await;
                println!("stopped");
                return Ok(());
            }
            _ => return Err("invalid peer control phase"),
        }
    }
    Err("peer control closed without stop")
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
