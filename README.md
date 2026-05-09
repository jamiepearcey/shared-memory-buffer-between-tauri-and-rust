# Shared Memory Buffer Between Tauri and Rust

This repository contains patched `wry` and `tauri` sources for experimenting with Tauri shared-memory IPC.

The implementation supports Windows through native WebView2 shared buffers and supports a non-Windows mmap-backed fallback channel for command-level interoperability.

## DuckDB Experiment

`tauri-duckdb-experiment/` contains a sample Tauri app that loads DuckDB natively in Rust and exposes a DuckDB-WASM-like JavaScript API over the shared IPC channel. Query results are serialized as Apache Arrow IPC stream bytes in Rust and decoded as Apache Arrow JS `Table` objects in the frontend.

Run its native bridge tests with:

```sh
cargo test --manifest-path tauri-duckdb-experiment/src-tauri/Cargo.toml
```

## Performance Baseline

Measured with:

```sh
cargo test -p tauri-plugin-shared-buffer -- --ignored --nocapture
```

| Scenario | Shared-memory frames | JSON-style IPC baseline |
| --- | ---: | ---: |
| Large binary payloads, 2,000 iterations, 125.0 MiB total | 16.763208 ms, 7456.8 MiB/s | 15.658740208 s, 8.0 MiB/s |
| Small binary payloads, 10,000 iterations, 2.4 MiB total | 16.249375 ms, 150.2 MiB/s | 384.971166 ms, 6.3 MiB/s |

Mmap fallback baseline (non-Windows):

| Scenario | Mmap fallback dispatch | JSON-style IPC baseline |
| --- | ---: | ---: |
| Large binary payloads, 2,000 iterations, 125.0 MiB total | 21.631042 ms, 5784.3 MiB/s | 19.874050333 s, 6.3 MiB/s |
| Small binary payloads, 10,000 iterations, 2.4 MiB total | 19.913041 ms, 120.5 MiB/s | 194.860917 ms, 12.3 MiB/s |

These are local frame/serialization baselines. They do not include a live WebView2 runtime, window creation, or end-to-end frontend event-loop timing.

## Test Commands

```sh
cd tauri
cargo check -p tauri-plugin-shared-buffer
cargo test -p tauri-plugin-shared-buffer
cargo test -p tauri-plugin-shared-buffer -- --ignored --nocapture
cargo check --target x86_64-pc-windows-msvc -p tauri-plugin-shared-buffer
```
