use crate::{Error, Result, SharedIpcRequest};

pub(crate) const BUFFER_MAGIC: u32 = 0x4950_4254;
pub(crate) const BUFFER_VERSION: u32 = 1;
pub(crate) const BUFFER_HEADER_SIZE: usize = 16;
pub(crate) const FRAME_HEADER_SIZE: usize = 32;
pub(crate) const MIN_CHANNEL_CAPACITY: usize = BUFFER_HEADER_SIZE + FRAME_HEADER_SIZE;
pub(crate) const FRAME_KIND_REQUEST: u32 = 1;
pub(crate) const FRAME_KIND_RESPONSE: u32 = 2;
pub(crate) const STATUS_OK: i32 = 0;
pub(crate) const STATUS_ERROR: i32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct ResponseFrame {
  pub status: i32,
  pub payload: Vec<u8>,
}

pub(crate) fn init_buffer(buffer: &mut [u8]) -> Result<()> {
  validate_capacity(buffer.len())?;
  write_u32(buffer, 0, BUFFER_MAGIC)?;
  write_u32(buffer, 4, BUFFER_VERSION)?;
  write_u32(buffer, 8, BUFFER_HEADER_SIZE as u32)?;
  write_u32(buffer, 12, 0)?;
  Ok(())
}

#[cfg(test)]
pub(crate) fn write_request_frame(
  buffer: &mut [u8],
  request_id: u32,
  method: &str,
  payload: &[u8],
) -> Result<usize> {
  validate_capacity(buffer.len())?;

  let method = method.as_bytes();
  let frame_len = align8(FRAME_HEADER_SIZE + method.len() + payload.len());
  let mut write_offset = read_u32(buffer, 8)? as usize;

  if write_offset < BUFFER_HEADER_SIZE || write_offset > buffer.len() {
    write_offset = BUFFER_HEADER_SIZE;
  }
  if write_offset + frame_len > buffer.len() {
    write_offset = BUFFER_HEADER_SIZE;
  }
  if write_offset + frame_len > buffer.len() {
    return Err(Error::ChannelBufferTooSmall);
  }

  write_u32(buffer, write_offset, frame_len as u32)?;
  write_u32(buffer, write_offset + 4, FRAME_KIND_REQUEST)?;
  write_u32(buffer, write_offset + 8, request_id)?;
  write_u32(buffer, write_offset + 12, method.len() as u32)?;
  write_u32(buffer, write_offset + 16, payload.len() as u32)?;
  write_i32(buffer, write_offset + 20, 0)?;
  write_u32(buffer, write_offset + 24, 0)?;
  write_u32(buffer, write_offset + 28, 0)?;

  let method_start = write_offset + FRAME_HEADER_SIZE;
  let payload_start = method_start + method.len();
  buffer[method_start..payload_start].copy_from_slice(method);
  buffer[payload_start..payload_start + payload.len()].copy_from_slice(payload);
  buffer[payload_start + payload.len()..write_offset + frame_len].fill(0);
  write_u32(buffer, 8, (write_offset + frame_len) as u32)?;

  Ok(write_offset)
}

pub(crate) fn dispatch_requests<F>(
  channel_id: u64,
  request: &mut [u8],
  response: &mut [u8],
  mut handle: F,
) -> Result<usize>
where
  F: for<'a> FnMut(SharedIpcRequest<'a>) -> (i32, Vec<u8>),
{
  validate_initialized(request)?;
  init_buffer(response)?;

  let request_write = read_u32(request, 8)? as usize;
  if request_write < BUFFER_HEADER_SIZE || request_write > request.len() {
    return Err(Error::InvalidFrame);
  }

  let mut request_offset = BUFFER_HEADER_SIZE;
  let mut response_offset = BUFFER_HEADER_SIZE;

  while request_offset < request_write {
    let frame = read_request_frame(request, request_offset, request_write)?;
    let (status, response_payload) = handle(SharedIpcRequest {
      channel_id,
      request_id: frame.request_id,
      method: frame.method,
      payload: frame.payload,
    });

    response_offset = write_response_frame(
      response,
      response_offset,
      frame.request_id,
      status,
      &response_payload,
    )?;
    request_offset += frame.frame_len;
  }

  write_u32(request, 8, BUFFER_HEADER_SIZE as u32)?;
  write_u32(response, 8, response_offset as u32)?;
  Ok(response_offset)
}

#[cfg(test)]
pub(crate) fn read_response_frame(
  buffer: &[u8],
  response_write_offset: usize,
  request_id: u32,
) -> Result<ResponseFrame> {
  validate_initialized(buffer)?;

  if response_write_offset < BUFFER_HEADER_SIZE || response_write_offset > buffer.len() {
    return Err(Error::InvalidFrame);
  }

  let mut offset = BUFFER_HEADER_SIZE;
  while offset + FRAME_HEADER_SIZE <= response_write_offset {
    let frame_len = read_u32(buffer, offset)? as usize;
    let kind = read_u32(buffer, offset + 4)?;
    let id = read_u32(buffer, offset + 8)?;
    let payload_len = read_u32(buffer, offset + 16)? as usize;
    let status = read_i32(buffer, offset + 20)?;

    if kind != FRAME_KIND_RESPONSE
      || frame_len < FRAME_HEADER_SIZE
      || offset + frame_len > response_write_offset
      || FRAME_HEADER_SIZE + payload_len > frame_len
    {
      return Err(Error::InvalidFrame);
    }

    if id == request_id {
      let payload_start = offset + FRAME_HEADER_SIZE;
      return Ok(ResponseFrame {
        status,
        payload: buffer[payload_start..payload_start + payload_len].to_vec(),
      });
    }

    offset += frame_len;
  }

  Err(Error::InvalidFrame)
}

fn read_request_frame(
  buffer: &[u8],
  offset: usize,
  request_write: usize,
) -> Result<RequestFrame<'_>> {
  let frame_len = read_u32(buffer, offset)? as usize;
  let kind = read_u32(buffer, offset + 4)?;
  let request_id = read_u32(buffer, offset + 8)?;
  let method_len = read_u32(buffer, offset + 12)? as usize;
  let payload_len = read_u32(buffer, offset + 16)? as usize;

  if kind != FRAME_KIND_REQUEST
    || frame_len < FRAME_HEADER_SIZE
    || offset + frame_len > request_write
    || FRAME_HEADER_SIZE + method_len + payload_len > frame_len
  {
    return Err(Error::InvalidFrame);
  }

  let method_start = offset + FRAME_HEADER_SIZE;
  let payload_start = method_start + method_len;
  let method =
    std::str::from_utf8(&buffer[method_start..payload_start]).map_err(|_| Error::InvalidFrame)?;
  let payload = &buffer[payload_start..payload_start + payload_len];

  Ok(RequestFrame {
    frame_len,
    request_id,
    method,
    payload,
  })
}

fn write_response_frame(
  buffer: &mut [u8],
  offset: usize,
  request_id: u32,
  status: i32,
  payload: &[u8],
) -> Result<usize> {
  let frame_len = align8(FRAME_HEADER_SIZE + payload.len());
  if offset + frame_len > buffer.len() {
    return Err(Error::ResponseBufferFull);
  }

  write_u32(buffer, offset, frame_len as u32)?;
  write_u32(buffer, offset + 4, FRAME_KIND_RESPONSE)?;
  write_u32(buffer, offset + 8, request_id)?;
  write_u32(buffer, offset + 12, 0)?;
  write_u32(buffer, offset + 16, payload.len() as u32)?;
  write_i32(buffer, offset + 20, status)?;
  write_u32(buffer, offset + 24, 0)?;
  write_u32(buffer, offset + 28, 0)?;

  let payload_start = offset + FRAME_HEADER_SIZE;
  buffer[payload_start..payload_start + payload.len()].copy_from_slice(payload);
  buffer[payload_start + payload.len()..offset + frame_len].fill(0);

  Ok(offset + frame_len)
}

fn validate_initialized(buffer: &[u8]) -> Result<()> {
  validate_capacity(buffer.len())?;
  if read_u32(buffer, 0)? != BUFFER_MAGIC || read_u32(buffer, 4)? != BUFFER_VERSION {
    return Err(Error::InvalidFrame);
  }
  Ok(())
}

fn validate_capacity(len: usize) -> Result<()> {
  if len < MIN_CHANNEL_CAPACITY {
    return Err(Error::ChannelBufferTooSmall);
  }
  if len > u32::MAX as usize {
    return Err(Error::BufferTooLarge);
  }
  Ok(())
}

fn read_u32(buffer: &[u8], offset: usize) -> Result<u32> {
  let bytes = buffer.get(offset..offset + 4).ok_or(Error::InvalidFrame)?;
  Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

#[cfg(test)]
fn read_i32(buffer: &[u8], offset: usize) -> Result<i32> {
  Ok(read_u32(buffer, offset)? as i32)
}

fn write_u32(buffer: &mut [u8], offset: usize, value: u32) -> Result<()> {
  let bytes = buffer
    .get_mut(offset..offset + 4)
    .ok_or(Error::InvalidFrame)?;
  bytes.copy_from_slice(&value.to_le_bytes());
  Ok(())
}

fn write_i32(buffer: &mut [u8], offset: usize, value: i32) -> Result<()> {
  write_u32(buffer, offset, value as u32)
}

fn align8(value: usize) -> usize {
  (value + 7) & !7
}

struct RequestFrame<'a> {
  frame_len: usize,
  request_id: u32,
  method: &'a str,
  payload: &'a [u8],
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde::{Deserialize, Serialize};
  use std::time::{Duration, Instant};

  #[test]
  fn request_response_roundtrip_uses_shared_frames() {
    let mut request = vec![0; 512];
    let mut response = vec![0; 512];
    init_buffer(&mut request).unwrap();

    write_request_frame(&mut request, 7, "uppercase", b"hello").unwrap();

    let response_write = dispatch_requests(42, &mut request, &mut response, |request| {
      assert_eq!(request.channel_id, 42);
      assert_eq!(request.request_id, 7);
      assert_eq!(request.method, "uppercase");
      (STATUS_OK, request.payload.to_ascii_uppercase())
    })
    .unwrap();

    let frame = read_response_frame(&response, response_write, 7).unwrap();
    assert_eq!(frame.status, STATUS_OK);
    assert_eq!(frame.payload, b"HELLO");
    assert_eq!(read_u32(&request, 8).unwrap(), BUFFER_HEADER_SIZE as u32);
  }

  #[test]
  fn dispatches_multiple_requests_in_one_doorbell() {
    let mut request = vec![0; 1024];
    let mut response = vec![0; 1024];
    init_buffer(&mut request).unwrap();

    write_request_frame(&mut request, 1, "echo", b"one").unwrap();
    write_request_frame(&mut request, 2, "echo", b"two").unwrap();

    let response_write = dispatch_requests(9, &mut request, &mut response, |request| {
      (STATUS_OK, request.payload.to_vec())
    })
    .unwrap();

    assert_eq!(
      read_response_frame(&response, response_write, 1)
        .unwrap()
        .payload,
      b"one"
    );
    assert_eq!(
      read_response_frame(&response, response_write, 2)
        .unwrap()
        .payload,
      b"two"
    );
  }

  #[test]
  fn handler_errors_are_encoded_as_error_frames() {
    let mut request = vec![0; 512];
    let mut response = vec![0; 512];
    init_buffer(&mut request).unwrap();
    write_request_frame(&mut request, 1, "fail", b"").unwrap();

    let response_write = dispatch_requests(1, &mut request, &mut response, |_| {
      (STATUS_ERROR, b"expected failure".to_vec())
    })
    .unwrap();

    let frame = read_response_frame(&response, response_write, 1).unwrap();
    assert_eq!(frame.status, STATUS_ERROR);
    assert_eq!(frame.payload, b"expected failure");
  }

  #[test]
  fn rejects_uninitialized_request_buffer() {
    let mut request = vec![0; 128];
    let mut response = vec![0; 128];

    assert!(matches!(
      dispatch_requests(1, &mut request, &mut response, |_| unreachable!()),
      Err(Error::InvalidFrame)
    ));
  }

  #[test]
  fn rejects_corrupt_write_offset() {
    let mut request = vec![0; 128];
    let mut response = vec![0; 128];
    init_buffer(&mut request).unwrap();
    write_u32(&mut request, 8, 129).unwrap();

    assert!(matches!(
      dispatch_requests(1, &mut request, &mut response, |_| unreachable!()),
      Err(Error::InvalidFrame)
    ));
  }

  #[test]
  fn rejects_invalid_frame_kind() {
    let mut request = vec![0; 128];
    let mut response = vec![0; 128];
    init_buffer(&mut request).unwrap();
    write_request_frame(&mut request, 1, "echo", b"data").unwrap();
    write_u32(&mut request, BUFFER_HEADER_SIZE + 4, 99).unwrap();

    assert!(matches!(
      dispatch_requests(1, &mut request, &mut response, |_| unreachable!()),
      Err(Error::InvalidFrame)
    ));
  }

  #[test]
  fn rejects_invalid_utf8_method_name() {
    let mut request = vec![0; 128];
    let mut response = vec![0; 128];
    init_buffer(&mut request).unwrap();
    write_request_frame(&mut request, 1, "echo", b"data").unwrap();
    request[BUFFER_HEADER_SIZE + FRAME_HEADER_SIZE] = 0xff;

    assert!(matches!(
      dispatch_requests(1, &mut request, &mut response, |_| unreachable!()),
      Err(Error::InvalidFrame)
    ));
  }

  #[test]
  fn rejects_oversized_request_payload() {
    let mut request = vec![0; 64];
    init_buffer(&mut request).unwrap();

    assert!(matches!(
      write_request_frame(&mut request, 1, "echo", &[1; 64]),
      Err(Error::ChannelBufferTooSmall)
    ));
  }

  #[test]
  fn rejects_response_buffer_overflow() {
    let mut request = vec![0; 128];
    let mut response = vec![0; 64];
    init_buffer(&mut request).unwrap();
    write_request_frame(&mut request, 1, "large", b"").unwrap();

    assert!(matches!(
      dispatch_requests(1, &mut request, &mut response, |_| {
        (STATUS_OK, vec![1; 64])
      }),
      Err(Error::ResponseBufferFull)
    ));
  }

  #[test]
  fn read_response_rejects_missing_request_id() {
    let mut request = vec![0; 128];
    let mut response = vec![0; 128];
    init_buffer(&mut request).unwrap();
    write_request_frame(&mut request, 1, "echo", b"data").unwrap();
    let response_write = dispatch_requests(1, &mut request, &mut response, |request| {
      (STATUS_OK, request.payload.to_vec())
    })
    .unwrap();

    assert!(matches!(
      read_response_frame(&response, response_write, 99),
      Err(Error::InvalidFrame)
    ));
  }

  #[test]
  fn wraps_request_writer_after_capacity_is_reached() {
    let mut request = vec![0; 112];
    init_buffer(&mut request).unwrap();
    write_request_frame(&mut request, 1, "echo", &[1; 24]).unwrap();
    let second_offset = write_request_frame(&mut request, 2, "echo", &[2; 24]).unwrap();

    assert_eq!(second_offset, BUFFER_HEADER_SIZE);
  }

  #[test]
  #[ignore = "performance baseline; run explicitly with `cargo test -p tauri-plugin-shared-buffer -- --ignored --nocapture`"]
  fn performance_shared_frames_vs_json_vec_ipc() {
    const ITERATIONS: usize = 2_000;
    const PAYLOAD_LEN: usize = 64 * 1024;

    #[derive(Serialize, Deserialize)]
    struct IpcPayload {
      method: String,
      bytes: Vec<u8>,
    }

    let payload = vec![42; PAYLOAD_LEN];
    let mut request = vec![0; PAYLOAD_LEN + 256];
    let mut response = vec![0; PAYLOAD_LEN + 256];
    init_buffer(&mut request).unwrap();

    let shared_start = Instant::now();
    for id in 0..ITERATIONS {
      write_request_frame(&mut request, id as u32, "echo", &payload).unwrap();
      let response_write = dispatch_requests(1, &mut request, &mut response, |request| {
        (STATUS_OK, request.payload.to_vec())
      })
      .unwrap();
      let frame = read_response_frame(&response, response_write, id as u32).unwrap();
      assert_eq!(frame.payload.len(), PAYLOAD_LEN);
    }
    let shared_elapsed = shared_start.elapsed();

    let json_start = Instant::now();
    for _ in 0..ITERATIONS {
      let json = serde_json::to_vec(&IpcPayload {
        method: "echo".into(),
        bytes: payload.clone(),
      })
      .unwrap();
      let decoded: IpcPayload = serde_json::from_slice(&json).unwrap();
      assert_eq!(decoded.bytes.len(), PAYLOAD_LEN);
    }
    let json_elapsed = json_start.elapsed();

    print_perf("shared frames", shared_elapsed, ITERATIONS, PAYLOAD_LEN);
    print_perf("json vec ipc", json_elapsed, ITERATIONS, PAYLOAD_LEN);
    assert!(
      shared_elapsed < json_elapsed,
      "shared frames should beat JSON Vec<u8> IPC baseline: shared={shared_elapsed:?}, json={json_elapsed:?}"
    );
  }

  #[test]
  #[ignore = "performance baseline; run explicitly with `cargo test -p tauri-plugin-shared-buffer -- --ignored --nocapture`"]
  fn performance_shared_frames_vs_json_array_ipc_small_messages() {
    const ITERATIONS: usize = 10_000;
    const PAYLOAD_LEN: usize = 256;

    let payload = vec![7; PAYLOAD_LEN];
    let mut request = vec![0; 4096];
    let mut response = vec![0; 4096];
    init_buffer(&mut request).unwrap();

    let shared_start = Instant::now();
    for id in 0..ITERATIONS {
      write_request_frame(&mut request, id as u32, "echo", &payload).unwrap();
      let response_write = dispatch_requests(1, &mut request, &mut response, |request| {
        (STATUS_OK, request.payload.to_vec())
      })
      .unwrap();
      let frame = read_response_frame(&response, response_write, id as u32).unwrap();
      assert_eq!(frame.payload.len(), PAYLOAD_LEN);
    }
    let shared_elapsed = shared_start.elapsed();

    let json_start = Instant::now();
    for _ in 0..ITERATIONS {
      let json = serde_json::to_vec(&serde_json::json!({
        "method": "echo",
        "bytes": payload,
      }))
      .unwrap();
      let decoded: serde_json::Value = serde_json::from_slice(&json).unwrap();
      assert_eq!(decoded["bytes"].as_array().unwrap().len(), PAYLOAD_LEN);
    }
    let json_elapsed = json_start.elapsed();

    print_perf(
      "shared frames small",
      shared_elapsed,
      ITERATIONS,
      PAYLOAD_LEN,
    );
    print_perf(
      "json array ipc small",
      json_elapsed,
      ITERATIONS,
      PAYLOAD_LEN,
    );
  }

  fn print_perf(label: &str, elapsed: Duration, iterations: usize, payload_len: usize) {
    let total_mib = (iterations * payload_len) as f64 / (1024.0 * 1024.0);
    let seconds = elapsed.as_secs_f64();
    println!(
      "{label}: {iterations} iterations, {total_mib:.1} MiB payload, {elapsed:?}, {:.1} MiB/s",
      total_mib / seconds
    );
  }
}
