//! Decode Herdr chords on the remote client's byte stream, never global mappings.
//! Keep escape sequences and bracketed paste intact across arbitrary read boundaries.
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Bytes(Vec<u8>),
    Action(&'static str),
    Tab(u8),
}

#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
    prefix: bool,
    navigator: bool,
    paste: bool,
    since: Option<Instant>,
}

impl Decoder {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.pending.extend_from_slice(bytes);
        self.since = Some(Instant::now());
        self.decode(false)
    }

    pub fn expire(&mut self) -> Vec<Event> {
        if self.paste {
            return vec![]; // A slow paste must not lose a fragmented closing marker.
        }
        let delay = if self.pending.first() == Some(&0x1b) {
            Duration::from_millis(150)
        } else {
            Duration::from_secs(1)
        };
        if self.since.is_some_and(|t| t.elapsed() >= delay) {
            self.decode(true)
        } else {
            vec![]
        }
    }

    fn decode(&mut self, expired: bool) -> Vec<Event> {
        let mut out = vec![];
        while !self.pending.is_empty() {
            let marker: &[u8] = if self.paste {
                b"\x1b[201~"
            } else {
                b"\x1b[200~"
            };
            if self.pending.starts_with(marker) {
                if self.prefix {
                    self.prefix = false;
                    out.push(Event::Bytes(vec![2]));
                }
                out.push(Event::Bytes(self.pending.drain(..marker.len()).collect()));
                self.paste = !self.paste;
                continue;
            }
            if marker.starts_with(&self.pending) && !expired {
                break;
            }
            if self.paste || (!self.prefix && !self.navigator) {
                let n = self
                    .pending
                    .iter()
                    .position(|b| *b == 0x1b || (!self.paste && *b == 2))
                    .unwrap_or(self.pending.len());
                if n > 0 {
                    out.push(Event::Bytes(self.pending.drain(..n).collect()));
                    continue;
                }
            }
            if self.paste {
                out.push(Event::Bytes(vec![self.pending.remove(0)]));
                continue;
            }
            // These are the Herdr defaults used by Moshi's pane/workspace gestures.
            let sequences: &[(&[u8], &str)] = &[
                (b"\x1b[Z", "previous_pane"),
                (b"\x1b[A", "up"),
                (b"\x1bOA", "up"),
                (b"\x1b[B", "down"),
                (b"\x1bOB", "down"),
                (b"\x1b[C", "right"),
                (b"\x1bOC", "right"),
                (b"\x1b[D", "left"),
                (b"\x1bOD", "left"),
            ];
            if self.prefix || self.navigator {
                if let Some((seq, action)) =
                    sequences.iter().find(|(s, _)| self.pending.starts_with(s))
                {
                    self.pending.drain(..seq.len());
                    out.push(Event::Action(if self.navigator {
                        match *action {
                            "up" => "workspace_up",
                            "down" => "workspace_down",
                            _ => "ignore",
                        }
                    } else {
                        action
                    }));
                    self.prefix = false;
                    continue;
                }
                if !expired && sequences.iter().any(|(s, _)| s.starts_with(&self.pending)) {
                    break;
                }
            }
            let byte = self.pending.remove(0);
            if self.navigator {
                let action = match byte {
                    b'k' => "workspace_up",
                    b'j' => "workspace_down",
                    b'\r' | b'\n' => {
                        self.navigator = false;
                        "workspace_accept"
                    }
                    0x1b | 3 | 2 => {
                        self.navigator = false;
                        "workspace_cancel"
                    }
                    _ => "ignore",
                };
                out.push(Event::Action(action));
            } else if self.prefix {
                self.prefix = false;
                let action = match byte {
                    b'n' => "next_tab",
                    b'p' => "previous_tab",
                    b'\t' => "next_pane",
                    b'z' => "zoom",
                    b'q' => "detach",
                    b'h' => "left",
                    b'j' => "down",
                    b'k' => "up",
                    b'l' => "right",
                    b'w' | b'g' => {
                        self.navigator = true;
                        "workspace_open"
                    }
                    b'c' => "new_tab",
                    b'1'..=b'9' => {
                        out.push(Event::Tab(byte - b'0'));
                        continue;
                    }
                    2 => {
                        out.push(Event::Bytes(vec![2]));
                        continue;
                    }
                    _ => {
                        out.push(Event::Bytes(vec![2, byte]));
                        continue;
                    }
                };
                out.push(Event::Action(action));
            } else if byte == 2 {
                self.prefix = true;
            } else {
                out.push(Event::Bytes(vec![byte]));
            }
        }
        if expired && self.prefix && self.pending.is_empty() {
            self.prefix = false;
            out.push(Event::Bytes(vec![2]));
        }
        // Coalesce ordinary typing/output so large pastes do not cause one write per byte.
        let mut merged = Vec::new();
        for event in out {
            match event {
                Event::Bytes(bytes) => match merged.last_mut() {
                    Some(Event::Bytes(previous)) => previous.extend(bytes),
                    _ => merged.push(Event::Bytes(bytes)),
                },
                _ => merged.push(event),
            }
        }
        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gestures_survive_every_read_boundary() {
        let bytes = b"\x02n\x02p\x02\t\x02\x1b[Z\x02w\x1b[B\r\x02z\x023\x02q";
        let expected = vec![
            Event::Action("next_tab"),
            Event::Action("previous_tab"),
            Event::Action("next_pane"),
            Event::Action("previous_pane"),
            Event::Action("workspace_open"),
            Event::Action("workspace_down"),
            Event::Action("workspace_accept"),
            Event::Action("zoom"),
            Event::Tab(3),
            Event::Action("detach"),
        ];
        for split in 0..=bytes.len() {
            let mut decoder = Decoder::default();
            let mut events = decoder.feed(&bytes[..split]);
            events.extend(decoder.feed(&bytes[split..]));
            assert_eq!(events, expected, "split {split}");
        }
        let mut decoder = Decoder::default();
        let events: Vec<_> = bytes.iter().flat_map(|b| decoder.feed(&[*b])).collect();
        assert_eq!(events, expected);
    }

    #[test]
    fn ordinary_input_and_paste_are_lossless() {
        let bytes = b"hello\x1b[A\xf0\x9f\x98\x80\x1b[200~\x02q\x02z\x02w\r\x1b[201~end";
        for split in 0..=bytes.len() {
            let mut decoder = Decoder::default();
            let mut events = decoder.feed(&bytes[..split]);
            events.extend(decoder.feed(&bytes[split..]));
            let actual: Vec<u8> = events
                .into_iter()
                .flat_map(|e| match e {
                    Event::Bytes(b) => b,
                    _ => panic!("paste triggered {e:?}"),
                })
                .collect();
            assert_eq!(actual, bytes);
        }
    }

    #[test]
    fn literal_prefix_unknown_chord_and_timeout() {
        let mut d = Decoder::default();
        assert_eq!(
            d.feed(b"\x02\x02\x02?"),
            vec![Event::Bytes(b"\x02\x02?".to_vec())]
        );
        assert!(d.feed(b"\x02").is_empty());
        assert_eq!(d.decode(true), vec![Event::Bytes(vec![2])]);
        assert_eq!(d.feed(b"\x02w"), vec![Event::Action("workspace_open")]);
        assert!(d.feed(b"\x1b").is_empty());
        assert_eq!(d.decode(true), vec![Event::Action("workspace_cancel")]);
    }

    #[test]
    fn delayed_paste_end_and_pending_prefix_do_not_trigger_commands() {
        let mut d = Decoder::default();
        assert_eq!(
            d.feed(b"\x02\x1b[200~\x02q"),
            vec![Event::Bytes(b"\x02\x1b[200~\x02q".to_vec())]
        );
        assert!(d.feed(b"\x1b[20").is_empty());
        d.since = Some(Instant::now() - Duration::from_secs(2));
        assert!(d.expire().is_empty());
        assert_eq!(
            d.feed(b"1~\x02z"),
            vec![Event::Bytes(b"\x1b[201~".to_vec()), Event::Action("zoom")]
        );
    }
}
