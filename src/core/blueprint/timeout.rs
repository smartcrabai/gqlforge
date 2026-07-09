use std::sync::Arc;
use std::time::Duration;

use gqlrs::extensions::{Extension, ExtensionContext, ExtensionFactory, NextExecute};
use gqlrs::{Response, ServerError};
use gqlrs_value::ConstValue;
use tokio::time::timeout;

pub struct GlobalTimeout;

impl ExtensionFactory for GlobalTimeout {
    fn create(&self) -> Arc<dyn Extension> {
        Arc::new(GlobalTimeoutExtension)
    }
}

struct GlobalTimeoutExtension;

#[async_trait::async_trait]
impl Extension for GlobalTimeoutExtension {
    async fn execute(
        &self,
        ctx: &ExtensionContext<'_>,
        operation_name: Option<&str>,
        next: NextExecute<'_>,
    ) -> Response {
        let future = next.run(ctx, operation_name);
        if let Ok(ConstValue::Number(number)) = ctx.data::<ConstValue>() {
            let timeout_duration = number.as_u64().unwrap_or(0);
            if timeout_duration > 0 {
                let result = timeout(Duration::from_millis(timeout_duration), future).await;
                if let Ok(result) = result {
                    return result;
                }

                let mut response = Response::new(ConstValue::Null);
                response.errors = vec![ServerError::new("Global timeout".to_string(), None)];
                return response;
            }
        }

        future.await
    }
}
