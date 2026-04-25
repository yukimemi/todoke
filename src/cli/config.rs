use std::io::Write;
use std::path::Path;
use std::process::Command as StdCommand;

use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;

use crate::config;

#[derive(Subcommand, Debug)]
pub enum ConfigSub {
    #[command(about = "Print the resolved config file path")]
    Path,
    #[command(
        about = "Write the embedded default config if no file exists yet (idempotent; never overwrites)"
    )]
    Init,
    #[command(about = "Open the config file in $EDITOR (writes the default first if missing)")]
    Edit,
    #[command(about = "Print the loaded config TOML")]
    Show {
        #[arg(
            long,
            help = "Print the TOML after Tera pre-render (no rule/file context is provided)"
        )]
        rendered: bool,
    },
}

pub async fn run(sub: ConfigSub, explicit_config: Option<&Path>) -> Result<()> {
    match sub {
        ConfigSub::Path => {
            let path = config::resolve_path(explicit_config)?;
            println!("{}", path.display());
            Ok(())
        }
        ConfigSub::Init => init(explicit_config),
        ConfigSub::Edit => edit(explicit_config),
        ConfigSub::Show { rendered } => show(explicit_config, rendered),
    }
}

fn init(explicit_config: Option<&Path>) -> Result<()> {
    let path = config::resolve_path(explicit_config)?;
    let created = ensure_config_exists(&path)?;
    if created {
        eprintln!("wrote default config to {}", path.display());
    }
    println!("{}", path.display());
    Ok(())
}

fn edit(explicit_config: Option<&Path>) -> Result<()> {
    let path = config::resolve_path(explicit_config)?;
    let created = ensure_config_exists(&path)?;
    if created {
        eprintln!("wrote default config to {}", path.display());
    }

    let editor = pick_editor();
    // POSIX-style shell tokenization handles quoted paths with spaces
    // (`"C:\Program Files\.../Code.exe" --wait`) and embedded args
    // (`code --wait`, `nvim -p`) uniformly. Unquoted paths with spaces are
    // unrepresentable in $EDITOR — same constraint git-core imposes.
    let parts = shlex::split(&editor)
        .ok_or_else(|| anyhow!("editor command has unbalanced quotes: {editor}"))?;
    let (cmd, extra) = parts
        .split_first()
        .ok_or_else(|| anyhow!("no editor configured; set $EDITOR or $VISUAL"))?;
    let status = StdCommand::new(cmd)
        .args(extra)
        .arg(&path)
        .status()
        .with_context(|| format!("failed to spawn editor `{editor}` for {}", path.display()))?;
    if !status.success() {
        bail!("editor `{editor}` exited with status {status}");
    }
    Ok(())
}

fn show(explicit_config: Option<&Path>, rendered: bool) -> Result<()> {
    let path = config::resolve_path(explicit_config)?;
    let text = if path.exists() {
        std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?
    } else {
        config::DEFAULT_CONFIG_TOML.to_string()
    };

    let out = if rendered {
        config::prerender(&text)?
    } else {
        text
    };

    if out.ends_with('\n') {
        print!("{out}");
    } else {
        println!("{out}");
    }
    Ok(())
}

/// Write the embedded default config to `path` if the file doesn't exist yet.
/// Returns `true` when a new file was created, `false` when one already existed.
///
/// Creates parent directories as needed. Never overwrites an existing file —
/// the user's edits stay intact even if the embedded default has drifted.
///
/// Atomicity: the write goes through `OpenOptions::create_new`, so the
/// "exists check" and the "create" are a single OS-level operation. A
/// concurrent process that creates the file between our check and our open
/// loses to `AlreadyExists`, and we report `Ok(false)` instead of clobbering
/// their content.
pub fn ensure_config_exists(path: &Path) -> Result<bool> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory: {}", parent.display())
            })?;
        }
    }
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => {
            return Err(e).with_context(|| {
                format!("failed to open config file for writing: {}", path.display())
            });
        }
    };
    file.write_all(config::DEFAULT_CONFIG_TOML.as_bytes())
        .with_context(|| format!("failed to write default config to: {}", path.display()))?;
    Ok(true)
}

/// Pick the editor command per the `$VISUAL` → `$EDITOR` → platform-default
/// chain. Empty / whitespace-only env values are treated as unset so the
/// fallback chain keeps going.
pub fn pick_editor() -> String {
    for key in ["VISUAL", "EDITOR"] {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                return v;
            }
        }
    }
    if cfg!(windows) {
        "notepad".to_string()
    } else {
        "vi".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("todoke-config-test-{stamp}-{pid}-{seq}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn ensure_config_exists_writes_default_when_missing() {
        let dir = temp_dir();
        let path = dir.join("nested").join("todoke.toml");
        assert!(!path.exists());

        let created = ensure_config_exists(&path).unwrap();

        assert!(created);
        assert!(path.exists());
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, config::DEFAULT_CONFIG_TOML);
    }

    #[test]
    fn ensure_config_exists_does_not_overwrite_existing() {
        let dir = temp_dir();
        let path = dir.join("todoke.toml");
        std::fs::write(&path, "# user-edited\n").unwrap();

        let created = ensure_config_exists(&path).unwrap();

        assert!(!created);
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "# user-edited\n");
    }

    #[test]
    fn ensure_config_exists_succeeds_when_path_has_no_parent_dir() {
        // A bare filename (no parent component) shouldn't trip the
        // create_dir_all guard — it should resolve to the cwd and just write.
        let dir = temp_dir();
        let path = dir.join("inline.toml");
        let created = ensure_config_exists(&path).unwrap();
        assert!(created);
        assert!(path.exists());
    }
}
