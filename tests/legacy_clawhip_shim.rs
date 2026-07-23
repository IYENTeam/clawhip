use std::process::Command;

#[test]
fn legacy_clawhip_shim_forwards_version_with_op_pi_branding() {
    let output = Command::new(env!("CARGO_BIN_EXE_clawhip"))
        .arg("--version")
        .output()
        .expect("run legacy clawhip shim");

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).starts_with("op-pi "),
        "stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
}
