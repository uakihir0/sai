# Weighted Route Finder

Weighted Route Finder finds a minimum-time path through a small, directed
street graph. It uses Dijkstra's algorithm over a fixed set of intersections
and edges, then reconstructs the selected route from predecessor links.

```text
Depot to Terminal
101 Depot -> 303 Old Bridge -> 202 River Crossing -> 404 Civic Square
  -> 505 Hospital -> 606 Terminal
17 minutes

Tie-break fixture
101 Depot -> 202 River Crossing -> 505 Hospital
10 minutes
```

The first fixture has several ways to reach the terminal. The chosen route
takes 17 minutes. The tie-break fixture gives two equal-time paths to Hospital;
it keeps the path through River Crossing because its index is lower than Old
Bridge's. When multiple unsettled nodes have the same distance, the lower node
index is selected first. Tests cover both stable tie rules.
The reverse trip from the isolated terminal has no route, and unknown node IDs
return distinct origin or destination errors.

`model.sai` defines the graph, route, and error records. `data.sai` provides
the primary and tie-break fixtures. `lookup.sai` maps public node IDs to their
fixed array positions. `path.sai` performs the search and path reconstruction;
`lib.sai` exposes the fixtures and boundary tests.

## Package and compiler status

This is a pure `wasm32-core` library. It has no process package, I/O, external
service, mutable global state, or dynamic graph input. The implementation is
intentionally limited to six nodes, positive whole-minute edge weights, and
fixed-capacity arrays. A candidate path whose sum would reach `U32_MAX` or
overflow `U32` is not considered; the displayed route examples stay within
range, while boundary tests cover the excluded sums. Node IDs must be unique,
and edge endpoint indices must be within the node array.

This project illustrates graph search, cumulative costs, stable tie-breaking,
predecessor-based path reconstruction, unreachable destinations, and
`Result`-based errors. It is an illustrative package rather than a build
guarantee: the current compiler has not completed the semantic and package
pipeline needed to build this source.
