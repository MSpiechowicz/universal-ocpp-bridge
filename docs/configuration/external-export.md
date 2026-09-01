# External export configuration

External database export is optional and independent of the selected bridge target. Disabling it
creates no provider, connection, polling task, or export queue. Enabling it selects exactly one
stable provider instance; the first concrete provider kind will be `postgresql`.

The safe PostgreSQL configuration shape is:

```toml
[data_export]
enabled = true
id = "analytics"
kind = "postgresql"
revision = 4

[data_export.settings]
host = "database.example"
port = 5432
database = "charging_production"
schema = "uob"
credentials_file = "/etc/uob/production/secrets/postgresql.toml"
tls_mode = "verify-full"
```

`schema` must name the bridge-owned schema accepted by the provider. Credentials are a protected
file reference resolved only when the provider starts; passwords, connection strings, raw SQL, and
database command fields are not configuration or management API surfaces. Production and staging
require TLS with certificate and hostname verification plus a credential reference. Only an
explicitly isolated demo may relax those transport requirements.

Every pending batch and checkpoint carries both the stable provider ID and immutable configuration
revision. A restart may continue using that exact destination. Changing the ID, revision, or
disabling export is rejected while records remain pending unless they first drain. Destructive
discard is a separate authorized operation: validation requires a durable audit-event ID naming the
exact old destination, and returns that proof to the host for journaling. Pending records are never
silently relabeled or sent to a newly configured database.

The provider catalog may advertise a recognized but unavailable kind so configuration clients can
distinguish “not installed in this executable” from an unknown kind. Catalog schemas never include
credential contents. The service currently reserves PostgreSQL this way; its later driver will use
the same offline registry and validation boundary.
