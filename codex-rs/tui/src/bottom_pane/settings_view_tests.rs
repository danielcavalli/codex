use super::*;
use pretty_assertions::assert_eq;

fn page() -> (
    SettingsView,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    watch::Sender<Tui>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (settings_tx, settings_rx) = watch::channel(toml::from_str("").unwrap());
    (
        SettingsView::new(
            settings_rx,
            AppEventSender::new(tx),
            crate::keymap::RuntimeKeymap::defaults().list,
        ),
        rx,
        settings_tx,
    )
}

fn snapshot(view: &SettingsView, name: &str, width: u16, height: u16) {
    let mut buffer = Buffer::empty(Rect::new(/*x*/ 0, /*y*/ 0, width, height));
    view.render(buffer.area, &mut buffer);
    let text = buffer
        .content
        .chunks(usize::from(width))
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(name, text);
}

#[test]
fn settings_page_sections_and_contextual_help() {
    let (mut view, mut events, _settings) = page();
    snapshot(
        &view,
        "settings_appearance_wide",
        /*width*/ 110,
        /*height*/ 20,
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Char('?')));
    snapshot(
        &view,
        "settings_explanation_narrow",
        /*width*/ 60,
        /*height*/ 28,
    );
    for _ in 0..SECTIONS.len() {
        view.handle_key_event(KeyEvent::from(KeyCode::Tab));
        snapshot(
            &view,
            &format!("settings_section_{}", view.section),
            /*width*/ 100,
            /*height*/ 25,
        );
    }
    assert!(events.try_recv().is_err());
}

#[test]
fn settings_choices_save_only_when_accepted_and_keep_errors_retryable() {
    let (mut view, mut events, settings) = page();
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    assert!(events.try_recv().is_err());
    snapshot(
        &view,
        "settings_choice_short",
        /*width*/ 40,
        /*height*/ 16,
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let AppEvent::SaveTuiSetting {
        index,
        choice,
        response_tx,
    } = events.try_recv().unwrap()
    else {
        panic!("expected save");
    };
    assert_eq!((SETTINGS[index].key, choice), ("whimsy", Some(1)));
    response_tx
        .send(Err("Read-only config file".to_string()))
        .unwrap();
    view.pre_draw_tick(Instant::now());
    snapshot(
        &view,
        "settings_save_failure",
        /*width*/ 100,
        /*height*/ 20,
    );
    assert_eq!(view.choice, Some(1));
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let AppEvent::SaveTuiSetting { response_tx, .. } = events.try_recv().unwrap() else {
        panic!("expected retry");
    };
    settings.send_modify(|tui| tui.whimsy = false);
    response_tx
        .send(Ok("Saved. Applies immediately.".to_string()))
        .unwrap();
    view.pre_draw_tick(Instant::now());
    assert_eq!(view.choice, None);
    assert!(!view.settings.borrow().whimsy);
    snapshot(
        &view,
        "settings_saved",
        /*width*/ 100,
        /*height*/ 20,
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Char('r')));
    assert!(matches!(
        events.try_recv().unwrap(),
        AppEvent::SaveTuiSetting { choice: None, .. }
    ));
}

#[test]
fn settings_cancel_and_picker_preserve_page() {
    let (mut view, mut events, settings) = page();
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    view.handle_key_event(KeyEvent::from(KeyCode::Esc));
    assert!(!view.is_complete());
    assert!(events.try_recv().is_err());
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(
        events.try_recv().unwrap(),
        AppEvent::OpenTuiSettingsPicker(crate::slash_command::SlashCommand::Theme)
    ));
    settings.send_modify(|tui| tui.theme = Some("example-theme".to_string()));
    assert_eq!(
        SETTINGS[view.indices()[view.row]].value_label(&view.settings.borrow()),
        "example-theme"
    );
    assert!(!view.is_complete());
    view.handle_key_event(KeyEvent::from(KeyCode::Esc));
    assert!(view.is_complete());
}

#[test]
fn settings_explanation_scrolls_in_short_terminal_without_changing_preferences() {
    let (mut view, mut events, _settings) = page();
    view.handle_key_event(KeyEvent::from(KeyCode::Char('?')));
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 40, /*height*/ 18,
    );
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer);
    assert!(view.detail_max_scroll.get() > 0);
    for _ in 0..10 {
        view.handle_key_event(KeyEvent::from(KeyCode::PageDown));
    }
    assert_eq!(view.detail_scroll, view.detail_max_scroll.get());
    snapshot(
        &view,
        "settings_scrolled_help",
        /*width*/ 40,
        /*height*/ 18,
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_eq!(view.detail_scroll, 0);
    assert_eq!(view.choice, Some(0));
    assert!(events.try_recv().is_err());
}
