use std::path::{Path, PathBuf};

pub const RELAY_AUTH_DIR_ENV_VAR: &str = "RELAY_AUTH_DIR";

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub codex_home: PathBuf,
    pub chatgpt_base_url: String,
    pub model: String,
    pub user_instructions: Option<String>,
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        // Relay credentials deliberately live outside ~/.codex and ~/.opencode.
        // The default sits next to the executable, matching the server layout
        // where /opt/relay/auth is mounted at /app/local_auth.
        let codex_home = find_relay_auth_dir()?;
        let user_instructions = Self::load_instructions(Some(&codex_home));

        Ok(Config {
            codex_home,
            chatgpt_base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            model: "gpt-5.4".to_string(), // Default, but can be changed to any gpt-5* variant
            user_instructions,
        })
    }

    fn load_instructions(codex_dir: Option<&std::path::Path>) -> Option<String> {
        let mut p = match codex_dir {
            Some(p) => p.to_path_buf(),
            None => return None,
        };

        p.push("AGENTS.md");
        std::fs::read_to_string(&p).ok().and_then(|s| {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        })
    }
}

fn find_relay_auth_dir() -> anyhow::Result<PathBuf> {
    if let Some(value) = std::env::var_os(RELAY_AUTH_DIR_ENV_VAR) {
        if value.is_empty() {
            anyhow::bail!("{RELAY_AUTH_DIR_ENV_VAR} must not be empty");
        }

        let path = PathBuf::from(value);
        return if path.is_absolute() {
            Ok(path)
        } else {
            Ok(std::env::current_dir()?.join(path))
        };
    }

    default_relay_auth_dir(&std::env::current_exe()?)
}

fn default_relay_auth_dir(executable: &Path) -> anyhow::Result<PathBuf> {
    let executable_dir = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Could not determine relay executable directory"))?;
    Ok(executable_dir.join("local_auth"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_auth_dir_is_next_to_executable() {
        let executable = PathBuf::from("bundle").join("praxis-relay.exe");
        assert_eq!(
            default_relay_auth_dir(&executable).unwrap(),
            PathBuf::from("bundle").join("local_auth")
        );
    }
}
