// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{
  plugin::{Builder, TauriPlugin},
  Manager, Runtime, State, WebviewWindow,
};

mod shared_ipc;

const DEFAULT_CHANNEL_CAPACITY: u64 = 1024 * 1024;

const INIT_SCRIPT: &str = r#"
  const textEncoder = new TextEncoder();
  const textDecoder = new TextDecoder();
  const BUFFER_HEADER_SIZE = 16;
  const FRAME_HEADER_SIZE = 32;
  const FRAME_KIND_REQUEST = 1;
  const FRAME_KIND_RESPONSE = 2;

  const sharedBufferApi = window.__TAURI_WEBVIEW2_SHARED_BUFFER__ || {
    buffers: new Map(),
    get(id) {
      return this.buffers.get(Number(id));
    },
    release(id) {
      const key = Number(id);
      const buffer = this.buffers.get(key);
      if (buffer && window.chrome && window.chrome.webview) {
        window.chrome.webview.releaseBuffer(buffer);
      }
      this.buffers.delete(key);
    }
  };

  const sharedIpcApi = window.__TAURI_WEBVIEW2_SHARED_IPC__ || {
    channels: new Map(),
    waiters: new Map(),
    nextRequestId: 1,
    invokeImpl: null,
    isWebView2: !!(window.chrome && window.chrome.webview),
    setInvoke(invoke) {
      this.invokeImpl = invoke;
    },
    async createChannel(options = {}) {
      const invoke = this.invokeImpl || getTauriInvoke();
      const info = await invoke("plugin:webview2-shared-buffer|create_shared_channel", {
        request: {
          requestCapacity: options.requestCapacity || 1048576,
          responseCapacity: options.responseCapacity || 1048576
        }
      });
      if (!sharedIpcApi.isWebView2 && info.requestBufferId && info.requestBufferId !== 0 && info.responseBufferId && info.responseBufferId !== 0) {
        const requestBuffer = new ArrayBuffer(info.requestCapacity);
        const responseBuffer = new ArrayBuffer(info.responseCapacity);
        const requestView = new DataView(requestBuffer);
        const responseView = new DataView(responseBuffer);
        requestView.setUint32(8, 16, true);
        responseView.setUint32(8, 16, true);
        const channel = new SharedIpcChannel(
          info.id,
          requestBuffer,
          responseBuffer,
          info.requestCapacity,
          info.responseCapacity,
          {
            requestBufferId: info.requestBufferId,
            responseBufferId: info.responseBufferId,
            requestPath: info.requestPath,
            responsePath: info.responsePath,
            useInvokeBridge: true
          }
        );
        sharedIpcApi.channels.set(info.id, channel);
        window.dispatchEvent(new CustomEvent("tauri://webview2-shared-ipc-channel", {
          detail: { id: info.id, channel }
        }));
        return channel;
      }
      if (!sharedIpcApi.isWebView2) {
        throw new Error("WebView2 shared IPC channel fallback metadata was not returned by the plugin");
      }
      return await waitForChannel(info.id);
    },
    getChannel(id) {
      return this.channels.get(Number(id));
    }
  };

  window.__TAURI_WEBVIEW2_SHARED_BUFFER__ = sharedBufferApi;
  window.__TAURI_WEBVIEW2_SHARED_IPC__ = sharedIpcApi;

  class SharedIpcChannel {
    constructor(id, requestBuffer, responseBuffer, requestCapacity, responseCapacity, nativeMetadata = null) {
      this.id = id;
      this.requestBuffer = requestBuffer;
      this.responseBuffer = responseBuffer;
      this.requestCapacity = requestCapacity;
      this.responseCapacity = responseCapacity;
      this.requestView = new DataView(requestBuffer);
      this.responseView = new DataView(responseBuffer);
      this.requestBytes = new Uint8Array(requestBuffer);
      this.responseBytes = new Uint8Array(responseBuffer);
      this.queue = Promise.resolve();
      this.nativeMetadata = nativeMetadata;
    }

    invoke(method, payload) {
      const run = async () => {
        const requestId = sharedIpcApi.nextRequestId++;
        if (sharedIpcApi.nextRequestId > 0x7fffffff) {
          sharedIpcApi.nextRequestId = 1;
        }

        writeRequestFrame(this, requestId, method, payload);
        if (this.nativeMetadata && this.nativeMetadata.useInvokeBridge) {
          const requestWriteOffset = this.requestView.getUint32(8, true);
          await invoke("plugin:webview2-shared-buffer|write_shared_buffer", {
            id: this.nativeMetadata.requestBufferId,
            offset: 0,
            bytes: this.requestBytes.subarray(0, requestWriteOffset)
          });
        }

        const invoke = sharedIpcApi.invokeImpl || getTauriInvoke();
        const result = await invoke("plugin:webview2-shared-buffer|dispatch_shared_channel", {
          channelId: this.id
        });
        if (this.nativeMetadata && this.nativeMetadata.useInvokeBridge) {
          const responseBytes = await invoke("plugin:webview2-shared-buffer|read_shared_buffer", {
            id: this.nativeMetadata.responseBufferId,
            offset: 0,
            length: result.responseWriteOffset
          });
          this.responseBytes = new Uint8Array(responseBytes);
          this.responseView = new DataView(this.responseBytes.buffer);
        }
        return readResponseFrame(this, requestId, result.responseWriteOffset);
      };

      const next = this.queue.then(run, run);
      this.queue = next.catch(() => {});
      return next;
    }
  }

  function getTauriInvoke() {
    const invoke = window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke;
    if (typeof invoke === "function") {
      return invoke;
    }
    throw new Error("Tauri invoke function is unavailable; call __TAURI_WEBVIEW2_SHARED_IPC__.setInvoke(invoke)");
  }

  function align8(value) {
    return (value + 7) & ~7;
  }

  function payloadBytes(payload) {
    if (payload == null) {
      return new Uint8Array(0);
    }
    if (payload instanceof ArrayBuffer) {
      return new Uint8Array(payload);
    }
    if (ArrayBuffer.isView(payload)) {
      return new Uint8Array(payload.buffer, payload.byteOffset, payload.byteLength);
    }
    if (typeof payload === "string") {
      return textEncoder.encode(payload);
    }
    return textEncoder.encode(JSON.stringify(payload));
  }

  function writeRequestFrame(channel, requestId, method, payload) {
    const methodBytes = textEncoder.encode(method);
    const bytes = payloadBytes(payload);
    const frameLen = align8(FRAME_HEADER_SIZE + methodBytes.byteLength + bytes.byteLength);
    let writeOffset = channel.requestView.getUint32(8, true);

    if (writeOffset < BUFFER_HEADER_SIZE || writeOffset > channel.requestBuffer.byteLength) {
      writeOffset = BUFFER_HEADER_SIZE;
    }
    if (writeOffset + frameLen > channel.requestBuffer.byteLength) {
      writeOffset = BUFFER_HEADER_SIZE;
    }
    if (writeOffset + frameLen > channel.requestBuffer.byteLength) {
      throw new Error("shared IPC request buffer is too small for this payload");
    }

    const view = channel.requestView;
    view.setUint32(writeOffset, frameLen, true);
    view.setUint32(writeOffset + 4, FRAME_KIND_REQUEST, true);
    view.setUint32(writeOffset + 8, requestId, true);
    view.setUint32(writeOffset + 12, methodBytes.byteLength, true);
    view.setUint32(writeOffset + 16, bytes.byteLength, true);
    view.setInt32(writeOffset + 20, 0, true);
    view.setUint32(writeOffset + 24, 0, true);
    view.setUint32(writeOffset + 28, 0, true);

    let cursor = writeOffset + FRAME_HEADER_SIZE;
    channel.requestBytes.set(methodBytes, cursor);
    cursor += methodBytes.byteLength;
    channel.requestBytes.set(bytes, cursor);
    channel.requestView.setUint32(8, writeOffset + frameLen, true);
  }

  function readResponseFrame(channel, requestId, responseWriteOffset) {
    let cursor = BUFFER_HEADER_SIZE;
    const end = responseWriteOffset || channel.responseView.getUint32(8, true);

    while (cursor + FRAME_HEADER_SIZE <= end) {
      const frameLen = channel.responseView.getUint32(cursor, true);
      const kind = channel.responseView.getUint32(cursor + 4, true);
      const id = channel.responseView.getUint32(cursor + 8, true);
      const payloadLen = channel.responseView.getUint32(cursor + 16, true);
      const status = channel.responseView.getInt32(cursor + 20, true);

      if (frameLen < FRAME_HEADER_SIZE || cursor + frameLen > channel.responseBuffer.byteLength) {
        throw new Error("invalid shared IPC response frame");
      }

      if (kind === FRAME_KIND_RESPONSE && id === requestId) {
        const start = cursor + FRAME_HEADER_SIZE;
        const payload = channel.responseBytes.subarray(start, start + payloadLen);
        if (status !== 0) {
          throw new Error(textDecoder.decode(payload));
        }
        return payload;
      }

      cursor += frameLen;
    }

    throw new Error(`missing shared IPC response for request ${requestId}`);
  }

  function waitForChannel(id) {
    const channelId = Number(id);
    const existing = sharedIpcApi.channels.get(channelId);
    if (existing instanceof SharedIpcChannel) {
      return Promise.resolve(existing);
    }
    return new Promise((resolve) => {
      const waiters = sharedIpcApi.waiters.get(channelId) || [];
      waiters.push(resolve);
      sharedIpcApi.waiters.set(channelId, waiters);
      maybeResolveChannel(channelId);
    });
  }

  function maybeResolveChannel(channelId) {
    const entry = sharedIpcApi.channels.get(channelId);
    if (!entry || entry instanceof SharedIpcChannel || !entry.request || !entry.response) {
      return;
    }

    const channel = new SharedIpcChannel(
      channelId,
      entry.request.buffer,
      entry.response.buffer,
      entry.request.capacity,
      entry.response.capacity
    );
    sharedIpcApi.channels.set(channelId, channel);

    const waiters = sharedIpcApi.waiters.get(channelId) || [];
    sharedIpcApi.waiters.delete(channelId);
    for (const resolve of waiters) {
      resolve(channel);
    }

    window.dispatchEvent(new CustomEvent("tauri://webview2-shared-ipc-channel", {
      detail: { id: channelId, channel }
    }));
  }

  if (window.chrome && window.chrome.webview && !sharedBufferApi.__listenerInstalled) {
    sharedBufferApi.__listenerInstalled = true;
    window.chrome.webview.addEventListener("sharedbufferreceived", (event) => {
      const additionalData = event.additionalData || {};
      const buffer = event.getBuffer();

      if (additionalData.__tauriSharedIpc) {
        const channelId = Number(additionalData.channelId);
        const role = additionalData.role;
        const entry = sharedIpcApi.channels.get(channelId) || {};
        entry[role] = { buffer, capacity: additionalData.capacity };
        sharedIpcApi.channels.set(channelId, entry);
        maybeResolveChannel(channelId);
        return;
      }

      const id = additionalData.__tauriSharedBufferId;
      if (typeof id === "number") {
        sharedBufferApi.buffers.set(id, buffer);
      }

      window.dispatchEvent(new CustomEvent("tauri://webview2-shared-buffer", {
        detail: { id, buffer, additionalData }
      }));
    });
  }
"#;

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("WebView2 shared buffers are only supported on Windows")]
  UnsupportedPlatform,
  #[error("buffer size must be greater than zero")]
  EmptyBuffer,
  #[error("buffer size does not fit in this process")]
  BufferTooLarge,
  #[error("buffer {0} was not found")]
  BufferNotFound(u64),
  #[error("channel {0} was not found")]
  ChannelNotFound(u64),
  #[error("initial contents are larger than the requested buffer")]
  InitialContentsTooLarge,
  #[error("write range is outside the shared buffer")]
  WriteOutOfBounds,
  #[error("read range is outside the shared buffer")]
  ReadOutOfBounds,
  #[error("shared IPC frame is invalid")]
  InvalidFrame,
  #[error("shared IPC buffer is too small")]
  ChannelBufferTooSmall,
  #[error("shared IPC response buffer is full")]
  ResponseBufferFull,
  #[error("shared IPC method `{0}` was not found")]
  MethodNotFound(String),
  #[error("failed to access WebView2: {0}")]
  WebView2(String),
  #[error(transparent)]
  Tauri(#[from] tauri::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl serde::Serialize for Error {
  fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
  where
    S: serde::Serializer,
  {
    serializer.serialize_str(&self.to_string())
  }
}

pub struct SharedIpcRequest<'a> {
  pub channel_id: u64,
  pub request_id: u32,
  pub method: &'a str,
  pub payload: &'a [u8],
}

pub type SharedIpcHandler =
  Arc<dyn for<'a> Fn(SharedIpcRequest<'a>) -> Result<Vec<u8>> + Send + Sync + 'static>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSharedBufferRequest {
  pub size: u64,
  #[serde(default)]
  pub read_only: bool,
  #[serde(default)]
  pub additional_data: serde_json::Value,
  #[serde(default)]
  pub initial_contents: Vec<u8>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedBufferInfo {
  pub id: u64,
  pub size: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteSharedBufferRequest {
  pub id: u64,
  #[serde(default)]
  pub offset: u64,
  pub bytes: Vec<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadSharedBufferRequest {
  pub id: u64,
  #[serde(default)]
  pub offset: u64,
  pub length: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSharedChannelRequest {
  #[serde(default = "default_channel_capacity")]
  pub request_capacity: u64,
  #[serde(default = "default_channel_capacity")]
  pub response_capacity: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedChannelInfo {
  pub id: u64,
  pub request_capacity: u64,
  pub response_capacity: u64,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub request_buffer_id: Option<u64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub response_buffer_id: Option<u64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub request_path: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub response_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedChannelDispatch {
  pub channel_id: u64,
  pub response_write_offset: u32,
}

fn default_channel_capacity() -> u64 {
  DEFAULT_CHANNEL_CAPACITY
}

#[cfg(windows)]
mod platform {
  use super::*;
  use std::{
    collections::HashMap,
    ptr::{self, NonNull},
    sync::{
      atomic::{AtomicU64, Ordering},
      Mutex,
    },
  };

  use tauri::webview::PlatformWebview;
  use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2Environment12, ICoreWebView2SharedBuffer, ICoreWebView2_17,
    COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY, COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_WRITE,
  };
  use windows_core::{Interface, PCWSTR};

  pub struct SharedBufferStore {
    next_buffer_id: AtomicU64,
    next_channel_id: AtomicU64,
    buffers: Mutex<HashMap<u64, SharedBufferEntry>>,
    channels: Mutex<HashMap<u64, SharedChannel>>,
    methods: Mutex<HashMap<String, SharedIpcHandler>>,
  }

  impl Default for SharedBufferStore {
    fn default() -> Self {
      Self {
        next_buffer_id: AtomicU64::new(1),
        next_channel_id: AtomicU64::new(1),
        buffers: Mutex::new(HashMap::new()),
        channels: Mutex::new(HashMap::new()),
        methods: Mutex::new(HashMap::new()),
      }
    }
  }

  struct SharedBufferEntry {
    buffer: ICoreWebView2SharedBuffer,
    ptr: NonNull<u8>,
    size: u64,
  }

  struct SharedChannel {
    request: SharedBufferEntry,
    response: SharedBufferEntry,
  }

  unsafe impl Send for SharedBufferEntry {}
  unsafe impl Sync for SharedBufferEntry {}
  unsafe impl Send for SharedChannel {}
  unsafe impl Sync for SharedChannel {}

  impl SharedBufferStore {
    pub fn reserve_buffer_id(&self) -> u64 {
      self.next_buffer_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn reserve_channel_id(&self) -> u64 {
      self.next_channel_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn register_method(&self, method: impl Into<String>, handler: SharedIpcHandler) {
      self.methods.lock().unwrap().insert(method.into(), handler);
    }

    pub fn create_and_post_buffer(
      &self,
      id: u64,
      platform_webview: PlatformWebview,
      request: CreateSharedBufferRequest,
    ) -> Result<()> {
      validate_size(request.size)?;

      if request.initial_contents.len() > request.size as usize {
        return Err(Error::InitialContentsTooLarge);
      }

      let (entry, webview17) = create_webview2_buffer(&platform_webview, request.size)?;

      if !request.initial_contents.is_empty() {
        unsafe {
          ptr::copy_nonoverlapping(
            request.initial_contents.as_ptr(),
            entry.ptr.as_ptr(),
            request.initial_contents.len(),
          );
        }
      }

      let mut additional_data = match request.additional_data {
        serde_json::Value::Null => serde_json::json!({}),
        serde_json::Value::Object(map) => serde_json::Value::Object(map),
        value => serde_json::json!({ "value": value }),
      };
      additional_data["__tauriSharedBufferId"] = serde_json::json!(id);
      additional_data["__tauriSharedBufferSize"] = serde_json::json!(request.size);

      post_shared_buffer(
        &webview17,
        &entry.buffer,
        request.read_only,
        &additional_data,
      )?;
      self.buffers.lock().unwrap().insert(id, entry);
      Ok(())
    }

    pub fn create_and_post_channel(
      &self,
      id: u64,
      platform_webview: PlatformWebview,
      request: CreateSharedChannelRequest,
    ) -> Result<()> {
      validate_channel_capacity(request.request_capacity)?;
      validate_channel_capacity(request.response_capacity)?;

      let (request_entry, webview17) =
        create_webview2_buffer(&platform_webview, request.request_capacity)?;
      let (response_entry, _) =
        create_webview2_buffer(&platform_webview, request.response_capacity)?;

      with_entry_slice_mut(&request_entry, shared_ipc::init_buffer)?;
      with_entry_slice_mut(&response_entry, shared_ipc::init_buffer)?;

      post_shared_buffer(
        &webview17,
        &request_entry.buffer,
        false,
        &serde_json::json!({
          "__tauriSharedIpc": true,
          "channelId": id,
          "role": "request",
          "capacity": request.request_capacity
        }),
      )?;
      post_shared_buffer(
        &webview17,
        &response_entry.buffer,
        false,
        &serde_json::json!({
          "__tauriSharedIpc": true,
          "channelId": id,
          "role": "response",
          "capacity": request.response_capacity
        }),
      )?;

      self.channels.lock().unwrap().insert(
        id,
        SharedChannel {
          request: request_entry,
          response: response_entry,
        },
      );
      Ok(())
    }

    pub fn dispatch_channel(&self, channel_id: u64) -> Result<SharedChannelDispatch> {
      let mut channels = self.channels.lock().unwrap();
      let channel = channels
        .get_mut(&channel_id)
        .ok_or(Error::ChannelNotFound(channel_id))?;

      with_entry_slice_mut(&channel.response, shared_ipc::init_buffer)?;
      let response_write_offset = drain_requests(channel_id, channel, &self.methods)?;

      Ok(SharedChannelDispatch {
        channel_id,
        response_write_offset,
      })
    }

    pub fn write(&self, request: WriteSharedBufferRequest) -> Result<()> {
      let buffers = self.buffers.lock().unwrap();
      let entry = buffers
        .get(&request.id)
        .ok_or(Error::BufferNotFound(request.id))?;
      let end = request
        .offset
        .checked_add(request.bytes.len() as u64)
        .ok_or(Error::WriteOutOfBounds)?;

      if end > entry.size {
        return Err(Error::WriteOutOfBounds);
      }

      unsafe {
        ptr::copy_nonoverlapping(
          request.bytes.as_ptr(),
          entry.ptr.as_ptr().add(request.offset as usize),
          request.bytes.len(),
        );
      }
      Ok(())
    }

    pub fn read(&self, request: ReadSharedBufferRequest) -> Result<Vec<u8>> {
      let buffers = self.buffers.lock().unwrap();
      let entry = buffers
        .get(&request.id)
        .ok_or(Error::BufferNotFound(request.id))?;
      let end = request
        .offset
        .checked_add(request.length)
        .ok_or(Error::ReadOutOfBounds)?;

      if end > entry.size {
        return Err(Error::ReadOutOfBounds);
      }

      let mut out = vec![0; request.length as usize];
      unsafe {
        ptr::copy_nonoverlapping(
          entry.ptr.as_ptr().add(request.offset as usize),
          out.as_mut_ptr(),
          out.len(),
        );
      }
      Ok(out)
    }

    pub fn close_buffer(&self, id: u64) -> Result<()> {
      let entry = self
        .buffers
        .lock()
        .unwrap()
        .remove(&id)
        .ok_or(Error::BufferNotFound(id))?;
      close_entry(entry)
    }

    pub fn close_channel(&self, id: u64) -> Result<()> {
      let channel = self
        .channels
        .lock()
        .unwrap()
        .remove(&id)
        .ok_or(Error::ChannelNotFound(id))?;
      close_entry(channel.request)?;
      close_entry(channel.response)
    }
  }

  fn drain_requests(
    channel_id: u64,
    channel: &mut SharedChannel,
    methods: &Mutex<HashMap<String, SharedIpcHandler>>,
  ) -> Result<u32> {
    let request = unsafe {
      std::slice::from_raw_parts_mut(channel.request.ptr.as_ptr(), channel.request.size as usize)
    };
    let response = unsafe {
      std::slice::from_raw_parts_mut(
        channel.response.ptr.as_ptr(),
        channel.response.size as usize,
      )
    };

    shared_ipc::dispatch_requests(channel_id, request, response, |request| {
      let handler = methods.lock().unwrap().get(request.method).cloned();
      match handler {
        Some(handler) => match handler(request) {
          Ok(response) => (shared_ipc::STATUS_OK, response),
          Err(error) => (shared_ipc::STATUS_ERROR, error.to_string().into_bytes()),
        },
        None => (
          shared_ipc::STATUS_ERROR,
          Error::MethodNotFound(request.method.to_string())
            .to_string()
            .into_bytes(),
        ),
      }
    })
    .map(|offset| offset as u32)
  }

  fn create_webview2_buffer(
    platform_webview: &PlatformWebview,
    size: u64,
  ) -> Result<(SharedBufferEntry, ICoreWebView2_17)> {
    validate_size(size)?;

    let environment = platform_webview.environment();
    let webview = platform_webview.webview();
    let environment12: ICoreWebView2Environment12 = environment
      .cast()
      .map_err(|e| Error::WebView2(e.to_string()))?;
    let webview17: ICoreWebView2_17 = webview.cast().map_err(|e| Error::WebView2(e.to_string()))?;

    let buffer = unsafe {
      environment12
        .CreateSharedBuffer(size)
        .map_err(|e| Error::WebView2(e.to_string()))?
    };
    let ptr = shared_buffer_ptr(&buffer, size)?;
    Ok((SharedBufferEntry { buffer, ptr, size }, webview17))
  }

  fn post_shared_buffer(
    webview17: &ICoreWebView2_17,
    buffer: &ICoreWebView2SharedBuffer,
    read_only: bool,
    additional_data: &serde_json::Value,
  ) -> Result<()> {
    let additional_json =
      serde_json::to_string(additional_data).map_err(|e| Error::WebView2(e.to_string()))?;
    let additional_json_wide = to_wide_null(&additional_json);
    let access = if read_only {
      COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY
    } else {
      COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_WRITE
    };

    unsafe {
      webview17
        .PostSharedBufferToScript(buffer, access, PCWSTR(additional_json_wide.as_ptr()))
        .map_err(|e| Error::WebView2(e.to_string()))?;
    }
    Ok(())
  }

  fn validate_size(size: u64) -> Result<()> {
    if size == 0 {
      return Err(Error::EmptyBuffer);
    }
    if size > usize::MAX as u64 {
      return Err(Error::BufferTooLarge);
    }
    Ok(())
  }

  fn validate_channel_capacity(size: u64) -> Result<()> {
    validate_size(size)?;
    if size < shared_ipc::MIN_CHANNEL_CAPACITY as u64 {
      return Err(Error::ChannelBufferTooSmall);
    }
    if size > u32::MAX as u64 {
      return Err(Error::BufferTooLarge);
    }
    Ok(())
  }

  fn shared_buffer_ptr(
    buffer: &ICoreWebView2SharedBuffer,
    expected_size: u64,
  ) -> Result<NonNull<u8>> {
    let mut size = 0;
    unsafe {
      buffer
        .Size(&mut size)
        .map_err(|e| Error::WebView2(e.to_string()))?;
    }

    if size != expected_size {
      return Err(Error::WebView2(format!(
        "shared buffer size changed from {expected_size} to {size}"
      )));
    }

    let mut ptr = ptr::null_mut();
    unsafe {
      buffer
        .Buffer(&mut ptr)
        .map_err(|e| Error::WebView2(e.to_string()))?;
    }

    NonNull::new(ptr).ok_or_else(|| Error::WebView2("shared buffer pointer was null".into()))
  }

  fn with_entry_slice_mut<T>(
    entry: &SharedBufferEntry,
    f: impl FnOnce(&mut [u8]) -> Result<T>,
  ) -> Result<T> {
    let slice = unsafe { std::slice::from_raw_parts_mut(entry.ptr.as_ptr(), entry.size as usize) };
    f(slice)
  }

  fn close_entry(entry: SharedBufferEntry) -> Result<()> {
    unsafe {
      entry
        .buffer
        .Close()
        .map_err(|e| Error::WebView2(e.to_string()))?;
    }
    Ok(())
  }

  fn to_wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
  }
}

#[cfg(not(windows))]
mod platform {
  use super::*;
  use std::collections::HashMap;
  use std::ops::DerefMut;
  use std::sync::atomic::{AtomicU64, Ordering};
  use std::sync::{Arc, Mutex};

  use memmap2::{MmapMut, MmapOptions};
  use tempfile::NamedTempFile;

  pub struct SharedBufferStore {
    next_buffer_id: AtomicU64,
    next_channel_id: AtomicU64,
    buffers: Mutex<HashMap<u64, SharedBufferEntry>>,
    channels: Mutex<HashMap<u64, SharedChannel>>,
    methods: Mutex<HashMap<String, SharedIpcHandler>>,
  }

  #[derive(Clone)]
  struct SharedBufferEntry {
    #[allow(dead_code)]
    _file: Arc<NamedTempFile>,
    path: String,
    size: usize,
    map: Arc<Mutex<MmapMut>>,
  }

  #[derive(Clone)]
  struct SharedChannel {
    request_id: u64,
    response_id: u64,
  }

  pub struct ChannelDescriptor {
    pub request_buffer_id: u64,
    pub response_buffer_id: u64,
    pub request_path: String,
    pub response_path: String,
  }

  impl Default for SharedBufferStore {
    fn default() -> Self {
      Self {
        next_buffer_id: AtomicU64::new(1),
        next_channel_id: AtomicU64::new(1),
        buffers: Mutex::new(HashMap::new()),
        channels: Mutex::new(HashMap::new()),
        methods: Mutex::new(HashMap::new()),
      }
    }
  }

  impl SharedBufferStore {
    pub fn reserve_buffer_id(&self) -> u64 {
      self.next_buffer_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn reserve_channel_id(&self) -> u64 {
      self.next_channel_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn register_method(&self, method: impl Into<String>, handler: SharedIpcHandler) {
      self.methods.lock().unwrap().insert(method.into(), handler);
    }

    pub fn create_and_post_buffer(
      &self,
      id: u64,
      request: CreateSharedBufferRequest,
    ) -> Result<()> {
      validate_create_request(&request)?;

      if request.initial_contents.len() > request.size as usize {
        return Err(Error::InitialContentsTooLarge);
      }

      let entry = create_mmap_buffer(id, request.size as usize)?;
      {
        let mut map = entry.map.lock().unwrap();
        let copy_len = request.initial_contents.len();
        if copy_len > 0 {
          map[..copy_len].copy_from_slice(&request.initial_contents);
        }
      }

      self.buffers.lock().unwrap().insert(id, entry);
      Ok(())
    }

    pub fn create_and_post_channel(
      &self,
      id: u64,
      request: CreateSharedChannelRequest,
    ) -> Result<ChannelDescriptor> {
      validate_channel_request(&request)?;
      let request_buffer_id = self.reserve_buffer_id();
      let response_buffer_id = self.reserve_buffer_id();

      let request_entry = create_mmap_buffer(request_buffer_id, request.request_capacity as usize)?;
      let response_entry =
        create_mmap_buffer(response_buffer_id, request.response_capacity as usize)?;
      let response_path = response_entry.path.clone();

      {
        let mut request_map = request_entry.map.lock().unwrap();
        shared_ipc::init_buffer(request_map.deref_mut())?;
      }

      {
        let mut response_map = response_entry.map.lock().unwrap();
        shared_ipc::init_buffer(response_map.deref_mut())?;
      }

      self
        .buffers
        .lock()
        .unwrap()
        .insert(request_buffer_id, request_entry.clone());
      self
        .buffers
        .lock()
        .unwrap()
        .insert(response_buffer_id, response_entry);
      self.channels.lock().unwrap().insert(
        id,
        SharedChannel {
          request_id: request_buffer_id,
          response_id: response_buffer_id,
        },
      );

      Ok(ChannelDescriptor {
        request_buffer_id,
        response_buffer_id,
        request_path: request_entry.path,
        response_path,
      })
    }

    pub fn dispatch_channel(&self, channel_id: u64) -> Result<SharedChannelDispatch> {
      let (request_id, response_id) = {
        let channels = self.channels.lock().unwrap();
        let channel = channels
          .get(&channel_id)
          .ok_or(Error::ChannelNotFound(channel_id))?;
        (channel.request_id, channel.response_id)
      };

      let request_entry = {
        let buffers = self.buffers.lock().unwrap();
        buffers
          .get(&request_id)
          .cloned()
          .ok_or(Error::BufferNotFound(request_id))?
      };
      let response_entry = {
        let buffers = self.buffers.lock().unwrap();
        buffers
          .get(&response_id)
          .cloned()
          .ok_or(Error::BufferNotFound(response_id))?
      };

      let response_write_offset = {
        let mut request = request_entry.map.lock().unwrap();
        let mut response = response_entry.map.lock().unwrap();
        shared_ipc::init_buffer(response.deref_mut())?;
        shared_ipc::dispatch_requests(
          channel_id,
          request.deref_mut(),
          response.deref_mut(),
          |request| {
            let handler = self.methods.lock().unwrap().get(request.method).cloned();
            match handler {
              Some(handler) => match handler(request) {
                Ok(response) => (shared_ipc::STATUS_OK, response),
                Err(error) => (shared_ipc::STATUS_ERROR, error.to_string().into_bytes()),
              },
              None => (
                shared_ipc::STATUS_ERROR,
                Error::MethodNotFound(request.method.to_string())
                  .to_string()
                  .into_bytes(),
              ),
            }
          },
        )?
      };

      Ok(SharedChannelDispatch {
        channel_id,
        response_write_offset: response_write_offset as u32,
      })
    }

    #[cfg(test)]
    pub fn access_buffer<R>(
      &self,
      id: u64,
      access: impl FnOnce(&mut [u8]) -> Result<R>,
    ) -> Result<R> {
      let buffers = self.buffers.lock().unwrap();
      let entry = buffers.get(&id).ok_or(Error::BufferNotFound(id))?.clone();
      let map = entry
        .map
        .lock()
        .map_err(|_| Error::WebView2("shared mmap lock was poisoned".into()))?;
      let mut map = map;
      access(map.deref_mut())
    }

    pub fn write(&self, request: WriteSharedBufferRequest) -> Result<()> {
      let buffers = self.buffers.lock().unwrap();
      let entry = buffers
        .get(&request.id)
        .ok_or(Error::BufferNotFound(request.id))?;
      let end = request
        .offset
        .checked_add(request.bytes.len() as u64)
        .ok_or(Error::WriteOutOfBounds)?;

      if end > entry.size as u64 {
        return Err(Error::WriteOutOfBounds);
      }

      let mut map = entry.map.lock().unwrap();
      map[request.offset as usize..end as usize].copy_from_slice(&request.bytes);
      Ok(())
    }

    pub fn read(&self, request: ReadSharedBufferRequest) -> Result<Vec<u8>> {
      let buffers = self.buffers.lock().unwrap();
      let entry = buffers
        .get(&request.id)
        .ok_or(Error::BufferNotFound(request.id))?;
      let end = request
        .offset
        .checked_add(request.length)
        .ok_or(Error::ReadOutOfBounds)?;

      if end > entry.size as u64 {
        return Err(Error::ReadOutOfBounds);
      }

      let map = entry.map.lock().unwrap();
      Ok(map[request.offset as usize..end as usize].to_vec())
    }

    pub fn close_buffer(&self, id: u64) -> Result<()> {
      self
        .buffers
        .lock()
        .unwrap()
        .remove(&id)
        .map(|_| ())
        .ok_or(Error::BufferNotFound(id))
    }

    pub fn close_channel(&self, id: u64) -> Result<()> {
      let channel = self
        .channels
        .lock()
        .unwrap()
        .remove(&id)
        .ok_or(Error::ChannelNotFound(id))?;
      self
        .buffers
        .lock()
        .unwrap()
        .remove(&channel.request_id)
        .ok_or(Error::BufferNotFound(channel.request_id))?;
      self
        .buffers
        .lock()
        .unwrap()
        .remove(&channel.response_id)
        .ok_or(Error::BufferNotFound(channel.response_id))?;
      Ok(())
    }
  }

  fn create_mmap_buffer(id: u64, size: usize) -> Result<SharedBufferEntry> {
    if size == 0 {
      return Err(Error::EmptyBuffer);
    }
    if size > u32::MAX as usize {
      return Err(Error::BufferTooLarge);
    }

    let file = NamedTempFile::new().map_err(|error| {
      Error::WebView2(format!(
        "failed to create mmap backing file for buffer {id}: {error}"
      ))
    })?;
    file.as_file().set_len(size as u64).map_err(|error| {
      Error::WebView2(format!(
        "failed to resize mmap backing file for buffer {id}: {error}"
      ))
    })?;

    let map = unsafe {
      MmapOptions::new()
        .len(size)
        .map_mut(file.as_file())
        .map_err(|error| Error::WebView2(format!("failed to map shared buffer {id}: {error}")))?
    };

    let path = file.path().to_string_lossy().into_owned();

    Ok(SharedBufferEntry {
      _file: Arc::new(file),
      path,
      size,
      map: Arc::new(Mutex::new(map)),
    })
  }
}

pub use platform::SharedBufferStore;

#[cfg(windows)]
fn validate_create_request(request: &CreateSharedBufferRequest) -> Result<()> {
  if request.size == 0 {
    return Err(Error::EmptyBuffer);
  }
  if request.size > usize::MAX as u64 {
    return Err(Error::BufferTooLarge);
  }
  if request.initial_contents.len() > request.size as usize {
    return Err(Error::InitialContentsTooLarge);
  }
  Ok(())
}

#[cfg(windows)]
fn validate_channel_request(request: &CreateSharedChannelRequest) -> Result<()> {
  if request.request_capacity < shared_ipc::MIN_CHANNEL_CAPACITY as u64
    || request.response_capacity < shared_ipc::MIN_CHANNEL_CAPACITY as u64
  {
    return Err(Error::ChannelBufferTooSmall);
  }
  if request.request_capacity > u32::MAX as u64 || request.response_capacity > u32::MAX as u64 {
    return Err(Error::BufferTooLarge);
  }
  Ok(())
}

#[cfg(not(windows))]
fn validate_create_request(request: &CreateSharedBufferRequest) -> Result<()> {
  if request.size == 0 {
    return Err(Error::EmptyBuffer);
  }
  if request.size > u32::MAX as u64 {
    return Err(Error::BufferTooLarge);
  }
  if request.initial_contents.len() > request.size as usize {
    return Err(Error::InitialContentsTooLarge);
  }
  Ok(())
}

#[cfg(not(windows))]
fn validate_channel_request(request: &CreateSharedChannelRequest) -> Result<()> {
  if request.request_capacity < shared_ipc::MIN_CHANNEL_CAPACITY as u64
    || request.response_capacity < shared_ipc::MIN_CHANNEL_CAPACITY as u64
  {
    return Err(Error::ChannelBufferTooSmall);
  }
  if request.request_capacity > u32::MAX as u64 || request.response_capacity > u32::MAX as u64 {
    return Err(Error::BufferTooLarge);
  }
  Ok(())
}

pub trait SharedBufferExt<R: Runtime> {
  fn shared_buffers(&self) -> State<'_, Arc<SharedBufferStore>>;

  fn register_shared_ipc_method<F>(&self, method: impl Into<String>, handler: F)
  where
    F: for<'a> Fn(SharedIpcRequest<'a>) -> Result<Vec<u8>> + Send + Sync + 'static;
}

impl<R: Runtime, T: Manager<R>> SharedBufferExt<R> for T {
  fn shared_buffers(&self) -> State<'_, Arc<SharedBufferStore>> {
    self.state::<Arc<SharedBufferStore>>()
  }

  fn register_shared_ipc_method<F>(&self, method: impl Into<String>, handler: F)
  where
    F: for<'a> Fn(SharedIpcRequest<'a>) -> Result<Vec<u8>> + Send + Sync + 'static,
  {
    self
      .state::<Arc<SharedBufferStore>>()
      .register_method(method, Arc::new(handler));
  }
}

#[tauri::command]
fn create_shared_buffer<R: Runtime>(
  webview_window: WebviewWindow<R>,
  state: State<'_, Arc<SharedBufferStore>>,
  request: CreateSharedBufferRequest,
) -> Result<SharedBufferInfo> {
  #[cfg(windows)]
  {
    validate_create_request(&request)?;

    let id = state.reserve_buffer_id();
    let size = request.size;
    let state = state.inner().clone();
    webview_window.with_webview(move |platform_webview| {
      if let Err(error) = state.create_and_post_buffer(id, platform_webview, request) {
        log::error!("failed to create WebView2 shared buffer {id}: {error}");
      }
    })?;

    Ok(SharedBufferInfo { id, size })
  }

  #[cfg(not(windows))]
  {
    let _ = webview_window;
    validate_create_request(&request)?;
    let id = state.reserve_buffer_id();
    let size = request.size;
    let state = state.inner().clone();
    state.create_and_post_buffer(id, request)?;
    Ok(SharedBufferInfo { id, size })
  }
}

#[tauri::command]
fn create_shared_channel<R: Runtime>(
  webview_window: WebviewWindow<R>,
  state: State<'_, Arc<SharedBufferStore>>,
  request: CreateSharedChannelRequest,
) -> Result<SharedChannelInfo> {
  #[cfg(windows)]
  {
    validate_channel_request(&request)?;

    let id = state.reserve_channel_id();
    let request_capacity = request.request_capacity;
    let response_capacity = request.response_capacity;
    let state = state.inner().clone();
    webview_window.with_webview(move |platform_webview| {
      if let Err(error) = state.create_and_post_channel(id, platform_webview, request) {
        log::error!("failed to create WebView2 shared IPC channel {id}: {error}");
      }
    })?;

    Ok(SharedChannelInfo {
      id,
      request_capacity,
      response_capacity,
    })
  }

  #[cfg(not(windows))]
  {
    let _ = webview_window;
    validate_channel_request(&request)?;
    let id = state.reserve_channel_id();
    let request_capacity = request.request_capacity;
    let response_capacity = request.response_capacity;
    let descriptor = state.create_and_post_channel(id, request)?;
    Ok(SharedChannelInfo {
      id,
      request_capacity,
      response_capacity,
      request_buffer_id: Some(descriptor.request_buffer_id),
      response_buffer_id: Some(descriptor.response_buffer_id),
      request_path: Some(descriptor.request_path),
      response_path: Some(descriptor.response_path),
    })
  }
}

#[tauri::command]
fn dispatch_shared_channel(
  state: State<'_, Arc<SharedBufferStore>>,
  channel_id: u64,
) -> Result<SharedChannelDispatch> {
  #[cfg(windows)]
  {
    state.dispatch_channel(channel_id)
  }

  #[cfg(not(windows))]
  {
    state.dispatch_channel(channel_id)
  }
}

#[tauri::command]
fn write_shared_buffer(
  state: State<'_, Arc<SharedBufferStore>>,
  request: WriteSharedBufferRequest,
) -> Result<()> {
  #[cfg(windows)]
  {
    state.write(request)
  }

  #[cfg(not(windows))]
  {
    state.write(request)
  }
}

#[tauri::command]
fn read_shared_buffer(
  state: State<'_, Arc<SharedBufferStore>>,
  request: ReadSharedBufferRequest,
) -> Result<Vec<u8>> {
  #[cfg(windows)]
  {
    state.read(request)
  }

  #[cfg(not(windows))]
  {
    state.read(request)
  }
}

#[tauri::command]
fn close_shared_buffer(state: State<'_, Arc<SharedBufferStore>>, id: u64) -> Result<()> {
  #[cfg(windows)]
  {
    state.close_buffer(id)
  }

  #[cfg(not(windows))]
  {
    state.close_buffer(id)
  }
}

#[tauri::command]
fn close_shared_channel(state: State<'_, Arc<SharedBufferStore>>, id: u64) -> Result<()> {
  #[cfg(windows)]
  {
    state.close_channel(id)
  }

  #[cfg(not(windows))]
  {
    state.close_channel(id)
  }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
  Builder::new("webview2-shared-buffer")
    .setup(|app, _api| {
      app.manage(Arc::new(SharedBufferStore::default()));
      Ok(())
    })
    .js_init_script(INIT_SCRIPT)
    .invoke_handler(tauri::generate_handler![
      create_shared_buffer,
      create_shared_channel,
      dispatch_shared_channel,
      write_shared_buffer,
      read_shared_buffer,
      close_shared_buffer,
      close_shared_channel
    ])
    .build()
}

#[cfg(test)]
#[cfg(not(windows))]
mod tests {
  use super::*;
  use std::time::Instant;

  fn with_mmap_store() -> Arc<SharedBufferStore> {
    Arc::new(SharedBufferStore::default())
  }

  fn echo_handler<'a>(request: SharedIpcRequest<'a>) -> Result<Vec<u8>> {
    Ok(request.payload.to_vec())
  }

  #[test]
  fn mmap_buffer_crud_round_trips() {
    let store = with_mmap_store();
    let id = store.reserve_buffer_id();
    store
      .create_and_post_buffer(
        id,
        CreateSharedBufferRequest {
          size: 4096,
          read_only: false,
          additional_data: serde_json::json!({ "test": "buffer-crud" }),
          initial_contents: b"hello".to_vec(),
        },
      )
      .unwrap();

    store
      .write(WriteSharedBufferRequest {
        id,
        offset: 16,
        bytes: b"world".to_vec(),
      })
      .unwrap();

    let bytes = store
      .read(ReadSharedBufferRequest {
        id,
        offset: 16,
        length: 5,
      })
      .unwrap();
    assert_eq!(bytes, b"world");

    let full = store
      .read(ReadSharedBufferRequest {
        id,
        offset: 0,
        length: 16,
      })
      .unwrap();
    assert_eq!(&full[..5], b"hello");

    store.close_buffer(id).unwrap();
    assert!(matches!(
      store.close_buffer(id),
      Err(Error::BufferNotFound(_))
    ));
  }

  #[test]
  fn mmap_shared_ipc_channel_dispatches_registered_method() -> Result<()> {
    let store = with_mmap_store();
    store.register_method("echo", Arc::new(echo_handler));
    let request = CreateSharedChannelRequest {
      request_capacity: 4096,
      response_capacity: 4096,
    };
    let channel_id = store.reserve_channel_id();
    let descriptor = store.create_and_post_channel(channel_id, request).unwrap();

    let _request_payload = {
      let request_write = store.access_buffer(descriptor.request_buffer_id, |map| {
        shared_ipc::init_buffer(map)?;
        shared_ipc::write_request_frame(map, 1, "echo", b"ping")
      })?;
      request_write
    };

    let dispatch = store.dispatch_channel(channel_id).unwrap();
    let response_bytes = store.access_buffer(descriptor.response_buffer_id, |map| {
      shared_ipc::read_response_frame(map, dispatch.response_write_offset as usize, 1)
    })?;

    assert_eq!(response_bytes.payload, b"ping");
    assert_eq!(response_bytes.status, shared_ipc::STATUS_OK);
    Ok(())
  }

  #[test]
  fn mmap_channel_close_removes_buffers() {
    let store = with_mmap_store();
    let channel_id = store.reserve_channel_id();
    let descriptor = store
      .create_and_post_channel(
        channel_id,
        CreateSharedChannelRequest {
          request_capacity: 4096,
          response_capacity: 4096,
        },
      )
      .unwrap();

    assert!(store
      .read(ReadSharedBufferRequest {
        id: descriptor.request_buffer_id,
        offset: 0,
        length: 4
      })
      .is_ok());

    store.close_channel(channel_id).unwrap();
    assert!(matches!(
      store.read(ReadSharedBufferRequest {
        id: descriptor.request_buffer_id,
        offset: 0,
        length: 4
      }),
      Err(Error::BufferNotFound(_))
    ));
    assert!(matches!(
      store.read(ReadSharedBufferRequest {
        id: descriptor.response_buffer_id,
        offset: 0,
        length: 4
      }),
      Err(Error::BufferNotFound(_))
    ));
    assert!(matches!(
      store.dispatch_channel(channel_id),
      Err(Error::ChannelNotFound(_))
    ));
  }

  #[test]
  #[ignore = "performance baseline; run explicitly with `cargo test -p tauri-plugin-shared-buffer -- --ignored --nocapture`"]
  fn performance_mmap_channel_vs_json_ipc_large_payload() -> Result<()> {
    let store = with_mmap_store();
    store.register_method("echo", Arc::new(echo_handler));

    let request = CreateSharedChannelRequest {
      request_capacity: 1_048_576,
      response_capacity: 1_048_576,
    };
    let channel_id = store.reserve_channel_id();
    let descriptor = store.create_and_post_channel(channel_id, request).unwrap();

    const ITERATIONS: usize = 2_000;
    const PAYLOAD_LEN: usize = 64 * 1024;
    let payload = vec![42; PAYLOAD_LEN];

    let shared_start = Instant::now();
    for request_id in 0..ITERATIONS {
      store.access_buffer(descriptor.request_buffer_id, |map| {
        shared_ipc::init_buffer(map)?;
        shared_ipc::write_request_frame(map, request_id as u32, "echo", &payload).map(|_| ())
      })?;
      let dispatch = store.dispatch_channel(channel_id).unwrap();
      store.access_buffer(descriptor.response_buffer_id, |response_map| {
        let _frame = shared_ipc::read_response_frame(
          response_map,
          dispatch.response_write_offset as usize,
          request_id as u32,
        )?;
        Ok(())
      })?;
    }
    let shared_elapsed = shared_start.elapsed();

    let json_start = Instant::now();
    for _ in 0..ITERATIONS {
      let json = serde_json::to_vec(&serde_json::json!({
        "method": "echo",
        "payload": payload,
      }))
      .unwrap();
      let decoded: serde_json::Value = serde_json::from_slice(&json).unwrap();
      let payload = decoded["payload"].as_array().unwrap();
      assert_eq!(payload.len(), PAYLOAD_LEN);
    }
    let json_elapsed = json_start.elapsed();

    println!(
      "mmap shared ipc (large payload): {shared_elapsed:?}, json baseline: {json_elapsed:?}"
    );
    assert!(shared_elapsed < json_elapsed);
    Ok(())
  }

  #[test]
  #[ignore = "performance baseline; run explicitly with `cargo test -p tauri-plugin-shared-buffer -- --ignored --nocapture`"]
  fn performance_mmap_channel_vs_json_ipc_small_payloads() -> Result<()> {
    let store = with_mmap_store();
    store.register_method("echo", Arc::new(echo_handler));

    let request = CreateSharedChannelRequest {
      request_capacity: 4096,
      response_capacity: 4096,
    };
    let channel_id = store.reserve_channel_id();
    let descriptor = store.create_and_post_channel(channel_id, request).unwrap();

    const ITERATIONS: usize = 5_000;
    const PAYLOAD_LEN: usize = 256;
    let payload = vec![7; PAYLOAD_LEN];

    let shared_start = Instant::now();
    for request_id in 0..ITERATIONS {
      store.access_buffer(descriptor.request_buffer_id, |map| {
        shared_ipc::init_buffer(map)?;
        shared_ipc::write_request_frame(map, request_id as u32, "echo", &payload).map(|_| ())
      })?;
      let dispatch = store.dispatch_channel(channel_id).unwrap();
      store.access_buffer(descriptor.response_buffer_id, |response_map| {
        let _frame = shared_ipc::read_response_frame(
          response_map,
          dispatch.response_write_offset as usize,
          request_id as u32,
        )?;
        Ok(())
      })?;
    }
    let shared_elapsed = shared_start.elapsed();

    let json_start = Instant::now();
    for _ in 0..ITERATIONS {
      let json = serde_json::to_vec(&serde_json::json!({
        "method": "echo",
        "payload": payload,
      }))
      .unwrap();
      let decoded: serde_json::Value = serde_json::from_slice(&json).unwrap();
      assert_eq!(decoded["payload"].as_array().unwrap().len(), PAYLOAD_LEN);
    }
    let json_elapsed = json_start.elapsed();

    println!(
      "mmap shared ipc (small payload): {shared_elapsed:?}, json baseline: {json_elapsed:?}"
    );
    assert!(shared_elapsed < json_elapsed);
    Ok(())
  }
}
