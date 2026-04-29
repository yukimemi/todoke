//! Discovery + control for live editor instances.
//!
//! Each `[todoke.<name>]` with `kind = "neovim"` exposes a `listen`
//! template that bakes the rule's `{{ group }}` into the OS pipe / socket
//! path. `discover` reverses that: it derives a [`ListenSkeleton`] from
//! the template, lists filesystem candidates that fit its prefix/suffix
//! shape, and probes each with a short RPC connect to mark `alive`.
//!
//! Used by `todoke list` and `todoke kill` (see `dispatcher`).

use std::collections::HashSet;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(unix)]
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use nvim_rs::{Handler, Neovim, compat::tokio::Compat, create::tokio as create};
use tokio::io::WriteHalf;
use tokio::time::timeout;
use tracing::debug;

use crate::config::{ResolvedConfig, TargetKind};
use crate::matcher::CaptureMap;
use crate::template::{Context, build_context, new_engine, render};

#[cfg(unix)]
type NvimConnection = tokio::net::UnixStream;
#[cfg(windows)]
type NvimConnection = tokio::net::windows::named_pipe::NamedPipeClient;

type NvimWriter = Compat<WriteHalf<NvimConnection>>;

#[derive(Clone)]
struct DummyHandler;

impl Handler for DummyHandler {
    type Writer = NvimWriter;
}

/// Sentinel value used to recover the `{{ group }}` slot in a listen
/// template. Long enough to be unique in any plausible rendered path
/// and lexically distinct from typical group names.
const GROUP_SENTINEL: &str = "__TODOKE_GROUP_SENTINEL_QHj9G2__";

/// One discovered editor instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    /// `[todoke.<name>]` key from the config.
    pub target: String,
    /// Group portion recovered from the listen path.
    pub group: String,
    /// Concrete listen path (Unix socket or Windows named pipe).
    pub listen: String,
    /// True when an RPC connect to `listen` succeeded within the probe
    /// timeout. Stale socket files left behind by a crashed nvim show
    /// up as `alive = false`.
    pub alive: bool,
}

/// The fixed prefix/suffix that bracket the `{{ group }}` substitution
/// in a listen template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListenSkeleton {
    pub(crate) prefix: String,
    pub(crate) suffix: String,
}

impl ListenSkeleton {
    /// Render the template with a sentinel group, then split on the
    /// sentinel to recover the (prefix, suffix) shape. Errors when the
    /// template doesn't reference `{{ group }}` (single-instance target —
    /// nothing to enumerate) or references it more than once (we can't
    /// unambiguously recover the group from a candidate path).
    pub(crate) fn from_template(cfg: &ResolvedConfig, template: &str) -> Result<Self> {
        let rendered = render_with_group(cfg, template, GROUP_SENTINEL)?;
        let Some((prefix, suffix)) = rendered.split_once(GROUP_SENTINEL) else {
            bail!("listen template does not reference {{{{ group }}}}");
        };
        if suffix.contains(GROUP_SENTINEL) {
            bail!("listen template references {{{{ group }}}} more than once");
        }
        // Discovery walks one directory level (Unix `read_dir` on
        // `prefix.parent()`, Windows `FindFirstFileW` on
        // `\\.\pipe\*`). A `{{ group }}` placed outside the final path
        // component (e.g. `/tmp/{{ group }}/nvim.sock`) survives the
        // skeleton split but lands in a directory the enumerator
        // never visits, silently producing zero hits. Reject it up
        // front so the misconfiguration surfaces at parse time.
        if suffix.contains('/') {
            bail!(
                "listen template uses {{{{ group }}}} outside the final path component \
                 (suffix `{suffix}` contains `/`); enumeration walks one directory level only",
            );
        }
        Ok(Self {
            prefix: prefix.to_string(),
            suffix: suffix.to_string(),
        })
    }

    /// Recover the group portion from a candidate listen path. Returns
    /// `None` when the candidate doesn't fit the shape, or when the
    /// recovered group would be empty.
    pub(crate) fn extract_group(&self, candidate: &str) -> Option<String> {
        let rest = candidate.strip_prefix(&self.prefix)?;
        let group = rest.strip_suffix(&self.suffix)?;
        if group.is_empty() {
            None
        } else {
            Some(group.to_string())
        }
    }

    /// Concrete listen path for a given group. Inverse of
    /// [`Self::extract_group`].
    #[allow(dead_code)] // used by future selective-kill paths and tests
    pub(crate) fn render_for(&self, group: &str) -> String {
        format!("{}{}{}", self.prefix, group, self.suffix)
    }
}

fn render_with_group(cfg: &ResolvedConfig, template: &str, group: &str) -> Result<String> {
    let mut tera = new_engine();
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cap = CaptureMap::new();
    let ctx = build_context(Context {
        input: None,
        command: "",
        cwd: &cwd,
        group,
        rule_name: "",
        vars: &cfg.raw.vars,
        cap: &cap,
        passthrough: &[],
    });
    render(&mut tera, template, &ctx)
}

/// Walk every `kind = "neovim"` target and return one [`Instance`] per
/// candidate listen path on disk. Sorted by `(target, group)` so the
/// caller can format directly. Targets whose listen template lacks
/// `{{ group }}` are skipped — there's nothing to enumerate.
///
/// Pings are issued in parallel via [`tokio::task::JoinSet`] so a
/// directory full of stale sockets doesn't accumulate `N × 500ms` of
/// sequential probe latency.
pub async fn discover(cfg: &ResolvedConfig) -> Vec<Instance> {
    let mut tasks = tokio::task::JoinSet::new();
    let mut seen: HashSet<String> = HashSet::new();

    for (target_name, target) in &cfg.raw.todoke {
        if target.kind != TargetKind::Neovim {
            continue;
        }
        let Some(template) = target.listen.as_deref() else {
            continue;
        };
        let skeleton = match ListenSkeleton::from_template(cfg, template) {
            Ok(s) => s,
            Err(e) => {
                debug!(target = %target_name, reason = %e, "skipping target during discovery");
                continue;
            }
        };
        for path in enumerate_candidates(&skeleton) {
            // Guard against the same path being claimed by multiple
            // targets that happen to share a prefix/suffix shape — first
            // declared target wins.
            if !seen.insert(path.clone()) {
                continue;
            }
            let Some(group) = skeleton.extract_group(&path) else {
                continue;
            };
            let target_name = target_name.clone();
            tasks.spawn(async move {
                let alive = ping(&path).await;
                Instance {
                    target: target_name,
                    group,
                    listen: path,
                    alive,
                }
            });
        }
    }

    let mut out = Vec::with_capacity(tasks.len());
    while let Some(res) = tasks.join_next().await {
        if let Ok(inst) = res {
            out.push(inst);
        }
    }
    out.sort_by(|a, b| a.target.cmp(&b.target).then_with(|| a.group.cmp(&b.group)));
    out
}

#[cfg(unix)]
fn enumerate_candidates(skeleton: &ListenSkeleton) -> Vec<String> {
    let prefix_path = Path::new(&skeleton.prefix);
    // When the prefix ends in a path separator (e.g. listen template
    // `/tmp/{{ group }}.sock` → prefix `/tmp/`), `Path::file_name`
    // returns the trailing dir component instead of an empty basename
    // and `Path::parent` walks one level too high. Detect that shape
    // explicitly so `parent = prefix_path` and the basename matcher
    // sees the full filename for prefix-comparison.
    let (parent, basename_prefix) = if skeleton.prefix.ends_with('/') {
        (prefix_path, "")
    } else {
        (
            prefix_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
            prefix_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(""),
        )
    };
    let basename_suffix = skeleton.suffix.as_str();

    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ent in entries.flatten() {
        // Filter to actual Unix sockets — a regular file whose name
        // happens to match the listen template would otherwise be
        // surfaced as `stale` and unlinked by `cleanup_stale`,
        // destroying unrelated user data.
        let Ok(file_type) = ent.file_type() else {
            continue;
        };
        if !file_type.is_socket() {
            continue;
        }
        let name = ent.file_name();
        let Some(name_s) = name.to_str() else {
            continue;
        };
        if name_s.starts_with(basename_prefix)
            && name_s.ends_with(basename_suffix)
            && name_s.len() > basename_prefix.len() + basename_suffix.len()
        {
            out.push(ent.path().to_string_lossy().into_owned());
        }
    }
    out
}

#[cfg(windows)]
fn enumerate_candidates(skeleton: &ListenSkeleton) -> Vec<String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindFirstFileW, FindNextFileW, WIN32_FIND_DATAW,
    };

    // Named pipes are enumerated via the special `\\.\pipe\*` pattern.
    // FindFirstFile only returns the bare pipe name (not the full
    // `\\.\pipe\<name>` path), so we reconstruct the full form before
    // comparing with the skeleton's prefix.
    let pattern: Vec<u16> = OsStr::new(r"\\.\pipe\*")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    let handle = unsafe { FindFirstFileW(pattern.as_ptr(), &mut data) };
    if handle == INVALID_HANDLE_VALUE {
        return Vec::new();
    }

    let prefix = skeleton.prefix.as_str();
    let suffix = skeleton.suffix.as_str();
    let mut out = Vec::new();
    loop {
        let len = data
            .cFileName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(data.cFileName.len());
        let name = std::ffi::OsString::from_wide(&data.cFileName[..len])
            .to_string_lossy()
            .into_owned();
        let full = format!(r"\\.\pipe\{name}");
        if full.starts_with(prefix)
            && full.ends_with(suffix)
            && full.len() > prefix.len() + suffix.len()
        {
            out.push(full);
        }
        if unsafe { FindNextFileW(handle, &mut data) } == 0 {
            break;
        }
    }
    unsafe {
        FindClose(handle);
    }
    out
}

const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
/// Window between sending `qall!` and re-checking whether nvim actually
/// exited. Long enough for normal shutdown autocmds (BufLeave /
/// VimLeavePre + write-only filewrite handlers); short enough that
/// `--force` doesn't feel sluggish.
const QUIT_GRACE: Duration = Duration::from_millis(800);

async fn ping(listen: &str) -> bool {
    timeout(PROBE_TIMEOUT, create::new_path(listen, DummyHandler))
        .await
        .is_ok_and(|r| r.is_ok())
}

/// Outcome of a `kill_instance` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillOutcome {
    /// `:qall!` landed and the process exited within [`QUIT_GRACE`].
    Quit,
    /// `:qall!` was sent but the process is still ping-able after
    /// [`QUIT_GRACE`]. Caller must pass `--force` to escalate.
    StillAlive,
    /// `:qall!` did not take effect, but `--force` was set and the
    /// OS-level kill succeeded.
    Forced { pid: u32 },
}

/// Upper bound on `--force` sweep rounds. A single named-pipe / socket
/// path can be served by more than one process in two situations:
///
/// * A nested `:terminal nvim --listen <same path>` re-binds the name,
///   so the parent and child both queue on the OS pipe.
/// * A wedged `qall!` leaves nvim alive while a sibling instance still
///   accepts on the same path.
///
/// Each round connects, captures the current server's PID, OS-kills it,
/// and re-pings. The cap stops a runaway sweep if something keeps
/// re-creating the listener (which would be a config bug, not a
/// recoverable state).
const MAX_FORCE_ROUNDS: usize = 8;

/// Send `qall!` to the instance at `listen`, then re-ping after a short
/// grace window to confirm the process actually exited. When
/// `force = true` and `qall!` doesn't take effect, escalate to an
/// OS-level kill (`SIGKILL` on Unix, `TerminateProcess` on Windows) using
/// the PID retrieved via `vim.fn.getpid()`. With `force = true` the kill
/// loop sweeps additional rounds (up to [`MAX_FORCE_ROUNDS`]) so a listen
/// path served by multiple processes (Windows pipe queue, nested
/// `:terminal nvim --listen`) is fully cleared.
///
/// All RPC awaits are bounded — `eval(getpid())` by [`PROBE_TIMEOUT`],
/// `command("qall!")` by [`QUIT_GRACE`] — so a wedged nvim (e.g. blocked
/// in a hit-enter prompt) can't hang the whole `todoke kill` invocation.
/// Errors from those calls are swallowed because the RPC connection
/// drops as nvim exits; the post-`qall!` ping is the authoritative
/// liveness check.
pub async fn kill_instance(listen: &str, force: bool) -> Result<KillOutcome> {
    // Round 0: graceful qall! attempt with post-ping. The post-ping is
    // mandatory here because it's the only way to distinguish three
    // outcomes — Quit (qall! cleared the path), StillAlive (no --force,
    // path still up), and "still alive, escalate" (--force, enter sweep).
    let (nvim, io_handle) = timeout(PROBE_TIMEOUT, create::new_path(listen, DummyHandler))
        .await
        .map_err(|_| anyhow!("connect timed out after {:?}", PROBE_TIMEOUT))?
        .map_err(|e| anyhow!("RPC connect failed: {e}"))?;

    let mut next_pid = if force {
        capture_pid(&nvim, listen).await
    } else {
        None
    };

    let _ = timeout(QUIT_GRACE, nvim.command("qall!")).await;
    drop(nvim);
    // The I/O task resolves as soon as nvim drops the RPC connection
    // (i.e. exits), so healthy quits early-exit and we only pay the
    // full grace window when the process is genuinely wedged.
    let _ = timeout(QUIT_GRACE, io_handle).await;

    if !ping(listen).await {
        return Ok(KillOutcome::Quit);
    }
    if !force {
        return Ok(KillOutcome::StillAlive);
    }

    // Sweep loop. Each round OS-kills the captured PID, then the *next*
    // round's connect attempt doubles as the liveness probe — we don't
    // re-`ping` separately at the round boundary. A failed connect
    // means the pipe is gone, which is what we want.
    let mut forced_pid: Option<u32> = None;
    for _ in 0..MAX_FORCE_ROUNDS {
        let Some(pid) = next_pid else {
            if forced_pid.is_none() {
                return Err(anyhow!(
                    "qall! did not take effect and PID lookup failed; cannot --force"
                ));
            }
            // We've already killed something this run, but the next
            // server queued on this path won't tell us its PID. Stop;
            // the partial-kill error below surfaces the situation.
            break;
        };

        // OS-kill failures are non-fatal during sweep: the targeted
        // process is often already gone (qall! reached it, or a
        // previous round killed it). The next connect attempt is the
        // authoritative liveness check.
        if os_kill(pid).is_ok() {
            forced_pid.get_or_insert(pid);
        }
        // SIGKILL / TerminateProcess bypasses nvim's normal teardown,
        // leaving the bound listen socket on disk as a stale entry on
        // Unix. Unlink it now so the next `todoke list` doesn't keep
        // flagging the corpse. Errors are swallowed — a residual stale
        // entry will be cleaned up next run.
        let _ = cleanup_stale(listen);

        // Probe by attempting another RPC connect. Failure → pipe is
        // gone → we're done. Success → another server is queued; grab
        // its PID and loop.
        match timeout(PROBE_TIMEOUT, create::new_path(listen, DummyHandler)).await {
            Ok(Ok((nvim, io_handle))) => {
                next_pid = capture_pid(&nvim, listen).await;
                drop(nvim);
                let _ = timeout(QUIT_GRACE, io_handle).await;
            }
            _ => {
                return Ok(match forced_pid {
                    Some(pid) => KillOutcome::Forced { pid },
                    None => KillOutcome::Quit,
                });
            }
        }
    }

    if !ping(listen).await {
        return Ok(match forced_pid {
            Some(pid) => KillOutcome::Forced { pid },
            None => KillOutcome::Quit,
        });
    }

    if let Some(pid) = forced_pid {
        return Err(anyhow!(
            "force-kill incomplete: pipe still alive after killing pid={pid}; \
             additional listeners may serve the same path"
        ));
    }

    Err(anyhow!(
        "force-kill exhausted after {MAX_FORCE_ROUNDS} rounds; listen path still alive"
    ))
}

/// Remove a stale listen entry from the filesystem. On Unix this
/// `unlink`s the stale socket file (the orphan a crashed nvim leaves
/// behind). On Windows there is no analogue: named pipes are
/// handle-tied and disappear when the process exits, so this is a
/// no-op (returns `Ok(false)` to signal nothing was done).
pub fn cleanup_stale(listen: &str) -> Result<bool> {
    cleanup_stale_inner(listen)
}

#[cfg(unix)]
fn cleanup_stale_inner(listen: &str) -> Result<bool> {
    match std::fs::remove_file(listen) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(anyhow!("{e}")),
    }
}

#[cfg(windows)]
fn cleanup_stale_inner(_listen: &str) -> Result<bool> {
    // Named pipes have handle-tied lifetimes; there's no on-disk
    // residue to unlink.
    Ok(false)
}

#[cfg(unix)]
fn os_kill(pid: u32) -> Result<()> {
    // SAFETY: libc::kill is a thin wrapper over the kernel's `kill(2)`.
    // It is safe for any signed PID; non-existent PIDs return ESRCH.
    let pid_t =
        libc::pid_t::try_from(pid).map_err(|_| anyhow!("pid {pid} does not fit in libc::pid_t"))?;
    let rc = unsafe { libc::kill(pid_t, libc::SIGKILL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(anyhow!(
            "SIGKILL pid={pid} failed: {}",
            std::io::Error::last_os_error()
        ))
    }
}

/// Recover the kill target's PID. Tries `eval(getpid())` first because
/// it works on every platform; if that fails (typical for a wedged
/// nvim that's not servicing RPC), falls back to a platform-specific
/// listen-path probe. Always bounded — never blocks indefinitely.
async fn capture_pid(nvim: &Neovim<NvimWriter>, listen: &str) -> Option<u32> {
    let from_eval = match timeout(PROBE_TIMEOUT, nvim.eval("getpid()")).await {
        Ok(Ok(v)) => v.as_i64().and_then(|n| u32::try_from(n).ok()),
        _ => None,
    };
    if from_eval.is_some() {
        return from_eval;
    }
    pid_from_listen(listen).await
}

/// Platform-specific shortcut: ask the OS for the PID of the process
/// listening on `listen`, without going through nvim's RPC. Used as a
/// fallback when `eval(getpid())` doesn't answer (e.g. nvim is stuck
/// in a hit-enter prompt or a slow autocmd).
#[cfg(windows)]
async fn pid_from_listen(listen: &str) -> Option<u32> {
    // `CreateFileW` is synchronous and can in principle block (e.g.
    // ERROR_PIPE_BUSY waits) — keep the tokio executor unblocked by
    // running it on the blocking pool.
    let listen = listen.to_string();
    tokio::task::spawn_blocking(move || pid_from_listen_blocking(&listen))
        .await
        .ok()
        .flatten()
}

#[cfg(windows)]
fn pid_from_listen_blocking(listen: &str) -> Option<u32> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_GENERIC_READ, OPEN_EXISTING};
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;

    // Only meaningful for `\\.\pipe\<name>` paths. Calling
    // `GetNamedPipeServerProcessId` on a non-pipe handle is undefined,
    // so guard against accidental misuse on, e.g., a regular file.
    if !listen.starts_with(r"\\.\pipe\") {
        return None;
    }

    let wide: Vec<u16> = OsStr::new(listen)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: CreateFileW with OPEN_EXISTING on `\\.\pipe\<name>` opens
    // a client connection to the named pipe. Pointers are valid for the
    // duration of the call (`wide` lives through it; the optional
    // SECURITY_ATTRIBUTES / template-handle args are null per docs).
    // Read-only access is sufficient — `GetNamedPipeServerProcessId`
    // requires `GENERIC_READ` only and write access can fail against
    // pipes with restrictive ACLs.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_GENERIC_READ,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }

    let mut pid: u32 = 0;
    // SAFETY: handle is a valid named-pipe client handle (just opened
    // above and verified non-INVALID); pid is a plain stack u32.
    let ok = unsafe { GetNamedPipeServerProcessId(handle, &mut pid) };
    // SAFETY: handle is non-null and non-INVALID.
    unsafe {
        CloseHandle(handle);
    }
    if ok == 0 || pid == 0 { None } else { Some(pid) }
}

#[cfg(unix)]
async fn pid_from_listen(_listen: &str) -> Option<u32> {
    // No equivalent shortcut on Unix domain sockets — `getpeercred`
    // and friends return the *peer's* (i.e. our own) PID, not the
    // server's. Fall back to the eval-based lookup, which is the only
    // reliable path here. A wedged nvim on Unix means the user has to
    // SIGKILL by hand; tracked as a follow-up (lsof-based escalation).
    None
}

#[cfg(windows)]
fn os_kill(pid: u32) -> Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    // SAFETY: OpenProcess with PROCESS_TERMINATE is the documented way
    // to obtain a handle suitable for TerminateProcess. The handle is
    // closed unconditionally in the same scope; pid is a plain DWORD.
    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, FALSE, pid) };
    if handle.is_null() {
        return Err(anyhow!(
            "OpenProcess(pid={pid}) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: handle is non-null per the check above; exit code 1 is
    // arbitrary and doesn't propagate (the target process is being
    // forcibly killed, no consumer reads its exit status).
    let rc = unsafe { TerminateProcess(handle, 1) };
    let term_err = std::io::Error::last_os_error();
    // SAFETY: CloseHandle on a valid HANDLE is always sound.
    unsafe {
        CloseHandle(handle);
    }
    if rc == 0 {
        Err(anyhow!("TerminateProcess(pid={pid}) failed: {term_err}"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load_from_str;

    fn cfg(src: &str) -> ResolvedConfig {
        load_from_str(src).expect("config parses")
    }

    fn min_cfg() -> ResolvedConfig {
        cfg(r#"
            [todoke.dummy]
            command = "true"

            [[rules]]
            match = '.*'
            to = "dummy"
        "#)
    }

    #[test]
    fn skeleton_extracts_prefix_and_suffix_around_group() {
        let c = min_cfg();
        let s = ListenSkeleton::from_template(&c, "/tmp/nvim-todoke-{{ group }}.sock").unwrap();
        assert_eq!(s.prefix, "/tmp/nvim-todoke-");
        assert_eq!(s.suffix, ".sock");
    }

    #[test]
    fn skeleton_handles_windows_branch_via_is_windows() {
        // Both branches are well-formed; the active one depends on the
        // host OS. Just check the result splits cleanly.
        let c = min_cfg();
        let template = r"{% if is_windows() %}\\.\pipe\nvim-todoke-{{ group }}{% else %}/tmp/nvim-todoke-{{ group }}.sock{% endif %}";
        let s = ListenSkeleton::from_template(&c, template).unwrap();
        if cfg!(target_os = "windows") {
            assert_eq!(s.prefix, r"\\.\pipe\nvim-todoke-");
            assert_eq!(s.suffix, "");
        } else {
            assert_eq!(s.prefix, "/tmp/nvim-todoke-");
            assert_eq!(s.suffix, ".sock");
        }
    }

    #[test]
    fn skeleton_errors_when_group_is_missing() {
        let c = min_cfg();
        let err = ListenSkeleton::from_template(&c, "/tmp/fixed.sock").unwrap_err();
        assert!(err.to_string().contains("does not reference"));
    }

    #[test]
    fn skeleton_errors_when_group_appears_twice() {
        let c = min_cfg();
        let err =
            ListenSkeleton::from_template(&c, "/tmp/{{ group }}/{{ group }}.sock").unwrap_err();
        assert!(err.to_string().contains("more than once"));
    }

    #[test]
    fn skeleton_errors_when_group_is_not_in_final_path_component() {
        // `/tmp/{{ group }}/nvim.sock` survives the split (one
        // sentinel reference), but the suffix `/nvim.sock` would
        // steer enumeration to a directory the walker never visits.
        // We surface that at parse time instead of producing zero
        // hits silently at discovery.
        let c = min_cfg();
        let err = ListenSkeleton::from_template(&c, "/tmp/{{ group }}/nvim.sock").unwrap_err();
        assert!(err.to_string().contains("final path component"));
    }

    #[test]
    fn extract_group_recovers_value_when_shape_matches() {
        let s = ListenSkeleton {
            prefix: "/tmp/nvim-todoke-".into(),
            suffix: ".sock".into(),
        };
        assert_eq!(
            s.extract_group("/tmp/nvim-todoke-default.sock"),
            Some("default".to_string()),
        );
        assert_eq!(
            s.extract_group("/tmp/nvim-todoke-git-commit.sock"),
            Some("git-commit".to_string()),
        );
    }

    #[test]
    fn extract_group_rejects_non_matching_paths() {
        let s = ListenSkeleton {
            prefix: "/tmp/nvim-todoke-".into(),
            suffix: ".sock".into(),
        };
        assert_eq!(s.extract_group("/tmp/other.sock"), None);
        assert_eq!(s.extract_group("/tmp/nvim-todoke-default.txt"), None);
        // No group between prefix and suffix.
        assert_eq!(s.extract_group("/tmp/nvim-todoke-.sock"), None);
    }

    #[test]
    fn render_for_round_trips_with_extract_group() {
        let s = ListenSkeleton {
            prefix: "/tmp/nvim-todoke-".into(),
            suffix: ".sock".into(),
        };
        let path = s.render_for("scratch");
        assert_eq!(path, "/tmp/nvim-todoke-scratch.sock");
        assert_eq!(s.extract_group(&path), Some("scratch".to_string()));
    }

    #[test]
    fn skeleton_resolves_vars_references() {
        // vars.* values are stable across both sentinel renders, so
        // they end up in the prefix/suffix as fixed text.
        let c = cfg(r#"
            [vars]
            base = "/var/run/todoke"

            [todoke.nvim]
            kind = "neovim"
            command = "nvim"
            listen = "{{ vars.base }}/nvim-{{ group }}.sock"

            [[rules]]
            match = '.*'
            to = "nvim"
        "#);
        let template = c.raw.todoke["nvim"].listen.as_deref().unwrap();
        let s = ListenSkeleton::from_template(&c, template).unwrap();
        assert_eq!(s.prefix, "/var/run/todoke/nvim-");
        assert_eq!(s.suffix, ".sock");
    }

    // The discovery tests stage real Unix sockets via `UnixListener::bind`
    // (then drop the listener immediately so the file remains but no one
    // accepts on it — the canonical "stale socket" shape). `alive` ends up
    // false because the post-drop fd is gone; the file inode persists.
    #[cfg(unix)]
    mod discovery_unix {
        use super::*;
        use std::fs::File;
        use std::os::unix::net::UnixListener;
        use std::path::PathBuf;

        /// Bind a UnixListener to create the on-disk socket file, then
        /// drop it so subsequent connect attempts get ECONNREFUSED.
        /// The path remains as a S_IFSOCK file — the exact shape a
        /// crashed nvim leaves behind.
        fn make_stale_socket(path: &std::path::Path) {
            let _l = UnixListener::bind(path).expect("bind unix socket");
            // _l drops at end of scope, fd closes, file inode stays.
        }

        fn unique_tempdir() -> PathBuf {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let pid = std::process::id();
            let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
            // Pin to /tmp instead of std::env::temp_dir(): on macOS
            // the latter resolves to `/var/folders/xx/<long-hash>/T/`,
            // which pushes AF_UNIX socket paths past sockaddr_un.sun_path's
            // 104-byte limit (`SUN_LEN`) and trips bind() with
            // `InvalidInput`. /tmp is short on every Unix and is the
            // canonical socket-friendly tempdir.
            let d = PathBuf::from("/tmp").join(format!("todoke-registry-{stamp}-{pid}-{seq}"));
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        fn cfg_with_tmpdir(tmp: &std::path::Path) -> ResolvedConfig {
            let src = format!(
                r#"
                    [vars]
                    tmp = "{tmp}"

                    [todoke.nvim]
                    kind = "neovim"
                    command = "nvim"
                    listen = "{{{{ vars.tmp }}}}/nvim-todoke-{{{{ group }}}}.sock"

                    [[rules]]
                    match = '.*'
                    to = "nvim"
                "#,
                tmp = tmp.display(),
            );
            cfg(&src)
        }

        #[tokio::test]
        async fn discover_finds_matching_filesystem_entries() {
            let tmp = unique_tempdir();
            make_stale_socket(&tmp.join("nvim-todoke-default.sock"));
            make_stale_socket(&tmp.join("nvim-todoke-git.sock"));
            // Decoys that should be ignored.
            make_stale_socket(&tmp.join("other.sock"));
            File::create(tmp.join("nvim-todoke-default.txt")).unwrap();

            let cfg = cfg_with_tmpdir(&tmp);
            let instances = discover(&cfg).await;

            let groups: Vec<&str> = instances.iter().map(|i| i.group.as_str()).collect();
            assert_eq!(groups, vec!["default", "git"]);
            for inst in &instances {
                assert_eq!(inst.target, "nvim");
                assert!(!inst.alive, "no real nvim → alive must be false");
            }
        }

        #[tokio::test]
        async fn discover_filters_out_regular_files_matching_the_pattern() {
            // A regular file at /tmp/nvim-todoke-imposter.sock would be
            // a name collision against the listen template. Without
            // the is_socket() filter, discovery would surface it as
            // `stale` and `cleanup_stale` would later unlink it,
            // destroying unrelated user data. Verify the filter holds.
            let tmp = unique_tempdir();
            make_stale_socket(&tmp.join("nvim-todoke-real.sock"));
            File::create(tmp.join("nvim-todoke-imposter.sock")).unwrap();

            let cfg = cfg_with_tmpdir(&tmp);
            let instances = discover(&cfg).await;
            let groups: Vec<&str> = instances.iter().map(|i| i.group.as_str()).collect();
            assert_eq!(groups, vec!["real"]);
        }

        #[tokio::test]
        async fn discover_returns_empty_when_no_targets_use_neovim() {
            let src = r#"
                [todoke.echo]
                command = "echo"

                [[rules]]
                match = '.*'
                to = "echo"
            "#;
            let cfg = cfg(src);
            assert!(discover(&cfg).await.is_empty());
        }

        #[tokio::test]
        async fn cleanup_stale_unlinks_existing_socket_file() {
            let tmp = unique_tempdir();
            let path = tmp.join("nvim-todoke-default.sock");
            std::fs::File::create(&path).unwrap();
            assert!(path.exists());
            let removed = cleanup_stale(&path.to_string_lossy()).unwrap();
            assert!(removed, "Unix should report removed = true");
            assert!(!path.exists(), "socket file should be gone");
        }

        #[tokio::test]
        async fn cleanup_stale_is_idempotent_on_missing_file() {
            let tmp = unique_tempdir();
            let path = tmp.join("never-existed.sock");
            // No-op on a missing path is success on Unix (the desired
            // post-condition is "file is gone", not "we did the unlink").
            let removed = cleanup_stale(&path.to_string_lossy()).unwrap();
            assert!(removed);
        }

        #[tokio::test]
        async fn discover_skips_targets_without_group_in_listen() {
            let tmp = unique_tempdir();
            // Listen has no `{{ group }}`, so the target is single-instance
            // and not enumerable. `discover` should silently skip it.
            let src = format!(
                r#"
                    [vars]
                    tmp = "{tmp}"

                    [todoke.nvim]
                    kind = "neovim"
                    command = "nvim"
                    listen = "{{{{ vars.tmp }}}}/fixed.sock"

                    [[rules]]
                    match = '.*'
                    to = "nvim"
                "#,
                tmp = tmp.display(),
            );
            File::create(tmp.join("fixed.sock")).unwrap();
            let cfg = cfg(&src);
            assert!(discover(&cfg).await.is_empty());
        }

        /// Stand up a Unix-domain server that accepts connections but
        /// never reads from or writes to them — the canonical "wedged
        /// nvim" shape (e.g. blocked in a hit-enter prompt). Caller
        /// drops the returned `JoinHandle` to tear it down.
        async fn spawn_stalling_server(path: &std::path::Path) -> tokio::task::JoinHandle<()> {
            let listener = tokio::net::UnixListener::bind(path).expect("bind stalled server");
            tokio::spawn(async move {
                // Hold every accepted stream open without responding so
                // any RPC the client issues hangs waiting for a reply.
                let mut held = Vec::new();
                while let Ok((stream, _)) = listener.accept().await {
                    held.push(stream);
                }
            })
        }

        /// `kill_instance` must not hang when nvim accepts the connection
        /// but never answers RPCs (eval / command). We don't care about
        /// the exact outcome here — we care that the future *completes*.
        /// PROBE_TIMEOUT (eval) + QUIT_GRACE (qall!) + QUIT_GRACE (io) +
        /// PROBE_TIMEOUT (ping) ≈ 2.6 s per round; a 15 s outer timeout
        /// catches any runaway loop while leaving generous slack.
        #[tokio::test]
        async fn kill_instance_does_not_hang_on_unresponsive_server() {
            let tmp = unique_tempdir();
            let path = tmp.join("stalled.sock");
            let server = spawn_stalling_server(&path).await;

            let listen = path.to_string_lossy().into_owned();
            let outer = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                kill_instance(&listen, true),
            )
            .await;

            server.abort();
            assert!(
                outer.is_ok(),
                "kill_instance hung past 15s on unresponsive server"
            );
            // With the server stalled, eval(getpid) times out → no PID,
            // qall! times out, ping still succeeds → bail with the
            // PID-lookup-failed message. The exact error text isn't the
            // point; the bounded return *is*.
            let inner = outer.unwrap();
            assert!(
                inner.is_err(),
                "expected an Err from kill_instance against stalled server, got {inner:?}"
            );
        }

        /// Same scenario without `--force`: kill_instance should fall
        /// out as `StillAlive` (or Err) within bounded time rather than
        /// hanging on `command("qall!")`. This guards the non-force
        /// branch too — the timeout wrapper applies in both modes.
        #[tokio::test]
        async fn kill_instance_does_not_hang_without_force() {
            let tmp = unique_tempdir();
            let path = tmp.join("stalled-noforce.sock");
            let server = spawn_stalling_server(&path).await;

            let listen = path.to_string_lossy().into_owned();
            let outer = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                kill_instance(&listen, false),
            )
            .await;

            server.abort();
            assert!(
                outer.is_ok(),
                "kill_instance(force=false) hung past 10s on unresponsive server"
            );
            let inner = outer.unwrap();
            // ping after qall! still succeeds (server keeps accepting),
            // and force=false → StillAlive.
            assert_eq!(inner.ok(), Some(KillOutcome::StillAlive));
        }
    }
}
