# Shared Memory Buffer Between Tauri and Rust

This repository contains patched `wry` and `tauri` sources for experimenting with WebView2 shared-memory buffers from Tauri plugins.

The current implementation is Windows-only and uses WebView2 shared buffers to expose host memory to JavaScript as an `ArrayBuffer`. It also adds a shared-memory RPC layer where normal Tauri IPC is used only as a small doorbell while method payload bytes stay in shared memory.

## Performance Baseline

Measured with:

```sh
cargo test -p tauri-plugin-shared-buffer -- --ignored --nocapture
```

| Scenario | Shared-memory frames | JSON-style IPC baseline |
| --- | ---: | ---: |
| Large binary payloads, 2,000 iterations, 125.0 MiB total | 14.316834 ms, 8731.0 MiB/s | 16.264259959 s, 7.7 MiB/s |
| Small binary payloads, 10,000 iterations, 2.4 MiB total | 15.827209 ms, 154.3 MiB/s | 369.489125 ms, 6.6 MiB/s |

These are local frame/serialization baselines. They do not include a live WebView2 runtime, window creation, or end-to-end frontend event-loop timing.

## Test Commands

```sh
cd tauri
cargo check -p tauri-plugin-shared-buffer
cargo test -p tauri-plugin-shared-buffer
cargo test -p tauri-plugin-shared-buffer -- --ignored --nocapture
cargo check --target x86_64-pc-windows-msvc -p tauri-plugin-shared-buffer
```
