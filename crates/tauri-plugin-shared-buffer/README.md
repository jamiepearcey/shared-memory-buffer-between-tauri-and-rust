# tauri-plugin-shared-buffer

Windows-only Tauri plugin that uses WebView2 shared buffers to expose host memory to JavaScript as an `ArrayBuffer`.

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

The channel writes request frames into a WebView2 shared buffer, calls a tiny Tauri command as a doorbell, and reads response frames from a second shared buffer. The command payload carries the channel id only; method payload bytes stay in shared memory.

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
| Large binary payloads, 2,000 iterations, 125.0 MiB total | 14.316834 ms, 8731.0 MiB/s | 16.264259959 s, 7.7 MiB/s |
| Small binary payloads, 10,000 iterations, 2.4 MiB total | 15.827209 ms, 154.3 MiB/s | 369.489125 ms, 6.6 MiB/s |

These numbers measure local frame dispatch and serialization overhead. They do not include a live WebView2 runtime or frontend event-loop timing.
