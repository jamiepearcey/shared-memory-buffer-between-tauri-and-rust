# tauri-plugin-shared-buffer

Cross-platform Tauri plugin that exposes host memory to JavaScript with:

- native WebView2 shared buffers on Windows
- mmap-backed channels on non-Windows

The exposed APIs and command contract stay the same across platforms.

Register the plugin:

```rust
use tauri_plugin_shared_buffer::SharedBufferExt;

tauri::Builder::default()
  .plugin(tauri_plugin_shared_buffer::init())
  .setup(|app| {
    app.register_shared_ipc_method("uppercase", |request| {
      Ok(request.payload.to_ascii_uppercase())
    });
    Ok(())
  })
  .run(tauri::generate_context!())?;
```

Create a shared-memory RPC channel from JavaScript:

```js
import { invoke } from "@tauri-apps/api/core";

window.__TAURI_WEBVIEW2_SHARED_IPC__.setInvoke(invoke);

const channel = await window.__TAURI_WEBVIEW2_SHARED_IPC__.createChannel({
  requestCapacity: 1024 * 1024,
  responseCapacity: 1024 * 1024
});

const response = await channel.invoke("uppercase", new TextEncoder().encode("hello"));
console.log(new TextDecoder().decode(response)); // HELLO
```

On Windows, the channel writes request frames into a WebView2 shared buffer, calls a tiny Tauri command as a doorbell, and reads response frames from a second shared buffer. The command payload carries the channel id only; method payload bytes stay in shared memory.

On non-Windows, the channel uses two file-backed mmap buffers and an invoke bridge for each call:

- request bytes are copied into the request buffer via `write_shared_buffer`
- the same `dispatch_shared_channel` command processes frames inside the same buffer format
- response bytes are read back through `read_shared_buffer`

This keeps binary payload shape in the same shared-frame format, while still using command calls for cross-process synchronization on platforms without WebView2 shared buffers.

Create and receive a shared buffer from JavaScript:

```js
window.addEventListener("tauri://webview2-shared-buffer", (event) => {
  const { id, buffer } = event.detail;
  const bytes = new Uint8Array(buffer);

  // Release the JS view when finished.
  window.__TAURI_WEBVIEW2_SHARED_BUFFER__.release(id);
});

await window.__TAURI__.core.invoke("plugin:webview2-shared-buffer|create_shared_buffer", {
  request: {
    size: 4096,
    readOnly: false,
    initialContents: [1, 2, 3, 4],
    additionalData: { kind: "example" }
  }
});
```

WebView2 delivers the shared memory to script as an `ArrayBuffer` backed by WebView2 shared memory, not as a JavaScript `SharedArrayBuffer` instance.

## Tests

Run correctness tests:

```sh
cargo test -p tauri-plugin-shared-buffer
```

Run the ignored performance baselines:

```sh
cargo test -p tauri-plugin-shared-buffer -- --ignored --nocapture
```

The performance tests compare shared-memory frame dispatch against JSON-style IPC payload encoding for small and large binary payloads.

Measured baseline:

| Scenario | Shared-memory frames | JSON-style IPC baseline |
| --- | ---: | ---: |
| Large binary payloads, 2,000 iterations, 125.0 MiB total | 16.763208 ms, 7456.8 MiB/s | 15.658740208 s, 8.0 MiB/s |
| Small binary payloads, 10,000 iterations, 2.4 MiB total | 16.249375 ms, 150.2 MiB/s | 384.971166 ms, 6.3 MiB/s |

Mmap fallback (non-Windows) in-process baseline:

| Scenario | Mmap fallback dispatch | JSON-style IPC baseline |
| --- | ---: | ---: |
| Large binary payloads, 2,000 iterations, 125.0 MiB total | 21.631042 ms, 5784.3 MiB/s | 19.874050333 s, 6.3 MiB/s |
| Small binary payloads, 10,000 iterations, 2.4 MiB total | 19.913041 ms, 120.5 MiB/s | 194.860917 ms, 12.3 MiB/s |

These numbers measure local frame dispatch and serialization overhead. They do not include a live WebView2 runtime or frontend event-loop timing.
