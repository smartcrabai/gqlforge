use gqlrs::parser::types::{ServiceDocument, TypeSystemDefinition};
use schemars::{JsonSchema, Schema};
mod common;
pub mod directive_definition;
mod enum_definition;
pub mod input_definition;
pub mod scalar_definition;

#[must_use]
pub fn into_schemars<T>() -> Schema
where
    T: JsonSchema,
{
    schemars::schema_for!(T)
}

pub struct ServiceDocumentBuilder {
    definitions: Vec<TypeSystemDefinition>,
}

impl Default for ServiceDocumentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceDocumentBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self { definitions: vec![] }
    }

    #[must_use]
    pub fn add_directive(
        mut self,
        definitions: Vec<TypeSystemDefinition>,
    ) -> ServiceDocumentBuilder {
        self.definitions.extend(definitions);
        self
    }

    #[must_use]
    pub fn add_scalar(mut self, definitions: TypeSystemDefinition) -> ServiceDocumentBuilder {
        self.definitions.push(definitions);
        self
    }

    #[must_use]
    pub fn add_input(mut self, definitions: TypeSystemDefinition) -> ServiceDocumentBuilder {
        self.definitions.push(definitions);
        self
    }

    #[must_use]
    pub fn build(self) -> ServiceDocument {
        ServiceDocument { definitions: self.definitions }
    }
}
