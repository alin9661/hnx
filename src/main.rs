use hnx::cli;

#[tokio::main]
async fn main() {
    if let Err(error) = cli::run().await {
        eprintln!("hnx: {error}");
        std::process::exit(error.exit_code());
    }
}
