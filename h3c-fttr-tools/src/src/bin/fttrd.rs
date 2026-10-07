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
    let eth1_path = Path::new("/sys/class/net/eth1");
    if !eth1_path.exists() {
        println!("[*] Physical interface eth1 not present, skipping VLAN eth1.1~eth1.16 bridge (control plane is active).");
        return;
    }

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

                    let opt = bosa::get_optical_status();

                    let mut resp = String::from("{\n");
                    // 1. Optical transceiver status
                    resp.push_str(&format!(
                        "  \"optical_transceiver\": {{\n\
                           \"model\": \"{}\",\n\
                           \"wavelength_tx_nm\": {},\n\
                           \"wavelength_rx_nm\": {},\n\
                           \"phy_rate_downlink_gbps\": {:.3},\n\
                           \"phy_rate_uplink_gbps\": {:.3},\n\
                           \"tx_power_dbm\": {:.2},\n\
                           \"laser_bias_current_ma\": {:.1},\n\
                           \"temperature_celsius\": {:.1},\n\
                           \"vcc_voltage\": {:.2},\n\
                           \"cdr_locked\": {}\n\
                         }},\n",
                        opt.model,
                        opt.wavelength_tx_nm,
                        opt.wavelength_rx_nm,
                        opt.phy_rate_downlink_gbps,
                        opt.phy_rate_uplink_gbps,
                        opt.tx_power_dbm,
                        opt.laser_bias_current_ma,
                        opt.temperature_celsius,
                        opt.vcc_voltage,
                        opt.cdr_locked
                    ));

                    // 2. Sub-gateways (connected ONUs)
                    resp.push_str("  \"sub_gateways\": [\n");
                    for (i, onu) in engine.active_onus.iter().enumerate() {
                        let sn_str = onu.full_sn_str();
                        let telem = engine.h3c_coordinator.get_subdev_telemetry(&sn_str, onu.onu_id);

                        resp.push_str(&format!(
                            "    {{\n\
                               \"onu_id\": {},\n\
                               \"vendor\": \"{}\",\n\
                               \"model\": \"{}\",\n\
                               \"serial_number\": \"{}\",\n\
                               \"state\": \"{:?}\",\n\
                               \"fiber_distance_m\": {:.1},\n\
                               \"ranging_delay_rtd_ns\": {},\n\
                               \"optical_rx_power_dbm\": {:.1},\n\
                               \"optical_tx_power_dbm\": {:.1},\n\
                               \"firmware_version\": \"{}\",\n\
                               \"hardware_version\": \"{}\",\n\
                               \"uptime_seconds\": {},\n\
                               \"data_path\": {{\n\
                                 \"interface\": \"{}\",\n\
                                 \"gem_ports\": {:?},\n\
                                 \"vlan_id\": {},\n\
                                 \"tx_bytes\": {},\n\
                                 \"rx_bytes\": {},\n\
                                 \"current_tx_kbps\": {},\n\
                                 \"current_rx_kbps\": {}\n\
                               }},\n\
                               \"wifi_mesh\": {{\n\
                                 \"channel_2g\": {},\n\
                                 \"channel_5g\": {},\n\
                                 \"bandwidth_5g\": \"{}\",\n\
                                 \"tx_power_pct\": {}\n\
                               }},\n\
                               \"connected_clients\": [\n",
                            onu.onu_id,
                            onu.vendor_str(),
                            onu.model,
                            sn_str,
                            onu.state,
                            onu.fiber_distance_m,
                            (onu.fiber_distance_m / 0.102) as u64,
                            onu.rx_power_dbm,
                            onu.tx_power_dbm,
                            onu.firmware_version,
                            onu.hardware_version,
                            onu.uptime_seconds,
                            telem.data_path.interface,
                            telem.data_path.gem_ports,
                            telem.data_path.vlan_id,
                            telem.data_path.tx_bytes,
                            telem.data_path.rx_bytes,
                            telem.data_path.current_tx_kbps,
                            telem.data_path.current_rx_kbps,
                            telem.wifi_mesh.channel_2g,
                            telem.wifi_mesh.channel_5g,
                            telem.wifi_mesh.bandwidth_5g,
                            telem.wifi_mesh.tx_power_pct
                        ));

                        for (ci, client) in telem.connected_clients.iter().enumerate() {
                            resp.push_str(&format!(
                                "        {{\n\
                                   \"mac\": \"{}\",\n\
                                   \"ip\": \"{}\",\n\
                                   \"band\": \"{}\",\n\
                                   \"rssi_dbm\": {},\n\
                                   \"rx_rate_mbps\": {},\n\
                                   \"tx_rate_mbps\": {}\n\
                                 }}",
                                client.mac,
                                client.ip,
                                client.band,
                                client.rssi_dbm,
                                client.rx_rate_mbps,
                                client.tx_rate_mbps
                            ));
                            if ci + 1 < telem.connected_clients.len() {
                                resp.push(',');
                            }
                            resp.push('\n');
                        }
                        resp.push_str("      ]\n    }");
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
