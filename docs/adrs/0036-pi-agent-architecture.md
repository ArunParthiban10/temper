# ADR-0036: Pi-Compatible Governed Agent Architecture

## Status

Accepted

## Context

`pi-mono` and OpenClaw already validate a useful agent architecture: append-only session trees, context compaction, a two-loop steering model, lazy skills, event streaming, and transport/channel adapters. The existing `TemperAgent` proves the basic governed loop, but it still stores flat conversation JSON, exposes only a poll-centric control plane, and keeps most capabilities inside a single agent/tool implementation boundary.

We want the Temper version of that architecture, but we do not want to wrap Pi as an opaque subprocess. The Temper runtime needs each capability to remain spec-driven, Cedar-governed, observable, and verifiable.

## Decision

Rebase `TemperAgent` onto Pi's architecture and express the missing capabilities as governed Temper specs and WASM integrations:

- Session tree storage with JSONL append-only entries and branch tracking
- Explicit compaction and steering states in the TemperAgent IOA
- Soul, skill, memory, hook, heartbeat, and cron capabilities as first-class entities
- SSE-based lifecycle and progress streaming for entities
- Channel adapters and routing entities for OpenClaw-style transports
- Thin tool dispatch that executes sandbox tools directly and routes entity capabilities through OData

The `TemperAgent` remains the execution boundary, but the richer architecture is decomposed into separate governed entities instead of extending a monolithic match-arm tool runner.

## Alternatives Considered

1. Wrap Pi as a subprocess

Rejected. This would preserve Pi semantics, but the actual runtime behavior would sit outside Temper governance, Cedar authorization, and IOA verification.

2. Build a new agent stack from scratch without Pi compatibility

Rejected. Pi and OpenClaw already validate the core interaction patterns we need. Re-learning those design choices inside a brand-new implementation adds unnecessary risk.

3. Extend the existing TemperAgent toward Pi incrementally

Chosen. This keeps the proven Temper dispatch/runtime model while migrating the storage format, state machine, event transport, and capability surface toward the Pi/OpenClaw architecture.

## Consequences

- `TemperAgent` conversation persistence changes from flat JSON to JSONL session-tree storage.
- New entity types are introduced in the `temper-agent` and `openclaw-channels` OS apps.
- Additional WASM modules are required for compaction, steering, heartbeat scanning, cron triggering, and channel routing.
- Event streaming becomes part of the agent contract instead of an optional side channel.
- Capability growth shifts from tool-runner branching to governed entity composition.
