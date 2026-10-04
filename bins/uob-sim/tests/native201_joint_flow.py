"""Real-process native cache, delayed/lost ACK, restart and no-replay phases."""
import hashlib
import json
from datetime import datetime, timedelta, timezone


def toml_value(value):
    if isinstance(value, dict):
        return "{" + ",".join(json.dumps(key) + "=" + toml_value(item) for key, item in value.items()) + "}"
    if isinstance(value, list):
        return "[" + ",".join(toml_value(item) for item in value) + "]"
    return json.dumps(value, ensure_ascii=False)


def scenario_document(steps, seed=112):
    return "schema_version=1\nseed=" + str(seed) + "\n" + "".join(
        "\n[[steps]]\n" + "\n".join(key + "=" + toml_value(value) for key, value in step.items()) + "\n"
        for step in steps)


def step(name, action, **fields):
    return {"id": name, "station": "alpha", "action": action, "timeout_ms": 5000, **fields}


def boot():
    return step("boot", "boot", fixture_id="sim.ocpp201.boot.accepted",
                payload={"reason": "PowerUp", "chargingStation": {"vendorName": "UOB", "model": "Simulator"}})


def authorize(name, token, status):
    return step(name, "authorize", fixture_id="wire.ocpp201.authorization.valid",
                payload={"idToken": {"idToken": token, "type": "Local"}},
                expect_response={"idTokenInfo": {"status": status}})


def parked():
    return step("park", "wait", timeout_ms=65000, duration_ms=60000)


def private_state(h):
    return json.loads((h.output / "sim-state.json").read_text())


def list_digest(state):
    return hashlib.sha256(json.dumps(state["list"], sort_keys=True).encode()).hexdigest()


def future_expiry():
    return (datetime.now(timezone.utc) + timedelta(seconds=60)).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def run_strengthened(h, daemon, sim, require, wait_for, private):
    state = private_state(h)
    require(state["version"] == 7 and len(state["list"]) == 1 and len(state["cache"]) == 2,
            "real distinct cache/list population missing")
    require(any(entry["entry"]["idTokenInfo"]["status"] == "Invalid" for entry in state["cache"].values()),
            "latest nonaccepted TransactionEvent response was not cached")
    before_list = list_digest(state)
    require(h.command("cache-only-clear", "ClearCache")["status"] == "Accepted", "nonempty ClearCache failed")
    state = private_state(h)
    require(state["version"] == 7 and not state["cache"] and list_digest(state) == before_list,
            "ClearCache changed list or failed to clear real cache")
    require(h.command("query-after-real-clear", "GetLocalListVersion")["version_number"] == 7,
            "cache clear changed installed version")
    h.evidence["checks"].append("actual ClearCache removes populated cache and preserves exact different-identity list")
    sim.kill()
    sim.wait(timeout=10)
    # Fault is armed before Boot, so an observed fresh Accepted Boot proves readiness.
    steps = [step("connect", "connect"),
             step("loaded-list-empty-cache", "assert_local_authorization",
                  expect_response={"listVersion": 7, "listEntries": 1, "cacheEntries": 0}),
             step("arm-delay", "delay_local_reply", duration_ms=300), boot(),
             authorize("central-authority-survives-clear", "central-policy-112", "Accepted"),
             authorize("local-list-is-not-central-authority", "list-marker-112-x", "Unknown"),
             step("delayed-commit", "await_local_authorization",
                  expect_response={"listVersion": 20, "listEntries": 1, "cacheEntries": 2}), parked()]
    path = private(h.output / "delayed-scenario.toml", scenario_document(steps))
    prior_boots = len(h.relay.accepted_boots())
    sim = h.start(h.simulator, ["run", "--config", str(h.output / "sim.toml"), "--scenario", str(path), "--format", "jsonl"], "delayed-simulator")
    h.await_station(sim, prior_boots)
    def repopulated():
        require(sim.poll() is None, "independent simulator exited during native Authorize cache repopulation")
        return len(private_state(h)["cache"]) == 2
    wait_for(repopulated, "actual positive/negative Authorize replies did not repopulate cache")
    require(sim.poll() is None, "independent simulator exited before delayed command admission")
    mutations = len(h.relay.mutations())
    require(h.command("delayed-diff", "SendLocalList", "delay-diff")["status"] == "Accepted", "delayed Diff failed")
    observed = h.relay.mutations()[mutations:]
    require(len(observed) == 1 and observed[0].get("reply_status") == "Accepted"
            and observed[0].get("reply_elapsed_ms", 0) >= 250, "no real delayed native ACK observed")
    state = private_state(h)
    require(state["version"] == 20 and len(state["cache"]) == 2 and list_digest(state) == before_list,
            "delayed update/cache persistence changed native list")
    h.evidence["checks"].append("real native delayed ACK, distinct CSMS authorization preserved after Clear")
    sim.kill()
    sim.wait(timeout=10)

    # Genuine killed-process reopen retains both actual list and actual cache.
    steps = [step("connect", "connect"),
             step("recovered-list-and-cache", "assert_local_authorization",
                  expect_response={"listVersion": 20, "listEntries": 1, "cacheEntries": 2}),
             step("arm-drop", "drop_local_reply"), boot(),
             step("dropped-commit", "await_local_authorization",
                  expect_response={"listVersion": 21, "listEntries": 1, "cacheEntries": 2}), parked()]
    path = private(h.output / "dropped-scenario.toml", scenario_document(steps))
    prior_boots = len(h.relay.accepted_boots())
    sim = h.start(h.simulator, ["run", "--config", str(h.output / "sim.toml"), "--scenario", str(path), "--format", "jsonl"], "dropped-simulator")
    h.await_station(sim, prior_boots)
    mutations = len(h.relay.mutations())
    uncertain = h.command("dropped-diff", "SendLocalList", "drop-diff", uncertain=True)
    require(uncertain["lifecycle"]["stage"] == "transmission_uncertain"
            and uncertain.get("local_authorization_201") is None, "dropped native ACK fabricated certainty")
    observed = h.relay.mutations()[mutations:]
    require(len(observed) == 1 and "reply_status" not in observed[0], "dropped mutation was retried or acknowledged")
    state = private_state(h)
    require(state["version"] == 21 and len(state["cache"]) == 2 and list_digest(state) == before_list,
            "dropped ACK did not retain actual private mutation/cache")
    private(h.output / "uncertain-result.json", json.dumps(uncertain, indent=2))
    h.evidence["checks"].append("actual lost mutating ACK commits privately while service persists transmission uncertainty")
    sim.kill()
    sim.wait(timeout=10)
    daemon.terminate()
    daemon.wait(timeout=10)
    baseline = h.relay.mutations()

    daemon = h.start_daemon("restarted-daemon")
    steps = [step("connect", "connect"),
             step("reopened-positive-list-cache", "assert_local_authorization",
                  expect_response={"listVersion": 21, "listEntries": 1, "cacheEntries": 2}), boot(),
             step("observation-window", "wait", timeout_ms=10500, duration_ms=10000),
             step("disconnect", "disconnect")]
    path = private(h.output / "reopened-scenario.toml", scenario_document(steps))
    prior_boots = len(h.relay.accepted_boots())
    sim = h.start(h.simulator, ["run", "--config", str(h.output / "sim.toml"), "--scenario", str(path), "--format", "jsonl"], "reopened-simulator")
    h.await_station(sim, prior_boots)
    require(h.relay.mutations() == baseline, "daemon restart automatically replayed a mutating native command")
    status, persisted = h.http("/api/v1/commands/dropped-diff", grant="read")
    require(status == 200 and persisted["lifecycle"]["stage"] == "transmission_uncertain"
            and persisted.get("local_authorization_201") is None, "restart rewrote historical uncertainty")
    require(h.command("fresh-query-after-service-restart", "GetLocalListVersion")["version_number"] == 21,
            "fresh independent query lost installed positive version")
    require(h.relay.mutations() == baseline, "fresh query caused an old mutation replay")
    h.evidence["checks"].append("genuine simulator kill/reopen list+cache; service restart preserves uncertainty without native command replay")

    require(h.command("diff-delete", "SendLocalList", "differential-delete-after-restart")["status"] == "Accepted", "native deletion failed")
    require(h.command("stale-diff", "SendLocalList", "drop-diff")["status"] == "VersionMismatch", "stale Diff mutated")
    require(h.command("query-deleted", "GetLocalListVersion")["version_number"] == 22, "emptied installed version lost")
    require(h.command("lower-full-empty", "SendLocalList", "full-absent")["status"] == "Accepted", "lower Full omission failed")
    require(h.command("query-empty-installed", "GetLocalListVersion")["version_number"] == 1, "positive empty list version lost")
    h.evidence["native_mutation_observations"] = h.relay.mutations()
    sim.wait(timeout=15)
    require(sim.returncode == 0, "reopened independent simulator scenario failed")
    return daemon
