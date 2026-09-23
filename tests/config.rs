use std::process::Command;

#[test]
fn moshi_config_preflight_validates_without_rewriting() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("custom.toml");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_neordr"))
            .args(args)
            .env("XDG_CONFIG_HOME", root.path())
            .env("HERDR_CONFIG_PATH", &path)
            .output()
            .unwrap()
    };
    assert!(run(&["config", "check"]).status.success());
    assert!(!path.exists());
    let contents = "# preserved\n[ui]\nsidebar_collapsed_mode = 'hidden'\n";
    std::fs::write(&path, contents).unwrap();
    assert!(run(&["config", "check"]).status.success());
    let result = run(&["--session", "editor", "server", "reload-config"]);
    assert!(result.status.success());
    let response: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["herdr_ui_settings_applied"], false);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
    for invalid in ["[broken", "[ui]\nsidebar_collapsed_mode = 2\n"] {
        std::fs::write(&path, invalid).unwrap();
        assert!(!run(&["config", "check"]).status.success());
        assert!(!run(&["server", "reload-config"]).status.success());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), invalid);
    }
}
