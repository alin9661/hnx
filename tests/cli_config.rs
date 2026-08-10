use std::{fs, process::Command};

#[test]
fn invalid_explicit_config_exits_two_before_headless_output() {
    let directory = tempfile::tempdir().expect("temp directory");
    let config = directory.path().join("config.toml");
    fs::write(&config, "[layout]\ntwo = [10, 90]\n").expect("write invalid config");

    let output = Command::new(env!("CARGO_BIN_EXE_hnx"))
        .args([
            "--config",
            config.to_str().expect("UTF-8 config path"),
            "--cache-dir",
            directory.path().to_str().expect("UTF-8 cache path"),
            "--offline",
            "feed",
            "top",
        ])
        .output()
        .expect("run hnx");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid layout configuration"));
}
