# Simulator evidence in Debug

Demo and staging consoles can explicitly read a bounded snapshot from a separate
`uob-sim serve` process. Production omits the panel, keeps its same-origin-only
connection policy, and does not link simulator code. Opening Debug performs no
simulator request and starts no scenario or capture.

In the simulator's existing control TOML, provision an additional random 32-byte
hexadecimal token in a restricted file and opt into exactly one console origin:

```toml
[debug]
console_origin = "http://127.0.0.1:8080"
token_file = "simulator-debug-read.token"
```

Place the block after the top-level settings; it is separate from `[[scenarios]]`.
The Debug read token must differ from the control token. An independent `[control_browser]`
block can allow browser controls for the same console origin; `[debug]` alone never grants them.
Only canonical literal-loopback HTTP origins (`127.0.0.1` or `[::1]`) are accepted.
Existing demo/staging station, endpoint and import-isolation validation still applies.
The operator must point the allowlisted console origin at the corresponding test bridge.
This is trusted local configuration, not remote attestation. The browser and simulator must share
the same loopback network context. An isolated staging namespace requires running
the browser in that test context; this feature creates no namespace bypass.

Start scenarios using the authenticated control API or, after separately configuring
`[control_browser]`, the demo/staging browser's **Simulator scenario controls** panel.
In Debug, enter the IPv4 simulator origin (`http://127.0.0.1:9001`), run ID and
**Debug read** credential, then choose **Read simulator evidence**. The form
identifies the destination before sending.
Bridge credentials are never copied to the simulator. The read credential stays
only in tab memory and is cleared on disconnect, navigation or a failed read.
Refresh is manual, with one request at a time, a five-second deadline and a 1 MiB
response limit. Every refresh rechecks bridge identity before and after reading;
environment mismatch or failure clears previous evidence. Hidden tabs are marked
stale and cannot refresh. Run IDs and seeds remain exact unsigned decimal strings.

`GET /api/v1/debug/runs/{id}` is disabled unless `[debug]` is configured. It requires
the exact configured Origin and the separate read bearer. CORS preflight permits
only GET with Authorization; no cookies, wildcard origins or mutation methods are
allowed. The read credential cannot authenticate the original control routes.
Demo/staging HTML allows HTTP IPv4 loopback connections in its CSP; production HTML
retains `connect-src 'self'`. The browser validates an exact loopback origin and
refuses redirects before sending the simulator credential.

The version-1 evidence projection retains at most 256 steps and 770 logical events
(three per step plus run boundaries). The browser holds one snapshot and renders
one selected step. It shows declared expectations, actual completed action events,
detail-assertion presence, outcome, safe failure codes, configured versus selected
faults, and scheduled intervention metadata. Expected/actual wire payloads and
exception messages are omitted. A completed action event can coexist with a failed
detail assertion; neither simulator completion nor an OCPP acceptance proves a
physical charging effect. Final logical events appear only after worker cleanup;
pending steps on failed/cancelled runs may never have executed.

Optional canonical UUID `correlation_id` fields on steps/events navigate the existing
authorized Debug timeline. Missing, malformed or evicted correlations remain visibly
unavailable. The current simulator runner does not receive bridge correlation UUIDs,
so its ordinary snapshots report missing links; it never substitutes its logical
event IDs. A producer that supplies UUIDs must obtain them from server evidence.
Browser tests exercise supplied-ID navigation separately from real simulator reads.

This Debug evidence panel remains inspection-only: it cannot replay imported events,
inject faults, start/stop scenarios or issue charging commands. A separate, explicitly
opted-in [simulator control panel](browser-console.md#simulator-scenario-controls)
uses the control bearer and server-derived scenario catalog; neither the Debug read
credential nor the bridge management credential authorizes simulator changes.

Tests use a real simulator run with an intentionally failed assertion and the actual
daemon console, verify exact seeds and missing correlation, then exercise optional
correlation navigation and inert hostile text. Rust tests verify origin/method/token
isolation and structured actual-event evidence. Existing real-socket simulator tests
continue to cover both OCPP editions and injected faults.
