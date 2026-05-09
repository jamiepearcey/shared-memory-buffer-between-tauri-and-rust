use std::{io::Cursor, sync::Mutex, time::Instant};

use arrow_ipc::writer::StreamWriter;
use duckdb::{arrow::error::ArrowError, Connection};
use serde::{Deserialize, Serialize};
use tauri_plugin_shared_buffer::{Error as SharedIpcError, Result as SharedIpcResult};

#[derive(Debug, thiserror::Error)]
pub enum NativeDuckDbError {
    #[error("invalid request JSON: {0}")]
    InvalidRequest(#[from] serde_json::Error),
    #[error("duckdb error: {0}")]
    DuckDb(#[from] duckdb::Error),
    #[error("arrow IPC error: {0}")]
    Arrow(#[from] ArrowError),
    #[error("database lock is poisoned")]
    Poisoned,
}

type Result<T> = std::result::Result<T, NativeDuckDbError>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryRequest {
    sql: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecResponse {
    rows_changed: usize,
    elapsed_ms: f64,
}

pub struct NativeDuckDb {
    conn: Mutex<Connection>,
}

impl NativeDuckDb {
    pub fn new() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        seed_database(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn handle_query_arrow(&self, payload: &[u8]) -> SharedIpcResult<Vec<u8>> {
        self.query_arrow_from_payload(payload)
            .map_err(shared_ipc_error)
    }

    pub fn handle_exec(&self, payload: &[u8]) -> SharedIpcResult<Vec<u8>> {
        self.exec_from_payload(payload).map_err(shared_ipc_error)
    }

    pub fn query_arrow_ipc(&self, sql: &str) -> Result<Vec<u8>> {
        let conn = self.conn.lock().map_err(|_| NativeDuckDbError::Poisoned)?;
        let mut stmt = conn.prepare(sql)?;
        let mut arrow = stmt.query_arrow([])?;
        let schema = arrow.get_schema();
        let mut out = Vec::new();

        {
            let cursor = Cursor::new(&mut out);
            let mut writer = StreamWriter::try_new(cursor, &schema)?;
            for batch in &mut arrow {
                writer.write(&batch)?;
            }
            writer.finish()?;
        }

        Ok(out)
    }

    fn query_arrow_from_payload(&self, payload: &[u8]) -> Result<Vec<u8>> {
        let request: QueryRequest = serde_json::from_slice(payload)?;
        self.query_arrow_ipc(&request.sql)
    }

    fn exec_from_payload(&self, payload: &[u8]) -> Result<Vec<u8>> {
        let request: QueryRequest = serde_json::from_slice(payload)?;
        let started = Instant::now();
        let conn = self.conn.lock().map_err(|_| NativeDuckDbError::Poisoned)?;
        let rows_changed = conn.execute_batch(&request.sql).map(|_| 0)?;
        let response = ExecResponse {
            rows_changed,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        };
        Ok(serde_json::to_vec(&response)?)
    }
}

fn seed_database(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
    CREATE TABLE orders AS
    SELECT
      range::INTEGER AS order_id,
      CASE range % 4
        WHEN 0 THEN 'north'
        WHEN 1 THEN 'south'
        WHEN 2 THEN 'east'
        ELSE 'west'
      END AS customer_region,
      CASE range % 3
        WHEN 0 THEN 'open'
        WHEN 1 THEN 'paid'
        ELSE 'refunded'
      END AS order_status,
      CAST(20 + (range % 17) * 3.25 AS DOUBLE) AS total_amount
    FROM range(1, 5001);
    "#,
    )?;
    Ok(())
}

fn shared_ipc_error(error: NativeDuckDbError) -> SharedIpcError {
    SharedIpcError::WebView2(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_ipc::reader::StreamReader;

    #[test]
    fn query_arrow_ipc_returns_readable_arrow_stream() {
        let db = NativeDuckDb::new().unwrap();
        let bytes = db
      .query_arrow_ipc(
        "SELECT order_status, count(*) AS orders FROM orders GROUP BY order_status ORDER BY order_status",
      )
      .unwrap();

        let reader = StreamReader::try_new(Cursor::new(bytes), None).unwrap();
        let batches = reader.collect::<std::result::Result<Vec<_>, _>>().unwrap();

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_columns(), 2);
        assert_eq!(batches[0].num_rows(), 3);
        assert_eq!(batches[0].schema().field(0).name(), "order_status");
        assert_eq!(batches[0].schema().field(1).name(), "orders");
    }

    #[test]
    fn shared_ipc_query_contract_accepts_json_sql_payload() {
        let db = NativeDuckDb::new().unwrap();
        let payload = br#"{"sql":"SELECT 42::INTEGER AS answer"}"#;
        let bytes = db.handle_query_arrow(payload).unwrap();

        let reader = StreamReader::try_new(Cursor::new(bytes), None).unwrap();
        let batches = reader.collect::<std::result::Result<Vec<_>, _>>().unwrap();

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 1);
        assert_eq!(batches[0].schema().field(0).name(), "answer");
    }

    #[test]
    fn exec_contract_mutates_native_duckdb_state() {
        let db = NativeDuckDb::new().unwrap();

        db.handle_exec(br#"{"sql":"CREATE TABLE local_items AS SELECT 7::INTEGER AS id"}"#)
            .unwrap();

        let bytes = db
            .handle_query_arrow(br#"{"sql":"SELECT id FROM local_items"}"#)
            .unwrap();
        let reader = StreamReader::try_new(Cursor::new(bytes), None).unwrap();
        let batches = reader.collect::<std::result::Result<Vec<_>, _>>().unwrap();

        assert_eq!(batches[0].num_rows(), 1);
        assert_eq!(batches[0].schema().field(0).name(), "id");
    }

    #[test]
    fn sql_errors_are_reported_to_shared_ipc() {
        let db = NativeDuckDb::new().unwrap();
        let error = db
            .handle_query_arrow(br#"{"sql":"SELECT * FROM missing_table"}"#)
            .unwrap_err();

        assert!(error.to_string().contains("duckdb error"));
        assert!(error.to_string().contains("missing_table"));
    }
}
