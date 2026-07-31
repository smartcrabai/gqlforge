use gqlforge::core::config::Config;
use gqlrs::parser::types::ServiceDocument;

pub fn build_service_document() -> ServiceDocument {
    Config::graphql_schema()
}

#[cfg(test)]
mod tests {
    #![expect(
        dead_code,
        reason = "test helper types define variants not used in assertions"
    )]
    use std::collections::HashSet;

    use gqlforge_typedefs_common::directive_definition::{
        Attrs, DirectiveDefinition, into_directive_definition,
    };
    use gqlforge_typedefs_common::scalar_definition::{ScalarDefinition, into_scalar_definition};
    use gqlforge_typedefs_common::{ServiceDocumentBuilder, into_schemars};
    use schemars::JsonSchema;

    #[derive(JsonSchema)]
    struct FooScalar(String);

    impl ScalarDefinition for FooScalar {
        fn scalar_definition() -> gqlrs::parser::types::TypeSystemDefinition {
            let schema = into_schemars::<Self>();
            into_scalar_definition(&schema, "FooScalar")
        }
    }

    #[derive(JsonSchema)]
    struct ComplexDirective {
        field1: i32,
        enum_field: FooEnum,
        custom_type: FooType,
    }

    impl DirectiveDefinition for ComplexDirective {
        fn directive_definition(
            generated_types: &mut HashSet<String>,
        ) -> Vec<gqlrs::parser::types::TypeSystemDefinition> {
            let root_schema = into_schemars::<Self>();

            into_directive_definition(
                &root_schema,
                Attrs {
                    name: "ComplexDirective",
                    repeatable: true,
                    locations: vec!["Schema"],
                    is_lowercase_name: false,
                },
                generated_types,
            )
        }
    }

    #[derive(JsonSchema)]
    enum FooEnum {
        Variant,
        Variant2,
        Variat3,
    }

    #[derive(JsonSchema)]
    struct FooType {
        field1: i32,
        field2: Option<i32>,
        field3: Vec<String>,
        inner_type: BarType,
    }

    #[derive(JsonSchema)]
    struct BarType {
        field2: BazType,
    }

    #[derive(JsonSchema)]
    struct BazType {
        field: i32,
    }

    #[test]
    fn it_works_for_into_scalar() {
        let builder = ServiceDocumentBuilder::new();
        let doc = builder.add_scalar(FooScalar::scalar_definition()).build();
        let actual = gqlforge::core::document::print(&doc);
        let expected = "scalar FooScalar".to_string();
        assert_eq!(actual, expected);
    }

    #[test]
    fn it_works_for_into_directive_with_complex_type() {
        let builder = ServiceDocumentBuilder::new();
        let doc = builder
            .add_directive(ComplexDirective::directive_definition(&mut HashSet::new()))
            .build();
        let actual = gqlforge::core::document::print(&doc);
        let expected = "directive @complexDirective(\n  field1: Int!\n  enum_field: FooEnum\n  custom_type: FooType\n) repeatable on SCHEMA\n\ninput FooType {\n  field1: Int!\n  field2: Int\n  field3: [String!]\n  inner_type: BarType\n}\n\ninput BarType {\n  field2: BazType\n}\n\ninput BazType {\n  field: Int!\n}\n\nenum FooEnum {\n  Variant\n  Variant2\n  Variat3\n}".to_string();

        assert_eq!(actual, expected);
    }
}
