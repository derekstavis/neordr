//! Default-socket sidebar order spans editors without moving jobs between them.
use crate::{
    api,
    contracts::api::{
        EmptyParams, ErrorBody, Method, ResponseResult, WorkspaceInfo, WorkspaceMoveParams,
    },
    protocol, session,
};
use anyhow::{Context, Result};
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path, sync::Mutex};

static MOVES: Mutex<()> = Mutex::new(());

pub fn sort(config: &Path, rows: &mut [WorkspaceInfo]) -> Result<()> {
    let path = config.join("herdr/.neordr-order.json");
    let order: Vec<String> = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    rows.sort_by_key(|row| {
        order
            .iter()
            .position(|id| row.workspace_id == *id)
            .unwrap_or(usize::MAX)
    });
    for (index, row) in rows.iter_mut().enumerate() {
        row.number = index + 1;
    }
    Ok(())
}

pub fn move_workspace(
    config: &Path,
    request_id: &str,
    params: &WorkspaceMoveParams,
) -> Result<api::Reply> {
    let _guard = MOVES.lock().unwrap_or_else(|e| e.into_inner());
    let list = api::request(request_id, Method::WorkspaceList(EmptyParams {}));
    let reply = crate::router::execute(config, &list)?;
    let ResponseResult::WorkspaceList {
        workspaces: mut rows,
    } = reply.result
    else {
        anyhow::bail!("invalid workspace list");
    };
    let id = &params.workspace_id;
    let source = rows
        .iter()
        .position(|row| row.workspace_id == *id)
        .ok_or_else(|| {
            protocol::RemoteError(ErrorBody {
                code: "not_found".into(),
                message: "Unknown workspace".into(),
            })
        })?;
    let index = params.insert_index;
    if index > rows.len() {
        return Err(protocol::RemoteError(ErrorBody {
            code: "invalid_params".into(),
            message: "insert_index is out of bounds".into(),
        })
        .into());
    }
    let destination = if source < index { index - 1 } else { index };
    let (name, _) = id.split_once(':').context("missing session prefix")?;
    let prefix = format!("{name}:");
    let row = rows.remove(source);
    rows.insert(destination, row);
    let local_destination = rows[..destination]
        .iter()
        .filter(|row| row.workspace_id.starts_with(&prefix))
        .count();
    let socket = session::socket_path(config, name)?;
    session::inspect_bridge(&socket)?;
    let local = protocol::call_typed(&socket, &list)?;
    let ResponseResult::WorkspaceList {
        workspaces: local_rows,
    } = local.result
    else {
        anyhow::bail!("invalid local workspace list");
    };
    let local_source = local_rows
        .iter()
        .position(|row| row.workspace_id == *id)
        .context("workspace disappeared")?;
    protocol::call_typed(
        &socket,
        &api::request(
            request_id,
            Method::WorkspaceMove(WorkspaceMoveParams {
                workspace_id: id.clone(),
                insert_index: if local_source < local_destination {
                    local_destination + 1
                } else {
                    local_destination
                },
            }),
        ),
    )?;
    let order: Vec<_> = rows.iter().map(|row| &row.workspace_id).collect();
    let root = config.join("herdr");
    let temporary = root.join(format!(".neordr-order-{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| -> Result<()> {
        serde_json::to_writer(&mut file, &order)?;
        file.flush()?;
        fs::rename(&temporary, root.join(".neordr-order.json"))?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result?;
    Ok(ResponseResult::Ok {}.into())
}
