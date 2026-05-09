# Tauri DuckDB Shared IPC Experiment

This experiment runs DuckDB natively in Rust and exposes a DuckDB-WASM-like JavaScript API over the forked Tauri shared IPC channel.

The important path is:

1. JavaScript writes a query request into a WebView2 shared IPC request buffer.
2. The shared-buffer plugin rings a tiny Tauri command doorbell with the channel id.
3. Rust executes the SQL against native DuckDB.
4. DuckDB returns Arrow `RecordBatch` values.
5. Rust serializes those batches as an Apache Arrow IPC stream directly into the shared IPC response buffer.
6. JavaScript reads the response bytes and returns an Apache Arrow JS `Table`.

On Windows this keeps query payloads and Arrow result bytes off normal JSON IPC.
On non-Windows, the transport uses a mmap-backed fallback channel in the plugin. It uses the same method contracts, with the same binary wire format and similar command surface, but with additional bridge hops for each request.

## API Shape

The frontend wrapper intentionally mirrors the common DuckDB-WASM flow:

```js
import { NativeAsyncDuckDB } from "./src/native-duckdb.js";

const db = await new NativeAsyncDuckDB().instantiate();
const conn = await db.connect();

await conn.send("CREATE TABLE t AS SELECT range AS id FROM range(10)");
const table = await conn.query("SELECT id, id * id AS square FROM t");
console.log(table.schema.fields.map((field) => field.name));
```

`conn.query(sql)` returns an Apache Arrow JS `Table` decoded from Arrow IPC stream bytes.

`conn.send(sql)` executes SQL that does not need to return Arrow batches and returns JSON metadata.

## Running

Install frontend dependencies:

```sh
npm install
```

Run the Tauri app:

```sh
npm run tauri dev
```

On Windows, this path uses native WebView2 buffers; on other platforms, it uses the mmap fallback path described above.

## Tests

Run the native bridge tests:

```sh
cargo test --manifest-path src-tauri/Cargo.toml
```

The tests verify:

- native DuckDB queries are returned as Arrow IPC stream bytes
- the shared IPC method accepts the JSON query contract
- `send` executes mutating SQL and exposes later query results
- SQL errors are propagated as shared IPC errors
