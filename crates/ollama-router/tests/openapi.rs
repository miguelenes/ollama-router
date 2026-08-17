//! OpenAPI admin surface parity with registered `/router/v1/*` routes.

use std::collections::BTreeSet;

use ollama_router::http::ADMIN_OPERATION_IDS;

fn openapi_yaml_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../site/openapi/openapi.yaml")
}

fn documented_operation_ids(yaml: &str) -> BTreeSet<String> {
    yaml.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed.strip_prefix("operationId: ")
        })
        .map(str::to_string)
        .collect()
}

#[test]
fn documented_operation_ids_match_registered_admin_routes() {
    let path = openapi_yaml_path();
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let documented = documented_operation_ids(&yaml);
    let registered: BTreeSet<_> = ADMIN_OPERATION_IDS
        .iter()
        .map(|id| (*id).to_string())
        .collect();
    assert_eq!(
        documented, registered,
        "OpenAPI operationIds must match ADMIN_OPERATION_IDS in http/mod.rs"
    );
}
