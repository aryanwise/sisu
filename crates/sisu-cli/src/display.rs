use sisu_core::RunOutcome;
use sisu_core::workspace::snapshot;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

pub fn handle_run_outcome(project_dir: &Path, outcome: RunOutcome) -> Result<(), String> {
    match outcome {
        RunOutcome::CompletedAll { completed_count } => {
            let head = snapshot::current_head(project_dir).unwrap_or_else(|_| "unknown".into());
            println!("\n[SISU] All {completed_count} milestone(s) passed verification.");
            println!("[SISU] Latest clean Git checkpoint: {head}");
            Ok(())
        }
        RunOutcome::FailedAtMilestone(report) => {
            println!("\n============================================================");
            println!(
                "[SISU] POINT A — STOPPED AT MILESTONE {}",
                report.milestone_id
            );
            println!("Reason: {}", report.reason);
            println!("============================================================\n");

            println!("Remaining diagnostics:");
            for d in &report.diagnostics {
                println!(
                    "  - {}:{} [{}] {}",
                    d.file,
                    d.line,
                    d.code.as_deref().unwrap_or("error"),
                    d.message
                );
            }

            // Persist the frontier handoff prompt so it's easy to copy/open
            let report_path = project_dir.join("sisu_debug_report.md");
            if fs::write(&report_path, &report.copy_paste_prompt).is_ok() {
                println!(
                    "\n[SISU] Saved copy-paste frontier debug bundle to: {}",
                    report_path.display()
                );
            }

            println!("\n--- COPY-PASTE FOR FRONTIER MODEL ---");
            println!("{}", report.copy_paste_prompt);
            println!("-------------------------------------\n");

            if !report.rolled_back && confirm_rollback()? {
                snapshot::rollback_hard(project_dir)?;
                let head = snapshot::current_head(project_dir).unwrap_or_else(|_| "HEAD".into());
                println!("[SISU] Workspace rolled back cleanly to checkpoint {head}.");
                println!("[SISU] Run again to retry from this clean state.");
            } else if !report.rolled_back {
                println!("[SISU] Left failing files on disk for manual inspection.");
            }

            Ok(())
        }
    }
}

fn confirm_rollback() -> Result<bool, String> {
    print!("Rollback workspace to the last passing milestone commit? [y/N]: ");
    io::stdout()
        .flush()
        .map_err(|e| format!("Failed to flush stdout: {e}"))?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read input: {e}"))?;

    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}
