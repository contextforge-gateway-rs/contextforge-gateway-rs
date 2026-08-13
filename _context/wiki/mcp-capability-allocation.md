# Phase 3 Final Architecture

> **Tentative:** this is the proposed Phase 3 final architecture and remains
> subject to review.

### 1. Create a Virtual Server and Select Capabilities

```mermaid
sequenceDiagram
    autonumber
    actor User
    participant UI as Admin UI or API
    participant CP as Control Plane
    participant DB as Control Plane DB
    participant MCP1 as MCP Server 1
    participant MCP2 as MCP Server 2
    participant Redis
    participant DP as Rust Dataplane

    User->>UI: Create virtual server
    UI->>CP: Submit virtual server
    CP->>DB: Store virtual server

    User->>UI: Assign MCP Server 1 and MCP Server 2
    UI->>CP: Update backend associations
    CP->>DB: Store backend associations

    par Discover MCP Server 1
        CP->>MCP1: server/discover and list methods
        MCP1-->>CP: Capabilities and catalog
    and Discover MCP Server 2
        CP->>MCP2: server/discover and list methods
        MCP2-->>CP: Capabilities and catalog
    end

    CP->>DB: Reconcile normalized catalog
    User->>UI: View available tools
    UI->>CP: Request reconciled catalog
    CP->>DB: Read catalog
    DB-->>CP: inc, sum, dec, diff
    CP-->>UI: Display available tools

    User->>UI: Allow inc and sum
    UI->>CP: Update virtual server policy
    CP->>DB: Store selected tools and policy

    CP->>CP: Compile user runtime snapshot
    CP->>Redis: Atomically publish revision N
    Redis-->>DP: Revision notification
    DP->>Redis: Load revision N
    DP->>DP: Replace local cache atomically
```

### 2. Discover the Server and List Tools

```mermaid
sequenceDiagram
    autonumber
    participant Client as Modern MCP Client
    participant Ingress
    participant DP as Rust Dataplane
    participant Cache as Local Cache
    participant Redis

    Client->>Ingress: server/discover
    Ingress->>DP: Forward modern MCP request
    DP->>DP: Validate authentication and metadata
    DP->>Cache: Get virtual server snapshot

    alt Snapshot available
        Cache-->>DP: Snapshot revision N
    else Snapshot missing or expired
        Cache-->>DP: Cache miss
        DP->>Redis: Read compiled snapshot
        Redis-->>DP: Snapshot revision N
        DP->>Cache: Store revision N
    end

    DP-->>Client: Server identity and capabilities
    Client->>Ingress: tools/list
    Ingress->>DP: Forward modern MCP request
    DP->>Cache: Read visible tools
    Cache-->>DP: inc and sum
    DP-->>Client: tools/list result

    Note over DP,Redis: Redis distributes compiled state
    Note over DP: No live upstream call for discovery or lists
```

### 3. Call a Tool

```mermaid
sequenceDiagram
    autonumber
    participant Client as Modern MCP Client
    participant Ingress
    participant DP as Rust Dataplane
    participant Cache as Local Cache
    participant CPEX as Policy and CPEX
    participant MCP as Selected MCP Server

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
    Note over DP: Control Plane, DB and Redis are not on this result path
```

### 4. Reconcile an Upstream Catalog Change

```mermaid
sequenceDiagram
    autonumber
    participant MCP as MCP Server
    participant CP as Control Plane Reconciler
    participant DB as Control Plane DB
    participant Redis
    participant DP as Rust Dataplane
    participant Client as Subscribed MCP Client

    MCP-->>CP: Tools list changed
    CP->>MCP: Refresh discovery and tools list
    MCP-->>CP: Updated catalog
    CP->>DB: Reconcile catalog changes
    CP->>CP: Recompile affected snapshots
    CP->>Redis: Atomically publish revision N plus 1

    Redis-->>DP: Revision notification
    DP->>Redis: Load revision N plus 1
    DP->>DP: Replace local cache atomically

    opt Client subscribed to tool list changes
        DP-->>Client: Tools list changed notification
    end

    Client->>DP: tools/list
    DP-->>Client: Updated list from local snapshot
```
