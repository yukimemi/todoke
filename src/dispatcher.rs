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
use crate::template::{FileParts, build_context, new_engine, render};

pub async fn dispatch(cli: &Cli, files: &[PathBuf]) -> Result<()> {
    if files.is_empty() {
        bail!("no files given; try `edtr --help`");
    }

    let cfg = config::load(cli.config.as_deref())?;
    let plan = plan_batches(cli, &cfg, files)?;

    debug!(batches = plan.len(), "dispatch plan built");

    if cli.dry_run {
        print_plan(&plan);
        return Ok(());
    }

    run_plan(&cfg, plan).await
}

pub async fn check(cli: &Cli, files: &[PathBuf]) -> Result<()> {
    let cfg = config::load(cli.config.as_deref())?;
    let plan = plan_batches(cli, &cfg, files)?;
    print_plan(&plan);
    Ok(())
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

        // Resolve editor: --editor flag wins, else rule.editor.
        let editor_name = cli.editor.clone().unwrap_or_else(|| rule.editor.clone());

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

            info!(
                editor = %batch.editor_name,
                group = %batch.group,
                mode = ?batch.mode,
                sync = batch.sync,
                listen = %listen,
                files = ?batch.files,
                "dispatching to neovim"
            );

            let backend = NeovimBackend { command, listen };
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

fn print_plan(plan: &[Batch]) {
    if plan.is_empty() {
        println!("no matching batches");
        return;
    }
    for (i, b) in plan.iter().enumerate() {
        println!(
            "[{i}] editor={} group={} mode={:?} sync={} rule={}",
            b.editor_name, b.group, b.mode, b.sync, b.rule_name
        );
        for f in &b.files {
            println!("      - {}", strip_verbatim(&f.to_string_lossy()));
        }
    }
}
