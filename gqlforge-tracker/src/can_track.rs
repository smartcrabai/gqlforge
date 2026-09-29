use std::env;

use gqlforge_version::VERSION;

const LONG_ENV_FILTER_VAR_NAME: &str = "GQLFORGE_TRACKER";
const SHORT_ENV_FILTER_VAR_NAME: &str = "GF_TRACKER";

/// Checks if tracking is enabled
pub fn can_track() -> bool {
    let is_prod = !VERSION.is_dev();
    let usage_enabled = env::var(LONG_ENV_FILTER_VAR_NAME)
        .or(env::var(SHORT_ENV_FILTER_VAR_NAME))
        .map(|v| !v.eq_ignore_ascii_case("false"))
        .ok();
    can_track_inner(is_prod, usage_enabled)
}

fn can_track_inner(is_prod_build: bool, usage_enabled: Option<bool>) -> bool {
    if let Some(usage_enabled) = usage_enabled {
        usage_enabled
    } else {
        is_prod_build
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usage_enabled_true() {
        for (is_prod_build, usage_enabled, expected) in [
            (true, Some(true), true),
            (false, Some(true), true),
            (true, Some(false), false),
            (false, Some(false), false),
            (true, None, true),
            (false, None, false),
        ] {
            assert_eq!(
                can_track_inner(is_prod_build, usage_enabled),
                expected,
                "is_prod_build={is_prod_build}, usage_enabled={usage_enabled:?}"
            );
        }
    }
}
