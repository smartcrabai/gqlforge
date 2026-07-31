use std::cell::Cell;

use gqlforge::core::Errata;
use gqlforge::core::tracing::default_tracing_gqlforge;
use tracing::subscriber::DefaultGuard;

thread_local! {
    static TRACING_GUARD: Cell<Option<DefaultGuard>> = const { Cell::new(None) };
}

fn run_blocking() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .on_thread_start(|| {
            // initialize default tracing setup for the cli execution for every thread that
            // is spawned based on https://github.com/tokio-rs/tracing/issues/593#issuecomment-589857097
            // and required due to the fact that later for tracing the global subscriber
            // will be set by `src/cli/opentelemetry.rs` and until that we need
            // to use the default tracing configuration for cli output. And
            // since `set_default` works only for current thread incorporate this
            // with tokio runtime
            let guard = tracing::subscriber::set_default(default_tracing_gqlforge());

            TRACING_GUARD.set(Some(guard));
        })
        .on_thread_stop(|| {
            TRACING_GUARD.take();
        })
        .enable_all()
        .build()?;
    rt.block_on(async { gqlforge::cli::run().await })
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "main returns Ok(()) after explicit process::exit on error"
)]
#[expect(
    clippy::expect_used,
    reason = "rustls provider initialization failure is fatal"
)]
fn main() -> anyhow::Result<()> {
    // Initialize rustls CryptoProvider first (using aws_lc_rs)
    // Explicit configuration required when both aws-lc-rs and ring are present as
    // dependencies
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls CryptoProvider");

    // enable tracing subscriber for current thread until this block ends
    // that will show any logs from cli itself to the user
    // despite of @telemetry settings that
    let _guard = tracing::subscriber::set_default(default_tracing_gqlforge());
    let result = run_blocking();
    match result {
        Ok(()) => {}
        Err(error) => {
            // Ensure all errors are converted to Errata before being printed.
            let cli_error: Errata = error.into();
            tracing::error!("{}", cli_error.color(true));
            std::process::exit(exitcode::CONFIG);
        }
    }
    Ok(())
}
