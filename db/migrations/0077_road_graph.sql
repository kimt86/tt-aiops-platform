-- Routable directed road graph — persisted from the hourly road-network inference so the Rust dispatch
-- cost can route on it (replacing the 225m grid, which can't represent narrow bridges / one-way lanes).
-- reinfer_roadgraph.sh already infers the skeleton links + orients each edge to the learned lane flow
-- (one-way + speed); this just writes that directed graph in a form the matcher can load + Dijkstra.
-- Rebuilt whole each run (TRUNCATE + reload), ~268 nodes / ~370 directed edges.
CREATE TABLE IF NOT EXISTS road_node (
  id  int PRIMARY KEY,
  lat double precision NOT NULL,
  lon double precision NOT NULL
);
CREATE TABLE IF NOT EXISTS road_edge (
  from_id   int NOT NULL,          -- flow-direction origin node (edge flipped to lane flow at build time)
  to_id     int NOT NULL,
  len_m     double precision NOT NULL,
  speed_kmh double precision,      -- learned lane mean speed at the edge (NULL → Rust uses a fallback)
  oneway    boolean NOT NULL       -- if false, the Rust router also traverses to→from
);
CREATE INDEX IF NOT EXISTS road_edge_from ON road_edge (from_id);
