//! The variable names devslot never manages, probes with a sentinel, or lets a
//! project configuration set: they steer the process or Docker itself.

const RESERVED_NAMES: [&str; 6] = ["PATH", "HOME", "PWD", "SHELL", "USER", "TMPDIR"];
const RESERVED_PREFIXES: [&str; 6] = ["LD_", "DOCKER_", "COMPOSE_", "BUILDX_", "XDG_", "DEVSLOT_"];

/// Names that make a shell, an interpreter, a compiler or a tool run code or
/// read files chosen by a project: refused for `[env]` and for port variables.
/// Compared case-insensitively.
const DENIED_NAMES: [&str; 42] = [
    "BASH_ENV",
    "ENV",
    "PROMPT_COMMAND",
    "PROMPT",
    "PROMPT2",
    "PROMPT3",
    "PROMPT4",
    "RPROMPT",
    "RPROMPT2",
    "RPS1",
    "RPS2",
    "SPROMPT",
    "PS0",
    "PS1",
    "PS2",
    "PS3",
    "PS4",
    "IFS",
    "CDPATH",
    "SHELLOPTS",
    "BASHOPTS",
    "ZDOTDIR",
    "FPATH",
    "PERLLIB",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "EDITOR",
    "VISUAL",
    "PAGER",
    "GCONV_PATH",
    "RUSTC_WRAPPER",
    "RUSTC",
    "RUSTDOC",
    "CC",
    "CXX",
    "LD",
    "AR",
    "GOFLAGS",
    "MAKEFLAGS",
    "MFLAGS",
    "GNUMAKEFLAGS",
];
const DENIED_PREFIXES: [&str; 12] = [
    "GIT_",
    "SSH_",
    "NPM_CONFIG_",
    "PYTHON",
    "PERL5",
    "RUBY",
    "LESS",
    "CARGO_",
    "YARN_",
    "PNPM_",
    "BUN_",
    "DENO_",
];
const DENIED_NODE_PREFIX: &str = "NODE_";

/// Whether the name steers the process, Docker or devslot itself, or is on
/// the denylist of names a project must never set.
pub fn is_reserved(name: &str) -> bool {
    RESERVED_NAMES.contains(&name)
        || RESERVED_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || is_denied(name)
}

/// Whether the name is on the denylist of shell, interpreter and tool
/// variables that can run code or read files chosen by a project.
pub fn is_denied(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    DENIED_NAMES.contains(&upper.as_str())
        || upper.starts_with(DENIED_NODE_PREFIX)
        || DENIED_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_reserve_the_exact_names_when_they_steer_the_process() {
        assert!(RESERVED_NAMES.iter().all(|name| is_reserved(name)));
    }

    #[test]
    fn should_reserve_every_name_with_a_reserved_prefix() {
        assert!(
            [
                "LD_PRELOAD",
                "DOCKER_HOST",
                "COMPOSE_FILE",
                "BUILDX_CONFIG",
                "XDG_RUNTIME_DIR",
                "DEVSLOT_INSTANCE"
            ]
            .iter()
            .all(|name| is_reserved(name))
        );
    }

    #[test]
    fn should_deny_the_shell_and_interpreter_names() {
        assert!(
            [
                "GIT_CONFIG_COUNT",
                "SSH_ASKPASS",
                "NPM_CONFIG_REGISTRY",
                "PYTHONPATH",
                "PERL5LIB",
                "RUBYOPT",
                "BASH_ENV",
                "ENV",
                "PROMPT_COMMAND",
                "PS4",
                "IFS",
                "NODE_OPTIONS",
                "JAVA_TOOL_OPTIONS",
                "EDITOR",
                "LESSOPEN",
                "GCONV_PATH",
                "npm_config_script_shell",
                "PROMPT",
                "RUSTC_WRAPPER",
                "CC",
                "cargo_home",
                "NODE_OPTIONS",
                "node_path",
                "YARN_RC_FILENAME",
                "GOFLAGS",
                "MAKEFLAGS"
            ]
            .iter()
            .all(|name| is_reserved(name) && is_denied(name))
        );
    }

    #[test]
    fn should_not_reserve_an_ordinary_name_when_it_only_contains_a_reserved_word() {
        assert!(
            !["PORT", "MY_HOME", "HOMEDIR", "DB_PORT"]
                .iter()
                .any(|name| is_reserved(name))
        );
    }
}
