//! Connection-local surface baselines. Optional scroll encoding is upstream code;
//! clients that did not negotiate it continue receiving complete surfaces.
use super::wire::{self, PaneSurfacePatch, PaneSurfacePatchRow, ServerMessage, Surface};
use crate::contracts::wire::surface_scroll;
use anyhow::Result;
use std::io::Write;

pub(super) const CAPABILITY: &str = surface_scroll::CAPABILITY;

pub(super) struct Stream {
    enabled: bool,
    previous: Option<Surface>,
    epoch: u64,
}
impl Stream {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            previous: None,
            epoch: 0,
        }
    }
    pub fn send(&mut self, writer: &mut impl Write, next: Surface, epoch: u64) -> Result<()> {
        let compact = self
            .previous
            .as_ref()
            .filter(|_| self.enabled && self.epoch == epoch)
            .and_then(|last| update(last, &next));
        if let Some(message) = compact {
            wire::send(writer, &message)?;
            self.previous = Some(next);
        } else {
            let full = ServerMessage::PaneSurface(next);
            wire::send(writer, &full)?;
            let ServerMessage::PaneSurface(next) = full else {
                unreachable!()
            };
            self.previous = self.enabled.then_some(next);
        }
        self.epoch = epoch;
        Ok(())
    }
}
fn size<T: serde::Serialize>(value: &T) -> Option<usize> {
    bincode::serde::encode_into_std_write(value, &mut std::io::sink(), bincode::config::standard())
        .ok()
}
fn patch(last: &Surface, next: &Surface) -> Option<PaneSurfacePatch> {
    if last.boot_id != next.boot_id
        || last.projection_revision != next.projection_revision
        || last.surface_revision.checked_add(1) != Some(next.surface_revision)
        || last.frame.width != next.frame.width
        || last.frame.height != next.frame.height
        || last.frame.width == 0
        || last.frame.cells.len() != next.frame.cells.len()
        || next.frame.cells.len() != usize::from(next.frame.width) * usize::from(next.frame.height)
        || last.frame.hyperlinks != next.frame.hyperlinks
        || last.frame.graphics != next.frame.graphics
        || last.graphics != next.graphics
        || last.splits != next.splits
        || last.popup != next.popup
        || last.panes.len() != next.panes.len()
        || !last.panes.iter().zip(&next.panes).all(|(a, b)| {
            a.pane_id == b.pane_id && a.rect == b.rect && a.inner_rect == b.inner_rect
        })
    {
        return None;
    }
    let width = usize::from(next.frame.width);
    let mut rows = Vec::new();
    for (y, (old, new)) in last
        .frame
        .cells
        .chunks(width)
        .zip(next.frame.cells.chunks(width))
        .enumerate()
    {
        let Some(start) = old.iter().zip(new).position(|(a, b)| a != b) else {
            continue;
        };
        let end = old.iter().zip(new).rposition(|(a, b)| a != b).unwrap() + 1;
        rows.push(PaneSurfacePatchRow {
            x: start as u16,
            y: y as u16,
            cells: new[start..end].to_vec(),
        });
    }
    Some(PaneSurfacePatch {
        boot_id: next.boot_id.clone(),
        projection_revision: next.projection_revision,
        base_surface_revision: last.surface_revision,
        surface_revision: next.surface_revision,
        rows,
        panes: next.panes.clone(),
        cursor: next.frame.cursor.clone(),
    })
}
fn update(last: &Surface, next: &Surface) -> Option<ServerMessage> {
    let patch = patch(last, next)?;
    let scroll = surface_scroll::message(last, &patch);
    let plain = ServerMessage::PaneSurfacePatch(patch);
    let message = scroll
        .filter(|m| size(m).unwrap_or(usize::MAX) < size(&plain).unwrap_or(0))
        .unwrap_or(plain);
    // The upstream enum tag is one byte (guarded by the contract regression).
    let full_size = size(next)?.checked_add(1)?;
    (size(&message)? < full_size).then_some(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::wire::{CellData, FrameData, PaneSurfacePane, SurfaceRect};
    use std::io::Cursor;

    fn sample(width: u16, height: u16, panes: u16) -> Surface {
        let cells = (0..height)
            .flat_map(|y| {
                (0..width).map(move |x| CellData {
                    symbol: char::from(b'!' + ((y * 3 + x) % 90) as u8).to_string(),
                    fg: 0x0200ff00,
                    bg: 0x02000000,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                })
            })
            .collect();
        let panes = (0..panes)
            .map(|i| {
                let rect = SurfaceRect {
                    x: 0,
                    y: i * (height / panes),
                    width,
                    height: height / panes,
                };
                PaneSurfacePane {
                    pane_id: format!("p{i}"),
                    content_revision: 1,
                    rect,
                    inner_rect: rect,
                    scrollbar_rect: None,
                    scroll: None,
                    focused: i == 0,
                    mouse_reporting: true,
                    sgr_pixel_mouse: false,
                    alternate_screen_active: true,
                    pixel_width: 0,
                    pixel_height: 0,
                }
            })
            .collect();
        Surface {
            boot_id: "test".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: FrameData {
                cells,
                width,
                height,
                cursor: None,
                hyperlinks: vec![],
                graphics: vec![],
            },
            panes,
            splits: vec![],
            popup: None,
            graphics: Default::default(),
        }
    }
    fn decode(bytes: &[u8]) -> ServerMessage {
        wire::decode(&wire::read(&mut Cursor::new(bytes)).unwrap()).unwrap()
    }
    fn scrolled(last: &Surface) -> Surface {
        let mut next = last.clone();
        next.surface_revision += 1;
        let width = usize::from(next.frame.width);
        for p in &next.panes {
            let start = usize::from(p.rect.y) * width;
            let end = start + usize::from(p.rect.height) * width;
            next.frame.cells[start..end].rotate_left(width);
            for c in &mut next.frame.cells[end - width..end] {
                c.symbol = "新".into();
            }
        }
        next
    }
    fn expand(last: &Surface, message: ServerMessage) -> Surface {
        let mut next = last.clone();
        let patch = match message {
            ServerMessage::PaneSurface(full) => return full,
            ServerMessage::PaneSurfacePatch(p) => p,
            ServerMessage::EndpointControl { kind, data } => {
                assert_eq!(kind, surface_scroll::MESSAGE_KIND);
                surface_scroll::apply(
                    &mut next.frame.cells,
                    next.frame.width,
                    next.frame.height,
                    surface_scroll::decode(&data).unwrap(),
                )
                .unwrap()
            }
            _ => panic!("unexpected surface message"),
        };
        assert_eq!(patch.base_surface_revision, last.surface_revision);
        for row in patch.rows {
            let start = usize::from(row.y) * usize::from(next.frame.width) + usize::from(row.x);
            next.frame.cells[start..start + row.cells.len()].clone_from_slice(&row.cells);
        }
        next.frame.cursor = patch.cursor;
        next.panes = patch.panes;
        next.surface_revision = patch.surface_revision;
        next
    }
    #[test]
    fn negotiated_scroll_and_sparse_edit_roundtrip() {
        for panes in [1, 15] {
            let original = sample(240, 90, panes);
            let mut stream = Stream::new(true);
            let mut bytes = vec![];
            stream.send(&mut bytes, original.clone(), 1).unwrap();
            assert!(matches!(decode(&bytes), ServerMessage::PaneSurface(_)));
            let full_size = bytes.len();
            let next = scrolled(&original);
            bytes.clear();
            stream.send(&mut bytes, next.clone(), 1).unwrap();
            assert!(matches!(
                decode(&bytes),
                ServerMessage::EndpointControl { .. }
            ));
            assert_eq!(expand(&original, decode(&bytes)), next);
            assert!(
                bytes.len() < full_size / 3,
                "{panes} panes: {} / {full_size}",
                bytes.len()
            );
            let mut edited = next.clone();
            edited.surface_revision += 1;
            edited.frame.cells[5].symbol = "界".into();
            edited.frame.cells[6].skip = true;
            bytes.clear();
            stream.send(&mut bytes, edited.clone(), 1).unwrap();
            assert!(matches!(decode(&bytes), ServerMessage::PaneSurfacePatch(_)));
            assert_eq!(expand(&next, decode(&bytes)), edited);
        }
    }
    #[test]
    fn baseline_boundaries_and_legacy_clients_get_full_frames() {
        let original = sample(120, 36, 1);
        let next = scrolled(&original);
        let mut legacy = Stream::new(false);
        for s in [&original, &next] {
            let mut bytes = vec![];
            legacy.send(&mut bytes, s.clone(), 1).unwrap();
            assert!(matches!(decode(&bytes), ServerMessage::PaneSurface(_)));
        }
        for change in 0..5 {
            let mut stream = Stream::new(true);
            stream.send(&mut vec![], original.clone(), 1).unwrap();
            let mut changed = next.clone();
            match change {
                0 => changed.projection_revision += 1,
                1 => changed.boot_id = "new".into(),
                2 => changed = sample(130, 40, 1),
                3 => changed.panes[0].pane_id = "other".into(),
                _ => {}
            }
            let mut bytes = vec![];
            stream
                .send(&mut bytes, changed, if change == 4 { 2 } else { 1 })
                .unwrap();
            assert!(matches!(decode(&bytes), ServerMessage::PaneSurface(_)));
        }
        assert_eq!(
            size(&ServerMessage::PaneSurface(original.clone())).unwrap(),
            size(&original).unwrap() + 1
        );
    }
    #[test]
    fn failed_write_does_not_advance_baseline() {
        struct Fail;
        impl Write for Fail {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut stream = Stream::new(true);
        let last = sample(120, 36, 1);
        stream.send(&mut vec![], last.clone(), 1).unwrap();
        assert!(stream.send(&mut Fail, scrolled(&last), 1).is_err());
        assert_eq!(stream.previous.as_ref().unwrap(), &last);
    }
    #[test]
    #[ignore = "opt-in diagnostic timings; no wall-clock assertions"]
    fn scaling() {
        for panes in [1, 15] {
            let last = sample(480, 150, panes);
            let next = scrolled(&last);
            let start = std::time::Instant::now();
            let mut bytes = 0;
            for _ in 0..30 {
                bytes = size(&update(&last, &next).unwrap()).unwrap();
            }
            eprintln!(
                "{panes} panes: {:.3} ms/frame, {bytes} bytes vs {} full",
                start.elapsed().as_secs_f64() * 1000.0 / 30.0,
                size(&last).unwrap() + 1
            );
        }
    }
}
