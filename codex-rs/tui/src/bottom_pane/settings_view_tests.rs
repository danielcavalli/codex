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
            crate::pets::PetImageSupport::Supported(crate::pets::ImageProtocol::Kitty),
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
    snapshot(
        &view,
        "settings_large_terminal",
        /*width*/ 200,
        /*height*/ 22,
    );
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
    view.handle_key_event(KeyEvent::from(KeyCode::Tab));
    view.handle_key_event(KeyEvent::from(KeyCode::Tab));
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    snapshot(
        &view,
        "settings_notification_delivery",
        /*width*/ 110,
        /*height*/ 20,
    );
    snapshot(
        &view,
        "settings_notification_delivery_short",
        /*width*/ 40,
        /*height*/ 18,
    );
    assert!(events.try_recv().is_err());
}

#[test]
fn settings_arrows_save_directly_and_keep_errors_retryable() {
    let (mut view, mut events, settings) = page();
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Up));
    assert!(events.try_recv().is_err());
    view.handle_key_event(KeyEvent::from(KeyCode::Right));
    snapshot(
        &view,
        "settings_choice_short",
        /*width*/ 40,
        /*height*/ 16,
    );
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
    view.handle_key_event(KeyEvent::from(KeyCode::Right));
    let AppEvent::SaveTuiSetting {
        index,
        choice,
        response_tx,
    } = events.try_recv().unwrap()
    else {
        panic!("expected retry");
    };
    assert_eq!((SETTINGS[index].key, choice), ("whimsy", Some(1)));
    settings.send_modify(|tui| tui.whimsy = false);
    response_tx
        .send(Ok("Saved. Applies immediately.".to_string()))
        .unwrap();
    view.pre_draw_tick(Instant::now());
    assert!(!view.settings.borrow().whimsy);
    snapshot(
        &view,
        "settings_saved",
        /*width*/ 100,
        /*height*/ 20,
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Left));
    let AppEvent::SaveTuiSetting {
        index,
        choice,
        response_tx,
    } = events.try_recv().unwrap()
    else {
        panic!("expected left arrow to save");
    };
    assert_eq!((SETTINGS[index].key, choice), ("whimsy", Some(0)));
    settings.send_modify(|tui| tui.whimsy = true);
    response_tx.send(Ok("Saved.".to_string())).unwrap();
    view.pre_draw_tick(Instant::now());
    view.handle_key_event(KeyEvent::from(KeyCode::Char('r')));
    assert!(matches!(
        events.try_recv().unwrap(),
        AppEvent::SaveTuiSetting { choice: None, .. }
    ));
}

#[test]
fn settings_cancel_and_picker_preserve_page() {
    let (mut view, mut events, settings) = page();
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Left));
    view.handle_key_event(KeyEvent::from(KeyCode::Right));
    assert!(events.try_recv().is_err());
    snapshot(
        &view,
        "settings_picker",
        /*width*/ 100,
        /*height*/ 22,
    );
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
    assert!(events.try_recv().is_err());
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    assert_eq!(view.detail_scroll, 0);
    assert!(events.try_recv().is_err());
}

#[test]
fn settings_arrows_cycle_presets_in_both_directions_from_custom_values() {
    let (mut view, mut events, settings) = page();
    settings.send_modify(|tui| tui.terminal_resize_reflow_max_rows = Some(1234));
    view.handle_key_event(KeyEvent::from(KeyCode::BackTab));
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    let setting = &SETTINGS[view.indices()[view.row]];
    let last = setting.choices().len() - 1;
    for (key, expected) in [
        (KeyCode::Left, last),
        (KeyCode::Right, 0),
        (KeyCode::Left, last),
        (KeyCode::Left, last - 1),
        (KeyCode::Right, last),
    ] {
        view.handle_key_event(KeyEvent::from(key));
        let AppEvent::SaveTuiSetting {
            index,
            choice,
            response_tx,
        } = events.try_recv().unwrap()
        else {
            panic!("expected arrow to save preset");
        };
        assert_eq!(
            (SETTINGS[index].key, choice),
            ("terminal_resize_reflow_max_rows", Some(expected))
        );
        let value = serde_json::from_str(setting.choices()[expected].1).unwrap();
        settings.send_modify(|tui| tui.terminal_resize_reflow_max_rows = value);
        response_tx.send(Ok("Saved.".to_string())).unwrap();
        view.pre_draw_tick(Instant::now());
    }
}

#[test]
fn settings_navigation_crosses_sections_and_remembers_each_selection() {
    let (mut view, mut events, _settings) = page();
    let mut visited = Vec::new();
    for _ in SETTINGS {
        visited.push(SETTINGS[view.indices()[view.row]].key);
        view.handle_key_event(KeyEvent::from(KeyCode::Down));
    }
    assert_eq!(
        visited,
        SETTINGS
            .iter()
            .map(|setting| setting.key)
            .collect::<Vec<_>>()
    );
    assert_eq!((view.section, view.row), (0, 0));
    view.handle_key_event(KeyEvent::from(KeyCode::Up));
    assert_eq!(
        SETTINGS[view.indices()[view.row]].key,
        "show_server_version_notice"
    );
    assert!(events.try_recv().is_err());
    let (mut view, mut events, _settings) = page();
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Tab));
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    let selection = (view.section, view.row);
    view.handle_key_event(KeyEvent::from(KeyCode::BackTab));
    assert_eq!(SETTINGS[view.indices()[view.row]].key, "theme");
    view.handle_key_event(KeyEvent::from(KeyCode::Tab));
    assert_eq!((view.section, view.row), selection);
    assert!(events.try_recv().is_err());
}

#[test]
fn settings_unavailable_pets_show_reasons_and_cannot_be_changed() {
    use crate::pets::PetImageSupport;
    use crate::pets::PetImageUnsupportedReason;
    use ratatui::style::Modifier;

    for reason in [
        PetImageUnsupportedReason::Tmux,
        PetImageUnsupportedReason::Zellij,
        PetImageUnsupportedReason::Iterm2TooOld,
        PetImageUnsupportedReason::Terminal,
    ] {
        let (mut view, mut events, settings) = page();
        view.pet_support = PetImageSupport::Unsupported(reason);
        settings.send_modify(|tui| tui.pet = Some("cat".to_string()));
        let original = settings.borrow().clone();
        for _ in 0..3 {
            view.handle_key_event(KeyEvent::from(KeyCode::Down));
        }
        for key in ["pet", "pet_anchor"] {
            assert_eq!(SETTINGS[view.indices()[view.row]].key, key);
            let mut buffer = Buffer::empty(Rect::new(
                /*x*/ 0, /*y*/ 0, /*width*/ 110, /*height*/ 24,
            ));
            view.render(buffer.area, &mut buffer);
            let marker = buffer
                .content
                .iter()
                .find(|cell| cell.symbol() == "›")
                .unwrap();
            assert!(marker.modifier.contains(Modifier::DIM));
            let details = view
                .detail_lines(/*width*/ 500)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(details.contains(view.pet_support.unsupported_message().unwrap()));
            for code in [
                KeyCode::Enter,
                KeyCode::Left,
                KeyCode::Right,
                KeyCode::Char('r'),
            ] {
                view.handle_key_event(KeyEvent::from(code));
            }
            assert!(events.try_recv().is_err());
            if reason == PetImageUnsupportedReason::Tmux {
                snapshot(
                    &view,
                    &format!("settings_unavailable_{key}"),
                    /*width*/ 110,
                    /*height*/ 24,
                );
            }
            view.handle_key_event(KeyEvent::from(KeyCode::Down));
        }
        assert_eq!(*settings.borrow(), original);
    }
}

#[test]
fn settings_supported_terminal_keeps_pet_picker_and_position_available() {
    let (mut view, mut events, _settings) = page();
    for _ in 0..3 {
        view.handle_key_event(KeyEvent::from(KeyCode::Down));
    }
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(
        events.try_recv().unwrap(),
        AppEvent::OpenTuiSettingsPicker(crate::slash_command::SlashCommand::Pets)
    ));
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Right));
    let AppEvent::SaveTuiSetting { index, choice, .. } = events.try_recv().unwrap() else {
        panic!("expected pet position save");
    };
    assert_eq!((SETTINGS[index].key, choice), ("pet_anchor", Some(1)));
}

#[test]
fn settings_navigation_stays_responsive_while_saving() {
    let (mut view, mut events, settings) = page();
    view.handle_key_event(KeyEvent::from(KeyCode::Right));
    let AppEvent::SaveTuiSetting { response_tx, .. } = events.try_recv().unwrap() else {
        panic!("expected save");
    };
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Tab));
    assert_eq!(SETTINGS[view.indices()[view.row]].key, "raw_output_mode");
    view.handle_key_event(KeyEvent::from(KeyCode::Right));
    assert!(events.try_recv().is_err());
    settings.send_modify(|tui| tui.whimsy = false);
    response_tx
        .send(Ok("Saved. Applies immediately.".to_string()))
        .unwrap();
    view.pre_draw_tick(Instant::now());
    assert_eq!(
        view.status,
        "Decorative effects: Saved. Applies immediately."
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Right));
    let AppEvent::SaveTuiSetting { index, .. } = events.try_recv().unwrap() else {
        panic!("expected another save after completion");
    };
    assert_eq!(SETTINGS[index].key, "raw_output_mode");
}
