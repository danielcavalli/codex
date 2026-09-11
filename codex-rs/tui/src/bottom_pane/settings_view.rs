//! A sectioned preference page with explicit choices and optional contextual explanations.
//! Navigation never writes config; accepting a choice saves it and reports the result in place.

use super::BottomPaneView;
use super::CancellationEvent;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::KeymapContext;
use crate::keymap::KeymapContextSet;
use crate::keymap::ListAction;
use crate::keymap::ListKeymap;
use crate::render::renderable::Renderable;
use crate::tui_settings::Control;
use crate::tui_settings::SECTIONS;
use crate::tui_settings::SETTINGS;
use codex_config::types::Tui;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::oneshot;
use tokio::sync::watch;

pub(crate) struct SettingsView {
    settings: watch::Receiver<Tui>,
    section: usize,
    row: usize,
    choice: Option<usize>,
    explain: bool,
    detail_scroll: u16,
    detail_max_scroll: std::cell::Cell<u16>,
    complete: bool,
    status: String,
    saving: Option<oneshot::Receiver<Result<String, String>>>,
    app_event_tx: AppEventSender,
    keymap: ListKeymap,
}

impl SettingsView {
    pub(crate) fn new(
        settings: watch::Receiver<Tui>,
        app_event_tx: AppEventSender,
        keymap: ListKeymap,
    ) -> Self {
        Self {
            settings,
            section: 0,
            row: 0,
            choice: None,
            explain: false,
            detail_scroll: 0,
            detail_max_scroll: std::cell::Cell::new(/*value*/ 0),
            complete: false,
            status: "Changes save to your user preferences.".to_string(),
            saving: None,
            app_event_tx,
            keymap,
        }
    }

    fn indices(&self) -> Vec<usize> {
        SETTINGS
            .iter()
            .enumerate()
            .filter_map(|(i, setting)| (setting.section == self.section).then_some(i))
            .collect()
    }

    fn save(&mut self, choice: Option<usize>) {
        let (response_tx, response_rx) = oneshot::channel();
        self.app_event_tx.send(AppEvent::SaveTuiSetting {
            index: self.indices()[self.row],
            choice,
            response_tx,
        });
        self.saving = Some(response_rx);
        self.status = "Saving…".to_string();
    }

    fn detail_lines(&self, width: u16) -> Vec<Line<'static>> {
        let setting = &SETTINGS[self.indices()[self.row]];
        let mut lines = vec![
            setting.label.bold().into(),
            setting.timing_label().dim().into(),
            "".into(),
        ];
        let settings = self.settings.borrow();
        match setting.control {
            Control::Picker(_) => lines.push("Open the picker to see and preview choices.".into()),
            Control::Toggle | Control::Choices(_) => {
                let value = setting.value(&settings);
                for (i, (label, json)) in setting.choices().iter().enumerate() {
                    let current = serde_json::from_str::<serde_json::Value>(json)
                        .ok()
                        .as_ref()
                        == Some(&value);
                    let cursor = if self.choice == Some(i) { "›" } else { " " };
                    let mark = if current { "●" } else { "○" };
                    let line = format!("{cursor} {mark} {label}");
                    lines.push(if self.choice == Some(i) {
                        line.cyan().bold().into()
                    } else {
                        line.into()
                    });
                }
            }
        }
        if self.explain {
            lines.push("".into());
            lines.extend(
                textwrap::wrap(setting.explanation, usize::from(width.max(1)))
                    .into_iter()
                    .map(|line| Line::from(line.into_owned())),
            );
        } else {
            lines.push("".into());
            lines.push("?  Explain this setting".dim().into());
        }
        lines
    }

    fn footer(&self) -> Line<'static> {
        let mut spans = vec!["Tab section · ".into()];
        for (action, label) in [
            (ListAction::MoveUp, " / "),
            (ListAction::MoveDown, " move · "),
            (
                ListAction::Accept,
                if self.choice.is_some() {
                    " save · "
                } else {
                    " choose · "
                },
            ),
            (ListAction::Cancel, " back"),
        ] {
            if let Some(hint) = self.keymap.primary_hint(action) {
                spans.extend([hint.into(), label.into()]);
            }
        }
        spans.push(" · ? help · r reset".into());
        if self.explain
            && let Some(hint) = self.keymap.primary_hint(ListAction::PageDown)
        {
            spans.extend([" · ".into(), hint.into(), " more".into()]);
        }
        Line::from(spans)
    }
}

impl BottomPaneView for SettingsView {
    fn keymap_contexts(&self) -> KeymapContextSet {
        KeymapContextSet::new(KeymapContext::List)
    }
    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }
    fn is_complete(&self) -> bool {
        self.complete
    }
    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.complete = true;
        CancellationEvent::Handled
    }

    fn handle_key_event(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if self.keymap.cancel.is_pressed(key) {
            if self.choice.take().is_none() {
                self.complete = true;
            }
            return;
        }
        if self.saving.is_some() {
            return;
        }
        if key.code == KeyCode::Char('?')
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            self.explain = !self.explain;
            self.detail_scroll = 0;
            return;
        }
        if self.keymap.page_down.is_pressed(key) {
            self.detail_scroll = self
                .detail_scroll
                .saturating_add(3)
                .min(self.detail_max_scroll.get());
            return;
        }
        if self.keymap.page_up.is_pressed(key) {
            self.detail_scroll = self.detail_scroll.saturating_sub(3);
            return;
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.section = (self.section
                + if key.code == KeyCode::BackTab {
                    SECTIONS.len() - 1
                } else {
                    1
                })
                % SECTIONS.len();
            self.row = 0;
            self.choice = None;
            self.detail_scroll = 0;
            return;
        }
        let indices = self.indices();
        let setting = &SETTINGS[indices[self.row]];
        if self.keymap.move_up.is_pressed(key) || self.keymap.move_down.is_pressed(key) {
            self.detail_scroll = 0;
            let len = if self.choice.is_some() {
                setting.choices().len()
            } else {
                indices.len()
            };
            let selected = self.choice.as_mut().unwrap_or(&mut self.row);
            *selected = (*selected
                + if self.keymap.move_up.is_pressed(key) {
                    len - 1
                } else {
                    1
                })
                % len;
        } else if self.keymap.accept.is_pressed(key) {
            self.detail_scroll = 0;
            match (setting.control, self.choice) {
                (Control::Picker(command), _) => self
                    .app_event_tx
                    .send(AppEvent::OpenTuiSettingsPicker(command)),
                (_, Some(choice)) => self.save(Some(choice)),
                (_, None) => {
                    self.choice = Some(
                        setting
                            .choices()
                            .iter()
                            .position(|(_, json)| {
                                serde_json::from_str::<serde_json::Value>(json)
                                    .ok()
                                    .as_ref()
                                    == Some(&setting.value(&self.settings.borrow()))
                            })
                            .unwrap_or(0),
                    )
                }
            }
        } else if key.code == KeyCode::Char('r')
            && key.modifiers == KeyModifiers::NONE
            && !matches!(setting.control, Control::Picker(_))
        {
            self.save(None);
        }
    }

    fn pre_draw_tick(&mut self, _now: Instant) -> bool {
        let changed = self.settings.has_changed().unwrap_or(false);
        if changed
            && let Ok(keymap) =
                crate::keymap::RuntimeKeymap::from_config(&self.settings.borrow_and_update().keymap)
        {
            self.keymap = keymap.list;
        }
        let Some(receiver) = self.saving.as_mut() else {
            return changed;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(oneshot::error::TryRecvError::Empty) => return changed,
            Err(oneshot::error::TryRecvError::Closed) => {
                Err("Save interrupted. Select the option again to retry.".to_string())
            }
        };
        self.saving = None;
        match result {
            Ok(message) => {
                self.status = message;
                self.choice = None;
            }
            Err(error) => self.status = format!("Could not save: {error}"),
        }
        true
    }

    fn next_frame_delay(&self) -> Option<Duration> {
        self.saving
            .as_ref()
            .map(|_| Duration::from_millis(/*millis*/ 100))
    }
}

impl Renderable for SettingsView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        ratatui::widgets::Block::default()
            .style(crate::style::user_message_style())
            .render(area, buf);
        let width = area.width.saturating_sub(4).max(1);
        let tabs = SECTIONS
            .iter()
            .enumerate()
            .map(|(i, name)| {
                if i == self.section {
                    format!("[{name}]")
                } else {
                    (*name).to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("  ");
        let mut header = vec![Line::from("Settings".bold())];
        header.extend(
            textwrap::wrap(&tabs, usize::from(width))
                .into_iter()
                .map(|line| Line::from(line.into_owned().cyan())),
        );
        let footer_line = self.footer();
        let footer_lines = super::selection_popup_common::wrap_styled_line(&footer_line, width);
        let [header_area, body, status, footer] = Layout::vertical([
            Constraint::Length(header.len() as u16 + 1),
            Constraint::Fill(1),
            Constraint::Length(2),
            Constraint::Length(footer_lines.len() as u16),
        ])
        .areas(area.inner(ratatui::layout::Margin::new(
            /*horizontal*/ 2, /*vertical*/ 0,
        )));
        Paragraph::new(header).render(header_area, buf);
        let indices = self.indices();
        let [fields, detail] = if body.width >= 90 {
            Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(body)
        } else {
            Layout::vertical([
                Constraint::Length((indices.len() as u16 + 1).min(body.height / 2)),
                Constraint::Fill(1),
            ])
            .areas(body)
        };
        let start = self
            .row
            .saturating_add(1)
            .saturating_sub(usize::from(fields.height));
        let settings = self.settings.borrow();
        let rows: Vec<Line<'static>> = indices
            .iter()
            .enumerate()
            .skip(start)
            .map(|(row, index)| {
                let setting = &SETTINGS[*index];
                let prefix = if row == self.row && self.choice.is_none() {
                    "›"
                } else {
                    " "
                };
                let label = format!(
                    "{prefix} {}  {}",
                    setting.label,
                    setting.value_label(&settings)
                );
                if row == self.row {
                    label.bold().into()
                } else {
                    label.into()
                }
            })
            .collect();
        Paragraph::new(rows).render(fields, buf);
        let details = self.detail_lines(detail.width);
        self.detail_max_scroll
            .set((details.len() as u16).saturating_sub(detail.height));
        // Keep the focused choice visible even in very short terminals.
        let scroll = self
            .choice
            .map_or(0, |choice| {
                (choice as u16 + 4).saturating_sub(detail.height)
            })
            .max(self.detail_scroll.min(self.detail_max_scroll.get()));
        Paragraph::new(details)
            .scroll((scroll, 0))
            .render(detail, buf);
        let status_lines = textwrap::wrap(&self.status, usize::from(width))
            .into_iter()
            .map(|line| Line::from(line.into_owned().dim()))
            .collect::<Vec<_>>();
        Paragraph::new(status_lines).render(status, buf);
        Paragraph::new(footer_lines).render(footer, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let detail_height = self.detail_lines(width.saturating_sub(4)).len() as u16;
        if width >= 94 {
            detail_height.max(7) + 8
        } else {
            detail_height + self.indices().len() as u16 + 9
        }
    }
}

#[cfg(test)]
#[path = "settings_view_tests.rs"]
mod tests;
