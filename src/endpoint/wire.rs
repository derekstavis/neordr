//! Framing only; every protocol DTO is the pinned upstream Herdr definition.
use anyhow::{Result, ensure};
use serde::{Serialize, de::DeserializeOwned};
use std::io::{Read, Write};

pub use crate::contracts::wire::*;
pub use crate::contracts::wire::{
    CellData as Cell, CursorState as Cursor, FrameData as Frame, PaneSurfaceFrame as Surface,
    PaneSurfacePane as Pane, SurfaceGraphicsScene as Graphics, SurfaceRect as Rect,
};

pub const MAX_FRAME: usize = MAX_FRAME_SIZE;
pub fn read(reader: &mut impl Read) -> Result<Vec<u8>> {
    let mut size = [0; 4];
    reader.read_exact(&mut size)?;
    let size = u32::from_le_bytes(size) as usize;
    ensure!(
        size > 0 && size <= MAX_FRAME,
        "invalid endpoint frame length"
    );
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let (value, used) = bincode::serde::decode_from_slice(
        bytes,
        bincode::config::standard().with_limit::<MAX_FRAME>(),
    )?;
    ensure!(used == bytes.len(), "trailing endpoint payload");
    Ok(value)
}
pub fn send(writer: &mut impl Write, message: &ServerMessage) -> Result<()> {
    let bytes = bincode::serde::encode_to_vec(message, bincode::config::standard())?;
    ensure!(
        bytes.len() <= MAX_FRAME,
        "endpoint surface exceeds frame limit"
    );
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(&bytes)?;
    Ok(())
}
pub fn control<T: Serialize>(writer: &mut impl Write, kind: &str, body: &T) -> Result<()> {
    send(
        writer,
        &ServerMessage::EndpointControl {
            kind: kind.to_owned(),
            data: serde_json::to_string(body)?,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Fragmented(Cursor<Vec<u8>>);
    impl Read for Fragmented {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let len = bytes.len().min(1);
            self.0.read(&mut bytes[..len])
        }
    }
    #[test]
    fn fragmented_control_and_trailing_data() {
        let mut bytes = vec![];
        let error = crate::contracts::endpoint::EndpointHandshakeError {
            code: "unsupported".into(),
            message: "unsupported generation".into(),
        };
        control(&mut bytes, "test.error", &error).unwrap();
        let frame = read(&mut Fragmented(Cursor::new(bytes))).unwrap();
        let ServerMessage::EndpointControl { kind, data } = decode(&frame).unwrap() else {
            panic!("expected endpoint control")
        };
        assert_eq!(kind, "test.error");
        assert_eq!(
            serde_json::from_str::<crate::contracts::endpoint::EndpointHandshakeError>(&data)
                .unwrap(),
            error
        );
        let mut bad = frame;
        bad.push(0);
        assert!(decode::<ServerMessage>(&bad).is_err());
    }
    #[test]
    fn rejects_oversized_and_truncated_frames() {
        assert!(read(&mut Cursor::new(((MAX_FRAME + 1) as u32).to_le_bytes())).is_err());
        assert!(read(&mut Cursor::new([3, 0, 0, 0, 20])).is_err());
        assert!(read(&mut Cursor::new([0, 0, 0, 0])).is_err());
    }
}
