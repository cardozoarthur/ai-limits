use std::time::Duration;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "ai-limits")]
#[command(about = "Show Codex and Gemini account usage limits without opening their TUIs")]
struct Args {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,

    /// Per-provider timeout in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    timeout_ms: u64,

    /// Provider filter: codex, gemini. Can be repeated or comma-separated.
    #[arg(long, value_delimiter = ',', value_name = "PROVIDER")]
    provider: Vec<String>,

    /// Model filter. Matches id/model/display name case-insensitively.
    #[arg(long, value_delimiter = ',', value_name = "MODEL")]
    model: Vec<String>,
}

fn main() {
    let args = Args::parse();
    let filters = ai_limits::ReportFilters {
        providers: args.provider,
        models: args.model,
    };
    if let Err(error) = ai_limits::validate_report_filters(&filters) {
        eprintln!("{error}");
        std::process::exit(2);
    }
    let options = ai_limits::CollectOptions {
        timeout: Duration::from_millis(args.timeout_ms),
        filters,
    };
    let report = ai_limits::collect_report(&options);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("report must serialize")
        );
    } else {
        print!("{}", ai_limits::render_human_report(&report));
    }

    if ai_limits::report_has_error(&report) {
        std::process::exit(1);
    }
}
