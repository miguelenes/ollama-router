CREATE TABLE IF NOT EXISTS model_operations (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at REAL NOT NULL,
    finished_at REAL,
    models_json TEXT NOT NULL,
    nodes_json TEXT NOT NULL,
    targets_json TEXT NOT NULL
);
