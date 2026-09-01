# Runtime identity configuration

The service composition root owns bridge, environment, release, process, and selected-target
identity. Network requests cannot supply or override these values. `RuntimeIdentity` is attached to
events, exports, and diagnostics, while `GET /api/v1/identity` exposes the complete trusted service
context. A service with no selected target still exposes this endpoint and does not construct or
enable MQTT.

Production is the default environment. A production deployment should supply configuration owned
by the service account and release metadata verified from the installed immutable artifact:

```toml
[bridge]
id = "site-01"
environment = "production"
target_id = "main"

[release]
id = "1.0.0-rc.1"
digest = "sha256:<verified-artifact-digest>"
```

Staging and demo instances must be explicit and use distinct bridge identities, listeners, data,
credentials, and targets:

```toml
[bridge]
id = "site-01-staging"
environment = "staging"
target_id = "staging-http"

[release]
id = "1.0.0-rc.1"
digest = "sha256:<verified-candidate-digest>"
```

```toml
[bridge]
id = "local-demo"
environment = "demo"
# No target_id: management API only.

[release]
id = "development"
digest = "sha256:<local-build-digest>"
```

Startup rejects a target selection whose bridge or environment differs from the service identity.
Every process invocation generates a new UUID process identity; bridge, environment, and release
identity remain stable until their trusted configuration or installed artifact changes.
