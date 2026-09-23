//! Pure translation from Herdr semantic input to Neovim API operations.
use crate::contracts::wire::{
    ClientKeyCode as Key, ClientKeyKind, ClientMouseButton as Button, ClientMouseKind as Mouse,
    ClientMousePosition as Position, ClientPaneInputEvent,
};
use rmpv::Value as Msg;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Operation {
    Input(Vec<u8>),
    Call {
        method: &'static str,
        args: Vec<Msg>,
    },
}

pub(super) fn translate(event: ClientPaneInputEvent) -> Vec<Operation> {
    match event {
        ClientPaneInputEvent::Key {
            code,
            modifiers,
            kind,
            repeat_count,
            ..
        } => {
            if kind == ClientKeyKind::Release {
                return vec![];
            }
            vec![Operation::Input(
                key_notation(&code, modifiers)
                    .repeat(repeat_count.clamp(1, 100) as usize)
                    .into_bytes(),
            )]
        }
        ClientPaneInputEvent::TextCommit(text) => {
            vec![Operation::Input(text.replace('<', "<LT>").into_bytes())]
        }
        ClientPaneInputEvent::Paste(text) => vec![Operation::Call {
            method: "nvim_paste",
            args: vec![text.into(), false.into(), (-1).into()],
        }],
        ClientPaneInputEvent::Mouse {
            kind,
            position,
            modifiers,
            lines,
            ..
        } => {
            let button_name = |button| match button {
                Button::Left => "left",
                Button::Right => "right",
                Button::Middle => "middle",
            };
            let (button, action) = match kind {
                Mouse::Down(b) => (button_name(b), "press"),
                Mouse::Up(b) => (button_name(b), "release"),
                Mouse::Drag(b) => (button_name(b), "drag"),
                Mouse::Moved => ("move", ""),
                Mouse::ScrollUp => ("wheel", "up"),
                Mouse::ScrollDown => ("wheel", "down"),
                Mouse::ScrollLeft => ("wheel", "left"),
                Mouse::ScrollRight => ("wheel", "right"),
            };
            let (column, row) = match position {
                Position::Cell { column, row } | Position::Pixels { column, row, .. } => {
                    (column, row)
                }
            };
            let mut mods = String::new();
            for (bit, name) in [(1, "S"), (2, "C"), (4, "A")] {
                if modifiers & bit != 0 {
                    mods.push_str(name);
                }
            }
            vec![
                Operation::Call {
                    method: "nvim_input_mouse",
                    args: vec![
                        button.into(),
                        action.into(),
                        mods.into(),
                        0.into(),
                        row.into(),
                        column.into(),
                    ]
                };
                if button == "wheel" {
                    lines.clamp(1, 100) as usize
                } else {
                    1
                }
            ]
        }
    }
}

pub(super) fn key_notation(code: &Key, modifiers: u8) -> String {
    let name = match code {
        Key::Char(c) if modifiers & !1 == 0 => {
            return if *c == '<' {
                "<LT>".into()
            } else {
                c.to_string()
            };
        }
        Key::Char(c) => {
            if *c == '<' {
                "LT".into()
            } else {
                c.to_string()
            }
        }
        Key::F(n) => format!("F{n}"),
        Key::Backspace => "BS".into(),
        Key::Enter => "CR".into(),
        Key::Left => "Left".into(),
        Key::Right => "Right".into(),
        Key::Up => "Up".into(),
        Key::Down => "Down".into(),
        Key::Home => "Home".into(),
        Key::End => "End".into(),
        Key::PageUp => "PageUp".into(),
        Key::PageDown => "PageDown".into(),
        Key::Tab => "Tab".into(),
        Key::BackTab => "S-Tab".into(),
        Key::Delete => "Del".into(),
        Key::Insert => "Insert".into(),
        Key::Esc => "Esc".into(),
        Key::Null => "Nul".into(),
    };
    let mut prefix = String::new();
    for (bit, name) in [(1, "S-"), (2, "C-"), (4, "M-"), (8, "D-")] {
        if modifiers & bit != 0 {
            prefix.push_str(name);
        }
    }
    format!("<{prefix}{name}>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_text_and_controls_remain_distinct() {
        assert_eq!(
            translate(ClientPaneInputEvent::TextCommit("<Esc>λ".into())),
            vec![Operation::Input("<LT>Esc>λ".as_bytes().to_vec())]
        );
        assert_eq!(key_notation(&Key::Char('c'), 2), "<C-c>");
        assert_eq!(key_notation(&Key::BackTab, 0), "<S-Tab>");
        assert_eq!(key_notation(&Key::Left, 6), "<C-M-Left>");
    }

    #[test]
    fn paste_and_wheel_keep_their_native_semantics() {
        assert_eq!(
            translate(ClientPaneInputEvent::Paste("<Esc>λ".into())),
            vec![Operation::Call {
                method: "nvim_paste",
                args: vec!["<Esc>λ".into(), false.into(), (-1).into()]
            }]
        );
        let wheel = translate(ClientPaneInputEvent::Mouse {
            kind: Mouse::ScrollDown,
            position: Position::Cell { column: 3, row: 7 },
            geometry: None,
            modifiers: 3,
            lines: 2,
        });
        assert_eq!(wheel.len(), 2);
        assert_eq!(
            wheel[0],
            Operation::Call {
                method: "nvim_input_mouse",
                args: vec![
                    "wheel".into(),
                    "down".into(),
                    "SC".into(),
                    0.into(),
                    7.into(),
                    3.into(),
                ]
            }
        );
    }
}
