//! The default `auth.json` location must match upstream's `getAuthPath()`
//! exactly — a user may share a machine between the TypeScript CLI and this
//! implementation, and both must find the same file.
//!
//! Lives in its own test binary because it mutates the process environment.

use pi_auth::{default_agent_dir, default_auth_path, ENV_AGENT_DIR};

/// Upstream: `PI_CODING_AGENT_DIR`, derived from `APP_NAME.toUpperCase()`.
#[test]
fn the_agent_dir_env_var_is_named_as_upstream_derives_it() {
    assert_eq!(ENV_AGENT_DIR, "PI_CODING_AGENT_DIR");
}

#[test]
fn the_default_path_follows_the_agent_dir_override_then_the_home_default() {
    let previous = std::env::var_os(ENV_AGENT_DIR);

    // Overridden: `$PI_CODING_AGENT_DIR/auth.json`, with `~` expanded.
    std::env::set_var(ENV_AGENT_DIR, "/tmp/pi-auth-test-agent-dir");
    assert_eq!(
        default_auth_path(),
        std::path::Path::new("/tmp/pi-auth-test-agent-dir/auth.json")
    );

    let home = std::env::var("HOME").expect("HOME is set");
    std::env::set_var(ENV_AGENT_DIR, "~/custom-agent-dir");
    assert_eq!(
        default_agent_dir(),
        std::path::Path::new(&home).join("custom-agent-dir")
    );

    // Unset: `~/.pi/agent/auth.json`.
    std::env::remove_var(ENV_AGENT_DIR);
    assert_eq!(
        default_auth_path(),
        std::path::Path::new(&home)
            .join(".pi")
            .join("agent")
            .join("auth.json")
    );

    if let Some(previous) = previous {
        std::env::set_var(ENV_AGENT_DIR, previous);
    }
}
