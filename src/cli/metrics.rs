use anyhow::Result;

use crate::core::runtime::TargetRuntime;

fn cache_metrics(runtime: &TargetRuntime) {
    let meter = opentelemetry::global::meter("cache");
    let cache = runtime.cache.clone();
    let _gauge = meter
        .f64_observable_gauge("cache.hit_rate")
        .with_description("Cache hit rate ratio")
        .with_callback(move |observer| {
            if let Some(hit_rate) = cache.hit_rate() {
                observer.observe(hit_rate, &[]);
            }
        })
        .build();
}

///
/// # Errors
///
/// Returns an error if the operation fails.
pub async fn init_metrics(runtime: &TargetRuntime) -> Result<()> {
    cache_metrics(runtime);
    Ok(())
}
