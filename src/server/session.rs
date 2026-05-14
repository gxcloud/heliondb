use quinn::Connecting;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::error::{HelionError, Result};
use crate::executor::ops::execute_as;
use crate::protocol::auth::SessionManager;
use crate::protocol::messages::{ClientMessage, ServerMessage};
use crate::sql::parser::parse;
use crate::sql::planner::plan;
use crate::storage::engine::DatabaseEngine;

pub type DatabaseMap = HashMap<String, Arc<DatabaseEngine>>;
type PreparedCache = HashMap<u64, String>;

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

    // Extract TLS peer certificate (DER bytes of the end-entity cert)
    let peer_cert_der: Option<Vec<u8>> = connection
        .peer_identity()
        .and_then(|id| id.downcast::<Vec<Vec<u8>>>().ok())
        .and_then(|chain| chain.into_iter().next());

    let sessions = Arc::new(SessionManager::new());
    let prepared_cache: Arc<tokio::sync::Mutex<PreparedCache>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    loop {
        match connection.accept_bi().await {
            Ok((mut send, mut recv)) => {
                let databases = databases.clone();
                let default_db = default_database.clone();
                let sessions = sessions.clone();
                let prepared = prepared_cache.clone();
                let peer_cert = peer_cert_der.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_stream(
                        &mut send,
                        &mut recv,
                        &databases,
                        &default_db,
                        &sessions,
                        &prepared,
                        peer_cert,
                    )
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

/// Extract the Common Name from a DER-encoded X.509 certificate.
fn extract_cn_from_der(der: &[u8]) -> Option<String> {
    use x509_parser::prelude::*;
    let (_, cert) = X509Certificate::from_der(der).ok()?;
    for attr in cert.subject().iter_attributes() {
        if attr.attr_type().to_string().contains("CN") {
            if let Ok(value) = attr.as_str() {
                let trimmed = value.trim().to_string();
                if !trimmed.is_empty() && trimmed.len() <= 255 {
                    return Some(trimmed);
                }
            }
        }
    }
    None
}

async fn handle_stream(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    databases: &DatabaseMap,
    default_database: &str,
    sessions: &Arc<SessionManager>,
    prepared: &Arc<tokio::sync::Mutex<HashMap<u64, String>>>,
    peer_cert_der: Option<Vec<u8>>,
) -> Result<()> {
    let msg = read_client_message(send, recv).await?;

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

            // Determine username and authenticate
            let (authenticated_username, authenticated) = if let Some(ref cert_der) = peer_cert_der
            {
                // Try certificate-based authentication
                if let Some(cn) = extract_cn_from_der(cert_der) {
                    if engine.user_exists(&cn).await {
                        info!("User '{}' authenticated via TLS client certificate", cn);
                        (cn, true)
                    } else {
                        warn!("Certificate CN '{}' does not match any database user", cn);
                        // Fall through to password auth
                        (
                            username.clone(),
                            engine.verify_user(username, password).await,
                        )
                    }
                } else {
                    // No CN in certificate — fall through to password auth
                    (
                        username.clone(),
                        engine.verify_user(username, password).await,
                    )
                }
            } else {
                // No client certificate — password auth only
                (
                    username.clone(),
                    engine.verify_user(username, password).await,
                )
            };

            if authenticated {
                let token = sessions.create_session(&authenticated_username);
                info!(
                    "User '{}' authenticated to database '{}'",
                    authenticated_username, db_name
                );
                send_auth_response(send, true, token, None).await?;
                (authenticated_username, token, engine.clone())
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
                let server_msg = process_authenticated_message(
                    &msg,
                    engine.as_ref(),
                    sessions,
                    prepared,
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

        let server_msg = process_authenticated_message(
            &msg,
            engine.as_ref(),
            sessions,
            prepared,
            &token,
            &username,
        )
        .await;
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
    prepared: &Arc<tokio::sync::Mutex<HashMap<u64, String>>>,
    token: &u64,
    username: &str,
) -> ServerMessage {
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
        ClientMessage::Query { sql, .. } => {
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
        ClientMessage::Prepare { sql, .. } => {
            // Prepare: parse+plan and cache
            let prepared_id = simple_hash(sql);
            let mut cache = prepared.lock().await;
            cache.insert(prepared_id, sql.clone());
            drop(cache);
            ServerMessage::Prepared { id: prepared_id }
        }
        ClientMessage::Execute {
            prepared_id,
            params,
            ..
        } => {
            // Execute: look up cached SQL, substitute $N
            let cache = prepared.lock().await;
            match cache.get(prepared_id) {
                Some(original_sql) => {
                    let sql = substitute_params(original_sql, params);
                    drop(cache);
                    match execute_sql_as(&sql, engine, Some(username)).await {
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
                None => ServerMessage::Error {
                    message: format!("Prepared statement {} not found", prepared_id),
                },
            }
        }
        ClientMessage::StructuredQuery { query_json, .. } => {
            match execute_structured_sql(query_json, engine, Some(username)).await {
                Ok(data_json) => ServerMessage::StructuredResult {
                    data_json,
                    error: None,
                },
                Err(e) => ServerMessage::Error {
                    message: e.to_string(),
                },
            }
        }
        ClientMessage::Auth { .. } => ServerMessage::Error {
            message: "Already authenticated".into(),
        },
    }
}

/// Simple string hash for generating prepared statement IDs.
fn simple_hash(s: &str) -> u64 {
    let mut h: u64 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u64);
    }
    h
}

/// Substitute $1, $2, ... placeholders with string values from params.
/// Performs simple string replacement. Values are quoted if they contain
/// spaces or special characters, otherwise inserted as literals.
fn substitute_params(sql: &str, params: &[String]) -> String {
    let mut result = sql.to_string();
    for (i, param) in params.iter().enumerate() {
        let placeholder = format!("${}", i + 1);
        let needs_quoting = param.is_empty()
            || param.contains(' ')
            || param.contains('\'')
            || param.contains(',')
            || param.contains(';');
        let value = if needs_quoting {
            format!("'{}'", param.replace('\'', "''"))
        } else {
            param.clone()
        };
        result = result.replace(&placeholder, &value);
    }
    result
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

        // Audit log DDL statements
        if let Some(user) = username {
            let action = match stmt {
                crate::sql::parser::HelionStatement::CreateTable { name, .. } => {
                    Some(format!("CREATE TABLE {}", name))
                }
                crate::sql::parser::HelionStatement::DropTable { name, .. } => {
                    Some(format!("DROP TABLE {}", name))
                }
                crate::sql::parser::HelionStatement::AlterTableEngine { name, .. } => {
                    Some(format!("ALTER TABLE {} ENGINE", name))
                }
                crate::sql::parser::HelionStatement::CreateUser { username, .. } => {
                    Some(format!("CREATE USER {}", username))
                }
                crate::sql::parser::HelionStatement::DropUser { username, .. } => {
                    Some(format!("DROP USER {}", username))
                }
                crate::sql::parser::HelionStatement::AlterUser { username, .. } => {
                    Some(format!("ALTER USER {}", username))
                }
                crate::sql::parser::HelionStatement::Grant {
                    username, table, ..
                } => Some(format!("GRANT ON {} TO {}", table, username)),
                crate::sql::parser::HelionStatement::Revoke {
                    username, table, ..
                } => Some(format!("REVOKE ON {} FROM {}", table, username)),
                crate::sql::parser::HelionStatement::CreateIndex { name, table, .. } => {
                    Some(format!("CREATE INDEX {} ON {}", name, table))
                }
                crate::sql::parser::HelionStatement::DropIndex { name, table, .. } => {
                    Some(format!("DROP INDEX {} ON {}", name, table))
                }
                _ => None,
            };
            if let Some(action) = action {
                info!(
                    target: "audit",
                    "user={} action=\"{}\"",
                    user, action
                );
            }
        }

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

async fn execute_structured_sql(
    query_json: &str,
    engine: &DatabaseEngine,
    username: Option<&str>,
) -> std::result::Result<String, HelionError> {
    use crate::protocol::structured::{execute_structured, StructuredQuery};
    let query: StructuredQuery = serde_json::from_str(query_json)
        .map_err(|e| HelionError::Parse(format!("Structured query parse error: {}", e)))?;
    execute_structured(engine, &query, username).await
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
