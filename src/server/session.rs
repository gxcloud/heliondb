use std::sync::Arc;
use tokio::sync::Mutex;
use quinn::Connecting;
use tracing::{debug, error, info};

use crate::error::{HelionError, Result};
use crate::executor::ops::execute;
use crate::protocol::messages::{ClientMessage, ServerMessage};
use crate::sql::parser::parse;
use crate::sql::planner::plan;
use crate::storage::engine::DatabaseEngine;

/// Handle an incoming QUIC connection.
pub async fn handle_connection(
    connecting: Connecting,
    engine: Arc<Mutex<DatabaseEngine>>,
) -> Result<()> {
    let connection = connecting.await
        .map_err(|e| HelionError::Protocol(format!("Connection failed: {}", e)))?;

    let remote = connection.remote_address();
    info!("New connection from {}", remote);

    loop {
        match connection.accept_bi().await {
            Ok((mut send, mut recv)) => {
                let engine = engine.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_stream(&mut send, &mut recv, &engine).await {
                        error!("Stream error: {}", e);
                    }
                });
            }
            Err(quinn::ConnectionError::ApplicationClosed { .. }) => {
                debug!("Connection {} closed", remote);
                break;
            }
            Err(e) => {
                debug!("Connection {} error: {}", remote, e);
                break;
            }
        }
    }

    Ok(())
}

/// Handle a single bidirectional QUIC stream.
async fn handle_stream(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    engine: &Arc<Mutex<DatabaseEngine>>,
) -> Result<()> {
    // Read message length (4 bytes, big-endian)
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await
        .map_err(|e| HelionError::Protocol(format!("Read length error: {}", e)))?;
    let msg_len = u32::from_be_bytes(len_buf) as usize;

    // Read message payload
    let mut msg_buf = vec![0u8; msg_len];
    recv.read_exact(&mut msg_buf).await
        .map_err(|e| HelionError::Protocol(format!("Read message error: {}", e)))?;

    // Deserialize client message
    let client_msg: ClientMessage = bincode::deserialize(&msg_buf)
        .map_err(|e| HelionError::Protocol(format!("Deserialize error: {}", e)))?;

    // Process query
    let server_msg = process_message(client_msg, engine).await;

    // Serialize response
    let resp_bytes = bincode::serialize(&server_msg)
        .map_err(|e| HelionError::Protocol(format!("Serialize error: {}", e)))?;

    // Write response length + payload
    let resp_len = resp_bytes.len() as u32;
    send.write_all(&resp_len.to_be_bytes()).await
        .map_err(|e| HelionError::Protocol(format!("Write length error: {}", e)))?;
    send.write_all(&resp_bytes).await
        .map_err(|e| HelionError::Protocol(format!("Write response error: {}", e)))?;
    let _ = send.finish();

    Ok(())
}

async fn process_message(
    msg: ClientMessage,
    engine: &Arc<Mutex<DatabaseEngine>>,
) -> ServerMessage {
    match msg {
        ClientMessage::Query { sql } => {
            match execute_sql(&sql, engine).await {
                Ok(result) => ServerMessage::QueryResult {
                    columns: result.columns,
                    rows: result.rows,
                    error: None,
                },
                Err(e) => ServerMessage::Error {
                    message: e.to_string(),
                },
            }
        }
        ClientMessage::Prepare { sql } => {
            let id = simple_hash(&sql);
            match execute_sql(&sql, engine).await {
                Ok(_) => ServerMessage::Prepared { id },
                Err(e) => ServerMessage::Error { message: e.to_string() },
            }
        }
        ClientMessage::Execute { prepared_id, params } => {
            ServerMessage::QueryResult {
                columns: vec!["prepared_id".to_string()],
                rows: vec![vec![prepared_id.to_string()], params],
                error: None,
            }
        }
    }
}

async fn execute_sql(
    sql: &str,
    engine: &Arc<Mutex<DatabaseEngine>>,
) -> Result<crate::executor::ops::QueryResult> {
    let engine = engine.lock().await;
    let stmts = parse(sql)?;

    let mut last_result = None;
    for stmt in &stmts {
        let tables = engine.get_tables().await;
        let logical_plan = plan(stmt, &tables)?;
        let result = execute(&engine, &logical_plan).await?;
        last_result = Some(result);
    }

    last_result.ok_or_else(|| HelionError::Internal("No statements executed".into()))
}

fn simple_hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}
