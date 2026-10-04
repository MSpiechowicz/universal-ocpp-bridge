use super::*;

#[tokio::test]
async fn complete_native_call_cap_rejects_one_byte_over_limit_and_accepts_exact_neighbor() {
    let native = boundary_native_request();
    let bytes = serde_json::to_vec(&native).unwrap();
    assert!(bytes.len() < LOCAL_AUTHORIZATION_BYTES_LIMIT_16);
    assert_eq!(frame_size("v", &native), LOCAL_AUTHORIZATION_BYTES_LIMIT_16);
    assert_eq!(
        frame_size("vv", &native),
        LOCAL_AUTHORIZATION_BYTES_LIMIT_16 + 1
    );
    let schema: Value = serde_json::from_str(include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/1.6/SendLocalList.json"
    ))
    .unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&native)
        .unwrap();
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, auth, _, _) = setup(&store, running.handle.clone()).await;
    snapshot.capabilities.operations.push(SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: "SendLocalList".to_owned(),
        },
        parameters: vec![],
    });
    let port = protected_session(&running, &snapshot, auth, &store, bytes);
    reject_without_native_call(
        port.clone(),
        request(&snapshot, "vv", "SendLocalList").admit(Clock.now()),
        &mut running,
    )
    .await;
    let coordinator = Arc::new(Coordinator::new(
        Arc::new(store.clone()),
        port,
        Arc::new(Clock),
    ));
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    ));
    let mut valid = request(&snapshot, "v", "SendLocalList");
    let correlation = CorrelationId::new("boundary-correlation").unwrap();
    valid.request.correlation_id = Some(correlation.clone());
    let pending = tokio::spawn(async move { commands.submit(valid).await.unwrap() });
    let frame = receive_json(&mut running.peer).await;
    assert_eq!(frame[1], "v");
    assert_eq!(frame[2], "SendLocalList");
    assert_eq!(
        serde_json::to_vec(&frame).unwrap().len(),
        LOCAL_AUTHORIZATION_BYTES_LIMIT_16
    );
    assert_eq!(frame[3], native);
    running
        .peer
        .send_text(json!([3,"v",{"status":"Accepted"}]).to_string())
        .await
        .unwrap();
    let result = pending.await.unwrap();
    assert_eq!(result.correlation_id, Some(correlation));
    assert_eq!(
        result.local_authorization_16,
        Some(LocalAuthorizationResult16::SendLocalList {
            list_version: -2,
            update_type: LocalListUpdateType16::Full,
            status: SendLocalListStatus16::Accepted,
        })
    );
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::ProtocolResponse { accepted: true, .. }
    ));
    assert_private_result(&database, &result);
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

fn protected_session(
    running: &RunningSession,
    snapshot: &StationSnapshot,
    auth: Arc<Auth>,
    store: &Store,
    bytes: Vec<u8>,
) -> Arc<RemoteControlSession> {
    let provider = Arc::new(
        LocalAuthorizationUpdates16::new(vec![ProtectedLocalListValue16 {
            resource: snapshot.station.clone(),
            reference: material::reference(),
            expires_at: time("2099-01-01T00:00:00Z"),
            update: ProtectedLocalListUpdate16::from_json_bytes(bytes).unwrap(),
        }])
        .unwrap(),
    );
    Arc::new(
        RemoteControlSession::new(
            running.handle.clone(),
            snapshot.clone(),
            Arc::new(LocalRemoteStartIdentity::new(vec![], auth).unwrap()),
            Arc::new(Clock),
            Arc::new(store.clone()),
        )
        .unwrap()
        .with_local_authorization_updates(provider),
    )
}

fn assert_private_result(database: &Database, result: &CommandResult) {
    let public = serde_json::to_string(result).unwrap();
    for forbidden in ["idTag", "parentIdTag", "localAuthorizationList", "list16:"] {
        assert!(!public.contains(forbidden));
    }
    for name in ["", "-wal"] {
        if let Ok(bytes) = std::fs::read(format!("{}{name}", database.0.display())) {
            for forbidden in ["idTag", "parentIdTag", "localAuthorizationList"] {
                assert!(
                    !bytes
                        .windows(forbidden.len())
                        .any(|window| window == forbidden.as_bytes())
                );
            }
        }
    }
}

async fn reject_without_native_call(
    port: Arc<RemoteControlSession>,
    command: Command<Value>,
    running: &mut RunningSession,
) {
    let mut pending = tokio::spawn(async move { port.dispatch(command).await.unwrap() });
    tokio::select! {
        result = &mut pending => assert!(matches!(result.unwrap(), CommandDispatchOutcome::NotTransmitted { .. })),
        frame = receive_json(&mut running.peer) => {
            assert_eq!(frame[1], "vv");
            assert_eq!(frame[2], "SendLocalList");
            assert_eq!(serde_json::to_vec(&frame).unwrap().len(), LOCAL_AUTHORIZATION_BYTES_LIMIT_16 + 1);
            panic!("a complete protected native CALL above the 64KiB feature cap reached the station");
        }
    }
}

fn frame_size(id: &str, native: &Value) -> usize {
    serde_json::to_vec(&json!([2, id, "SendLocalList", native]))
        .unwrap()
        .len()
}
fn boundary_native_request() -> Value {
    let overhead = frame_size("v", &json!({})) - 2;
    let target = LOCAL_AUTHORIZATION_BYTES_LIMIT_16 - overhead;
    let entries = (0..LOCAL_AUTHORIZATION_ENTRIES_LIMIT_16)
        .map(|index| {
            json!({"idTag":format!("{index:03}{}", "\0".repeat(17)),
            "idTagInfo":{"status":"Accepted","parentIdTag":"a".repeat(20)}})
        })
        .collect::<Vec<_>>();
    let mut native = json!({"listVersion":-2,"updateType":"Full","localAuthorizationList":entries});
    let base = serde_json::to_vec(&native).unwrap().len();
    assert!(base < target);
    let mut remaining = target - base;
    for entry in native["localAuthorizationList"].as_array_mut().unwrap() {
        let parent = (0..20)
            .map(|_| {
                if remaining >= 5 {
                    remaining -= 5;
                    '\0'
                } else if remaining > 0 {
                    remaining -= 1;
                    '\n'
                } else {
                    'a'
                }
            })
            .collect::<String>();
        entry["idTagInfo"]["parentIdTag"] = json!(parent);
    }
    assert_eq!(remaining, 0);
    assert_eq!(serde_json::to_vec(&native).unwrap().len(), target);
    native
}
