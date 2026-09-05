fn main() {
    println!(
        "{}",
        serde_json::to_string_pretty(&uob_ems_scada_http_target_adapter::openapi_document())
            .expect("OpenAPI JSON")
    );
}
