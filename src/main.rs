use clap::Parser;
use serde::Serialize;

use wipsaw::cli::{Cli, run};

fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    if let Err(error) = run(cli) {
        if json {
            let payload = ErrorEnvelope {
                error: ErrorBody {
                    code: error.code(),
                    message: error.to_string(),
                },
            };
            eprintln!(
                "{}",
                serde_json::to_string(&payload).unwrap_or_else(|_| {
                    "{\"error\":{\"code\":\"serialization_error\",\"message\":\"failed to serialize error\"}}".to_string()
                })
            );
        } else {
            eprintln!("wipsaw: {error}");
        }
        std::process::exit(1);
    }
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: String,
}
