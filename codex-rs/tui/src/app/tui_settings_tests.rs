use super::*;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;

async fn configured_app(config: &str) -> (App, tempfile::TempDir) {
    let mut app = crate::app::test_support::make_test_app().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("preferences.toml");
    std::fs::write(&path, config).unwrap();
    app.loader_overrides.user_config_path = Some(path.abs());
    app.local_settings.user_config_path = path.abs();
    (app, dir)
}

#[tokio::test]
async fn settings_save_and_reset_preserve_unrelated_preferences() {
    let (mut app, _dir) =
        configured_app("# Keep my comment\n[tui]\nwhimsy = true\nshow_tooltips = false\n").await;
    // Unrelated runtime changes must not be replaced with disk defaults.
    app.local_settings.tui.notification_settings.notifications =
        codex_config::types::Notifications::Enabled(false);
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    let before = app.local_settings.tui.clone();
    let message = app
        .save_tui_setting(&mut tui, /*index*/ 0, Some(1))
        .await
        .unwrap();
    let mut expected = before;
    expected.whimsy = false;
    assert_eq!(app.local_settings.tui, expected);
    assert!(!app.chat_widget.local_settings.tui.whimsy);
    assert_eq!(message, "Saved. Applies immediately.");
    let text = std::fs::read_to_string(&app.local_settings.user_config_path).unwrap();
    assert_eq!(
        text,
        "# Keep my comment\n[tui]\nwhimsy = false\nshow_tooltips = false\n"
    );
    app.save_tui_setting(&mut tui, /*index*/ 0, /*choice*/ None)
        .await
        .unwrap();
    assert!(app.local_settings.tui.whimsy);
    assert!(app.chat_widget.local_settings.tui.whimsy);
    let text = std::fs::read_to_string(&app.local_settings.user_config_path).unwrap();
    assert_eq!(text, "# Keep my comment\n[tui]\nshow_tooltips = false\n");
}

#[tokio::test]
async fn settings_save_honors_launch_override_and_failed_write_keeps_runtime() {
    let (mut app, dir) = configured_app("").await;
    app.cli_kv_overrides
        .push(("tui.whimsy".to_string(), toml::Value::Boolean(true)));
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    let result = app
        .save_tui_setting(&mut tui, /*index*/ 0, Some(1))
        .await
        .unwrap();
    assert!(result.contains("takes precedence (On)"));
    assert!(app.chat_widget.local_settings.tui.whimsy);
    assert!(
        std::fs::read_to_string(&app.local_settings.user_config_path)
            .unwrap()
            .contains("whimsy = false")
    );
    let before = app.local_settings.tui.clone();
    let blocked = dir.path().join("not-a-directory");
    std::fs::write(&blocked, "file").unwrap();
    app.local_settings.user_config_path = blocked.join("config.toml").abs();
    assert!(
        app.save_tui_setting(&mut tui, /*index*/ 0, Some(1))
            .await
            .is_err()
    );
    assert_eq!(app.local_settings.tui, before);
}

#[tokio::test]
async fn settings_save_enum_and_integer_values_round_trip_through_config_loader() {
    let (mut app, _dir) = configured_app("").await;
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    for (key, choice, expected) in [
        ("notification_method", 2, serde_json::json!("bel")),
        (
            "terminal_resize_reflow_max_rows",
            1,
            serde_json::json!(1000),
        ),
    ] {
        let index = SETTINGS
            .iter()
            .position(|setting| setting.key == key)
            .unwrap();
        app.save_tui_setting(&mut tui, index, Some(choice))
            .await
            .unwrap();
        assert_eq!(SETTINGS[index].value(&app.local_settings.tui), expected);
        assert_eq!(
            SETTINGS[index].value(&app.chat_widget.local_settings.tui),
            expected
        );
    }
}
