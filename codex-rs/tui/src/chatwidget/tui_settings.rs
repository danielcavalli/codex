//! Entry points for the settings page and its existing specialized pickers.

use super::ChatWidget;
use crate::bottom_pane::SettingsView;
use crate::slash_command::SlashCommand;
use codex_config::types::Tui;

impl ChatWidget {
    pub(crate) fn apply_live_tui_setting(
        &mut self,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), String> {
        let mut current =
            serde_json::to_value(&self.local_settings.tui).map_err(|err| err.to_string())?;
        current[key] = value;
        self.local_settings.tui = serde_json::from_value(current).map_err(|err| err.to_string())?;
        self.refresh_model_display();
        Ok(())
    }

    pub(crate) fn open_tui_settings(&mut self, settings: Tui) {
        let (tx, rx) = tokio::sync::watch::channel(settings);
        self.tui_settings_tx = Some(tx);
        self.bottom_pane.show_view(Box::new(SettingsView::new(
            rx,
            self.app_event_tx.clone(),
            self.bottom_pane.list_keymap(),
        )));
    }

    pub(crate) fn open_tui_settings_picker(&mut self, command: SlashCommand) {
        self.dispatch_command(command);
    }

    pub(crate) fn refresh_tui_settings_page(&self, settings: &Tui) {
        if let Some(tx) = &self.tui_settings_tx {
            tx.send_if_modified(|current| {
                if current == settings {
                    return false;
                }
                *current = settings.clone();
                true
            });
        }
    }
}
