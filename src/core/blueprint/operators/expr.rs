use gqlforge_valid::{Valid, Validator};
use gqlrs_value::ConstValue;

use crate::core::blueprint::{BlueprintError, DynamicValue, to_json_schema};
use crate::core::config;
use crate::core::config::Expr;
use crate::core::ir::model::IR;
use crate::core::ir::model::IR::Dynamic;

fn validate_data_with_schema(
    config: &config::Config,
    field: &config::Field,
    gql_value: &ConstValue,
) -> Valid<(), BlueprintError> {
    match to_json_schema(&field.type_of, config)
        .validate(gql_value)
        .to_result()
    {
        Ok(()) => Valid::succeed(()),
        Err(err) => Valid::from_validation_err(BlueprintError::from_validation_str(&err)),
    }
}

#[derive(Clone, Copy)]
pub struct CompileExpr<'a> {
    pub config_module: &'a config::ConfigModule,
    pub field: &'a config::Field,
    pub expr: &'a Expr,
    pub validate: bool,
}

#[must_use]
pub fn compile_expr(inputs: CompileExpr) -> Valid<IR, BlueprintError> {
    let config_module = inputs.config_module;
    let field = inputs.field;
    let value = &inputs.expr.body;
    let validate = inputs.validate;

    match DynamicValue::try_from(&value.clone()) {
        Ok(data) => Valid::succeed(data),
        Err(err) => Valid::fail(BlueprintError::Error(err)),
    }
    .and_then(|value| {
        if value.is_const() {
            let data = &value;
            match data.try_into() {
                Ok(gql) => {
                    let validation = if validate {
                        validate_data_with_schema(config_module, field, &gql)
                    } else {
                        Valid::succeed(())
                    };
                    validation.map(|()| Dynamic(value.clone()))
                }
                Err(e) => Valid::fail(BlueprintError::InvalidJson(e)),
            }
        } else {
            // TODO: Add validation for const with Mustache here
            Valid::succeed(Dynamic(value.clone()))
        }
    })
}
