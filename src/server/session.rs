use quinn::Connecting;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::error::{HelionError, Result};
use crate::executor::ops::execute_as;
use crate::protocol::auth::SessionManager;
use crate::protocol::messages::{ClientMessage, ServerMessage};
use crate::sql::parser::parse;
use crate::sql::planner::plan;
use crate::storage::engine::DatabaseEngine;

/// Map of database name to database engine.
pub type DatabaseMap = std::collections::HashMap<String, Arc<DatabaseEngine>>;

/// Handle an incoming QUIC connection.
pub async fn handle_connection(
    connecting: Connecting,
    databases: Arc<DatabaseMap>,
    default_database: String,
) -> Result<()> {
    let connection = connecting
        .await
        .map_err(|e| HelionError::Protocol(format!("Connection failed: {}", e)))?;

    let remote = connection.remote_address();
    info!("New connection from {}", remote);

    let sessions = Arc::new(SessionManager::new());

    loop {
        match connection.accept_bi().await {
            Ok((mut send, mut recv)) => {
                let databases = databases.clone();
                let default_db = default_database.clone();
                let sessions = sessions.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        handle_stream(&mut send, &mut recv, &databases, &default_db, &sessions)
                            .await
                    {
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

async fn handle_stream(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    databases: &DatabaseMap,
    default_database: &str,
    sessions: &Arc<SessionManager>,
) -> Result<()> {
    let msg = read_client_message(send, recv).await?;

    // Handle auth — extract database, select the right engine
    let (username, token, mut engine) = match &msg {
        ClientMessage::Auth {
            username,
            password,
            database,
        } => {
            let db_name = if database.is_empty() {
                default_database
            } else {
                database.as_str()
            };
            let engine = databases.get(db_name).ok_or_else(|| {
                HelionError::Protocol(format!("Database '{}' not found", db_name))
            })?;
            if engine.verify_user(username, password).await {
                let token = sessions.create_session(username);
                info!(
                    "User '{}' authenticated to database '{}'",
                    username, db_name
                );
                send_auth_response(send, true, token, None).await?;
                (username.clone(), token, engine.clone())
            } else {
                warn!("Authentication failed for user '{}'", username);
                send_auth_response(send, false, 0, Some("Invalid credentials".into())).await?;
                return Ok(());
            }
        }
        _ => {
            send_error(send, "Authentication required. Send Auth message first.").await?;
            return Ok(());
        }
    };

    // Process subsequent messages on this stream
    loop {
        let msg = match read_client_message_raw(recv).await {
            Ok(m) => m,
            Err(e) => {
                debug!("Stream read error: {}", e);
                break;
            }
        };

        let sql = match msg {
            ClientMessage::Query { ref sql, .. } => sql.clone(),
            _ => {
                // Non-Query messages are handled normally
                let server_msg = process_authenticated_message(
                    &msg,
                    engine.as_ref(),
                    sessions,
                    &token,
                    &username,
                )
                .await;
                send_server_message(send, &server_msg).await?;
                continue;
            }
        };

        let sql_trimmed = sql.trim().trim_end_matches(';').trim().to_string();
        let upper = sql_trimmed.to_uppercase();

        // SHOW DATABASES — needs the database map
        if upper == "SHOW DATABASES" {
            let mut db_names: Vec<String> = databases.keys().cloned().collect();
            db_names.sort();
            let rows: Vec<Vec<String>> = db_names.iter().map(|n| vec![n.clone()]).collect();
            send_server_message(
                send,
                &ServerMessage::QueryResult {
                    columns: vec!["database_name".into()],
                    rows,
                    rows_affected: db_names.len() as u64,
                    error: None,
                },
            )
            .await?;
            continue;
        }

        // USE database — validate at session layer, then swap engine after execution
        let db_name = if upper.starts_with("USE ") {
            let name = sql_trimmed[3..].trim().to_string();
            if !databases.contains_key(&name) {
                send_server_message(
                    send,
                    &ServerMessage::Error {
                        message: format!("Database '{}' not found", name),
                    },
                )
                .await?;
                continue;
            }
            Some(name)
        } else {
            None
        };

        let server_msg =
            process_authenticated_message(&msg, engine.as_ref(), sessions, &token, &username).await;
        send_server_message(send, &server_msg).await?;

        if let Some(ref name) = db_name {
            if let Some(new_engine) = databases.get(name) {
                engine = new_engine.clone();
                info!("User '{}' switched to database '{}'", username, name);
            }
        }
    }

    Ok(())
}

async fn process_authenticated_message(
    msg: &ClientMessage,
    engine: &DatabaseEngine,
    sessions: &SessionManager,
    token: &u64,
    username: &str,
) -> ServerMessage {
    // Verify token
    if sessions
        .verify_token(*token)
        .map(|u| u != username)
        .unwrap_or(true)
    {
        return ServerMessage::Error {
            message: "Invalid session token".into(),
        };
    }

    match msg {
        ClientMessage::Query { sql, .. } | ClientMessage::Prepare { sql, .. } => {
            match execute_sql_as(sql, engine, Some(username)).await {
                Ok(result) => ServerMessage::QueryResult {
                    columns: result.columns,
                    rows: result.rows,
                    rows_affected: result.rows_affected,
                    error: None,
                },
                Err(e) => ServerMessage::Error {
                    message: e.to_string(),
                },
            }
        }
        ClientMessage::Execute { .. } => ServerMessage::QueryResult {
            columns: vec![],
            rows: vec![],
            rows_affected: 0,
            error: None,
        },
        ClientMessage::Auth { .. } => ServerMessage::Error {
            message: "Already authenticated".into(),
        },
    }
}

async fn execute_sql_as(
    sql: &str,
    engine: &DatabaseEngine,
    username: Option<&str>,
) -> Result<crate::executor::ops::QueryResult> {
    let stmts = parse(sql)?;
    let mut last_result = None;
    for stmt in &stmts {
        let tables = engine.get_tables().await;
        let logical_plan = plan(stmt, &tables)?;
        let result = execute_as(engine, &logical_plan, username).await?;
        last_result = Some(result);
    }
    last_result.ok_or_else(|| HelionError::Internal("No statements executed".into()))
}

// ── Protocol I/O ──────────────────────────────────────────────────────

async fn read_client_message(
    _send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
) -> Result<ClientMessage> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| HelionError::Protocol(format!("Read length error: {}", e)))?;
    let msg_len = u32::from_be_bytes(len_buf) as usize;

    let mut buf = vec![0u8; msg_len];
    if msg_len > 0 {
        recv.read_exact(&mut buf)
            .await
            .map_err(|e| HelionError::Protocol(format!("Read message error: {}", e)))?;
    }

    bincode::deserialize(&buf)
        .map_err(|e| HelionError::Protocol(format!("Deserialize error: {}", e)))
}

async fn read_client_message_raw(recv: &mut quinn::RecvStream) -> Result<ClientMessage> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| HelionError::Protocol(format!("Read length error: {}", e)))?;
    let msg_len = u32::from_be_bytes(len_buf) as usize;

    let mut buf = vec![0u8; msg_len];
    if msg_len > 0 {
        recv.read_exact(&mut buf)
            .await
            .map_err(|e| HelionError::Protocol(format!("Read message error: {}", e)))?;
    }

    bincode::deserialize(&buf)
        .map_err(|e| HelionError::Protocol(format!("Deserialize error: {}", e)))
}

async fn send_auth_response(
    send: &mut quinn::SendStream,
    success: bool,
    token: u64,
    error: Option<String>,
) -> Result<()> {
    let msg = ServerMessage::AuthResult {
        success,
        token,
        error,
    };
    send_server_message(send, &msg).await
}

async fn send_error(send: &mut quinn::SendStream, message: &str) -> Result<()> {
    let msg = ServerMessage::Error {
        message: message.to_string(),
    };
    send_server_message(send, &msg).await
}

async fn send_server_message(send: &mut quinn::SendStream, msg: &ServerMessage) -> Result<()> {
    let bytes = bincode::serialize(msg)
        .map_err(|e| HelionError::Protocol(format!("Serialize error: {}", e)))?;
    let len = bytes.len() as u32;
    send.write_all(&len.to_be_bytes())
        .await
        .map_err(|e| HelionError::Protocol(format!("Write error: {}", e)))?;
    send.write_all(&bytes)
        .await
        .map_err(|e| HelionError::Protocol(format!("Write error: {}", e)))?;
    Ok(())
}
