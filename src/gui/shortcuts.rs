//! Keyboard shortcuts as each platform names them: macOS symbols
//! (« ⌥⌘L », « ⌘↩ ») there, French key names elsewhere (« Ctrl+Alt+L »,
//! « Ctrl+Entrée »). Every function takes the platform (`mac`) so both are
//! tested anywhere; the `_here` variants use the running one.

use egui::{Event, InputState, Key, KeyboardShortcut, Modifiers};

/// Whether the app runs on macOS.
pub fn is_mac() -> bool {
    cfg!(target_os = "macos")
}

/// `modifiers` alone: « ⌃⌥⇧⌘ » order on macOS, « Ctrl+Alt+Maj+ » elsewhere
/// (each name followed by `+` off macOS, so a key or « clic » can follow).
/// [`Modifiers::COMMAND`] is ⌘ on macOS and Ctrl elsewhere.
pub fn modifiers_label(m: Modifiers, mac: bool) -> String {
    if mac {
        let mut s = String::new();
        for (on, sym) in [
            (m.ctrl, "⌃"),
            (m.alt, "⌥"),
            (m.shift, "⇧"),
            (m.command || m.mac_cmd, "⌘"),
        ] {
            if on {
                s.push_str(sym);
            }
        }
        s
    } else {
        let mut s = String::new();
        for (on, name) in [
            (m.ctrl || m.command || m.mac_cmd, "Ctrl+"),
            (m.alt, "Alt+"),
            (m.shift, "Maj+"),
        ] {
            if on {
                s.push_str(name);
            }
        }
        s
    }
}

/// `key` as printed on the platform's keyboards.
fn key_label(key: Key, mac: bool) -> &'static str {
    match (key, mac) {
        (Key::Enter, true) => "↩",
        (Key::Enter, false) => "Entrée",
        (Key::Backspace, true) => "⌫",
        (Key::Backspace, false) => "Retour arrière",
        (Key::Delete, true) => "⌦",
        (Key::Delete, false) => "Suppr",
        (Key::Tab, true) => "⇥",
        (Key::Insert, _) => "Inser",
        (Key::Space, _) => "Espace",
        (Key::Escape, _) => "Échap",
        (Key::ArrowUp, _) => "↑",
        (Key::ArrowDown, _) => "↓",
        (Key::ArrowLeft, _) => "←",
        (Key::ArrowRight, _) => "→",
        _ => key.symbol_or_name(),
    }
}

/// « ⌥⌘L » / « Ctrl+Alt+L », « ⌘↩ » / « Ctrl+Entrée », « F5 ».
pub fn label(s: &KeyboardShortcut, mac: bool) -> String {
    modifiers_label(s.modifiers, mac) + key_label(s.logical_key, mac)
}

/// [`label`] for this platform.
pub fn label_here(s: &KeyboardShortcut) -> String {
    label(s, is_mac())
}

/// The shortcuts of one action, the main one first: only that one on macOS
/// (the others are fallbacks for PC keyboards), all of them elsewhere
/// (« F5 / Ctrl+Maj+Entrée »).
pub fn hint(keys: &[KeyboardShortcut], mac: bool) -> String {
    let shown = if mac {
        &keys[..keys.len().min(1)]
    } else {
        keys
    };
    shown
        .iter()
        .map(|k| label(k, mac))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// [`hint`] for this platform.
pub fn hint_here(keys: &[KeyboardShortcut]) -> String {
    hint(keys, is_mac())
}

/// Consume the first of `keys` pressed this frame.
pub fn consume_any(input: &mut InputState, keys: &[KeyboardShortcut]) -> bool {
    keys.iter().any(|k| input.consume_shortcut(k))
}

/// Whether this frame's input carries typed text. On Windows AltGr is
/// reported as Ctrl+Alt, so typing `€` (AltGr+E on AZERTY) also looks like
/// Ctrl+Alt+E: such a key is a character, not a shortcut.
pub fn typed_text(events: &[Event]) -> bool {
    events.iter().any(|e| match e {
        Event::Text(t) => !t.is_empty(),
        Event::Ime(egui::ImeEvent::Preedit(t) | egui::ImeEvent::Commit(t)) => !t.is_empty(),
        _ => false,
    })
}

/// Consume `shortcut`, a Ctrl+Alt one (⌥⌘ on macOS), whose Alt also types
/// characters. Off macOS text typed with it means AltGr: a character for the
/// focused field, the shortcut does not fire. On macOS ⌘ never types, so the
/// shortcut fires and the text Option produced alongside (`¬` for ⌥L, the
/// dead key `´` for ⌥E) is dropped before the editor inserts it.
pub fn consume_cmd_alt(input: &mut InputState, shortcut: &KeyboardShortcut, mac: bool) -> bool {
    if !mac && typed_text(&input.events) {
        return false;
    }
    if !input.consume_shortcut(shortcut) {
        return false;
    }
    if mac {
        input.events.retain(|e| {
            !matches!(
                e,
                Event::Text(_) | Event::Ime(egui::ImeEvent::Preedit(_) | egui::ImeEvent::Commit(_))
            )
        });
    }
    true
}

#[cfg(test)]
pub mod test_support {
    use super::*;

    /// The modifiers egui-winit reports when `pattern` is held on the
    /// platform: ⌘ sets `mac_cmd` and `command` on macOS, Ctrl sets `ctrl`
    /// and `command` elsewhere.
    pub fn pressed(pattern: Modifiers, mac: bool) -> Modifiers {
        let cmd = pattern.command || pattern.mac_cmd;
        Modifiers {
            alt: pattern.alt,
            shift: pattern.shift,
            ctrl: pattern.ctrl || (cmd && !mac),
            mac_cmd: cmd && mac,
            command: cmd || (pattern.ctrl && !mac),
        }
    }

    /// The key press of `s` as egui-winit sends it on the platform.
    pub fn press(s: &KeyboardShortcut, mac: bool) -> Event {
        Event::Key {
            key: s.logical_key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: pressed(s.modifiers, mac),
        }
    }

    /// Run one frame with `events`; what `f` returned from the input.
    pub fn frame<R>(events: Vec<Event>, f: impl FnOnce(&mut InputState) -> R) -> R {
        let ctx = egui::Context::default();
        let mut f = Some(f);
        let mut out = None;
        let _ = ctx.run(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ctx| {
                if let Some(f) = f.take() {
                    out = Some(ctx.input_mut(f));
                }
            },
        );
        out.expect("one pass")
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    const fn ks(m: Modifiers, k: Key) -> KeyboardShortcut {
        KeyboardShortcut::new(m, k)
    }

    #[test]
    fn labels_use_symbols_on_mac_and_french_names_elsewhere() {
        let cmd_alt = Modifiers::COMMAND | Modifiers::ALT;
        let cases = [
            (ks(cmd_alt, Key::L), "⌥⌘L", "Ctrl+Alt+L"),
            (ks(Modifiers::COMMAND, Key::Enter), "⌘↩", "Ctrl+Entrée"),
            (
                ks(Modifiers::COMMAND, Key::Backspace),
                "⌘⌫",
                "Ctrl+Retour arrière",
            ),
            (ks(Modifiers::COMMAND, Key::Delete), "⌘⌦", "Ctrl+Suppr"),
            (
                ks(Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter),
                "⇧⌘↩",
                "Ctrl+Maj+Entrée",
            ),
            (ks(Modifiers::CTRL, Key::Space), "⌃Espace", "Ctrl+Espace"),
            (ks(Modifiers::ALT, Key::Insert), "⌥Inser", "Alt+Inser"),
            (ks(Modifiers::ALT, Key::Enter), "⌥↩", "Alt+Entrée"),
            (ks(Modifiers::NONE, Key::F5), "F5", "F5"),
            (ks(Modifiers::NONE, Key::Escape), "Échap", "Échap"),
            (ks(Modifiers::COMMAND, Key::Z), "⌘Z", "Ctrl+Z"),
        ];
        for (s, mac, other) in cases {
            assert_eq!(label(&s, true), mac);
            assert_eq!(label(&s, false), other);
        }
        // Apple's order: ⌃⌥⇧⌘.
        let all = Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT | Modifiers::COMMAND;
        assert_eq!(modifiers_label(all, true), "⌃⌥⇧⌘");
        assert_eq!(modifiers_label(Modifiers::COMMAND, false), "Ctrl+");
    }

    #[test]
    fn hints_show_the_main_key_on_mac_and_every_key_elsewhere() {
        let keys = [
            ks(Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter),
            ks(Modifiers::NONE, Key::F5),
        ];
        assert_eq!(hint(&keys, true), "⇧⌘↩");
        assert_eq!(hint(&keys, false), "Ctrl+Maj+Entrée / F5");
        assert_eq!(hint(&[], true), "");
    }

    #[test]
    fn simulated_presses_match_like_egui_winit_ones() {
        let run = ks(Modifiers::COMMAND, Key::Enter);
        for mac in [true, false] {
            assert!(frame(vec![press(&run, mac)], |i| i.consume_shortcut(&run)));
        }
        // ⌃↩ on macOS is not ⌘↩.
        let ctrl_enter = press(&ks(Modifiers::CTRL, Key::Enter), true);
        assert!(!frame(vec![ctrl_enter], |i| i.consume_shortcut(&run)));
    }

    #[test]
    fn cmd_alt_shortcuts_drop_option_text_on_mac_and_yield_to_altgr_elsewhere() {
        let format = ks(Modifiers::COMMAND | Modifiers::ALT, Key::L);
        // macOS: ⌥⌘L fires, the `¬` produced with it is not typed.
        let (fired, left) = frame(vec![press(&format, true), Event::Text("¬".into())], |i| {
            (consume_cmd_alt(i, &format, true), typed_text(&i.events))
        });
        assert!(fired && !left);
        let dead = Event::Ime(egui::ImeEvent::Preedit("´".into()));
        let history = ks(Modifiers::COMMAND | Modifiers::ALT, Key::E);
        let (fired, left) = frame(vec![press(&history, true), dead], |i| {
            (consume_cmd_alt(i, &history, true), typed_text(&i.events))
        });
        assert!(fired && !left, "dead key of ⌥E");
        // Windows: Ctrl+Alt+E with a `€` is AltGr+E, the character stays.
        let (fired, left) = frame(
            vec![press(&history, false), Event::Text("€".into())],
            |i| (consume_cmd_alt(i, &history, false), typed_text(&i.events)),
        );
        assert!(!fired && left);
        // A real Ctrl+Alt+E.
        assert!(frame(vec![press(&history, false)], |i| consume_cmd_alt(
            i, &history, false
        )));
        // Nothing pressed: no text is dropped.
        let kept = frame(vec![Event::Text("a".into())], |i| {
            consume_cmd_alt(i, &format, true);
            typed_text(&i.events)
        });
        assert!(kept);
    }

    #[test]
    fn typed_text_ignores_empty_text() {
        assert!(!typed_text(&[Event::Text(String::new())]));
        assert!(typed_text(&[Event::Text("€".into())]));
    }
}
