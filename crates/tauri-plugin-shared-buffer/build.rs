const COMMANDS: &[&str] = &[
  "create_shared_buffer",
  "write_shared_buffer",
  "read_shared_buffer",
  "close_shared_buffer",
  "create_shared_channel",
  "dispatch_shared_channel",
  "close_shared_channel",
];

fn main() {
  tauri_plugin::Builder::new(COMMANDS).build();
}
