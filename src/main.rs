use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    match curly::run().await {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("curly: {err}");
            ExitCode::from(err.exit_code())
        }
    }
}
