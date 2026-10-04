use super::schemas::Schemas;

impl Schemas {
    pub fn admitted(&self) -> jsonschema::Validator {
        self.validator(
            &self.document["paths"]["/bridge/v1/commands"]["post"]["responses"]["202"]["content"]
                ["application/json"]["schema"],
        )
    }
}
