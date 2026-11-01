use std::process::Command;

#[test]
fn version_prints_name_and_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_tiller"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("tiller {}", env!("CARGO_PKG_VERSION"))
    );
}
