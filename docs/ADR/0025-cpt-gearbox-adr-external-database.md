# External database configuration belongs to the deployment profile

Status: accepted, 2026-10-18.

## Context

Issue Artifizer/gearbox#4 describes a product whose projected `cap.db` is known
and whose generated AppConfig nevertheless lacks a database. `needs_db` alone
cannot configure the runtime. Cluster provider connections configure a different
service and cannot stand in for toolkit-db connections.

## Decision

Each deployment profile may declare `database = postgres(...)`, as documented in
`docs/gdl.md`. Version 1 supports one external Postgres server per profile, a
password environment reference, optional per-gear database-name overrides and
an existing Kubernetes Secret. No connection is made while evaluating, resolving
or generating. No credentials are expanded or embedded.

The resolver expands the selected DB gears, checks all resulting names, and
records the connection and complete gear/name map in schema-2 `product.lock`.
A missing connection is GBX0306, attached to the profile and naming the gear.
Generators consume the recorded decision. Applications with DB gears explicitly
enable `cf-gears-toolkit-db`'s `pg` feature: toolkit-db has no default backend,
so the declaration must affect the build as well as the configuration. Registry
planning includes this dependency alongside toolkit, preserving the existing
version checks and path-patch unification. Each process receives database
sections only for its own DB gears; merely spawning a DB worker does not give
the host that worker's database configuration.

The Helm values/schema expose a dedicated `database` Secret mapping, so a DB
password and a cluster-provider credential may come from different Secrets.
The operator supplies existing infrastructure, databases, roles and Secrets.
Bundled database subcharts remain outside the PRD's release scope.

GDL advances from 1.0 to 1.1. Existing non-DB descriptions remain valid. Existing
DB descriptions must declare their connection; schema-1 locks must be resolved
again, never silently reinterpreted or rehashed.

## Confirmation

`gearbox-engine/tests/database.rs` exercises real GDL evaluation, resolution,
canonical lock read/write/diff, runtime YAML and Helm generation. GDL product
tests reject credential literals, invalid ports and invalid names. Runtime and
render measurements are reported separately: parser or fixture success alone
is not evidence that an external server accepted a connection.
