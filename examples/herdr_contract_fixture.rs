//! Test driver using the exact upstream types, not a second wire schema.
//! Input/output are newline-delimited serde JSON; surfaces are decoded but not
//! printed, keeping stdout suitable for deterministic integration assertions.
use anyhow::{Context, Result, ensure};
use neordr::contracts::wire::{ClientMessage, ServerMessage};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

fn main() -> Result<()> {
    let address = std::env::args()
        .nth(1)
        .context("expected endpoint socket")?;
    let mut stream = UnixStream::connect(address)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    std::thread::spawn(move || {
        let mut receive = || -> Result<()> {
            loop {
                let mut prefix = [0; 4];
                match reader.read_exact(&mut prefix) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                    Err(error) => return Err(error.into()),
                }
                let len = u32::from_le_bytes(prefix) as usize;
                ensure!(len <= 2 * 1024 * 1024, "oversized server frame");
                let mut bytes = vec![0; len];
                reader.read_exact(&mut bytes)?;
                let (message, used): (ServerMessage, usize) =
                    bincode::serde::decode_from_slice(&bytes, bincode::config::standard())?;
                ensure!(used == bytes.len(), "trailing server frame bytes");
                match message {
                    ServerMessage::EndpointControl { .. }
                    | ServerMessage::ClientShellEndpointResponseChunk { .. } => {
                        println!("{}", serde_json::to_string(&message)?);
                        io::stdout().flush()?;
                    }
                    _ => {}
                }
            }
        };
        if let Err(error) = receive() {
            eprintln!("native fixture: {error:#}");
            std::process::exit(1);
        }
    });
    for line in io::stdin().lock().lines() {
        let message: ClientMessage = serde_json::from_str(&line?)?;
        let bytes = bincode::serde::encode_to_vec(message, bincode::config::standard())?;
        ensure!(bytes.len() <= 2 * 1024 * 1024, "oversized client frame");
        stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
        stream.write_all(&bytes)?;
        stream.flush()?;
    }
    Ok(())
}
