// SPDX-License-Identifier: GPL-2.0-only
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const SOCKET_PATH: &str = "/var/run/fttrd.sock";
pub const STATUS_FILE: &str = "/tmp/fttr_status.json";
pub const SUBDEV_DIR: &str = "/tmp/fttr_subdev";

#[derive(Debug, Clone)]
pub struct SubDevice {
    pub id: String,
    pub last_seen: u64,
    pub payload_bytes: usize,
}

pub fn create_socket_listener() -> io::Result<UnixListener> {
    if Path::new(SOCKET_PATH).exists() {
        let _ = fs::remove_file(SOCKET_PATH);
    }
    UnixListener::bind(SOCKET_PATH)
}

pub fn send_ipc_command(cmd: &str) -> io::Result<String> {
    let mut stream = UnixStream::connect(SOCKET_PATH)?;
    stream.write_all(format!("{cmd}\n").as_bytes())?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

pub fn update_status_file(devices: &[SubDevice]) -> io::Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut json = String::from("{\n  \"updated_at\": ");
    json.push_str(&now.to_string());
    json.push_str(",\n  \"devices\": [\n");

    for (i, dev) in devices.iter().enumerate() {
        json.push_str(&format!(
            "    {{\n      \"id\": \"{}\",\n      \"last_seen_sec_ago\": {},\n      \"last_payload_bytes\": {}\n    }}",
            dev.id,
            now.saturating_sub(dev.last_seen),
            dev.payload_bytes
        ));
        if i + 1 < devices.len() {
            json.push(',');
        }
        json.push('\n');
    }

    json.push_str("  ]\n}\n");
    fs::write(STATUS_FILE, json)
}
