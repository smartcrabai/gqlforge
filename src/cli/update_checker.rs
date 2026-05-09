use colored::Colorize;
use ctrlc::set_handler;
use gqlforge_version::VERSION;
use update_informer::{Check, Version, registry};
use which::which;

#[derive(Default)]
enum InstallationMethod {
    Npm,
    Npx,
    Brew,
    #[default]
    Direct,
}

impl InstallationMethod {
    /// figure out the installation method is used by user.
    pub fn get_installation_method() -> Self {
        if std::env::var("npm_execpath").is_ok() {
            return InstallationMethod::Npx;
        }

        if let Ok(output) = std::process::Command::new("npm")
            .arg("ls")
            .arg("--global")
            .output()
            && String::from_utf8_lossy(&output.stdout).contains("@gqlforge/gqlforge")
        {
            return InstallationMethod::Npm;
        }

        if let Ok(result) = which("gqlforge")
            && result.to_str().is_some_and(|s| s.contains("homebrew"))
        {
            return InstallationMethod::Brew;
        }

        InstallationMethod::default()
    }

    fn format_upgrade_message(command: &str) -> String {
        format!("{} {}", "Please run:".white(), command.yellow())
    }

    /// displays the message to upgrade gqlforge depending on the
    /// installation method used.
    pub fn display_message(&self) -> String {
        match self {
            InstallationMethod::Npx => {
                Self::format_upgrade_message("npx @gqlforge/gqlforge@latest")
            }
            InstallationMethod::Npm => {
                Self::format_upgrade_message("npm update -g @gqlforge/gqlforge")
            }
            InstallationMethod::Brew => Self::format_upgrade_message("brew upgrade gqlforge"),
            InstallationMethod::Direct => {
                "Please update by downloading the latest release from GitHub".to_string()
            }
        }
    }
}

fn show_update_message(name: &str, latest_version: &Version) {
    let github_release_url = format!("https://github.com/{name}/releases/tag/{latest_version}");
    tracing::warn!(
        "{} {} {} {}. {}. Release notes: {}",
        "A new release of gqlforge is available:",
        VERSION.as_str().cyan(),
        "\u{279C}",
        latest_version.to_string().cyan(),
        InstallationMethod::get_installation_method().display_message(),
        github_release_url.yellow()
    );
}

pub fn check_for_update() {
    if VERSION.is_dev() {
        // skip validation if it's not a release
        return;
    }

    let name: &str = "takumi3488/gqlforge";

    let informer = update_informer::new(registry::GitHub, name, VERSION.as_str());

    if let Some(latest_version) = informer.check_version().ok().flatten() {
        // schedules the update message to be shown when the user presses Ctrl+C on cli.
        let _ = set_handler(move || {
            show_update_message(name, &latest_version);
            std::process::exit(exitcode::OK);
        });
    }
}
