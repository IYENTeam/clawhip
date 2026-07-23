use std::env;
use std::path::PathBuf;

pub const PRODUCT_NAME: &str = "op_pi";
pub const PACKAGE_NAME: &str = "op_pi";
pub const RUST_CRATE_NAME: &str = "op_pi";
pub const CLI_NAME: &str = "op_pi";
pub const CONFIG_ENV: &str = "OP_PI_CONFIG";
pub const HOME_ENV: &str = "OP_PI_HOME";
pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const STATE_HOME_DIR_NAME: &str = ".op_pi";

pub fn default_config_path() -> PathBuf {
    config_path_with(|name| env::var(name).ok())
}

pub fn state_dir() -> PathBuf {
    state_dir_with(|name| env::var(name).ok())
}

fn config_path_with<F>(mut get_env: F) -> PathBuf
where
    F: FnMut(&str) -> Option<String>,
{
    env_path(&mut get_env, CONFIG_ENV).unwrap_or_else(|| {
        home_dir(&mut get_env)
            .join(STATE_HOME_DIR_NAME)
            .join(CONFIG_FILE_NAME)
    })
}

fn state_dir_with<F>(mut get_env: F) -> PathBuf
where
    F: FnMut(&str) -> Option<String>,
{
    env_path(&mut get_env, HOME_ENV)
        .unwrap_or_else(|| home_dir(&mut get_env).join(STATE_HOME_DIR_NAME))
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
    fn identifiers_use_the_canonical_op_pi_identity() {
        assert_eq!(PRODUCT_NAME, "op_pi");
        assert_eq!(PACKAGE_NAME, "op_pi");
        assert_eq!(RUST_CRATE_NAME, "op_pi");
        assert_eq!(CLI_NAME, "op_pi");
        assert_eq!(CONFIG_ENV, "OP_PI_CONFIG");
        assert_eq!(HOME_ENV, "OP_PI_HOME");
        assert_eq!(STATE_HOME_DIR_NAME, ".op_pi");
    }

    #[test]
    fn config_path_uses_override_or_canonical_home() {
        let override_path = config_path_with(env_with(
            "/home/operator",
            &[(CONFIG_ENV, "/state/config.toml")],
        ));
        assert_eq!(override_path, PathBuf::from("/state/config.toml"));

        let default_path = config_path_with(env_with("/home/operator", &[]));
        assert_eq!(
            default_path,
            PathBuf::from("/home/operator/.op_pi/config.toml")
        );
    }

    #[test]
    fn state_dir_uses_override_or_canonical_home() {
        let override_path =
            state_dir_with(env_with("/home/operator", &[(HOME_ENV, "/state/op_pi")]));
        assert_eq!(override_path, PathBuf::from("/state/op_pi"));

        let default_path = state_dir_with(env_with("/home/operator", &[]));
        assert_eq!(default_path, PathBuf::from("/home/operator/.op_pi"));
    }
}
