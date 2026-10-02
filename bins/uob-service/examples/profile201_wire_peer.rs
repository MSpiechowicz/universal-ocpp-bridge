//! Private, loopback-only independent wire peer for actual-process qualification.
#[path = "profile201_wire_peer/config.rs"]
mod config;
#[path = "../tests/charging_profiles201/peer.rs"]
#[allow(dead_code)]
mod peer;

use peer::{
    Peer, Reply, Socket,
    wire::{envelope, receive, send},
};
use serde_json::{Value, json};
use std::{io, time::Duration};
use tokio::{io::BufReader, time::Instant};

#[tokio::main]
async fn main() {
    if run().await.is_err() {
        println!("{{\"event\":\"peer_failed\"}}");
        std::process::exit(1);
    }
    // A cancelled blocking stdin read must not hold the completed peer process alive.
    std::process::exit(0);
}

async fn run() -> io::Result<()> {
    let config = config::startup("configuration", config::configuration())?;
    let (mut socket, mut peer) = connect(&config).await?;
    let mut input = BufReader::new(tokio::io::stdin());
    let mut control_line = Vec::new();
    let mut pending: Vec<(Instant, Value)> = vec![];
    println!("{{\"event\":\"ready\"}}");
    loop {
        let next = pending
            .iter()
            .map(|(deadline, _)| *deadline)
            .min()
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            call = receive(&mut socket) => {
                let call = call?;
                if call[0] == 3 {
                    println!("{{\"event\":\"station_result\"}}");
                    continue;
                }
                envelope(&call, Reply::Error("FormationViolation"))?;
                let reply = peer.apply(call[2].as_str().unwrap(), &call[3])?;
                if std::mem::take(&mut peer.controls.disconnect_after_apply) {
                    socket.close(None).await.map_err(|_| io::Error::other("close failed"))?;
                    println!("{{\"event\":\"disconnected\"}}");
                    return Ok(());
                }
                if pending.len() >= 128 { return Err(io::Error::other("pending bound")) }
                pending.push((Instant::now() + peer.controls.delay, envelope(&call, reply)?));
                println!("{{\"event\":\"received\"}}");
            }
            () = tokio::time::sleep_until(next), if !pending.is_empty() => {
                let now = Instant::now();
                let mut index = 0;
                while index < pending.len() {
                    if pending[index].0 <= now {
                        let (_, response) = pending.remove(index);
                        send(&mut socket, response).await?;
                        println!("{{\"event\":\"reply\"}}");
                    } else {
                        index += 1;
                    }
                }
            }
            line = config::line(&mut input, &mut control_line) => {
                let Some(line) = line? else { return Ok(()) };
                let command: Value = serde_json::from_slice(&line)
                    .map_err(|_| io::Error::other("invalid control"))?;
                match command["action"].as_str() {
                    Some("inspect") => inspect(&peer),
                    Some("phase") => {
                        peer.set_phase(evse(&command)?, command["supported"].as_bool()
                            .ok_or_else(|| io::Error::other("invalid phase"))?);
                        println!("{{\"event\":\"phase\"}}");
                    }
                    Some("transaction") => {
                        let id = command["id"].as_str()
                            .ok_or_else(|| io::Error::other("invalid transaction"))?;
                        peer.transaction(&mut socket, evse(&command)?, id,
                            command["ended"].as_bool().unwrap_or(false)).await?;
                        println!("{{\"event\":\"transaction\"}}");
                    }
                    Some("heartbeat") => {
                        send(&mut socket, json!([2,uuid::Uuid::new_v4().to_string(),"Heartbeat",{}])).await?;
                        println!("{{\"event\":\"heartbeat\"}}");
                    }
                    Some("controls") => {
                        let millis = command["delay_ms"].as_u64().unwrap_or(0);
                        if millis > 30000 { return Err(io::Error::other("invalid delay")) }
                        peer.controls.delay = Duration::from_millis(millis);
                        peer.controls.reject_next = command["reject_next"].as_bool().unwrap_or(false);
                        peer.controls.disconnect_after_apply = command["disconnect_after_apply"].as_bool().unwrap_or(false);
                        println!("{{\"event\":\"controls\"}}");
                    }
                    Some("reconnect") => {
                        socket.close(None).await.map_err(|_| io::Error::other("close failed"))?;
                        pending.clear();
                        (socket, peer) = connect(&config).await?;
                        println!("{{\"event\":\"ready\"}}");
                    }
                    Some("quit") => return Ok(()),
                    _ => return Err(io::Error::other("invalid control action")),
                }
            }
        }
    }
}

async fn connect(config: &config::Config) -> io::Result<(Socket, Peer)> {
    let mut socket = config::connect(config).await?;
    let mut peer = config::startup("state_open", Peer::open(config.state_file.clone()))?;
    peer.set_phase(config.phase_evse, config.phase_supported);
    let result = config::startup("boot_exchange", Peer::boot(&mut socket).await)?;
    if result["status"] != "Accepted" {
        return config::startup("boot_registration", Err(io::Error::other("boot denied")));
    }
    Ok((socket, peer))
}

fn evse(command: &Value) -> io::Result<u32> {
    command["evse"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0 && i32::try_from(*n).is_ok())
        .ok_or_else(|| io::Error::other("invalid EVSE"))
}

fn inspect(peer: &Peer) {
    let profiles: Vec<_> = peer
        .profiles()
        .iter()
        .map(|payload| {
            let p = &payload["chargingProfile"];
            let s = &p["chargingSchedule"][0];
            json!({"id":p["id"],"evse":payload["evseId"],"purpose":p["chargingProfilePurpose"],
            "stack":p["stackLevel"],"schedule_id":s["id"],"unit":s["chargingRateUnit"],
            "periods":s["chargingSchedulePeriod"]})
        })
        .collect();
    println!(
        "{}",
        json!({"event":"inspect","profiles":profiles,
        "counters":{"set":peer.counters.set,"clear":peer.counters.clear,
            "get_variables":peer.counters.get_variables}})
    );
}
