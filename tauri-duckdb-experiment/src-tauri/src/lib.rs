mod native_duckdb;

use std::sync::Arc;

use native_duckdb::NativeDuckDb;
use tauri::Manager;
use tauri_plugin_shared_buffer::SharedBufferExt;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shared_buffer::init())
        .setup(|app| {
            let db = Arc::new(NativeDuckDb::new().map_err(|error| error.to_string())?);
            app.manage(db.clone());

            app.register_shared_ipc_method("duckdb.queryArrow", {
                let db = db.clone();
                move |request| db.handle_query_arrow(request.payload)
            });

            app.register_shared_ipc_method("duckdb.exec", move |request| {
                db.handle_exec(request.payload)
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Tauri DuckDB experiment");
}
