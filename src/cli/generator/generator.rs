use std::fs;
use std::path::Path;

use anyhow::anyhow;
use gqlforge_valid::{ValidateInto, Validator};
use http::header::{HeaderMap, HeaderName, HeaderValue};
use inquire::Confirm;
use pathdiff::diff_paths;

use super::config::{Config, Resolved, Source};
use super::source::ConfigSource;
use crate::core::Mustache;
use crate::core::config::transformer::Preset;
use crate::core::config::{self, ConfigModule, ConfigReaderContext};
use crate::core::generator::{Generator as ConfigGenerator, Input};
use crate::core::proto_reader::ProtoReader;
use crate::core::resource_reader::{Resource, ResourceReader};
use crate::core::runtime::TargetRuntime;

/// CLI that reads the the config file and generates the required gqlforge
/// configuration.
pub struct Generator {
    /// path of config file.
    config_path: String,
    runtime: TargetRuntime,
}

impl Generator {
    #[must_use]
    pub fn new(config_path: &str, runtime: TargetRuntime) -> Self {
        Self { config_path: config_path.to_string(), runtime }
    }

    /// Writes the configuration to the output file if allowed.
    async fn write(self, graphql_config: &ConfigModule, output_path: &str) -> anyhow::Result<()> {
        let output_source = config::Source::detect(output_path)?;
        let config = match output_source {
            config::Source::GraphQL => graphql_config.to_sdl(),
            _ => return Err(anyhow!("Only graphql output format is currently supported")),
        };

        if Self::should_overwrite(output_path)? {
            self.runtime
                .file
                .write(output_path, config.as_bytes())
                .await?;

            tracing::info!("Config successfully generated at {output_path}");
        }

        Ok(())
    }

    /// Checks if the output file already exists and prompts for overwrite
    /// confirmation.
    fn should_overwrite(output_path: &str) -> anyhow::Result<bool> {
        if is_exists(output_path) {
            let should_overwrite = Confirm::new(
                format!(
                    "The output file '{output_path}' already exists. Do you want to overwrite it?"
                )
                .as_str(),
            )
            .with_default(false)
            .prompt()?;
            if !should_overwrite {
                return Ok(false);
            }
        }
        Ok(true)
    }

    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub async fn read(&self) -> anyhow::Result<Config<Resolved>> {
        let config_path = &self.config_path;
        let source = ConfigSource::detect(config_path)?;
        let mut config_content = self.runtime.file.read(config_path).await?;

        // While reading resolve the internal paths and mustache headers of
        // generalized config.
        let reader_context = ConfigReaderContext::new(&self.runtime);
        config_content = Mustache::parse(&config_content).render(&reader_context);

        let config: Config = match source {
            ConfigSource::Json => serde_json::from_str(&config_content)?,
            ConfigSource::Yml => serde_yaml_ng::from_str(&config_content)?,
        };

        config.into_resolved(config_path)
    }

    /// performs all the i/o's required in the config file and generates
    /// concrete vec containing data for generator.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub async fn resolve_io(&self, config: Config<Resolved>) -> anyhow::Result<Vec<Input>> {
        let mut input_samples = vec![];

        let reader = ResourceReader::cached(self.runtime.clone());
        let proto_reader = ProtoReader::init(reader.clone(), self.runtime.clone());
        let output_dir = Path::new(&config.output.path.0)
            .parent()
            .unwrap_or(Path::new(""));

        for input in config.inputs {
            match input.source {
                Source::Curl {
                    src,
                    field_name,
                    headers,
                    body,
                    method,
                    is_mutation,
                    is_subscription,
                } => {
                    let url = src.0;
                    let req_body = body.unwrap_or_default();
                    let method = method.unwrap_or_default();
                    let is_mutation = is_mutation.unwrap_or_default();
                    let is_subscription = is_subscription.unwrap_or_default();

                    if is_mutation && is_subscription {
                        return Err(anyhow!(
                            "A field cannot be both a mutation and a subscription"
                        ));
                    }

                    let request_method = method.to_hyper();
                    let mut request = reqwest::Request::new(request_method, url.parse()?);
                    if !req_body.is_null() {
                        request.body_mut().replace(req_body.to_string().into());
                    }
                    if let Some(headers_inner) = headers.as_btree_map() {
                        let mut header_map = HeaderMap::new();
                        for (key, value) in headers_inner {
                            let header_name = HeaderName::try_from(key)?;
                            let header_value = HeaderValue::try_from(value.clone())?;
                            header_map.insert(header_name, header_value);
                        }
                        *request.headers_mut() = header_map;
                    }

                    let resource: Resource = request.into();
                    let response = reader.read_file(resource).await?;
                    input_samples.push(Input::Json {
                        url: url.parse()?,
                        method,
                        req_body,
                        res_body: serde_json::from_str(&response.content)?,
                        field_name,
                        is_mutation,
                        is_subscription,
                        headers: headers.into_btree_map(),
                    });
                }
                Source::Proto { src, url, proto_paths, connect_rpc } => {
                    let path = src.0;
                    let proto_paths =
                        proto_paths.map(|paths| paths.into_iter().map(|l| l.0).collect::<Vec<_>>());
                    let mut metadata = proto_reader.read(&path, proto_paths.as_deref()).await?;
                    if let Some(relative_path_to_proto) = to_relative_path(output_dir, &path) {
                        metadata.path = relative_path_to_proto;
                    }
                    input_samples.push(Input::Proto { metadata, url, connect_rpc });
                }
                Source::Config { src } => {
                    let path = src.0;
                    let source = config::Source::detect(&path)?;
                    let schema = reader.read_file(path).await?.content;
                    input_samples.push(Input::Config { schema, source });
                }
            }
        }

        Ok(input_samples)
    }

    /// generates the final configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub async fn generate(self) -> anyhow::Result<ConfigModule> {
        let config = self.read().await?;
        let path = config.output.path.0.clone();
        let query_type = config.schema.query.clone();
        let mutation_type_name = config.schema.mutation.clone();
        let subscription_type_name = config.schema.subscription.clone();

        let preset = config.preset.clone().unwrap_or_default();
        let preset: Preset = preset.validate_into().to_result()?;
        let input_samples = self.resolve_io(config).await?;
        let mut config_gen = ConfigGenerator::default()
            .inputs(input_samples)
            .transformers(vec![Box::new(preset)]);

        if let Some(query_name) = query_type {
            config_gen = config_gen.query(query_name);
        }

        let config = config_gen
            .mutation(mutation_type_name)
            .subscription(subscription_type_name)
            .generate(true)?;

        self.write(&config, &path).await?;
        Ok(config)
    }
}

/// Checks if file or folder already exists or not.
fn is_exists(path: &str) -> bool {
    fs::metadata(path).is_ok()
}

/// Expects both paths to be absolute and returns a relative path from `from` to
/// `to`. expects `from` to be directory.
fn to_relative_path(from: &Path, to: &str) -> Option<String> {
    let from_path = Path::new(from).to_path_buf();
    let to_path = Path::new(to).to_path_buf();

    // Calculate the relative path from `from_path` to `to_path`
    diff_paths(to_path, from_path).map(|p| p.to_string_lossy().to_string())
}
