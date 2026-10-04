use serde_json::Value;
use std::sync::LazyLock;

const SCHEMAS: [&str; 4] = [
    include_str!(
        "../../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SendLocalListRequest.json"
    ),
    include_str!(
        "../../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetLocalListVersionResponse.json"
    ),
    include_str!(
        "../../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SendLocalListResponse.json"
    ),
    include_str!("../../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ClearCacheResponse.json"),
];
pub(super) fn valid(index: usize, value: &Value) -> bool {
    static VALIDATORS: LazyLock<Vec<jsonschema::Validator>> = LazyLock::new(|| {
        SCHEMAS
            .iter()
            .map(|source| {
                let schema: Value = serde_json::from_str(source).expect("pinned native schema");
                jsonschema::options()
                    .should_validate_formats(true)
                    .build(&schema)
                    .expect("pinned native schema")
            })
            .collect()
    });
    VALIDATORS[index].is_valid(value)
}
