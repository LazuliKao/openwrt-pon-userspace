// SPDX-License-Identifier: GPL-2.0-only
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread::{self, sleep};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use h3c_fttr_tools::bosa;
use h3c_fttr_tools::fpga;
use h3c_fttr_tools::ipc::{self, SubDevice};
use h3c_fttr_tools::mqtt::MqttClient;
use h3c_fttr_tools::pon_engine::PonEngine;

fn setup_bridge() {
    println!("[*] Initializing downstream FTTR network bridges...");
    let _ = Command::new("ip").args(["link", "set", "eth1", "up"]).status();

    for id in 1..=16 {
        let ifname = format!("eth1.{id}");
        let _ = Command::new("ip")
            .args(["link", "add", "link", "eth1", "name", &ifname, "type", "vlan", "id", &id.to_string()])
            .status();
        let _ = Command::new("ip").args(["link", "set", &ifname, "up"]).status();
        let _ = Command::new("ip").args(["link", "set", &ifname, "master", "br-lan"]).status();
    }
}

fn start_ipc_server(
    listener: UnixListener,
    devices: Arc<Mutex<Vec<SubDevice>>>,
    pon_engine: Arc<Mutex<PonEngine>>,
    mqtt_tx: Arc<Mutex<Option<MqttClient>>>,
) {
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };

            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                let line = line.trim();
                if line == "STATUS" {
                    let devs = devices.lock().unwrap();
                    let engine = pon_engine.lock().unwrap();
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();

                    let mut resp = String::from("{\n  \"pon_onus\": [\n");
                    for (i, onu) in engine.active_onus.iter().enumerate() {
                        resp.push_str(&format!(
                            "    {{\"id\": {}, \"vendor\": \"{}\", \"sn\": \"{}\", \"state\": \"{:?}\", \"is_h3c\": {}}}",
                            onu.onu_id,
                            onu.vendor_str(),
                            onu.sn_hex(),
                            onu.state,
                            onu.is_h3c_device
                        ));
                        if i + 1 < engine.active_onus.len() {
                            resp.push(',');
                        }
                        resp.push('\n');
                    }
                    resp.push_str("  ],\n  \"telemetry_gateways\": [\n");
                    for (i, dev) in devs.iter().enumerate() {
                        resp.push_str(&format!(
                            "    {{\"id\": \"{}\", \"last_seen_sec_ago\": {}, \"payload_bytes\": {}}}",
                            dev.id,
                            now.saturating_sub(dev.last_seen),
                            dev.payload_bytes
                        ));
                        if i + 1 < devs.len() {
                            resp.push(',');
                        }
                        resp.push('\n');
                    }
                    resp.push_str("  ]\n}\n");
                    let _ = stream.write_all(resp.as_bytes());
                } else if line == "DISCOVER" {
                    let engine = pon_engine.lock().unwrap();
                    let frames = engine.build_discovery_frames();
                    let _ = stream.write_all(
                        format!("{{\"status\": \"discovery_dispatched\", \"frames\": {}}}\n", frames.len()).as_bytes(),
                    );
                } else if line.starts_with("PROVISION ") {
                    if let Ok(id) = line["PROVISION ".len()..].trim().parse::<u8>() {
                        let mut engine = pon_engine.lock().unwrap();
                        let omci_frames = engine.build_omci_provisioning(id);
                        let _ = stream.write_all(
                            format!("{{\"status\": \"provisioned\", \"omci_messages\": {}}}\n", omci_frames.len()).as_bytes(),
                        );
                    } else {
                        let _ = stream.write_all(b"{\"error\": \"invalid onu id\"}\n");
                    }
                } else if line.starts_with("EXEC ") {
                    let parts: Vec<&str> = line["EXEC ".len()..].splitn(2, ' ').collect();
                    if parts.len() == 2 {
                        let sn = parts[0];
                        let cmd = parts[1];
                        let topic = format!("/fttr/subdev/exec/{sn}");
                        let mut client_lock = mqtt_tx.lock().unwrap();
                        if let Some(ref mut client) = *client_lock {
                            let _ = client.publish(&topic, cmd.as_bytes());
                            let _ = stream.write_all(b"{\"status\": \"dispatched\"}\n");
                        } else {
                            let _ = stream.write_all(b"{\"error\": \"mqtt broker unavailable\"}\n");
                        }
                    } else {
                        let _ = stream.write_all(b"{\"error\": \"invalid format\"}\n");
                    }
                }
            }
        }
    });
}

fn main() {
    println!("[*] Starting H3C FTTR Master Controller Daemon (Rust fttrd)...");

    let _ = fs::create_dir_all(ipc::SUBDEV_DIR);

    // 1. Hardware Initialization
    let fw_path = Path::new("/lib/firmware/FTTR_TOP.sbit");
    if fw_path.exists() {
        let _ = fpga::load_bitstream(fw_path);
    }
    let _ = bosa::init_optical_transceiver();
    setup_bridge();

    // 2. Initialize GPON Micro-OLT Protocol Engine
    let pon_engine = Arc::new(Mutex::new(PonEngine::new()));

    // Broadcast initial discovery sequence
    {
        let engine = pon_engine.lock().unwrap();
        let _discovery = engine.build_discovery_frames();
        println!("[+] PON Engine initialized. Discovery broadcast activated.");
    }

    let devices: Arc<Mutex<Vec<SubDevice>>> = Arc::new(Mutex::new(Vec::new()));
    let mqtt_client_arc: Arc<Mutex<Option<MqttClient>>> = Arc::new(Mutex::new(None));

    // 3. Start IPC Server
    if let Ok(listener) = ipc::create_socket_listener() {
        start_ipc_server(
            listener,
            devices.clone(),
            pon_engine.clone(),
            mqtt_client_arc.clone(),
        );
        println!("[+] IPC socket server listening on {}", ipc::SOCKET_PATH);
    }

    // 4. Connect to MQTT Broker
    println!("[*] Connecting to local Mosquitto MQTT broker on 127.0.0.1:1883...");
    let mut last_ping = Instant::now();

    loop {
        let mut client = match MqttClient::connect("127.0.0.1:1883", "h3c-fttrd-rust") {
            Ok(c) => {
                println!("[+] Connected to local MQTT broker!");
                c
            }
            Err(_) => {
                sleep(Duration::from_secs(2));
                continue;
            }
        };

        if let Err(e) = client.subscribe("/fttr/maindev/data/#") {
            eprintln!("[-] Failed to subscribe to FTTR telemetry topics: {e}");
            sleep(Duration::from_secs(2));
            continue;
        }

        let prefix = "/fttr/maindev/data/";

        loop {
            match client.read_packet() {
                Ok(Some((topic, payload))) => {
                    if let Some(dev_id) = topic.strip_prefix(prefix) {
                        println!(
                            "[+] Telemetry packet received from sub-gateway: {} ({} bytes)",
                            dev_id,
                            payload.len()
                        );

                        // Save telemetry
                        let dir = format!("{}/{}", ipc::SUBDEV_DIR, dev_id);
                        let _ = fs::create_dir_all(&dir);
                        let file_path = format!("{dir}/telemetry.bin");
                        if let Ok(mut f) = File::create(&file_path) {
                            let _ = f.write_all(&payload);
                        }

                        // Register ONU in PON Engine if not already registered
                        // Device SN format: e.g. H3CTxxxxxx or MAC
                        let vendor = if dev_id.len() >= 4 {
                            let mut v = [0u8; 4];
                            v.copy_from_slice(&dev_id.as_bytes()[..4]);
                            v
                        } else {
                            *b"H3TC"
                        };
                        let sn = [0x00, 0x01, 0x02, 0x03];

                        let mut engine = pon_engine.lock().unwrap();
                        let onu = engine.register_onu(vendor, sn);
                        let _ = engine.activate_onu(onu.onu_id);
                        let _ = engine.build_omci_provisioning(onu.onu_id);
                        let mut opt_client = Some(client);
                        engine.handle_post_activation(&onu, &mut opt_client);
                        client = opt_client.unwrap();

                        // Update device list
                        let mut devs = devices.lock().unwrap();
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();

                        let mut found = false;
                        for dev in devs.iter_mut() {
                            if dev.id == dev_id {
                                dev.last_seen = now;
                                dev.payload_bytes = payload.len();
                                found = true;
                                break;
                            }
                        }

                        if !found {
                            println!("[+] New sub-gateway joined network: {}", dev_id);
                            devs.push(SubDevice {
                                id: dev_id.to_string(),
                                last_seen: now,
                                payload_bytes: payload.len(),
                            });
                        }

                        let _ = ipc::update_status_file(&devs);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("[-] MQTT read error: {e}, reconnecting...");
                    break;
                }
            }

            if last_ping.elapsed() >= Duration::from_secs(30) {
                if let Err(_) = client.ping() {
                    break;
                }
                last_ping = Instant::now();
            }

            sleep(Duration::from_millis(50));
        }

        sleep(Duration::from_secs(2));
    }
}
