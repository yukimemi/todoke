//! Background auto-update + the `self-update` subcommand.
//!
//! Built on [`kaishin`] 0.5.0. Three behaviours, picked by
//! `[options] auto_update` (default `install`) and overridden by the
//! `TODOKE_NO_AUTOUPDATE` env kill-switch (which always wins):
//!
//! - **off**     — never check or install.
//! - **notify**  — background check; print a one-line banner if a newer
//!   release exists, but never install.
//! - **install** — silently download + swap the binary in the background
//!   (the default). The running process keeps the old binary; the new
//!   version applies on the next launch. Exactly one stderr line is printed
//!   when an install actually happened.
//!
//! Everything here is resilience-first: a missing config, an unreadable
//! state file, a network failure, or a slow GitHub never panics and never
//! hangs a fast command — failures are silently swallowed.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use kaishin::{Checker, KaishinOptions, LatestRelease, UpdateOptions};

use crate::config::{self, AutoUpdateMode};

const OWNER: &str = "yukimemi";

/// How long `finalize_auto_update_check` waits for an in-flight background
/// install to report before giving up (silently). Keeps fast commands snappy.
const INSTALL_FINALIZE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long `finalize_auto_update_check` waits for an in-flight notify check.
const NOTIFY_FINALIZE_TIMEOUT: Duration = Duration::from_secs(1);

fn kaishin_opts() -> KaishinOptions {
    KaishinOptions::new(
        OWNER,
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
    )
}

/// Resolve the transient update-check state file path:
/// `<cache dir>/todoke/last_update_check.json`. This is throttle/cache state
/// that can be safely deleted and re-created, so it lives under the XDG cache
/// directory (`cache_dir()`), not the persistent data directory. Returns
/// `None` if the cache dir can't be resolved (then auto-update is skipped —
/// resilience).
fn state_path() -> Option<PathBuf> {
    directories::BaseDirs::new()
        .map(|d| d.cache_dir().join("todoke").join("last_update_check.json"))
}

/// Pure truthiness of the kill-switch value: disabled when the var is present,
/// non-empty (after trim), and not `"0"` / `"false"` (case-insensitive).
///
/// Split out from [`auto_update_disabled_by_env`] so the decision logic can be
/// unit-tested **by value** without mutating the global process environment
/// (which would race under the default parallel test runner).
fn env_value_disables(value: Option<&str>) -> bool {
    match value {
        Some(v) => {
            let v = v.trim();
            !v.is_empty() && !v.eq_ignore_ascii_case("0") && !v.eq_ignore_ascii_case("false")
        }
        None => false,
    }
}

/// `TODOKE_NO_AUTOUPDATE` kill-switch: disabled when the var is set, non-empty,
/// and not `"0"` / `"false"` (case-insensitive). Takes precedence over config.
fn auto_update_disabled_by_env() -> bool {
    env_value_disables(std::env::var("TODOKE_NO_AUTOUPDATE").ok().as_deref())
}

/// `todoke self-update` — explicit, user-driven update.
pub async fn run_self_update(yes: bool, check: bool) -> Result<()> {
    let opts = kaishin_opts();
    let upd_opts = UpdateOptions::new().yes(yes).check_only(check);
    kaishin::run_self_update(&opts, upd_opts).await
}

/// Handle for an in-flight (or already-decided) background update action,
/// consumed by [`finalize_auto_update_check`].
pub enum AutoUpdateHandle {
    /// `notify`: a cached check already shows a newer release — just banner it.
    CachedAvailable {
        checker: Checker,
        latest: LatestRelease,
    },
    /// `notify`: a background fetch is running; banner on its result, or fall
    /// back to the cached release on timeout / error.
    Pending {
        checker: Checker,
        handle: tokio::task::JoinHandle<Result<Option<LatestRelease>>>,
        cached_latest: Option<LatestRelease>,
    },
    /// `install`: a silent background install is running. On completion within
    /// the bounded wait, print exactly one "installed in the background" line.
    Installing {
        handle: tokio::task::JoinHandle<Result<Option<LatestRelease>>>,
    },
}

/// Build a [`Checker`] for the current binary, wired to the resolved interval
/// and state path. Returns `None` if the state path can't be resolved.
fn build_checker(interval: Duration) -> Option<Checker> {
    let path = state_path()?;
    Some(
        Checker::new(env!("CARGO_PKG_NAME"), kaishin_opts())
            .interval(interval)
            .state_path(path),
    )
}

/// Best-effort spawn of the background auto-update action, per config + env.
///
/// Returns `None` (silent skip) when auto-update is disabled by env or config,
/// when the config can't be loaded, or when the state path can't be resolved.
/// Never panics.
pub async fn maybe_spawn_auto_update_check(
    explicit_config: Option<&std::path::Path>,
) -> Option<AutoUpdateHandle> {
    if auto_update_disabled_by_env() {
        return None;
    }

    // Load config best-effort. A broken / missing config must never break a
    // normal dispatch, so fall back to None (silent skip) on any error.
    let cfg = config::load(explicit_config).ok()?;
    let mode = cfg.raw.options.auto_update;
    if mode == AutoUpdateMode::Off {
        return None;
    }

    let interval = cfg
        .raw
        .options
        .update_interval
        .as_deref()
        .and_then(|s| kaishin::parse_interval(s).ok())
        .unwrap_or_else(kaishin::default_interval);

    let checker = build_checker(interval)?;

    match mode {
        AutoUpdateMode::Off => None, // handled above; keep match exhaustive
        AutoUpdateMode::Notify => {
            if !checker.should_check() {
                // Throttled: surface a cached newer release if we have one.
                return checker
                    .cached_update()
                    .map(|latest| AutoUpdateHandle::CachedAvailable { checker, latest });
            }
            let cached_latest = checker.cached_update();
            let checker_clone = checker.clone();
            let handle = tokio::spawn(async move { checker_clone.check_and_save().await });
            Some(AutoUpdateHandle::Pending {
                checker,
                handle,
                cached_latest,
            })
        }
        AutoUpdateMode::Install => {
            // `auto_update()` self-throttles (`should_check()`), serialises via
            // an OS lock, refuses dev builds, and installs silently — returning
            // Ok(Some(latest)) only when it actually swapped the binary.
            let handle = tokio::spawn(async move { checker.auto_update().await });
            Some(AutoUpdateHandle::Installing { handle })
        }
    }
}

/// Consume the handle: print the notify banner / installed notice, with a hard
/// bounded wait so fast commands never hang. All failures stay silent.
pub async fn finalize_auto_update_check(handle: AutoUpdateHandle) {
    match handle {
        AutoUpdateHandle::CachedAvailable { checker, latest } => {
            eprintln!("\n{}", checker.format_banner(&latest));
        }
        AutoUpdateHandle::Pending {
            checker,
            handle,
            cached_latest,
        } => {
            let res = tokio::time::timeout(NOTIFY_FINALIZE_TIMEOUT, handle).await;
            match res {
                Ok(Ok(Ok(Some(latest)))) => {
                    eprintln!("\n{}", checker.format_banner(&latest));
                }
                Ok(Ok(Ok(None))) => {
                    // Fetched successfully, nothing newer — no banner.
                }
                _ => {
                    // Timeout / join error / fetch error: fall back to cache.
                    if let Some(latest) = cached_latest {
                        eprintln!("\n{}", checker.format_banner(&latest));
                    }
                }
            }
        }
        AutoUpdateHandle::Installing { handle } => {
            // Short bounded wait. If the install hasn't finished, the detached
            // worker keeps going (fire-and-forget) and we just stop waiting.
            if let Ok(Ok(Ok(Some(latest)))) =
                tokio::time::timeout(INSTALL_FINALIZE_TIMEOUT, handle).await
            {
                let version = latest.tag_name.trim_start_matches('v');
                eprintln!(
                    "\u{2713} {bin} {version} installed in the background \u{2014} restart to apply.",
                    bin = env!("CARGO_PKG_NAME"),
                );
            }
            // Timeout / no install / error: silent (resilience).
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Kill-switch decision logic (pure, tested by value) -----------------
    //
    // The truthiness rule is exercised through `env_value_disables`, which
    // takes the value as an argument. No process-env mutation, so these run
    // safely under the default parallel test runner (no data race).

    #[test]
    fn env_kill_switch_unset_is_enabled() {
        assert!(!env_value_disables(None));
    }

    #[test]
    fn env_kill_switch_truthy_disables() {
        for v in ["1", "true", "TRUE", "yes", "on", " 1 "] {
            assert!(env_value_disables(Some(v)), "{v:?} should disable");
        }
    }

    #[test]
    fn env_kill_switch_falsey_stays_enabled() {
        for v in ["", "0", "false", "FALSE", "  ", " 0 ", " false "] {
            assert!(!env_value_disables(Some(v)), "{v:?} should NOT disable");
        }
    }

    // --- build_checker ------------------------------------------------------

    #[test]
    fn build_checker_returns_some_with_resolved_state_path() {
        // On any supported platform `directories::BaseDirs` resolves, so the
        // checker is built and its state file sits under the XDG cache dir.
        let checker = build_checker(Duration::from_secs(3600))
            .expect("checker should build when the cache dir resolves");
        // `cached_update` reads the (most likely absent) state file under the
        // cache dir and must never panic — fresh state means no cached update.
        let _ = checker.cached_update();
    }

    #[test]
    fn state_path_is_under_cache_dir() {
        // Regression guard for the XDG fix: transient state must live under
        // the cache dir, never the data dir.
        if let (Some(path), Some(dirs)) = (state_path(), directories::BaseDirs::new()) {
            assert!(
                path.starts_with(dirs.cache_dir()),
                "state file should be under the cache dir, got {path:?}"
            );
            assert!(path.ends_with("last_update_check.json"));
        }
    }

    // --- maybe_spawn_auto_update_check decision logic -----------------------
    //
    // These tests need to control the process environment (the kill-switch and
    // the config path), so they serialise through a shared mutex to avoid the
    // parallel-test env data race. The pure truthiness above is what's tested
    // broadly; here we only check the two short-circuit branches.

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `maybe_spawn_auto_update_check` on a fresh current-thread runtime
    /// while holding the env lock, so the env mutation around it is serialised
    /// against other env-touching tests. The lock is never held across an
    /// `.await` (we `block_on` inside the synchronous critical section), so it
    /// stays clippy-clean and races are impossible.
    fn with_env_locked<R>(f: impl FnOnce() -> R) -> R {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        f()
    }

    #[test]
    fn maybe_spawn_skips_when_disabled_by_env() {
        let handle = with_env_locked(|| {
            // SAFETY: serialised via ENV_LOCK so no other test mutates env
            // concurrently; we restore the previous value before releasing.
            let prev = std::env::var("TODOKE_NO_AUTOUPDATE").ok();
            unsafe {
                std::env::set_var("TODOKE_NO_AUTOUPDATE", "1");
            }
            let handle = tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(maybe_spawn_auto_update_check(None));
            unsafe {
                match prev {
                    Some(v) => std::env::set_var("TODOKE_NO_AUTOUPDATE", v),
                    None => std::env::remove_var("TODOKE_NO_AUTOUPDATE"),
                }
            }
            handle
        });
        assert!(
            handle.is_none(),
            "env kill-switch must short-circuit before any spawn"
        );
    }

    #[test]
    fn maybe_spawn_skips_when_mode_off() {
        let dir = std::env::temp_dir().join(format!("todoke-test-off-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("todoke.toml");
        std::fs::write(
            &cfg_path,
            "[options]\nauto_update = \"off\"\n\n[todoke.a]\ncommand = \"echo\"\n\n[[rules]]\nmatch = \".*\"\nto = \"a\"\n",
        )
        .unwrap();

        let handle = with_env_locked(|| {
            // SAFETY: serialised via ENV_LOCK; restored before release.
            let prev = std::env::var("TODOKE_NO_AUTOUPDATE").ok();
            unsafe {
                std::env::remove_var("TODOKE_NO_AUTOUPDATE");
            }
            let handle = tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(maybe_spawn_auto_update_check(Some(&cfg_path)));
            unsafe {
                match prev {
                    Some(v) => std::env::set_var("TODOKE_NO_AUTOUPDATE", v),
                    None => std::env::remove_var("TODOKE_NO_AUTOUPDATE"),
                }
            }
            handle
        });

        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            handle.is_none(),
            "auto_update = \"off\" must short-circuit before any spawn"
        );
    }
}
