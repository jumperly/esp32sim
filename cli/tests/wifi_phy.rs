use std::{path::PathBuf, process::Command};

/// Optional private Robot Face starter build: bootloader.bin, partitions.bin,
/// firmware.bin and esp32s3_rev0_rom.elf (hashes in docs/evidence/s3-phy-no-ap).
/// No user firmware. Skip these external tests without the matching artefacts.
fn robot_face(wifi: Option<&str>, seconds: &str) -> String {
    let dir = PathBuf::from(std::env::var("ROBOT_FACE_FW_DIR")
        .expect("set ROBOT_FACE_FW_DIR to private Robot Face boot artefacts and S3 ROM"));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_esp32sim"));
    cmd.args(["--chip", "s3", "--boot", "rom", "--rom"])
        .arg(dir.join("esp32s3_rev0_rom.elf"))
        .arg("--bootloader").arg(dir.join("bootloader.bin"))
        .arg("--ptable").arg(dir.join("partitions.bin"))
        .arg("--app").arg(dir.join("firmware.bin"))
        .args(["--flash-mb", "8", "--psram-mb", "0", "--board", "none",
            "--net", "none", "--max-seconds", seconds, "--no-dump"]);
    if let Some(wifi) = wifi { cmd.args(["--wifi", wifi]); }
    let out = cmd.output().expect("run esp32sim");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("TFT initialised"));
    assert!(text.contains("Connecting to"));
    text
}

#[test]
#[ignore = "set ROBOT_FACE_FW_DIR to unchanged private Robot Face boot artefacts and S3 ROM"]
fn external_robot_face_no_ap_leaves_phy_poll() {
    // 4 nominal seconds = 960,000,000 guest cycles, not a host timing benchmark.
    let text = robot_face(None, "4");
    assert!(!text.contains("Wi-Fi connected — IP:"));
    assert!(!text.contains("Setup complete"));
    let text = robot_face(None, "20");
    assert!(text.contains("Wi-Fi not connected — continuing offline"));
    assert!(text.contains("Setup complete"));
    assert!(!text.contains("Wi-Fi connected — IP:"));
}

#[test]
#[ignore = "set ROBOT_FACE_FW_DIR to unchanged private Robot Face boot artefacts and S3 ROM"]
fn external_robot_face_matching_ap_connects() {
    // These are placeholder literals in the starter, not saved credentials.
    let text = robot_face(Some("ssid=YOUR_WIFI_NAME,psk=YOUR_WIFI_PASSWORD,chan=6"), "10");
    assert!(text.contains("Wi-Fi connected — IP:"));
    assert!(text.contains("Setup complete"));
    assert!(!text.contains("continuing offline"));
}

#[test]
#[ignore = "set ROBOT_FACE_FW_DIR to unchanged private Robot Face boot artefacts and S3 ROM"]
fn external_robot_face_mismatched_ap_waits() {
    let text = robot_face(Some("ssid=fixture-unmatched,chan=6"), "10");
    assert!(!text.contains("Wi-Fi connected — IP:"));
    assert!(!text.contains("Setup complete"));
    assert!(!text.contains("continuing offline"));
}
