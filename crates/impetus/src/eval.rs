//! Offline harness eval CLI (`impetus eval`) — #408 / parent #397.
//!
//! No daemon socket, no live keys. Promotion never auto-applies.

use anyhow::{Result, bail};
use clap::Subcommand;
use impetus_core::{
    PromotionRequest, analyze_report, builtin_offline_fixtures, default_harness_revision,
    default_policy_digest_labels, propose_candidate, request_promotion, run_offline_eval,
};

#[derive(Subcommand)]
pub enum EvalAction {
    /// Run built-in offline mock fixtures (no live credentials)
    Run {
        /// Emit JSON report
        #[arg(long)]
        json: bool,
        /// Override harness revision label (default: env / crate version)
        #[arg(long)]
        harness_revision: Option<String>,
        /// Opt-in promotion request (still requires --confirm-human-promote)
        #[arg(long, default_value_t = false)]
        request_promote: bool,
        /// Human confirmation for promotion staging (never auto-applies)
        #[arg(long, default_value_t = false)]
        confirm_human_promote: bool,
    },
    /// Show role-separation stubs (proposer / executor / analyzer / promotion)
    Roles {
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(action: EvalAction) -> Result<()> {
    match action {
        EvalAction::Run {
            json,
            harness_revision,
            request_promote,
            confirm_human_promote,
        } => {
            run_eval(
                json,
                harness_revision,
                request_promote,
                confirm_human_promote,
            )
            .await
        }
        EvalAction::Roles { json } => show_roles(json),
    }
}

async fn run_eval(
    json: bool,
    harness_revision: Option<String>,
    request_promote: bool,
    confirm_human_promote: bool,
) -> Result<()> {
    let fixtures = builtin_offline_fixtures();
    let revision = harness_revision.unwrap_or_else(default_harness_revision);
    let policy_labels = default_policy_digest_labels();
    let report = run_offline_eval(&fixtures, revision, policy_labels).await?;
    let analysis = analyze_report(&report);
    let proposal = propose_candidate("offline builtin baseline (no candidate change)");
    let promotion = request_promotion(&PromotionRequest {
        capsule_digest: report.capsule.digest.clone(),
        opt_in: request_promote,
        human_confirmed: confirm_human_promote,
    });

    if json {
        let out = serde_json::json!({
            "report": report,
            "analysis": analysis,
            "proposal": proposal,
            "promotion": promotion,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("ExperimentCapsule {}", report.capsule.capsule_id);
        println!("  digest:            {}", report.capsule.digest);
        println!("  harness_revision:  {}", report.capsule.harness_revision);
        println!(
            "  policy_labels:     {}",
            report.capsule.policy_digest_labels.join(", ")
        );
        println!(
            "  fixtures:          {}",
            report.capsule.fixture_ids.join(", ")
        );
        println!();
        for r in &report.results {
            let mark = if r.passed { "PASS" } else { "FAIL" };
            println!("  [{mark}] {} — {}", r.fixture_id, r.detail);
        }
        println!();
        println!(
            "analysis: {}/{} passed (role={})",
            analysis.pass_count,
            analysis.fixture_count,
            analysis.role.as_str()
        );
        println!(
            "proposal: {} (role={})",
            proposal.summary,
            proposal.role.as_str()
        );
        println!("promotion: {promotion:?}");
    }

    if !report.all_passed {
        if json {
            std::process::exit(1);
        }
        bail!("eval fixtures did not all pass");
    }
    Ok(())
}

fn show_roles(json: bool) -> Result<()> {
    let roles = [
        (
            "proposer",
            "proposes a small candidate change; must not execute/verify/promote",
        ),
        (
            "executor",
            "runs the immutable capsule fixtures offline (mock provider)",
        ),
        ("analyzer", "reads evidence/results only; no mutation"),
        (
            "promotion",
            "explicit opt-in + human confirm stages only; never automatic",
        ),
    ];
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "roles": roles }))?
        );
    } else {
        println!("Eval roles (#408) — promotion never automatic:\n");
        for (name, desc) in roles {
            println!("  • {name} — {desc}");
        }
        println!(
            "\nCLI: impetus eval run [--request-promote --confirm-human-promote]\n\
             Both flags required to stage; still does not apply to main."
        );
    }
    Ok(())
}
