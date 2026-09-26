//! Server-initiated calls and explicit station-originated observation phases.
use super::{Result, Socket, boot_alpha, connect, seed_call, seed_observations};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::tungstenite::Message;

#[path = "control/phases.rs"]
mod phases;
#[path = "control/state.rs"]
mod state;

use phases::phase_calls;
use state::{Counts, State};
#[derive(Clone, Copy)]
enum Edition {
    Alpha,
    Bravo,
}

enum Control {
    Phase(Phase, oneshot::Sender<Result<Counts>>),
    Close(oneshot::Sender<()>),
}
#[derive(Clone, Copy)]
enum Phase {
    Prepare,
    Start,
    Stop,
    Availability,
    Counts,
}
struct Peer {
    tx: mpsc::Sender<Control>,
    task: tokio::task::JoinHandle<Result<State>>,
}

async fn send_frame(socket: &mut Socket, frame: Value) -> Result<()> {
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .map_err(|_| "peer send failed")
}

async fn actor(
    mut socket: Socket,
    edition: Edition,
    mut state: State,
    mut rx: mpsc::Receiver<Control>,
) -> Result<State> {
    loop {
        tokio::select! {
            frame = socket.next() => {
                let frame = frame.ok_or("peer closed unexpectedly")?.map_err(|_| "peer read failed")?;
                let Message::Text(text) = frame else {
                    if frame.is_close() { return Err("peer closed unexpectedly"); }
                    continue;
                };
                let message: Value = serde_json::from_str(&text).map_err(|_| "invalid peer frame")?;
                let fields = message.as_array().ok_or("invalid peer frame")?;
                match fields.first().and_then(Value::as_u64) {
                    Some(2) if fields.len() == 4 => {
                        let id = fields[1].as_str().filter(|s| !s.is_empty()).ok_or("invalid server call id")?;
                        let action = fields[2].as_str().ok_or("invalid server call action")?;
                        let result = state.accept(edition, action, &fields[3]);
                        match result {
                            Ok(payload) => send_frame(&mut socket, json!([3, id, payload])).await?,
                            Err(_) => send_frame(&mut socket, json!([4, id, "FormationViolation", "Invalid peer command", {}])).await?,
                        }
                    }
                    Some(3 | 4) => return Err("unmatched peer reply"),
                    _ => return Err("invalid peer frame"),
                }
            }
            control = rx.recv() => {
                let control = control.ok_or("peer control closed")?;
                match control {
                    Control::Phase(phase, reply) => {
                        let result = phase_calls(&mut state, edition, phase);
                        match result {
                            Ok(calls) => {
                                // Observation replies remain correlated even if server CALLs arrive first.
                                let mut result = Ok(());
                                for (action, payload) in calls {
                                    state.next_id = state.next_id.checked_add(1).ok_or("peer call capacity exceeded")?;
                                    let id = match edition { Edition::Alpha => format!("a-phase-{}", state.next_id), Edition::Bravo => format!("b-phase-{}", state.next_id) };
                                    send_frame(&mut socket, json!([2, id, action, payload])).await?;
                                    match await_reply(&mut socket, &mut state, edition, &id).await {
                                        Ok(value) if action == "StartTransaction" => {
                                            if value["idTagInfo"]["status"] != "Accepted" { result = Err("authorized 1.6 start denied"); break; }
                                            state.active16 = value["transactionId"].as_i64();
                                            if state.active16.is_none() { result = Err("missing active transaction id"); break; }
                                        }
                                        Ok(value) if action == "TransactionEvent" && payload["eventType"] == "Started" => {
                                            // The demo accepts the event but does not equate its token with a charging grant.
                                            if value["idTokenInfo"]["status"] != "Invalid" { result = Err("unexpected 2.0.1 transaction authorization"); break; }
                                        }
                                        Ok(_) => {}
                                        Err(error) => { result = Err(error); break; }
                                    }
                                }
                                let _ = reply.send(result.map(|()| state.counts.clone()));
                            }
                            Err(error) => { let _ = reply.send(Err(error)); }
                        }
                    }
                    Control::Close(reply) => {
                        let _ = socket.close(None).await;
                        let _ = reply.send(());
                        return Ok(state);
                    }
                }
            }
        }
    }
}

// While waiting for an observation acknowledgement, continue serving central-system CALLs.
async fn await_reply(
    socket: &mut Socket,
    state: &mut State,
    edition: Edition,
    expected: &str,
) -> Result<Value> {
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let frame = tokio::time::timeout_at(until, socket.next())
            .await
            .map_err(|_| "peer response timed out")?
            .ok_or("peer closed unexpectedly")?
            .map_err(|_| "peer read failed")?;
        let Message::Text(text) = frame else {
            return Err("unexpected peer response kind");
        };
        let message: Value = serde_json::from_str(&text).map_err(|_| "invalid peer response")?;
        let fields = message.as_array().ok_or("invalid peer response")?;
        match (fields.first().and_then(Value::as_u64), fields.len()) {
            (Some(2), 4) => {
                let id = fields[1].as_str().ok_or("invalid server call id")?;
                let action = fields[2].as_str().ok_or("invalid server call action")?;
                match state.accept(edition, action, &fields[3]) {
                    Ok(payload) => send_frame(socket, json!([3, id, payload])).await?,
                    Err(_) => {
                        send_frame(
                            socket,
                            json!([4, id, "FormationViolation", "Invalid peer command", {}]),
                        )
                        .await?;
                    }
                }
            }
            (Some(3), 3) if fields[1] == expected => return Ok(fields[2].clone()),
            (Some(4), 5) if fields[1] == expected => return Err("station observation rejected"),
            _ => return Err("unmatched peer reply"),
        }
    }
}

fn spawn_peer(socket: Socket, edition: Edition, state: State) -> Peer {
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(actor(socket, edition, state, rx));
    Peer { tx, task }
}

async fn phase(peer: &Peer, phase: Phase) -> Result<Counts> {
    let (tx, rx) = oneshot::channel();
    peer.tx
        .send(Control::Phase(phase, tx))
        .await
        .map_err(|_| "peer task closed")?;
    tokio::time::timeout(Duration::from_secs(15), rx)
        .await
        .map_err(|_| "peer phase timed out")?
        .map_err(|_| "peer phase failed")?
}

async fn close_peer(peer: Peer) -> Result<State> {
    let (tx, rx) = oneshot::channel();
    peer.tx
        .send(Control::Close(tx))
        .await
        .map_err(|_| "peer task closed")?;
    rx.await.map_err(|_| "peer close failed")?;
    peer.task.await.map_err(|_| "peer task failed")?
}

async fn connect_peers(port: u16, a: &[u8], b: &[u8]) -> Result<(Peer, Peer)> {
    if connect(port, "station-a", "ocpp1.6", b).await.is_ok()
        || connect(port, "station-a", "ocpp2.0.1", a).await.is_ok()
    {
        return Err("invalid station peer authenticated");
    }
    let mut alpha_socket = connect(port, "station-a", "ocpp1.6", a).await?;
    let mut bravo_socket = connect(port, "station-b", "ocpp2.0.1", b).await?;
    boot_alpha(&mut alpha_socket, "a-boot").await?;
    if seed_call(&mut bravo_socket, "b-boot", "BootNotification", json!({"chargingStation":{"vendorName":"BrowserVendorB","model":"BrowserModelB"},"reason":"PowerUp"})).await?["status"] != "Accepted" {
        return Err("2.0.1 boot not accepted");
    }
    let seed_transaction = seed_observations(&mut alpha_socket, &mut bravo_socket).await?;
    let alpha = spawn_peer(
        alpha_socket,
        Edition::Alpha,
        State {
            seed_transaction: Some(seed_transaction),
            ..State::default()
        },
    );
    let bravo = spawn_peer(bravo_socket, Edition::Bravo, State::default());
    Ok((alpha, bravo))
}

async fn reconnect_alpha(port: u16, key: &[u8], disconnected: &mut Option<State>) -> Result<Peer> {
    let mut socket = connect(port, "station-a", "ocpp1.6", key).await?;
    boot_alpha(&mut socket, "a-reboot").await?;
    seed_call(
        &mut socket,
        "a-restored",
        "StatusNotification",
        json!({"connectorId":1,"status":"Available","errorCode":"NoError"}),
    )
    .await?;
    Ok(spawn_peer(
        socket,
        Edition::Alpha,
        disconnected.take().ok_or("not disconnected")?,
    ))
}

fn print_counts(a: &Counts, b: &Counts) {
    println!(
        "counts a-start={} a-stop={} a-limit={} a-availability={} b-start={} b-stop={} b-limit={} b-availability={} a-started={} a-ended={} a-availability-observed={} b-started={} b-ended={} b-availability-observed={}",
        a.start,
        a.stop,
        a.limit,
        a.availability,
        b.start,
        b.stop,
        b.limit,
        b.availability,
        a.started,
        a.ended,
        a.availability_observed,
        b.started,
        b.ended,
        b.availability_observed
    );
}

pub(super) async fn run(port: u16, a: &[u8], b: &[u8]) -> Result<()> {
    let (alpha_peer, bravo) = connect_peers(port, a, b).await?;
    let mut alpha = Some(alpha_peer);
    println!("ready");
    let mut disconnected_alpha = None;

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(command) = lines.next_line().await.map_err(|_| "peer control failed")? {
        match command.as_str() {
            "disconnect" => {
                disconnected_alpha =
                    Some(close_peer(alpha.take().ok_or("already disconnected")?).await?);
                println!("disconnected");
            }
            "reconnect" => {
                if alpha.is_some() {
                    return Err("already connected");
                }
                alpha = Some(reconnect_alpha(port, a, &mut disconnected_alpha).await?);
                println!("reconnected");
            }
            "prepare-controls" => {
                phase(alpha.as_ref().ok_or("alpha disconnected")?, Phase::Prepare).await?;
                phase(&bravo, Phase::Prepare).await?;
                println!("controls-prepared");
            }
            "counts" => {
                let a = phase(alpha.as_ref().ok_or("alpha disconnected")?, Phase::Counts).await?;
                let b = phase(&bravo, Phase::Counts).await?;
                print_counts(&a, &b);
            }
            "start-a" => {
                phase(alpha.as_ref().ok_or("alpha disconnected")?, Phase::Start).await?;
                println!("started-a");
            }
            "start-b" => {
                phase(&bravo, Phase::Start).await?;
                println!("started-b");
            }
            "stop-a" => {
                phase(alpha.as_ref().ok_or("alpha disconnected")?, Phase::Stop).await?;
                println!("ended-a");
            }
            "stop-b" => {
                phase(&bravo, Phase::Stop).await?;
                println!("ended-b");
            }
            "availability-a" => {
                phase(
                    alpha.as_ref().ok_or("alpha disconnected")?,
                    Phase::Availability,
                )
                .await?;
                println!("availability-observed-a");
            }
            "availability-b" => {
                phase(&bravo, Phase::Availability).await?;
                println!("availability-observed-b");
            }
            "stop" => {
                if let Some(peer) = alpha {
                    close_peer(peer).await?;
                }
                close_peer(bravo).await?;
                println!("stopped");
                return Ok(());
            }
            _ => return Err("invalid peer control phase"),
        }
    }
    Err("peer control closed without stop")
}
