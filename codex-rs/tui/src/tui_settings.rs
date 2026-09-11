//! Curated terminal preferences. Internal bookkeeping is deliberately not an editable setting.

use crate::slash_command::SlashCommand;
use codex_config::types::Tui;
use serde_json::Value;

pub(crate) const SECTIONS: [&str; 6] = [
    "Appearance",
    "Input & copying",
    "Notifications",
    "Conversation",
    "Sessions",
    "Advanced",
];

#[derive(Clone, Copy, Debug)]
pub(crate) enum Control {
    Toggle,
    Choices(&'static [(&'static str, &'static str)]),
    Picker(SlashCommand),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Timing {
    Immediate,
    Restart,
    NextSession,
}

#[derive(Debug)]
pub(crate) struct Setting {
    pub section: usize,
    pub key: &'static str,
    pub label: &'static str,
    pub explanation: &'static str,
    pub control: Control,
    pub timing: Timing,
}

impl Setting {
    const fn new(
        section: usize,
        key: &'static str,
        label: &'static str,
        explanation: &'static str,
        control: Control,
        timing: Timing,
    ) -> Self {
        Self {
            section,
            key,
            label,
            explanation,
            control,
            timing,
        }
    }

    pub(crate) fn choices(&self) -> &'static [(&'static str, &'static str)] {
        match self.control {
            Control::Toggle => &[("On", "true"), ("Off", "false")],
            Control::Choices(choices) => choices,
            Control::Picker(_) => &[],
        }
    }

    pub(crate) fn value(&self, settings: &Tui) -> Value {
        match serde_json::to_value(settings) {
            Ok(values) => values[self.key].clone(),
            Err(error) => {
                tracing::error!(%error, "could not display terminal preferences");
                Value::Null
            }
        }
    }

    pub(crate) fn value_label(&self, settings: &Tui) -> String {
        let value = self.value(settings);
        self.choices()
            .iter()
            .find_map(|(label, json)| {
                (serde_json::from_str::<Value>(json).ok().as_ref() == Some(&value))
                    .then(|| (*label).to_string())
            })
            .unwrap_or_else(|| match value {
                Value::Null => match self.key {
                    "theme" => "Automatic",
                    "pet" => "None",
                    _ => "Default",
                }
                .to_string(),
                Value::String(value) => value,
                Value::Array(values) => format!("{} selected", values.len()),
                Value::Object(_) => "Customize…".to_string(),
                value => value.to_string(),
            })
    }

    pub(crate) fn timing_label(&self) -> &'static str {
        match self.timing {
            Timing::Immediate => "Applies immediately",
            Timing::Restart => "Applies after restarting Codex",
            Timing::NextSession => "Applies to the next session",
        }
    }
}

use Control::Choices;
use Control::Picker;
use Control::Toggle;
use Timing::Immediate;
use Timing::NextSession;
use Timing::Restart;

pub(crate) static SETTINGS: &[Setting] = &[
    Setting::new(
        /*section*/ 0,
        "whimsy",
        "Decorative effects",
        "Adds fading stars to the composer when using Astra. Turn off if the repeated terminal redraws disrupt text selection and copying. Stars also require animations and a true-color terminal.",
        Toggle,
        Immediate,
    ),
    Setting::new(
        /*section*/ 0,
        "animations",
        "Animations",
        "Controls welcome animations, shimmer effects and spinners. Turning this off also disables Astra stars. Your system's reduced-motion preference can also suppress animations.",
        Toggle,
        Restart,
    ),
    Setting::new(
        /*section*/ 0,
        "theme",
        "Theme",
        "Choose a syntax-highlighting theme with a live preview. Custom themes can also be installed in your Codex themes directory.",
        Picker(SlashCommand::Theme),
        Immediate,
    ),
    Setting::new(
        /*section*/ 0,
        "pet",
        "Terminal pet",
        "Choose a companion or hide the pet using the existing pet picker.",
        Picker(SlashCommand::Pets),
        Immediate,
    ),
    Setting::new(
        /*section*/ 0,
        "pet_anchor",
        "Pet position",
        "Let the pet follow the composer or anchor it to the bottom of the terminal screen.",
        Choices(&[
            ("Composer", "\"composer\""),
            ("Screen bottom", "\"screen-bottom\""),
        ]),
        Immediate,
    ),
    Setting::new(
        /*section*/ 1,
        "raw_output_mode",
        "Copy-friendly output",
        "Uses raw scrollback rendering to make transcript text easier to select and copy with your terminal. The transcript is redrawn when you change this setting. You can also use /raw.",
        Toggle,
        Immediate,
    ),
    Setting::new(
        /*section*/ 1,
        "vim_mode_default",
        "Start in Vim mode",
        "Start new composers in Vim Normal mode. Use /vim to change the current composer's mode.",
        Toggle,
        NextSession,
    ),
    Setting::new(
        /*section*/ 1,
        "disable_paste_burst",
        "Disable paste detection",
        "Insert rapidly arriving characters immediately instead of buffering them as a possible paste. Leave off for automatic paste detection. This is separate from decorative redraws disrupting terminal selection.",
        Toggle,
        Restart,
    ),
    Setting::new(
        /*section*/ 1,
        "question_esc_back",
        "Escape from questions",
        "Escape returns from asynchronous questions to the composer while preserving your answer draft.",
        Toggle,
        Restart,
    ),
    Setting::new(
        /*section*/ 1,
        "keymap",
        "Keyboard shortcuts",
        "Inspect and change shortcuts by context. Context-specific bindings take precedence over global bindings.",
        Picker(SlashCommand::Keymap),
        Immediate,
    ),
    Setting::new(
        /*section*/ 2,
        "notifications",
        "Notifications",
        "Allow or silence terminal notifications. If you configured a custom event list, selecting On replaces it with all supported notification events.",
        Toggle,
        Immediate,
    ),
    Setting::new(
        /*section*/ 2,
        "notification_method",
        "Delivery",
        "Automatic chooses an appropriate terminal notification backend. Desktop uses OSC 9 when supported; Bell uses the terminal bell, whose sound or visual behavior is controlled by your terminal.",
        Choices(&[
            ("Automatic", "\"auto\""),
            ("Desktop (OSC 9)", "\"osc9\""),
            ("Bell", "\"bel\""),
        ]),
        Immediate,
    ),
    Setting::new(
        /*section*/ 2,
        "notification_condition",
        "When to notify",
        "Notify only while the terminal is unfocused, or regardless of focus. Focus detection depends on your terminal's support.",
        Choices(&[
            ("When unfocused", "\"unfocused\""),
            ("Always", "\"always\""),
        ]),
        Immediate,
    ),
    Setting::new(
        /*section*/ 3,
        "auto_recap",
        "Automatic recaps",
        "Generate conversation recaps when the terminal is unfocused. Turning this off keeps /recap available on demand.",
        Toggle,
        Immediate,
    ),
    Setting::new(
        /*section*/ 3,
        "show_tooltips",
        "Startup tips",
        "Show helpful tips on the welcome screen when Codex starts.",
        Toggle,
        Restart,
    ),
    Setting::new(
        /*section*/ 3,
        "status_line",
        "Status line",
        "Choose and order the information below the composer, such as model, directory, Git branch and context usage.",
        Picker(SlashCommand::Statusline),
        Immediate,
    ),
    Setting::new(
        /*section*/ 3,
        "terminal_title",
        "Window title",
        "Choose which activity and session details appear in your terminal's window or tab title.",
        Picker(SlashCommand::Title),
        Immediate,
    ),
    Setting::new(
        /*section*/ 3,
        "status_line_use_colors",
        "Status line colors",
        "Use colors from the active syntax theme for status line items.",
        Toggle,
        Immediate,
    ),
    Setting::new(
        /*section*/ 4,
        "session_picker_view",
        "Session picker layout",
        "Dense fits more saved sessions on screen. Comfortable provides more space between session results.",
        Choices(&[("Dense", "\"dense\""), ("Comfortable", "\"comfortable\"")]),
        NextSession,
    ),
    Setting::new(
        /*section*/ 4,
        "resume_cwd",
        "Resume directory",
        "Ask when launch and saved session directories differ, always use the launch directory, or use the saved session directory.",
        Choices(&[
            ("Ask", "null"),
            ("Launch directory", "\"current\""),
            ("Session directory", "\"session\""),
        ]),
        NextSession,
    ),
    Setting::new(
        /*section*/ 5,
        "alternate_screen",
        "Terminal screen buffer",
        "Automatic lets Codex choose. Always uses the alternate screen; Never keeps the interface inline with ordinary terminal scrollback. This is separate from copy-friendly transcript rendering.",
        Choices(&[
            ("Automatic", "\"auto\""),
            ("Always alternate", "\"always\""),
            ("Never alternate", "\"never\""),
        ]),
        Restart,
    ),
    Setting::new(
        /*section*/ 5,
        "terminal_resize_reflow_max_rows",
        "Resize history",
        "Choose how many rendered terminal rows Codex replays after resizing. Automatic uses a terminal-specific limit. All rows preserves the entire rendered history but can make resizing slower. Existing custom limits are shown unchanged until you select an option.",
        Choices(&[
            ("Automatic", "null"),
            ("1,000 rows", "1000"),
            ("All rows", "0"),
        ]),
        Immediate,
    ),
    Setting::new(
        /*section*/ 5,
        "show_server_version_notice",
        "Server version notice",
        "Show a notice when the connected app server is running an older stable release than this TUI.",
        Toggle,
        Immediate,
    ),
];
