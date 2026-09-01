# ADR 0001: Modular monolith and package boundaries

- Status: accepted
- Date: 2026-09-01
- Decision scope: issue #1

## Context

The bridge must run predictably on 64-bit Raspberry Pi 4/5 devices while also
producing Linux ARM64 and x86-64 artifacts. Charging workflows need stable,
application-owned contracts and must not become coupled to a selected protocol,
target, database, management framework, or browser build.

## Decision

Use one Cargo workspace and a modular-monolith production service. Contracts,
domain rules, and application coordination are protected packages. Protocol,
target, operational-storage, external-export, provider, and management concerns
are adapter packages. Concrete libraries belong only to their owning adapters,
and construction happens in `uob-service`.

The charger simulator and release manager are separate executable packages. The
production service cannot depend on or start either executable. Browser assets
will use a separately isolated TypeScript/React/Vite build; Rust packages do not
require a Node.js or Python runtime.

Extensions are compiled Rust implementations registered at the service
composition root. Dynamic plugins and scripting engines are excluded from the
first release.

The initial release explicitly excludes hardware/electrical-safety control,
OCPP SOAP, OCPP 2.1, live payment processing, an OPC UA implementation, and
vendor-specific client integrations. Those exclusions do not weaken the adapter
boundaries reserved for later work.

Run `./scripts/check-boundaries.sh` locally and in CI integration. It rejects
forbidden dependency edges and transport, database, UI, topic, or industrial
mapping details in protected packages. `./scripts/test-boundaries.sh` also proves
that representative dependency and source violations are rejected.

## Consequences

- Application workflows can be tested without a concrete adapter or runtime.
- Adding an adapter requires registration in the composition root, not branches
  inside charging workflows.
- Adapter packages may depend inward; protected packages cannot depend outward.
- The monorepo enables atomic protocol/scenario review without linking simulator
  implementation into production.
