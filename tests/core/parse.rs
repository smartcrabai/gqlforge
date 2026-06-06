#![expect(clippy::unwrap_used, reason = "test code")]
extern crate core;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::panic;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::anyhow;
use async_graphql_value::ConstValue;
use gqlforge::cli::javascript;
use gqlforge::core::app_context::AppContext;
use gqlforge::core::blueprint::Blueprint;
use gqlforge::core::cache::InMemoryCache;
use gqlforge::core::config::{ConfigModule, Link, RuntimeConfig, Source};
use gqlforge::core::merge_right::MergeRight;
use gqlforge::core::runtime::TargetRuntime;
use gqlforge::core::worker::{Command, Event};
use gqlforge::core::{EnvIO, WorkerIO};
use markdown::ParseOptions;
use markdown::mdast::Node;

use super::file::File;
use super::http::Http;
use super::model::{APIRequest, Annotation, Mock};
use super::runtime::ExecutionSpec;

struct Env {
    env: HashMap<String, String>,
}

impl EnvIO for Env {
    fn get(&self, key: &str) -> Option<Cow<'_, str>> {
        self.env.get(key).map(Cow::from)
    }
}

impl Env {
    pub fn init(map: HashMap<String, String>) -> Self {
        Self { env: map }
    }
}

impl ExecutionSpec {
    #[expect(clippy::too_many_lines, reason = "test spec parser")]
    pub fn from_source(path: &Path, contents: &str) -> anyhow::Result<Self> {
        let ast = markdown::to_mdast(contents, &ParseOptions::default()).unwrap();
        let mut children = ast
            .children()
            .unwrap_or_else(|| panic!("Failed to parse {}: empty file unexpected", path.display()))
            .iter()
            .peekable();

        let mut name: Option<String> = None;
        let mut config = RuntimeConfig::default();
        let mut mock: Option<Vec<Mock>> = None;
        let mut env: Option<HashMap<String, String>> = None;
        let mut files: BTreeMap<String, String> = BTreeMap::new();
        let mut test: Option<Vec<APIRequest>> = None;
        let mut runner: Option<Annotation> = None;
        let mut check_identity = false;
        let mut sdl_error = false;
        let mut links_counter = 0;

        while let Some(node) = children.next() {
            match node {
                Node::Heading(heading) => {
                    if heading.depth == 1 {
                        // Parse test name
                        if name.is_none() {
                            if let Some(Node::Text(text)) = heading.children.first() {
                                name = Some(text.value.clone());
                            } else {
                                return Err(anyhow!(
                                    "Unexpected content of level 1 heading in {}: {heading:#?}",
                                    path.display()
                                ));
                            }
                        } else {
                            return Err(anyhow!(
                                "Unexpected double-declaration of test name in {}",
                                path.display()
                            ));
                        }

                        // Consume optional test description
                        if let Some(Node::Paragraph(_)) = children.peek() {
                            let _ = children.next();
                        }
                    } else if heading.depth == 2 {
                        // TODO: use frontmatter parsing instead of handle it as heading?
                        if let Some(Node::Text(expect)) = heading.children.first() {
                            let split = expect.value.splitn(2, ':').collect::<Vec<&str>>();
                            match split[..] {
                                [a, b] => {
                                    check_identity = a.contains("identity") && b.ends_with("true");
                                    sdl_error = a.contains("error") && b.ends_with("true");
                                    if a.contains("skip") && b.ends_with("true") {
                                        runner = Some(Annotation::Skip);
                                    }
                                }
                                _ => {
                                    return Err(anyhow!(
                                        "Unexpected header annotation {:?} in {}",
                                        expect.value,
                                        path.display(),
                                    ));
                                }
                            }
                        }
                    } else if heading.depth == 5 {
                        // Parse annotation
                        return if runner.is_none() {
                            if let Some(Node::Text(text)) = heading.children.first() {
                                Err(anyhow!(
                                    "Unexpected runner annotation {:?} in {}",
                                    text.value,
                                    path.display(),
                                ))
                            } else {
                                Err(anyhow!(
                                    "Unexpected content of level 5 heading in {}: {heading:#?}",
                                    path.display()
                                ))
                            }
                        } else {
                            Err(anyhow!(
                                "Unexpected double-declaration of runner annotation in {}",
                                path.display()
                            ))
                        };
                    } else if heading.depth == 4 {
                    } else {
                        return Err(anyhow!(
                            "Unexpected level {} heading in {}: {:#?}",
                            heading.depth,
                            path.display(),
                            heading
                        ));
                    }
                }
                Node::Code(code) => {
                    // Parse following code block
                    let (content, lang, meta) =
                        { (code.value.clone(), code.lang.clone(), code.meta.clone()) };
                    if let Some(meta_str) = meta.as_ref().filter(|s| s.contains('@')) {
                        let temp_cleaned_meta = meta_str.replace('@', "");
                        let name: &str = &temp_cleaned_meta;
                        if let Some(name) = name.strip_prefix("file:") {
                            if files.insert(name.to_string(), content).is_some() {
                                return Err(anyhow!(
                                    "Double declaration of file {name:?} in {}",
                                    path.display()
                                ));
                            }
                        } else {
                            let lang = match lang {
                                Some(x) => Ok(x),
                                None => Err(anyhow!(
                                    "Unexpected code block with no specific language in {}",
                                    path.display()
                                )),
                            }?;

                            let source = Source::from_str(&lang)?;

                            match name {
                                "config" => {
                                    config = config.merge_right(
                                        RuntimeConfig::from_source(source, &content).unwrap(),
                                    );
                                }
                                "schema" => {
                                    // Schemas configs are only parsed if the test isn't skipped.
                                    let name = format!("schema_{links_counter}.graphql");
                                    files.insert(name.clone(), content);
                                    config.links.push(Link { src: name, ..Default::default() });
                                    links_counter += 1;
                                }
                                "mock" => {
                                    if mock.is_none() {
                                        mock = match source {
                                            Source::Json => Ok(serde_json::from_str(&content)?),
                                            Source::Yml => Ok(serde_yaml_ng::from_str(&content)?),
                                            Source::GraphQL => Err(anyhow!(
                                                "Unexpected language in mock block in {} (only JSON and YAML are supported)",
                                                path.display()
                                            )),
                                        }?;
                                    } else {
                                        return Err(anyhow!(
                                            "Unexpected number of mock blocks in {} (only one is allowed)",
                                            path.display()
                                        ));
                                    }
                                }
                                "env" => {
                                    if env.is_none() {
                                        env = match source {
                                            Source::Json => Ok(serde_json::from_str(&content)?),
                                            Source::Yml => Ok(serde_yaml_ng::from_str(&content)?),
                                            Source::GraphQL => Err(anyhow!(
                                                "Unexpected language in env block in {} (only JSON and YAML are supported)",
                                                path.display()
                                            )),
                                        }?;
                                    } else {
                                        return Err(anyhow!(
                                            "Unexpected number of env blocks in {} (only one is allowed)",
                                            path.display()
                                        ));
                                    }
                                }
                                "test" => {
                                    if test.is_none() {
                                        test = match source {
                                            Source::Json => Ok(serde_json::from_str(&content)?),
                                            Source::Yml => Ok(serde_yaml_ng::from_str(&content)?),
                                            Source::GraphQL => Err(anyhow!(
                                                "Unexpected language in test block in {} (only JSON and YAML are supported)",
                                                path.display()
                                            )),
                                        }?;
                                    } else {
                                        return Err(anyhow!(
                                            "Unexpected number of test blocks in {} (only one is allowed)",
                                            path.display()
                                        ));
                                    }
                                }
                                _ => {
                                    return Err(anyhow!(
                                        "Unexpected component {name:?} in {}: {meta:#?}",
                                        path.display()
                                    ));
                                }
                            }
                        }
                    } else {
                        return Err(anyhow!(
                            "Unexpected content of code in {}: {meta:#?}",
                            path.display()
                        ));
                    }
                }
                Node::Definition(d) => {
                    if let Some(title) = &d.title {
                        tracing::info!("Comment found in: {:?} with title: {}", path, title);
                    }
                }
                Node::ThematicBreak(_) => {
                    // skip this for and put execute logic in heading.depth
                    // above to escape ThematicBreaks like
                    // `---`, `***` or `___`
                }
                _ => return Err(anyhow!("Unexpected node in {}: {node:#?}", path.display())),
            }
        }

        if links_counter == 0 {
            return Err(anyhow!(
                "Unexpected blocks in {}: You must define a GraphQL Schema in an execution test.",
                path.display()
            ));
        }

        let spec = ExecutionSpec {
            path: path.to_owned(),
            name: name.unwrap_or_else(|| path.file_name().unwrap().to_str().unwrap().to_string()),
            safe_name: path.file_name().unwrap().to_str().unwrap().to_string(),

            config,
            mock,
            env,
            test,
            files,

            runner,

            check_identity,
            sdl_error,
        };

        anyhow::Ok(spec)
    }

    pub async fn app_context(
        &self,
        config: &ConfigModule,
        env: HashMap<String, String>,
        http: Arc<Http>,
    ) -> Arc<AppContext> {
        let blueprint = Blueprint::try_from(config).unwrap();

        let script = blueprint.server.script.clone();

        let http2_only = http.clone();

        let http_worker: Option<Arc<dyn WorkerIO<Event, Command>>> =
            if let Some(script) = script.clone() {
                Some(javascript::init_worker_io(script))
            } else {
                None
            };

        let worker: Option<Arc<dyn WorkerIO<ConstValue, ConstValue>>> = if let Some(script) = script
        {
            Some(javascript::init_worker_io(script))
        } else {
            None
        };

        let runtime = TargetRuntime {
            http,
            http2_only,
            file: Arc::new(File::new(self.clone())),
            env: Arc::new(Env::init(env)),
            cache: Arc::new(InMemoryCache::default()),
            extensions: Arc::new(vec![]),
            cmd_worker: http_worker,
            worker,
            postgres: HashMap::new(),
            postgres_listeners: HashMap::new(),
            s3: HashMap::new(),
        };

        let endpoints = config
            .extensions()
            .endpoint_set
            .clone()
            .into_checked(&blueprint, runtime.clone())
            .await
            .unwrap();

        Arc::new(AppContext::new(blueprint, runtime, endpoints))
    }
}
