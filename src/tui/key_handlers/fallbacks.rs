//! Keys standing for shortcuts that default macOS terminals (Terminal.app,
//! iTerm2) do not send: F-keys need fn and are often taken by the system,
//! Ctrl+Enter arrives as a plain Enter, Ctrl+Space switches the input
//! source, Option types characters (`≠`, `–`) instead of acting as Alt.
//! Each fallback is turned into the key it stands for before dispatch.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::tui::ActivePanel;

/// The key `code` + `modifiers` stands for in `panel`, if it is a fallback:
///
/// | Fallback          | Panel          | Stands for                     |
/// |-------------------|----------------|--------------------------------|
/// | Ctrl+E            | Query editor   | Ctrl+Enter (run the statement) |
/// | Ctrl+K            | Query editor   | Ctrl+Space (completion)        |
/// | Ctrl+G            | Any            | F5 (run the whole editor)      |
/// | Ctrl+O            | Any            | F6 (export the results)        |
/// | Ctrl+L            | Any            | F7 (import a CSV)              |
/// | `E` / `I`         | Tables         | Shift+F6 / Shift+F7 (batch)    |
/// | `+` `=` / `-`     | Outside editor | Alt+`+` / Alt+`-` (resize)     |
///
/// Ctrl+letter with Alt is AltGr (a character), never a fallback.
pub fn fallback(
    code: KeyCode,
    modifiers: KeyModifiers,
    panel: ActivePanel,
) -> Option<(KeyCode, KeyModifiers)> {
    let KeyCode::Char(c) = code else {
        return None;
    };
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);
    let editor = panel == ActivePanel::QueryEditor;
    if ctrl && !alt {
        return match c.to_ascii_lowercase() {
            'e' if editor => Some((KeyCode::Enter, KeyModifiers::CONTROL)),
            'k' if editor => Some((KeyCode::Char(' '), KeyModifiers::CONTROL)),
            'g' => Some((KeyCode::F(5), KeyModifiers::NONE)),
            'o' => Some((KeyCode::F(6), KeyModifiers::NONE)),
            'l' => Some((KeyCode::F(7), KeyModifiers::NONE)),
            _ => None,
        };
    }
    if ctrl || alt || editor {
        return None;
    }
    match c {
        'E' if panel == ActivePanel::Tables => Some((KeyCode::F(6), KeyModifiers::SHIFT)),
        'I' if panel == ActivePanel::Tables => Some((KeyCode::F(7), KeyModifiers::SHIFT)),
        '+' | '=' | '-' => Some((KeyCode::Char(c), KeyModifiers::ALT)),
        _ => None,
    }
}

/// [`fallback`], or the key itself.
pub fn resolve(
    code: KeyCode,
    modifiers: KeyModifiers,
    panel: ActivePanel,
) -> (KeyCode, KeyModifiers) {
    fallback(code, modifiers, panel).unwrap_or((code, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ActivePanel::*;

    const CTRL: KeyModifiers = KeyModifiers::CONTROL;
    const NONE: KeyModifiers = KeyModifiers::NONE;

    #[test]
    fn editor_fallbacks_run_and_complete() {
        assert_eq!(
            fallback(KeyCode::Char('e'), CTRL, QueryEditor),
            Some((KeyCode::Enter, CTRL))
        );
        assert_eq!(
            fallback(KeyCode::Char('k'), CTRL, QueryEditor),
            Some((KeyCode::Char(' '), CTRL))
        );
        // Outside the editor, Ctrl+E / Ctrl+K stand for nothing.
        for panel in [Connections, Tables, Results] {
            assert_eq!(fallback(KeyCode::Char('e'), CTRL, panel), None);
            assert_eq!(fallback(KeyCode::Char('k'), CTRL, panel), None);
        }
    }

    #[test]
    fn function_key_fallbacks_work_in_every_panel() {
        for panel in [Connections, Tables, QueryEditor, Results] {
            for (c, f) in [('g', 5), ('o', 6), ('l', 7)] {
                assert_eq!(
                    fallback(KeyCode::Char(c), CTRL, panel),
                    Some((KeyCode::F(f), NONE)),
                    "Ctrl+{c} in {panel:?}"
                );
                // Ctrl+Shift sends the same letter, upper case.
                let upper = KeyCode::Char(c.to_ascii_uppercase());
                assert!(fallback(upper, CTRL | KeyModifiers::SHIFT, panel).is_some());
                // AltGr (Ctrl+Alt) types a character.
                assert_eq!(
                    fallback(KeyCode::Char(c), CTRL | KeyModifiers::ALT, panel),
                    None
                );
            }
        }
    }

    #[test]
    fn batch_fallbacks_belong_to_the_tables_panel() {
        let shift = KeyModifiers::SHIFT;
        assert_eq!(
            fallback(KeyCode::Char('E'), shift, Tables),
            Some((KeyCode::F(6), shift))
        );
        assert_eq!(
            fallback(KeyCode::Char('I'), shift, Tables),
            Some((KeyCode::F(7), shift))
        );
        for panel in [Connections, QueryEditor, Results] {
            assert_eq!(fallback(KeyCode::Char('E'), shift, panel), None);
            assert_eq!(fallback(KeyCode::Char('I'), shift, panel), None);
        }
        assert_eq!(fallback(KeyCode::Char('e'), NONE, Tables), None);
    }

    #[test]
    fn plain_resize_keys_outside_the_editor() {
        let alt = KeyModifiers::ALT;
        for panel in [Connections, Tables, Results] {
            for c in ['+', '=', '-'] {
                assert_eq!(
                    fallback(KeyCode::Char(c), NONE, panel),
                    Some((KeyCode::Char(c), alt))
                );
            }
            // `+` typed with Shift on most layouts.
            assert!(fallback(KeyCode::Char('+'), KeyModifiers::SHIFT, panel).is_some());
        }
        // The editor types them.
        assert_eq!(fallback(KeyCode::Char('-'), NONE, QueryEditor), None);
        assert_eq!(fallback(KeyCode::Char('+'), NONE, QueryEditor), None);
    }

    #[test]
    fn other_keys_stand_for_themselves() {
        assert_eq!(
            resolve(KeyCode::Char('x'), NONE, QueryEditor),
            (KeyCode::Char('x'), NONE)
        );
        assert_eq!(
            resolve(KeyCode::Enter, CTRL, QueryEditor),
            (KeyCode::Enter, CTRL)
        );
        assert_eq!(fallback(KeyCode::Char('r'), CTRL, Tables), None, "Ctrl+R");
    }
}
