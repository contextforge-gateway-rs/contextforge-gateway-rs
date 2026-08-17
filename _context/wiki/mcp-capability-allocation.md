# Phase 3 Final Architecture

> **Tentative:** this is the proposed Phase 3 final architecture and remains
> subject to review.

## Scope and Boundaries

The examples below use tools, but the same ownership applies to resources,
prompts, completions, and other aggregate or targeted MCP methods.

- The fast dataplane serves modern MCP `2026-07-28` clients. Legacy downstream
  clients remain on the slow path.
- The control plane owns upstream fan-out, capability aggregation, catalog
  retrieval and pagination, selection policy, and liveness polling.
- The control plane compiles effective configuration per user, team, or other
  principal and publishes it one way to the dataplane through the shared
  configuration store. Redis is the store shown in these examples.
- For targeted methods, the fast dataplane calls exactly one selected modern
  or legacy upstream. ContextForge does not depend on a durable upstream MCP
  session; legacy upstream session handling is best effort.
- MCP subscriptions and list-changed notifications remain Phase 4 work and are
  outside this Phase 3 architecture.

### 1. Create a Virtual Server and Select Capabilities

```mermaid
sequenceDiagram
    autonumber
    actor User
    participant UI as Admin UI or API
    participant CP as Control Plane
    participant DB as Control Plane DB
    participant MCP1 as Modern MCP Server
    participant MCP2 as Legacy MCP Server
    participant Store as Shared Config Store (Redis)
    participant DP as Fast Rust Dataplane

    User->>UI: Create virtual server
    UI->>CP: Submit virtual server
    CP->>DB: Store virtual server

    User->>UI: Assign MCP Server 1 and MCP Server 2
    UI->>CP: Update backend associations
    CP->>DB: Store backend associations

    par Inspect modern upstream
        CP->>MCP1: Discover capabilities and retrieve catalogs
        MCP1-->>CP: Capabilities and catalog
    and Inspect legacy upstream
        CP->>MCP2: Initialize or discover and retrieve catalogs
        MCP2-->>CP: Capabilities and catalog
    end

    CP->>DB: Reconcile normalized catalog
    User->>UI: View available catalog entries
    UI->>CP: Request reconciled catalog
    CP->>DB: Read catalog
    DB-->>CP: inc, sum, dec, diff
    CP-->>UI: Display available catalog entries

    User->>UI: Allow inc and sum
    UI->>CP: Update virtual server policy
    CP->>DB: Store selected tools and policy

    CP->>CP: Compile effective principal snapshot
    CP->>Store: Atomically publish revision N
    Store-->>DP: Configuration revision available
    DP->>Store: Load revision N
    DP->>DP: Replace local cache atomically

    Note over CP,MCP2: Control Plane handles upstream protocol and pagination
    Note over CP,DP: Effective configuration flows one way from CP to DP
```

### 2. Discover the Server and List Tools

```mermaid
sequenceDiagram
    autonumber
    participant Client as Modern MCP Client
    participant Ingress
    participant DP as Fast Rust Dataplane
    participant Cache as Local Cache
    participant Store as Shared Config Store (Redis)

    Client->>Ingress: server/discover
    Ingress->>DP: Forward modern MCP request
    DP->>DP: Validate authentication and metadata
    DP->>Cache: Get virtual server snapshot

    alt Snapshot available
        Cache-->>DP: Snapshot revision N
    else Snapshot missing or expired
        Cache-->>DP: Cache miss
        DP->>Store: Read compiled snapshot
        Store-->>DP: Snapshot revision N
        DP->>Cache: Store revision N
    end

    DP-->>Client: Server identity and capabilities
    Client->>Ingress: tools/list
    Ingress->>DP: Forward modern MCP request
    DP->>Cache: Read visible tools
    Cache-->>DP: inc and sum
    DP-->>Client: tools/list result

    Note over DP,Store: The shared store distributes compiled state
    Note over DP: No live upstream call for discovery or aggregate lists
```

### 3. Call a Tool

```mermaid
sequenceDiagram
    autonumber
    participant Client as Modern MCP Client
    participant Ingress
    participant DP as Fast Rust Dataplane
    participant Cache as Local Cache
    participant CPEX as Policy and CPEX
    participant MCP as Selected Modern or Legacy MCP Server

    Client->>Ingress: tools/call name inc
    Ingress->>DP: Forward modern MCP request
    DP->>DP: Validate authentication and metadata
    DP->>Cache: Resolve exposed tool inc
    Cache-->>DP: Backend MCP, upstream name inc, allowed

    DP->>CPEX: Run pre-call policy
    CPEX-->>DP: Allow or modify request
    DP->>MCP: tools/call name inc
    MCP-->>DP: Tool result
    DP->>CPEX: Run post-call policy
    CPEX-->>DP: Allow or modify result
    DP-->>Client: Return tool result directly

    Note over DP,MCP: Exactly one upstream is called
    Note over DP,MCP: No durable upstream MCP session is required
    Note over DP: Control Plane, DB and Redis are not on this result path
```

### 4. Reconcile an Upstream Catalog Change

```mermaid
sequenceDiagram
    autonumber
    participant MCP as Modern or Legacy MCP Server
    participant CP as Control Plane Reconciler
    participant DB as Control Plane DB
    participant Store as Shared Config Store (Redis)
    participant DP as Fast Rust Dataplane
    participant Client as Modern MCP Client

    CP->>MCP: Poll liveness and refresh discovery and lists
    MCP-->>CP: Updated catalog
    CP->>DB: Reconcile catalog changes
    CP->>CP: Recompile affected snapshots
    CP->>Store: Atomically publish revision N plus 1

    Store-->>DP: Configuration revision available
    DP->>Store: Load revision N plus 1
    DP->>DP: Replace local cache atomically

    Client->>DP: tools/list
    DP-->>Client: Updated list from local snapshot

    Note over DP,Client: Phase 4 owns MCP list-change notifications
```
