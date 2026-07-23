use std::env;
use std::path::{Path, PathBuf};

pub const PRODUCT_NAME: &str = "op-pi";
pub const PACKAGE_NAME: &str = "op-pi";
pub const RUST_CRATE_NAME: &str = "op_pi";
pub const CLI_NAME: &str = "op-pi";
pub const CONFIG_ENV: &str = "OP_PI_CONFIG";
pub const LEGACY_CONFIG_ENV: &str = "CLAWHIP_CONFIG";
pub const HOME_ENV: &str = "OP_PI_HOME";
pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const STATE_HOME_DIR_NAME: &str = ".op-pi";
pub const LEGACY_STATE_HOME_DIR_NAME: &str = ".clawhip";

pub fn default_config_path() -> PathBuf {
    config_path_with(|name| env::var(name).ok(), Path::exists)
}

pub fn state_dir() -> PathBuf {
    state_dir_with(|name| env::var(name).ok(), Path::exists)
}

fn config_path_with<F, E>(mut get_env: F, exists: E) -> PathBuf
where
    F: FnMut(&str) -> Option<String>,
    E: Fn(&Path) -> bool,
{
    env_path(&mut get_env, CONFIG_ENV)
        .or_else(|| env_path(&mut get_env, LEGACY_CONFIG_ENV))
        .unwrap_or_else(|| {
            let home = home_dir(&mut get_env);
            let current = home.join(STATE_HOME_DIR_NAME).join(CONFIG_FILE_NAME);
            let legacy = home.join(LEGACY_STATE_HOME_DIR_NAME).join(CONFIG_FILE_NAME);

            if exists(&current) || !exists(&legacy) {
                current
            } else {
                legacy
            }
        })
}

fn state_dir_with<F, E>(mut get_env: F, exists: E) -> PathBuf
where
    F: FnMut(&str) -> Option<String>,
    E: Fn(&Path) -> bool,
{
    if let Some(path) = env_path(&mut get_env, HOME_ENV) {
        return path;
    }

    let home = home_dir(&mut get_env);
    let current = home.join(STATE_HOME_DIR_NAME);
    let legacy = home.join(LEGACY_STATE_HOME_DIR_NAME);

    if exists(&current) || !exists(&legacy) {
        current
    } else {
        legacy
    }
}

fn env_path<F>(get_env: &mut F, name: &str) -> Option<PathBuf>
where
    F: FnMut(&str) -> Option<String>,
{
    get_env(name)
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
}

fn home_dir<F>(get_env: &mut F) -> PathBuf
where
    F: FnMut(&str) -> Option<String>,
{
    env_path(get_env, "HOME").unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with<'a>(
        home: &'a str,
        values: &'a [(&'a str, &'a str)],
    ) -> impl FnMut(&str) -> Option<String> + 'a {
        move |name| {
            if name == "HOME" {
                Some(home.to_string())
            } else {
                values
                    .iter()
                    .find_map(|(key, value)| (*key == name).then(|| (*value).to_string()))
            }
        }
    }

    #[test]
    fn primary_identifiers_are_centralized() {
        assert_eq!(PRODUCT_NAME, "op-pi");
        assert_eq!(PACKAGE_NAME, "op-pi");
        assert_eq!(RUST_CRATE_NAME, "op_pi");
        assert_eq!(CLI_NAME, "op-pi");
        assert_eq!(CONFIG_ENV, "OP_PI_CONFIG");
        assert_eq!(HOME_ENV, "OP_PI_HOME");
        assert_eq!(LEGACY_CONFIG_ENV, "CLAWHIP_CONFIG");
        assert_eq!(LEGACY_STATE_HOME_DIR_NAME, ".clawhip");
    }

    #[test]
    fn config_path_prefers_the_new_environment_override() {
        let path = config_path_with(
            env_with(
                "/home/operator",
                &[
                    (CONFIG_ENV, "/new/config.toml"),
                    (LEGACY_CONFIG_ENV, "/old/config.toml"),
                ],
            ),
            |_| false,
        );
        assert_eq!(path, PathBuf::from("/new/config.toml"));
    }

    #[test]
    fn config_path_uses_legacy_environment_override_when_needed() {
        let path = config_path_with(
            env_with("/home/operator", &[(LEGACY_CONFIG_ENV, "/old/config.toml")]),
            |_| false,
        );
        assert_eq!(path, PathBuf::from("/old/config.toml"));
    }

    #[test]
    fn config_path_prefers_existing_new_config_then_legacy_config() {
        let home = PathBuf::from("/home/operator");
        let current = home.join(STATE_HOME_DIR_NAME).join(CONFIG_FILE_NAME);
        let legacy = home.join(LEGACY_STATE_HOME_DIR_NAME).join(CONFIG_FILE_NAME);

        let current_path =
            config_path_with(env_with("/home/operator", &[]), |path| path == current);
        assert_eq!(current_path, current);

        let legacy_path = config_path_with(env_with("/home/operator", &[]), |path| path == legacy);
        assert_eq!(legacy_path, legacy);
    }

    #[test]
    fn config_path_defaults_to_the_new_home() {
        let path = config_path_with(env_with("/home/operator", &[]), |_| false);
        assert_eq!(path, PathBuf::from("/home/operator/.op-pi/config.toml"));
    }

    #[test]
    fn state_dir_prefers_override_then_new_then_legacy() {
        let override_path = state_dir_with(
            env_with("/home/operator", &[(HOME_ENV, "/state/op-pi")]),
            |_| false,
        );
        assert_eq!(override_path, PathBuf::from("/state/op-pi"));

        let current = PathBuf::from("/home/operator/.op-pi");
        let current_path = state_dir_with(env_with("/home/operator", &[]), |path| path == current);
        assert_eq!(current_path, current);

        let legacy = PathBuf::from("/home/operator/.clawhip");
        let legacy_path = state_dir_with(env_with("/home/operator", &[]), |path| path == legacy);
        assert_eq!(legacy_path, legacy);
    }

    #[test]
    fn state_dir_defaults_to_the_new_home() {
        let path = state_dir_with(env_with("/home/operator", &[]), |_| false);
        assert_eq!(path, PathBuf::from("/home/operator/.op-pi"));
    }
}
