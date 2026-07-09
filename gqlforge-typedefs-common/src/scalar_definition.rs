use gqlrs::Name;
use gqlrs::parser::types::{TypeDefinition, TypeKind, TypeSystemDefinition};
use schemars::Schema;

use crate::common::{get_description, pos};

pub trait ScalarDefinition {
    fn scalar_definition() -> TypeSystemDefinition;
}

pub fn into_scalar_definition(schema: &Schema, name: &str) -> TypeSystemDefinition {
    let description = schema
        .as_object()
        .and_then(|o| get_description(o))
        .map(std::borrow::ToOwned::to_owned);
    TypeSystemDefinition::Type(pos(TypeDefinition {
        name: pos(Name::new(name)),
        kind: TypeKind::Scalar,
        description: description.map(pos),
        directives: vec![],
        extend: false,
    }))
}
