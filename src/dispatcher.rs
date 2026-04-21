//! Orchestrator: routes files through matcher → template → editor.
//!
//! Flow for every dispatch:
//! 1. Canonicalize file path and derive the normalized form for matching.
//! 2. Look up the first matching rule.
//! 3. Expand `rule.group` through Tera (file context available).
//! 4. Group files by the resolved target `(editor, group, mode, sync)`.
//! 5. For each target, render editor.command / args / listen and invoke the
//!    appropriate backend.
//! 6. Aggregate errors; exit code non-zero if any batch failed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use tracing::{debug, info, warn};

use crate::cli::Cli;
use crate::config::{self, EditorKind, Mode, ResolvedConfig, Rule};
use crate::editors::{
    generic::{DispatchCtx as GenericCtx, GenericBackend},
    neovim::NeovimBackend,
};
use crate::matcher::{first_match, normalize_path, strip_verbatim};
use crate::style::{
    accent, bold, dim, level_error, level_info, level_ok, level_warn, muted, styled,
};
use crate::template::{FileParts, build_context, new_engine, render};

pub async fn dispatch(cli: &Cli, files: &[PathBuf]) -> Result<()> {
    let cfg = config::load(cli.config.as_deref())?;
    let plan = if files.is_empty() {
        plan_no_args(cli, &cfg)?
    } else {
        plan_batches(cli, &cfg, files)?
    };

    debug!(batches = plan.len(), "dispatch plan built");

    if cli.dry_run {
        print_plan(&plan);
        return Ok(());
    }

    run_plan(&cfg, plan).await
}

/// Build a single-batch plan for the no-args case (`todoke` with nothing else).
/// Matches rules against the empty string so a catch-all rule (e.g.
/// `match = '.*'`) wins; the resulting batch carries no files and each
/// backend is expected to interpret that as "open the editor empty".
fn plan_no_args(cli: &Cli, cfg: &ResolvedConfig) -> Result<Vec<Batch>> {
    let cwd = std::env::current_dir()
        .context("could not read current directory")?
        .to_string_lossy()
        .into_owned();

    let rule_idx = first_match(cfg, "").or_else(|| {
        // If the user forced an editor/group and no rule matched empty path,
        // fall back to the first rule to pick up mode/sync defaults.
        if (cli.editor.is_some() || cli.group.is_some()) && !cfg.raw.rules.is_empty() {
            Some(0)
        } else {
            None
        }
    });

    let Some(rule_idx) = rule_idx else {
        bail!(
            "no rule matches empty-args invocation; add a catch-all rule (e.g. `match = '.*'`) or pass a file"
        );
    };

    let rule = cfg.rule(rule_idx);
    let rule_name = rule
        .name
        .clone()
        .unwrap_or_else(|| format!("rule[{rule_idx}]"));

    let mut tera = new_engine();
    let placeholder_file = FileParts::from_path(Path::new(""));
    let ctx_phase2 = build_context(&placeholder_file, None, &cwd, "", &rule_name, &cfg.raw.vars);

    let group = if let Some(g) = cli.group.clone() {
        g
    } else if let Some(tmpl) = &rule.group {
        render(&mut tera, tmpl, &ctx_phase2).context("rendering rule.group template")?
    } else {
        config::DEFAULT_GROUP.to_string()
    };

    let editor_name = match cli.editor.clone() {
        Some(e) => e,
        None => render(&mut tera, &rule.editor, &ctx_phase2)
            .context("rendering rule.editor template")?,
    };

    Ok(vec![Batch {
        editor_name,
        group,
        mode: rule.mode,
        sync: rule.sync,
        rule_name,
        files: Vec::new(),
    }])
}

pub async fn check(cli: &Cli, files: &[PathBuf]) -> Result<()> {
    let cfg = config::load(cli.config.as_deref())?;
    let plan = plan_batches(cli, &cfg, files)?;
    print_plan(&plan);
    Ok(())
}

/// Static analysis of the loaded config.
/// Exits with a non-zero status when any issue is flagged at warn level or
/// higher, so `todoke doctor` is useful as a pre-commit / CI gate.
pub async fn doctor(cli: &Cli) -> Result<()> {
    let cfg = config::load(cli.config.as_deref())?;
    let path = config::resolve_path(cli.config.as_deref())?;

    let editor_names = cfg
        .raw
        .editors
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");

    println!(
        "{} {}",
        styled("config:", muted()),
        styled(path.display(), accent()),
    );
    println!(
        "{} {} {}",
        styled("editors:", muted()),
        if editor_names.is_empty() {
            styled("<none>", dim())
        } else {
            styled(&editor_names, accent())
        },
        styled(format!("({})", cfg.raw.editors.len()), dim()),
    );
    println!(
        "{} {}",
        styled("rules:", muted()),
        styled(cfg.raw.rules.len(), accent()),
    );
    println!();

    let mut issues = 0usize;

    // 1. rule.editor references that are literal strings (not templates) must
    //    resolve to a known editor. Templates are checked at dispatch time.
    for (i, rule) in cfg.raw.rules.iter().enumerate() {
        let name = rule.name.as_deref().unwrap_or("<unnamed>");
        if !rule.editor.contains("{{") && !rule.editor.contains("{%") {
            if !cfg.raw.editors.contains_key(&rule.editor) {
                println!(
                    "{} {}: editor {} is not defined in [editors.*]",
                    styled("error", level_error()),
                    styled(format!("rule[{i}] ({name})"), bold()),
                    styled(format!("'{}'", rule.editor), accent()),
                );
                issues += 1;
            }
        } else {
            println!(
                "{}  {}: editor {} is a Tera template, resolved at dispatch time",
                styled("info", level_info()),
                styled(format!("rule[{i}] ({name})"), bold()),
                styled(format!("'{}'", rule.editor), accent()),
            );
        }
    }

    // 2. unreachable rules — anything after a literal catch-all (match='.*',
    //    no exclude) is dead code.
    let mut catch_all_at: Option<usize> = None;
    for (i, rule) in cfg.raw.rules.iter().enumerate() {
        if rule.exclude.is_some() {
            continue;
        }
        let patterns = rule.match_.as_slice();
        if patterns.iter().any(|p| *p == ".*" || *p == "^.*$") {
            catch_all_at = Some(i);
            break;
        }
    }
    if let Some(ca) = catch_all_at {
        for (i, rule) in cfg.raw.rules.iter().enumerate().skip(ca + 1) {
            let name = rule.name.as_deref().unwrap_or("<unnamed>");
            let ca_name = cfg.raw.rules[ca].name.as_deref().unwrap_or("<unnamed>");
            println!(
                "{}  {}: unreachable — preceded by {} with catch-all match and no exclude",
                styled("warn", level_warn()),
                styled(format!("rule[{i}] ({name})"), bold()),
                styled(format!("rule[{ca}] ({ca_name})"), bold()),
            );
            issues += 1;
        }
    }

    // 3. no catch-all at the end — paths that match no rule are silently
    //    skipped (with a warn log), which usually isn't what the user wants.
    if catch_all_at.is_none() {
        println!(
            "{}  no catch-all rule at end of config — paths that match no rule (or are excluded from every rule) will be skipped with a warning",
            styled("warn", level_warn()),
        );
        issues += 1;
    }

    // 4. duplicate rule names — confusing in `check` / `doctor` output.
    let mut seen: std::collections::HashMap<&str, Vec<usize>> = std::collections::HashMap::new();
    for (i, rule) in cfg.raw.rules.iter().enumerate() {
        if let Some(n) = rule.name.as_deref() {
            seen.entry(n).or_default().push(i);
        }
    }
    for (name, idxs) in seen.iter() {
        if idxs.len() > 1 {
            println!(
                "{}  duplicate rule name {} at indices {idxs:?} — use distinct names for readability",
                styled("warn", level_warn()),
                styled(format!("'{name}'"), accent()),
            );
            issues += 1;
        }
    }

    println!();
    if issues == 0 {
        println!("{} no issues found", styled("ok", level_ok()));
        Ok(())
    } else {
        bail!("{issues} issue(s) found")
    }
}

pub async fn list(_alive_only: bool) -> Result<()> {
    bail!("list: not implemented yet (v0.2)")
}

pub async fn kill(group: Option<&str>, all: bool) -> Result<()> {
    if group.is_none() && !all {
        bail!("specify <group> or --all");
    }
    bail!("kill: not implemented yet (v0.2)")
}

/// A single dispatch batch: one editor, one group, one (mode, sync) pair, and
/// all files that resolve to it.
#[derive(Debug)]
pub struct Batch {
    pub editor_name: String,
    pub group: String,
    pub mode: Mode,
    pub sync: bool,
    pub rule_name: String,
    pub files: Vec<PathBuf>,
}

fn plan_batches(cli: &Cli, cfg: &ResolvedConfig, files: &[PathBuf]) -> Result<Vec<Batch>> {
    let cwd = std::env::current_dir()
        .context("could not read current directory")?
        .to_string_lossy()
        .into_owned();

    let mut tera = new_engine();
    let mut groups: BTreeMap<BatchKey, Batch> = BTreeMap::new();

    for raw in files {
        let canonical = raw
            .canonicalize()
            .with_context(|| format!("cannot resolve path: {}", raw.display()))?;
        let normalized = normalize_path(&canonical);

        let (rule_idx, rule) = match resolve_rule(cli, cfg, &normalized)? {
            Some(tuple) => tuple,
            None => {
                warn!(path = %normalized, "no rule matched, skipping");
                continue;
            }
        };

        let rule_name = rule
            .name
            .clone()
            .unwrap_or_else(|| format!("rule[{rule_idx}]"));

        // Resolve group: --group flag wins, else rule.group template, else default.
        let group = resolve_group(cli, cfg, &mut tera, rule, &canonical, &cwd, &rule_name)?;

        // Resolve editor: --editor flag wins, else rule.editor (Tera-rendered
        // so `editor = "{{ vars.gui }}"` works for swapping editors via vars).
        let editor_name = match cli.editor.clone() {
            Some(e) => e,
            None => {
                let file = FileParts::from_path(&canonical);
                let ctx = build_context(&file, None, &cwd, "", &rule_name, &cfg.raw.vars);
                render(&mut tera, &rule.editor, &ctx).context("rendering rule.editor template")?
            }
        };

        let key = BatchKey {
            editor: editor_name.clone(),
            group: group.clone(),
            mode: rule.mode,
            sync: rule.sync,
        };

        groups
            .entry(key)
            .or_insert_with(|| Batch {
                editor_name: editor_name.clone(),
                group: group.clone(),
                mode: rule.mode,
                sync: rule.sync,
                rule_name: rule_name.clone(),
                files: Vec::new(),
            })
            .files
            .push(canonical);
    }

    Ok(groups.into_values().collect())
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct BatchKey {
    editor: String,
    group: String,
    mode: Mode,
    sync: bool,
}

fn resolve_rule<'a>(
    cli: &Cli,
    cfg: &'a ResolvedConfig,
    normalized_path: &str,
) -> Result<Option<(usize, &'a Rule)>> {
    // --editor and --group bypass normal matching by using a synthetic rule,
    // but we still need a "base rule" for mode/sync inference. Take the first
    // rule that matches the path (if any), else fall back to the first rule.
    // If there are no rules at all, that's a config error.
    if cli.editor.is_some() || cli.group.is_some() {
        if cfg.raw.rules.is_empty() {
            bail!(
                "--editor/--group requires at least one [[rules]] in config for mode/sync defaults"
            );
        }
        if let Some(idx) = first_match(cfg, normalized_path) {
            return Ok(Some((idx, cfg.rule(idx))));
        }
        return Ok(Some((0, cfg.rule(0))));
    }

    Ok(first_match(cfg, normalized_path).map(|idx| (idx, cfg.rule(idx))))
}

fn resolve_group(
    cli: &Cli,
    cfg: &ResolvedConfig,
    tera: &mut tera::Tera,
    rule: &Rule,
    canonical_file: &Path,
    cwd: &str,
    rule_name: &str,
) -> Result<String> {
    if let Some(g) = cli.group.clone() {
        return Ok(g);
    }
    match &rule.group {
        None => Ok(config::DEFAULT_GROUP.to_string()),
        Some(tmpl) => {
            let file = FileParts::from_path(canonical_file);
            let ctx = build_context(
                &file,
                None,
                cwd,
                "", // group not yet resolved
                rule_name,
                &cfg.raw.vars,
            );
            render(tera, tmpl, &ctx).context("rendering rule.group template")
        }
    }
}

async fn run_plan(cfg: &ResolvedConfig, plan: Vec<Batch>) -> Result<()> {
    let mut had_err = false;
    for batch in plan {
        let result = run_batch(cfg, &batch).await;
        if let Err(e) = result {
            had_err = true;
            tracing::error!(
                editor = %batch.editor_name,
                group = %batch.group,
                error = %e,
                "batch failed"
            );
        }
    }
    if had_err {
        bail!("one or more dispatch batches failed (see logs above)")
    }
    Ok(())
}

async fn run_batch(cfg: &ResolvedConfig, batch: &Batch) -> Result<()> {
    let editor = cfg.editor(&batch.editor_name)?;
    let cwd = std::env::current_dir()
        .context("cwd")?
        .to_string_lossy()
        .into_owned();

    let mut tera = new_engine();
    let first_file = batch.files.first().expect("non-empty batch");
    let file_parts = FileParts::from_path(first_file);
    let editor_parts = FileParts::from_path(Path::new(&editor.command));
    let ctx = build_context(
        &file_parts,
        Some(&editor_parts),
        &cwd,
        &batch.group,
        &batch.rule_name,
        &cfg.raw.vars,
    );

    match editor.kind {
        EditorKind::Neovim => {
            let listen_tmpl = editor.listen.as_deref().ok_or_else(|| {
                anyhow!(
                    "editor '{}' has kind=neovim but no listen",
                    batch.editor_name
                )
            })?;
            let listen =
                render(&mut tera, listen_tmpl, &ctx).context("rendering editor.listen template")?;
            let command = render(&mut tera, &editor.command, &ctx)
                .context("rendering editor.command template")?;
            let args_remote = render_arg_list(&mut tera, &editor.args_remote, &ctx)?;
            let args_new = render_arg_list(&mut tera, &editor.args_new, &ctx)?;

            info!(
                editor = %batch.editor_name,
                group = %batch.group,
                mode = ?batch.mode,
                sync = batch.sync,
                listen = %listen,
                files = ?batch.files,
                "dispatching to neovim"
            );

            let backend = NeovimBackend {
                command,
                listen,
                args_remote,
                args_new,
            };
            backend.dispatch(&batch.files, batch.mode, batch.sync).await
        }
        EditorKind::Generic => {
            let command = render(&mut tera, &editor.command, &ctx)
                .context("rendering editor.command template")?;

            info!(
                editor = %batch.editor_name,
                group = %batch.group,
                mode = ?batch.mode,
                sync = batch.sync,
                files = ?batch.files,
                "dispatching to generic"
            );

            let backend = GenericBackend {
                command,
                args_new: editor.args_new.clone(),
                args_remote: editor.args_remote.clone(),
                env: editor.env.clone(),
            };
            let dctx = GenericCtx {
                files: &batch.files,
                mode: batch.mode,
                sync: batch.sync,
                group: &batch.group,
                rule_name: &batch.rule_name,
                vars: &cfg.raw.vars,
                cwd: &cwd,
            };
            backend.dispatch(dctx)
        }
    }
}

fn render_arg_list(
    tera: &mut tera::Tera,
    args: &[String],
    ctx: &tera::Context,
) -> Result<Vec<String>> {
    args.iter()
        .map(|a| render(tera, a, ctx).with_context(|| format!("rendering arg template: {a}")))
        .collect()
}

fn print_plan(plan: &[Batch]) {
    if plan.is_empty() {
        println!("{}", styled("no matching batches", dim()));
        return;
    }
    for (i, b) in plan.iter().enumerate() {
        let sync_label = if b.sync { "sync" } else { "async" };
        let sync_style = if b.sync { level_warn() } else { level_info() };
        println!(
            "{} editor={} group={} mode={} {} rule={}",
            styled(format!("[{i}]"), muted()),
            styled(&b.editor_name, accent()),
            styled(&b.group, accent()),
            styled(format!("{:?}", b.mode).to_lowercase(), level_info()),
            styled(sync_label, sync_style),
            styled(&b.rule_name, bold()),
        );
        for f in &b.files {
            println!(
                "      {} {}",
                styled("-", muted()),
                styled(strip_verbatim(&f.to_string_lossy()), dim()),
            );
        }
    }
}
