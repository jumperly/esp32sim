// `--flash-octal` models the ESP32-S3-WROOM-2's octal flash; the other chips refuse it instead of
// running with the flag silently ignored.
#[test]
fn only_the_s3_accepts_the_flash_octal_option() {
    for (chip, name) in [("esp32", "the classic ESP32"), ("c3", "the C3"), ("c6", "the C6")] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_esp32sim"))
            .args(["--chip", chip, "--flash-octal", "--max-insns", "1"]).output().unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(2), "{chip}: {stderr}");
        assert!(stderr.contains(&format!("--flash-octal is not available on {name}")), "{chip}: {stderr}");
    }
}
