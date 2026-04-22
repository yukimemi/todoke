//! Orchestrator: routes inputs through matcher → template → backend.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use tracing::{debug, info, warn};

use crate::backends::{
    exec::{DispatchCtx as ExecCtx, ExecBackend},
    neovim::NeovimBackend,
};
use crate::cli::Cli;
use crate::config::{self, ResolvedConfig, Rule, Target, TargetKind};
use crate::input::{Input, InputKind};
use crate::matcher::{CaptureMap, first_match};
use crate::style::{
    accent, bold, dim, level_error, level_info, level_ok, level_warn, muted, styled,
};
use crate::template::{Context, build_context, new_engine, render};

pub async fn dispatch(cli: &Cli, files: &[PathBuf]) -> Result<()> {
    let cfg = config::load(cli.config.as_deref())?;
    let inputs = load_inputs(cli, files)?;
    let plan = if inputs.is_empty() {
        plan_no_args(cli, &cfg)?
    } else {
        plan_batches(cli, &cfg, &inputs)?
    };

    debug!(batches = plan.len(), "dispatch plan built");

    if cli.dry_run {
        print_plan(&plan);
        return Ok(());
    }

    run_plan(&cfg, plan).await
}

pub async fn check(cli: &Cli, files: &[PathBuf]) -> Result<()> {
    let cfg = config::load(cli.config.as_deref())?;
    let inputs = load_inputs(cli, files)?;
    let plan = plan_batches(cli, &cfg, &inputs)?;
    print_plan(&plan);
    Ok(())
}

fn load_inputs(cli: &Cli, raws: &[PathBuf]) -> Result<Vec<Input>> {
    raws.iter()
        .map(|p| Input::from_arg_as(&p.to_string_lossy(), cli.as_kind))
        .collect()
}

/// Static analysis of the loaded config.
pub async fn doctor(cli: &Cli) -> Result<()> {
    let cfg = config::load(cli.config.as_deref())?;
    let path = config::resolve_path(cli.config.as_deref())?;

    let target_names = cfg
        .raw
        .todoke
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
        styled("todoke:", muted()),
        if target_names.is_empty() {
            styled("<none>", dim())
        } else {
            styled(&target_names, accent())
        },
        styled(format!("({})", cfg.raw.todoke.len()), dim()),
    );
    println!(
        "{} {}",
        styled("rules:", muted()),
        styled(cfg.raw.rules.len(), accent()),
    );
    println!();

    let mut issues = 0usize;

    for (i, rule) in cfg.raw.rules.iter().enumerate() {
        let name = rule.name.as_deref().unwrap_or("<unnamed>");
        if !rule.to.contains("{{") && !rule.to.contains("{%") {
            if !cfg.raw.todoke.contains_key(&rule.to) {
                println!(
                    "{} {}: to {} is not defined in [todoke.*]",
                    styled("error", level_error()),
                    styled(format!("rule[{i}] ({name})"), bold()),
                    styled(format!("'{}'", rule.to), accent()),
                );
                issues += 1;
            }
        } else {
            println!(
                "{}  {}: to {} is a Tera template, resolved at dispatch time",
                styled("info", level_info()),
                styled(format!("rule[{i}] ({name})"), bold()),
                styled(format!("'{}'", rule.to), accent()),
            );
        }
    }

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
    if catch_all_at.is_none() {
        println!(
            "{}  no catch-all rule at end of config — inputs that match no rule (or are excluded from every rule) will be skipped with a warning",
            styled("warn", level_warn()),
        );
        issues += 1;
    }

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
    bail!("list: not implemented yet")
}

pub async fn kill(group: Option<&str>, all: bool) -> Result<()> {
    if group.is_none() && !all {
        bail!("specify <group> or --all");
    }
    bail!("kill: not implemented yet")
}

/// One batch of inputs bound for a single (target, group, mode, sync) quad.
/// All inputs in a batch share the same resolved target; their individual
/// capture maps are kept so per-input arg rendering can reference them.
#[derive(Debug)]
pub struct Batch {
    pub target_name: String,
    pub group: String,
    pub mode: String,
    pub sync: bool,
    pub rule_name: String,
    pub inputs: Vec<Input>,
    /// Captures from the first input that resolved to this batch — used for
    /// rendering the target's command / listen / args templates. For a
    /// per-input capture model see the per-input rendering in the backends.
    pub cap: CaptureMap,
}

fn plan_no_args(cli: &Cli, cfg: &ResolvedConfig) -> Result<Vec<Batch>> {
    let cwd = std::env::current_dir()
        .context("could not read current directory")?
        .to_string_lossy()
        .into_owned();

    let hit = first_match(cfg, "", None);
    let (rule_idx, cap) = match hit {
        Some((i, c)) => (Some(i), c),
        None => {
            let fallback =
                (cli.editor.is_some() || cli.group.is_some()) && !cfg.raw.rules.is_empty();
            (fallback.then_some(0), CaptureMap::new())
        }
    };

    let Some(rule_idx) = rule_idx else {
        bail!(
            "no rule matches empty-args invocation; add a catch-all rule (e.g. `match = '.*'`) or pass an input"
        );
    };

    let rule = cfg.rule(rule_idx);
    let rule_name = rule
        .name
        .clone()
        .unwrap_or_else(|| format!("rule[{rule_idx}]"));

    let mut tera = new_engine();
    let ctx_phase2 = build_context(Context {
        input: None,
        command: "",
        cwd: &cwd,
        group: "",
        rule_name: &rule_name,
        vars: &cfg.raw.vars,
        cap: &cap,
    });

    let group = resolve_group_with_ctx(cli, rule, &mut tera, &ctx_phase2)?;
    let target_name = resolve_target_name(cli, rule, &mut tera, &ctx_phase2)?;

    Ok(vec![Batch {
        target_name,
        group,
        mode: rule.mode.clone(),
        sync: rule.sync,
        rule_name,
        inputs: Vec::new(),
        cap,
    }])
}

fn plan_batches(cli: &Cli, cfg: &ResolvedConfig, inputs: &[Input]) -> Result<Vec<Batch>> {
    let cwd = std::env::current_dir()
        .context("could not read current directory")?
        .to_string_lossy()
        .into_owned();

    let mut tera = new_engine();
    let mut groups: BTreeMap<BatchKey, Batch> = BTreeMap::new();

    for input in inputs {
        let subject = input.match_string();
        let kind = input.kind();

        let (rule_idx, rule, cap) = match resolve_rule(cli, cfg, &subject, kind)? {
            Some(tuple) => tuple,
            None => {
                warn!(subject = %subject, "no rule matched, skipping");
                continue;
            }
        };

        let rule_name = rule
            .name
            .clone()
            .unwrap_or_else(|| format!("rule[{rule_idx}]"));

        let ctx = build_context(Context {
            input: Some(input),
            command: "",
            cwd: &cwd,
            group: "",
            rule_name: &rule_name,
            vars: &cfg.raw.vars,
            cap: &cap,
        });

        let group = resolve_group_with_ctx(cli, rule, &mut tera, &ctx)?;
        let target_name = resolve_target_name(cli, rule, &mut tera, &ctx)?;

        let key = BatchKey {
            target: target_name.clone(),
            group: group.clone(),
            mode: rule.mode.clone(),
            sync: rule.sync,
        };

        groups
            .entry(key)
            .or_insert_with(|| Batch {
                target_name: target_name.clone(),
                group: group.clone(),
                mode: rule.mode.clone(),
                sync: rule.sync,
                rule_name: rule_name.clone(),
                inputs: Vec::new(),
                cap: cap.clone(),
            })
            .inputs
            .push(input.clone());
    }

    Ok(groups.into_values().collect())
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct BatchKey {
    target: String,
    group: String,
    mode: String,
    sync: bool,
}

fn resolve_rule<'a>(
    cli: &Cli,
    cfg: &'a ResolvedConfig,
    subject: &str,
    kind: InputKind,
) -> Result<Option<(usize, &'a Rule, CaptureMap)>> {
    if cli.editor.is_some() || cli.group.is_some() {
        if cfg.raw.rules.is_empty() {
            bail!(
                "--editor/--group requires at least one [[rules]] in config for mode/sync defaults"
            );
        }
        if let Some((idx, cap)) = first_match(cfg, subject, Some(kind)) {
            return Ok(Some((idx, cfg.rule(idx), cap)));
        }
        return Ok(Some((0, cfg.rule(0), CaptureMap::new())));
    }
    Ok(first_match(cfg, subject, Some(kind)).map(|(idx, cap)| (idx, cfg.rule(idx), cap)))
}

fn resolve_group_with_ctx(
    cli: &Cli,
    rule: &Rule,
    tera: &mut tera::Tera,
    ctx: &tera::Context,
) -> Result<String> {
    if let Some(g) = cli.group.clone() {
        return Ok(g);
    }
    match &rule.group {
        None => Ok(config::DEFAULT_GROUP.to_string()),
        Some(tmpl) => render(tera, tmpl, ctx).context("rendering rule.group template"),
    }
}

fn resolve_target_name(
    cli: &Cli,
    rule: &Rule,
    tera: &mut tera::Tera,
    ctx: &tera::Context,
) -> Result<String> {
    if let Some(e) = cli.editor.clone() {
        return Ok(e);
    }
    render(tera, &rule.to, ctx).context("rendering rule.to template")
}

async fn run_plan(cfg: &ResolvedConfig, plan: Vec<Batch>) -> Result<()> {
    let mut had_err = false;
    for batch in plan {
        if let Err(e) = run_batch(cfg, &batch).await {
            had_err = true;
            tracing::error!(
                target = %batch.target_name,
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
    let target = cfg.target(&batch.target_name)?;
    let cwd = std::env::current_dir()
        .context("cwd")?
        .to_string_lossy()
        .into_owned();

    let mut tera = new_engine();
    let first_input = batch.inputs.first();
    let ctx = build_context(Context {
        input: first_input,
        command: &target.command,
        cwd: &cwd,
        group: &batch.group,
        rule_name: &batch.rule_name,
        vars: &cfg.raw.vars,
        cap: &batch.cap,
    });

    let command =
        render(&mut tera, &target.command, &ctx).context("rendering target.command template")?;
    let rendered_args = render_arg_list(&mut tera, target.args_for(&batch.mode), &ctx)?;

    match target.kind {
        TargetKind::Neovim => {
            run_neovim(target, &command, &rendered_args, batch, &mut tera, &ctx).await
        }
        TargetKind::Exec => run_exec(target, &command, &rendered_args, batch, &cwd, &cfg.raw.vars),
    }
}

async fn run_neovim(
    target: &Target,
    command: &str,
    _rendered_args: &[String],
    batch: &Batch,
    tera: &mut tera::Tera,
    ctx: &tera::Context,
) -> Result<()> {
    let listen_tmpl = target.listen.as_deref().ok_or_else(|| {
        anyhow!(
            "target '{}' has kind=neovim but no listen",
            batch.target_name
        )
    })?;
    let listen = render(tera, listen_tmpl, ctx).context("rendering target.listen template")?;

    // neovim backend takes files only — URL inputs are filtered out and
    // warned about at this boundary.
    let mut files: Vec<PathBuf> = Vec::with_capacity(batch.inputs.len());
    for inp in &batch.inputs {
        if let Some(p) = inp.as_file() {
            files.push(p.to_path_buf());
        } else {
            warn!(input = %inp.display_string(), "neovim backend cannot handle URL inputs; skipping");
        }
    }

    let args_remote = render_arg_list(tera, target.args_for("remote"), ctx)?;
    let args_new = render_arg_list(tera, target.args_for("new"), ctx)?;

    info!(
        target = %batch.target_name,
        group = %batch.group,
        mode = %batch.mode,
        sync = batch.sync,
        listen = %listen,
        files = ?files,
        "dispatching to neovim"
    );

    let backend = NeovimBackend {
        command: command.to_string(),
        listen,
        args_remote,
        args_new,
    };
    backend.dispatch(&files, &batch.mode, batch.sync).await
}

fn run_exec(
    target: &Target,
    command: &str,
    rendered_args: &[String],
    batch: &Batch,
    cwd: &str,
    vars: &BTreeMap<String, toml::Value>,
) -> Result<()> {
    info!(
        target = %batch.target_name,
        group = %batch.group,
        mode = %batch.mode,
        sync = batch.sync,
        inputs = batch.inputs.len(),
        "dispatching to exec"
    );

    let backend = ExecBackend {
        command: command.to_string(),
        args: rendered_args.to_vec(),
        env: target.env.clone(),
        append_inputs: target.append_inputs,
    };
    let dctx = ExecCtx {
        inputs: &batch.inputs,
        mode: &batch.mode,
        sync: batch.sync,
        group: &batch.group,
        rule_name: &batch.rule_name,
        vars,
        cwd,
        cap: &batch.cap,
    };
    backend.dispatch(dctx)
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
            "{} to={} group={} mode={} {} rule={}",
            styled(format!("[{i}]"), muted()),
            styled(&b.target_name, accent()),
            styled(&b.group, accent()),
            styled(&b.mode, level_info()),
            styled(sync_label, sync_style),
            styled(&b.rule_name, bold()),
        );
        for inp in &b.inputs {
            println!(
                "      {} {} {}",
                styled("-", muted()),
                styled(format!("[{}]", inp.kind_label()), dim()),
                styled(inp.display_string(), dim()),
            );
        }
    }
}
