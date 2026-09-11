//! Persist one terminal preference without replacing unrelated config or server settings.

use super::App;
use crate::config_update::format_config_error;
use crate::legacy_core::config::edit::ConfigEdit;
use crate::legacy_core::config::edit::ConfigEditsBuilder;
use crate::tui;
use crate::tui_settings::SETTINGS;
use crate::tui_settings::Timing;
use serde_json::Value;

impl App {
    pub(super) async fn save_tui_setting(
        &mut self,
        tui: &mut tui::Tui,
        index: usize,
        choice: Option<usize>,
    ) -> Result<String, String> {
        let setting = SETTINGS.get(index).ok_or("Unknown setting")?;
        let value = match choice {
            Some(choice) => {
                let (_, json) = setting.choices().get(choice).ok_or("Unknown choice")?;
                serde_json::from_str::<Value>(json).map_err(|err| err.to_string())?
            }
            None => Value::Null,
        };
        let segments = vec!["tui".to_string(), setting.key.to_string()];
        let edit = match &value {
            Value::Null => ConfigEdit::ClearPath { segments },
            Value::Bool(value) => ConfigEdit::SetPath {
                segments,
                value: (*value).into(),
            },
            Value::String(value) => ConfigEdit::SetPath {
                segments,
                value: value.as_str().into(),
            },
            Value::Number(value) => ConfigEdit::SetPath {
                segments,
                value: value.as_i64().ok_or("Invalid integer")?.into(),
            },
            Value::Array(_) | Value::Object(_) => {
                return Err("Unsupported setting value".to_string());
            }
        };
        ConfigEditsBuilder::for_config_path(self.local_settings.user_config_path.as_path())
            .with_edits([edit])
            .apply()
            .await
            .map_err(|err| format_config_error(&err))?;
        // Reload local layers so project/CLI overrides and inherited defaults remain authoritative.
        let config = self
            .rebuild_config_for_cwd(self.chat_widget.config_ref().cwd.to_path_buf())
            .await
            .map_err(|err| format!("Preference was saved, but could not be reloaded: {err}"))?;
        let resolved = crate::local_settings::LocalSettings::from(&config);
        let effective = setting.value(&resolved.tui);
        let mut preferences =
            serde_json::to_value(&self.local_settings.tui).map_err(|err| err.to_string())?;
        preferences[setting.key] = effective.clone();
        self.local_settings.tui =
            serde_json::from_value(preferences).map_err(|err| err.to_string())?;
        if setting.timing == Timing::Immediate {
            self.chat_widget
                .apply_live_tui_setting(setting.key, effective.clone())?;
            self.refresh_status_line();
            if setting.key == "raw_output_mode" {
                self.apply_raw_output_mode(
                    tui,
                    self.local_settings.tui.raw_output_mode,
                    /*notify*/ false,
                );
            }
            if matches!(
                setting.key,
                "notification_method" | "notification_condition"
            ) {
                let settings = &self.local_settings.tui.notification_settings;
                tui.set_notification_settings(settings.method, settings.condition);
            }
        }
        if setting.key == "show_server_version_notice" {
            self.refresh_server_version_overview_notice(crate::version::CODEX_CLI_VERSION);
        }
        tui.frame_requester().schedule_frame();
        if !value.is_null() && value != effective {
            Ok(format!(
                "Saved. Another config layer or system preference currently takes precedence ({}).",
                setting.value_label(&self.local_settings.tui)
            ))
        } else {
            Ok(format!("Saved. {}.", setting.timing_label()))
        }
    }
}

#[cfg(test)]
#[path = "tui_settings_tests.rs"]
mod tests;
