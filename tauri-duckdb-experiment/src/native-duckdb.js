import { invoke } from "@tauri-apps/api/core";
import { tableFromIPC } from "apache-arrow";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export class NativeAsyncDuckDB {
  constructor(options = {}) {
    this.requestCapacity = options.requestCapacity ?? 4 * 1024 * 1024;
    this.responseCapacity = options.responseCapacity ?? 64 * 1024 * 1024;
    this.channel = null;
  }

  async instantiate() {
    const sharedIpc = getSharedIpc();
    sharedIpc.setInvoke(invoke);
    this.channel = await sharedIpc.createChannel({
      requestCapacity: this.requestCapacity,
      responseCapacity: this.responseCapacity
    });
    return this;
  }

  async connect() {
    if (!this.channel) {
      await this.instantiate();
    }
    return new NativeDuckDBConnection(this.channel);
  }
}

export class NativeDuckDBConnection {
  constructor(channel) {
    this.channel = channel;
  }

  async query(sql) {
    const response = await this.channel.invoke("duckdb.queryArrow", encodeJson({ sql }));
    const stableBytes = new Uint8Array(response.byteLength);
    stableBytes.set(response);
    return tableFromIPC(stableBytes);
  }

  async send(sql) {
    const response = await this.channel.invoke("duckdb.exec", encodeJson({ sql }));
    return JSON.parse(decoder.decode(response));
  }

  async close() {
    return undefined;
  }
}

function getSharedIpc() {
  const sharedIpc = window.__TAURI_WEBVIEW2_SHARED_IPC__;
  if (!sharedIpc) {
    throw new Error("Tauri WebView2 shared IPC is unavailable");
  }
  return sharedIpc;
}

function encodeJson(value) {
  return encoder.encode(JSON.stringify(value));
}

