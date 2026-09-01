use std::sync::Arc;

use futures_util::{StreamExt, TryFutureExt};
use gqlrs::dynamic::{self, FieldFuture, FieldValue, SchemaBuilder, TypeRef};
use gqlrs_value::ConstValue;
use tracing::Instrument;

use crate::core::blueprint::{Blueprint, Definition, ObjectTypeDefinition};
use crate::core::graphql::execute_graphql_streaming_request;
use crate::core::grpc::request::execute_grpc_streaming_request;
use crate::core::http::{RequestContext, execute_http_streaming_request};
use crate::core::ir::model::{IO, IR};
use crate::core::ir::{EvalContext, ResolverContext, TypedValue};
use crate::core::jit::graphql_error::ErrorExtensions;
use crate::core::redis::RedisStreamSource;
use crate::core::scalar::Scalar;

/// We set the default value for an `InputValue` by reading it from the
/// blueprint and assigning it to the provided `InputValue` during the
/// generation of the `gqlrs::Schema`. The `InputValue` represents the
/// structure of arguments and their types that can be passed to a field. In
/// other GraphQL implementations, this is commonly referred to as
/// `InputValueDefinition`.
fn set_default_value(
    input_value: dynamic::InputValue,
    value: Option<serde_json::Value>,
) -> dynamic::InputValue {
    if let Some(value) = value {
        match ConstValue::from_json(value) {
            Ok(const_value) => input_value.default_value(const_value),
            Err(err) => {
                tracing::warn!(
                    "conversion from serde_json::Value to ConstValue failed for default_value with error {err:?}"
                );
                input_value
            }
        }
    } else {
        input_value
    }
}

fn to_field_value(value: gqlrs::Value) -> FieldValue<'static> {
    match value {
        ConstValue::List(vec) => FieldValue::list(vec.into_iter().map(to_field_value)),
        value => {
            let type_name = value.get_type_name().map(std::string::ToString::to_string);

            let field_value = FieldValue::from(value);

            if let Some(type_name) = type_name {
                field_value.with_type(type_name)
            } else {
                field_value
            }
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "handles all type definition variants"
)]
#[expect(
    clippy::unwrap_used,
    reason = "RequestContext is always present in the GraphQL resolver context"
)]
fn to_type(def: &Definition) -> dynamic::Type {
    match def {
        Definition::Object(def) => {
            let mut object = dynamic::Object::new(def.name.clone());
            for field in &def.fields {
                let field = field.clone();
                let type_ref = TypeRef::from(&field.of_type);
                let field_name = &field.name.clone();

                let mut dyn_schema_field = dynamic::Field::new(
                    field_name,
                    type_ref.clone(),
                    move |ctx| {
                        // region: HOT CODE
                        // --------------------------------------------------
                        //                HOT CODE STARTS HERE
                        // --------------------------------------------------

                        let req_ctx = ctx.ctx.data::<Arc<RequestContext>>().unwrap();
                        let field_name = &field.name;

                        match &field.resolver {
                            None => {
                                let ctx: ResolverContext = ctx.into();
                                let ctx = EvalContext::new(req_ctx, &ctx);

                                match ctx
                                    .path_value(&[field_name])
                                    .map(std::borrow::Cow::into_owned)
                                {
                                    Some(ConstValue::Null) => FieldFuture::Value(FieldValue::NONE),
                                    a => FieldFuture::from_value(a),
                                }
                            }
                            Some(expr) => {
                                let span = tracing::info_span!(
                                    "field_resolver",
                                    otel.name = ctx.path_node.map(|p| p.to_string()).unwrap_or(field_name.clone()), graphql.returnType = %type_ref
                                );

                                let expr = expr.to_owned();
                                FieldFuture::new(
                                    async move {
                                        let ctx: ResolverContext = ctx.into();
                                        let ctx = &mut EvalContext::new(req_ctx, &ctx);

                                        let value =
                                            expr.eval(ctx).await.map_err(|err| err.extend())?;

                                        if let ConstValue::Null = value {
                                            Ok(FieldValue::NONE)
                                        } else {
                                            Ok(Some(to_field_value(value)))
                                        }
                                    }
                                    .instrument(span)
                                    .inspect_err(|err| tracing::error!(?err)),
                                )
                            }
                        }

                        // --------------------------------------------------
                        //                HOT CODE ENDS HERE
                        // --------------------------------------------------
                        // endregion: hot_code
                    },
                );
                if let Some(description) = &field.description {
                    dyn_schema_field = dyn_schema_field.description(description);
                }
                for arg in &field.args {
                    dyn_schema_field = dyn_schema_field.argument(set_default_value(
                        dynamic::InputValue::new(arg.name.clone(), TypeRef::from(&arg.of_type)),
                        arg.default_value.clone(),
                    ));
                }
                object = object.field(dyn_schema_field);
            }
            if let Some(description) = &def.description {
                object = object.description(description);
            }
            for interface in &def.implements {
                object = object.implement(interface.clone());
            }

            dynamic::Type::Object(object)
        }
        Definition::Interface(def) => {
            let mut interface = dynamic::Interface::new(def.name.clone());
            for field in &def.fields {
                interface = interface.field(dynamic::InterfaceField::new(
                    field.name.clone(),
                    TypeRef::from(&field.of_type),
                ));
            }

            dynamic::Type::Interface(interface)
        }
        Definition::InputObject(def) => {
            let mut input_object = dynamic::InputObject::new(def.name.clone());
            for field in &def.fields {
                let mut input_field =
                    dynamic::InputValue::new(field.name.clone(), TypeRef::from(&field.of_type));
                if let Some(description) = &field.description {
                    input_field = input_field.description(description);
                }
                let input_field = set_default_value(input_field, field.default_value.clone());
                input_object = input_object.field(input_field);
            }
            if let Some(description) = &def.description {
                input_object = input_object.description(description);
            }

            dynamic::Type::InputObject(input_object)
        }
        Definition::Scalar(def) => {
            let mut scalar = dynamic::Scalar::new(def.name.clone());
            if let Some(description) = &def.description {
                scalar = scalar.description(description);
            }
            let name = def.scalar.clone();
            scalar = scalar.validator(move |v| name.validate(v));
            dynamic::Type::Scalar(scalar)
        }
        Definition::Enum(def) => {
            let mut enum_type = dynamic::Enum::new(def.name.clone());
            for value in &def.enum_values {
                enum_type = enum_type.item(dynamic::EnumItem::new(value.name.clone()));
            }
            if let Some(desc) = def.description.clone() {
                enum_type = enum_type.description(desc);
            }
            dynamic::Type::Enum(enum_type)
        }
        Definition::Union(def) => {
            let mut union = dynamic::Union::new(def.name.clone());
            for type_ in &def.types {
                union = union.possible_type(type_.clone());
            }
            dynamic::Type::Union(union)
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "handles all subscription field types"
)]
#[expect(
    clippy::unwrap_used,
    reason = "RequestContext is always present in the GraphQL subscription context"
)]
fn to_subscription_type(def: &ObjectTypeDefinition) -> dynamic::Type {
    let mut subscription = dynamic::Subscription::new(def.name.clone());

    for field in &def.fields {
        let field = field.clone();
        let type_ref = TypeRef::from(&field.of_type);
        let field_for_closure = field.clone();

        let mut sub_field =
            dynamic::SubscriptionField::new(field.name.clone(), type_ref, move |ctx| {
                let field = field_for_closure.clone();
                dynamic::SubscriptionFieldFuture::new(async move {
                    let req_ctx = ctx.ctx.data::<Arc<RequestContext>>().unwrap().clone();

                    // Extract the streaming IO from the resolver
                    let io = match &field.resolver {
                        Some(IR::IO(io)) => io.as_ref().clone(),
                        _ => {
                            return Err(gqlrs::Error::new("Subscription field missing resolver"));
                        }
                    };

                    let ctx: ResolverContext = ctx.into();
                    let eval_ctx = EvalContext::new(&req_ctx, &ctx);

                    let stream: std::pin::Pin<
                        Box<
                            dyn futures_util::Stream<
                                    Item = Result<ConstValue, crate::core::ir::Error>,
                                > + Send,
                        >,
                    > = match io {
                        IO::GrpcStream { req_template, hook: _hook } => {
                            let rendered = req_template.render(&eval_ctx).map_err(|e| {
                                gqlrs::Error::new(format!("Failed to render gRPC request: {e}"))
                            })?;

                            let request = rendered.to_request().map_err(|e| {
                                gqlrs::Error::new(format!("Failed to build gRPC request: {e}"))
                            })?;

                            execute_grpc_streaming_request(
                                &req_ctx.runtime,
                                &req_template.operation,
                                request,
                            )
                            .await
                            .map_err(|e| gqlrs::Error::new(format!("gRPC streaming failed: {e}")))?
                        }
                        IO::GraphQLStream { req_template, field_name, stream_url } => {
                            let request = req_template.to_request(&eval_ctx).map_err(|e| {
                                gqlrs::Error::new(format!("Failed to build GraphQL request: {e}"))
                            })?;

                            execute_graphql_streaming_request(
                                &req_ctx.runtime,
                                &stream_url,
                                request,
                                &field_name,
                            )
                            .await
                            .map_err(|e| {
                                gqlrs::Error::new(format!("GraphQL SSE streaming failed: {e}"))
                            })?
                        }
                        IO::HttpStream { req_template, hook: _hook } => {
                            let request = req_template
                                .to_request(&eval_ctx)
                                .map_err(|e| {
                                    gqlrs::Error::new(format!("Failed to build HTTP request: {e}"))
                                })?
                                .into_request();

                            execute_http_streaming_request(&req_ctx.runtime, request)
                                .await
                                .map_err(|e| {
                                    gqlrs::Error::new(format!("HTTP SSE streaming failed: {e}"))
                                })?
                        }
                        IO::PostgresStream { connection_id, channel, payload_type } => {
                            let listener = req_ctx
                                .runtime
                                .postgres_listeners
                                .get(&connection_id)
                                .ok_or_else(|| {
                                gqlrs::Error::new(format!(
                                    "PostgreSQL listener '{connection_id}' not configured"
                                ))
                            })?;
                            listener
                                .subscribe(&channel, payload_type)
                                .await
                                .map_err(|e| gqlrs::Error::new(format!("LISTEN failed: {e}")))?
                                .map(|r| r.map_err(|e| crate::core::ir::Error::IO(e.to_string())))
                                .boxed()
                        }
                        IO::RedisStream { connection_id, source, payload_type } => {
                            let listener = req_ctx
                                .runtime
                                .redis_listeners
                                .get(&connection_id)
                                .ok_or_else(|| {
                                    gqlrs::Error::new(format!(
                                        "Redis listener '{connection_id}' not configured"
                                    ))
                                })?;

                            match source {
                                RedisStreamSource::PubSub { channel } => {
                                    let channel = channel.render(&eval_ctx);
                                    listener
                                        .subscribe(&channel, payload_type)
                                        .await
                                        .map_err(|e| {
                                            gqlrs::Error::new(format!("SUBSCRIBE failed: {e}"))
                                        })?
                                        .map(|r| {
                                            r.map_err(|e| crate::core::ir::Error::IO(e.to_string()))
                                        })
                                        .boxed()
                                }
                                RedisStreamSource::Stream { key, start_id } => {
                                    let key = key.render(&eval_ctx);
                                    let start_id = start_id.render(&eval_ctx);
                                    listener
                                        .read_stream(&key, &start_id, payload_type)
                                        .await
                                        .map_err(|e| {
                                            gqlrs::Error::new(format!("XREAD failed: {e}"))
                                        })?
                                        .map(|r| {
                                            r.map_err(|e| crate::core::ir::Error::IO(e.to_string()))
                                        })
                                        .boxed()
                                }
                            }
                        }
                        _ => {
                            return Err(gqlrs::Error::new(
                                "Subscription field does not have a streaming resolver",
                            ));
                        }
                    };

                    Ok(stream.map(
                        |result: Result<ConstValue, crate::core::ir::Error>| match result {
                            Ok(value) => Ok(FieldValue::from(value)),
                            Err(e) => Err(gqlrs::Error::new(e.to_string())),
                        },
                    ))
                })
            });

        if let Some(description) = &field.description {
            sub_field = sub_field.description(description);
        }
        for arg in &field.args {
            sub_field = sub_field.argument(set_default_value(
                dynamic::InputValue::new(arg.name.clone(), TypeRef::from(&arg.of_type)),
                arg.default_value.clone(),
            ));
        }
        subscription = subscription.field(sub_field);
    }

    if let Some(description) = &def.description {
        subscription = subscription.description(description);
    }

    dynamic::Type::Subscription(subscription)
}

impl From<&Blueprint> for SchemaBuilder {
    fn from(blueprint: &Blueprint) -> Self {
        let query = blueprint.query();
        let mutation = blueprint.mutation();
        let subscription = blueprint.subscription();
        let mut schema =
            dynamic::Schema::build(query.as_str(), mutation.as_deref(), subscription.as_deref());

        schema = inject_custom_scalars(schema, blueprint);

        for def in &blueprint.definitions {
            // Register subscription type definitions as Subscription, not
            // Object
            if let Definition::Object(obj_def) = def
                && subscription.as_deref() == Some(&obj_def.name)
            {
                schema = schema.register(to_subscription_type(obj_def));
                continue;
            }
            schema = schema.register(to_type(def));
        }

        schema
    }
}

fn inject_custom_scalars(mut schema: SchemaBuilder, blueprint: &Blueprint) -> SchemaBuilder {
    fn inject_scalar(schema: SchemaBuilder, type_name: &str) -> SchemaBuilder {
        if let Some(scalar) = Scalar::find(type_name) {
            let scalar = scalar.clone();
            schema.register(dynamic::Type::Scalar(
                dynamic::Scalar::new(scalar.name()).validator(move |val| scalar.validate(val)),
            ))
        } else {
            schema
        }
    }

    for ty in blueprint.definitions.clone() {
        match ty {
            Definition::Interface(interface_type_definition) => {
                for field in interface_type_definition.fields {
                    let type_name = field.of_type.name();
                    schema = inject_scalar(schema, type_name);
                }
            }
            Definition::Object(object_type_definition) => {
                for field in object_type_definition.fields {
                    let type_name = field.of_type.name();
                    schema = inject_scalar(schema, type_name);
                }
            }
            Definition::InputObject(input_object_type_definition) => {
                for field in input_object_type_definition.fields {
                    let type_name = field.of_type.name();
                    schema = inject_scalar(schema, type_name);
                }
            }
            _ => (),
        }
    }
    schema
}
