# Headless service CLI

The production binary is `uob`. Every command is noninteractive: it never reads a prompt from
standard input and never launches a browser. Service diagnostics go to standard error; commands
that produce machine-readable records use standard output only.

## Configuration

Both startup and offline validation read the same strict TOML document. A minimal API-only demo is:

```toml
[bridge]
id = "site-01"
environment = "demo"

[management]
listen_addr = "127.0.0.1:8080"
```

`environment` defaults to `production`, and the management listener defaults to
`127.0.0.1:8080`. The current service fails closed on non-loopback management listeners because
the TLS listener is not yet composed. Unknown sections and fields are rejected.

When a target is configured, `bridge.target_id` must name the single enabled `[[targets]]` entry.
Target `settings` are converted to the shared typed configuration boundary; names ending in
`_file` or containing `credential` are credential references, not inline secret values. The
registry still rejects kinds whose concrete factory is not present in this build.

The concrete outbound MQTT target, its TLS/plaintext rules, topic taxonomy, and broker integration
test are documented in [`MQTT target`](../configuration/mqtt-target.md).

## Commands and exit codes

Validate without binding a socket, resolving DNS, reading credentials, or starting adapters:

```text
uob config check --config bridge.toml
```

Success writes one JSON object to stdout:

```json
{"status":"valid"}
```

Start the service with the optional browser entry asset enabled or disabled:

```text
uob serve --config bridge.toml
uob serve --config bridge.toml --no-ui
```

`--no-ui` removes only the static browser route. Health, identity, metrics, and every management
API route remain mounted. The process does not open a browser in either mode.

Consume the configured management event stream as newline-delimited JSON:

```text
uob events --format jsonl
uob events --config staging.toml --after uob:event:41 --format jsonl
```

Without `--config`, `events` reads `bridge.toml`. Its default endpoint is the configured local
management listener at `/api/v1/events`. An explicit remote endpoint must use HTTPS and provide a
`credentials_file`; the file is read only when `events` connects and its trimmed content is sent
as a bearer credential. Redirects and proxy use are disabled so credentials stay bound to the
validated endpoint. SSE `data` records must be bounded valid JSON and are re-encoded as compact
JSONL. A server response that reflects the bearer value is rejected before it reaches stdout.

Exit code `0` means the requested operation completed successfully. Exit code `2` identifies
invalid arguments or configuration. Exit code `1` identifies runtime, network, stream, or output
failure. Diagnostics use stable sanitized categories and do not reproduce rejected configuration
values, credential contents, response bodies, or filesystem paths.
