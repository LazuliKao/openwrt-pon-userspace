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
use h3c_fttr_tools::h3c_subdev;
use h3c_fttr_tools::ipc::{self, SubDevice};
use h3c_fttr_tools::mqtt::MqttClient;
use h3c_fttr_tools::pon_engine::omci;
use h3c_fttr_tools::pon_engine::PonEngine;

fn dispatch_subdev_cmd(
    client_opt: &mut Option<MqttClient>,
    sn: &str,
    cmd: &str,
) -> Result<&'static str, &'static str> {
    if let Some(ref mut client) = client_opt {
        let topic = h3c_subdev::topic_subdev_exec(sn);
        let _ = client.publish(&topic, cmd.as_bytes());
        Ok("dispatched")
    } else {
        Err("mqtt broker unavailable")
    }
}

fn setup_bridge() {
    println!("[*] Initializing downstream FTTR DSA switch ports (fttr1 ~ fttr16)...");
    for id in 1..=16 {
        let ifname = format!("fttr{id}");
        let _ = Command::new("ip").args(["link", "set", &ifname, "up"]).status();
        let _ = Command::new("ip").args(["link", "set", &ifname, "master", "br-lan"]).status();
    }
    println!("[+] Downstream FTTR switch ports fttr1 ~ fttr16 bridged to br-lan.");
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

                    // 2. Downstream DSA switch ports (fttr1 ~ fttr16)
                    resp.push_str("  \"switch_ports\": [\n");
                    for port in 1..=16 {
                        let ifname = format!("fttr{port}");
                        let onu_opt = engine.active_onus.iter().find(|o| o.onu_id == port as u8);
                        let carrier = onu_opt.is_some();
                        let status_str = if carrier { "UP" } else { "DOWN" };
                        let onu_sn_str = match onu_opt {
                            Some(o) => format!("\"{}\"", o.full_sn_str()),
                            None => "null".to_string(),
                        };
                        resp.push_str(&format!(
                            "    {{\"port\": {}, \"interface\": \"{}\", \"carrier\": {}, \"status\": \"{}\", \"onu_id\": {}, \"onu_sn\": {}}}",
                            port,
                            ifname,
                            carrier,
                            status_str,
                            if carrier { port.to_string() } else { "null".to_string() },
                            onu_sn_str
                        ));
                        if port < 16 {
                            resp.push(',');
                        }
                        resp.push('\n');
                    }
                    resp.push_str("  ],\n");

                    // 3. Sub-gateways (connected ONUs)
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
                    for frame in &frames {
                        let bytes = frame.to_bytes();
                        let _ = bosa::send_ploam_msg(&bytes);
                    }
                    let _ = stream.write_all(
                        format!("{{\"status\": \"discovery_dispatched\", \"frames\": {}}}\n", frames.len()).as_bytes(),
                    );
                } else if line.starts_with("REGISTER ") {
                    let sn_str = line["REGISTER ".len()..].trim();
                    if sn_str.len() >= 8 {
                        let mut vendor = *b"H3TC";
                        let mut sn = [0u8; 4];
                        let prefix = &sn_str[..4];
                        if prefix == "H3CT" || prefix == "H3TC" {
                            vendor = *b"H3TC";
                        } else {
                            vendor.copy_from_slice(prefix.as_bytes());
                        }
                        let hex_sn = &sn_str[4..];
                        if hex_sn.len() >= 8 {
                            for i in 0..4 {
                                if let Ok(b) = u8::from_str_radix(&hex_sn[i * 2..i * 2 + 2], 16) {
                                    sn[i] = b;
                                }
                            }
                        }
                        let mut engine = pon_engine.lock().unwrap();
                        let onu = engine.register_onu(vendor, sn);
                        let onu_id = onu.onu_id;
                        let act_frames = engine.activate_onu(onu_id);
                        let omci_frames = engine.build_omci_provisioning(onu_id);
                        drop(engine);

                        for f in &act_frames {
                            let _ = bosa::send_ploam_msg(&f.to_bytes());
                            thread::sleep(Duration::from_millis(10));
                        }
                        for f in &omci_frames {
                            let _ = omci::send_omci_frame(onu_id, f);
                            thread::sleep(Duration::from_millis(20));
                        }
                        let _ = bosa::set_fttr_carrier(onu_id as u32, true);

                        let _ = stream.write_all(
                            format!(
                                "{{\"status\": \"registered\", \"onu_id\": {}, \"port\": \"fttr{}\", \"sn\": \"{}\", \"state\": \"O5Operation\", \"omci_messages\": {}}}\n",
                                onu_id, onu_id, sn_str, omci_frames.len()
                            ).as_bytes(),
                        );
                    } else {
                        let _ = stream.write_all(b"{\"error\": \"invalid sn\"}\n");
                    }
                } else if line.starts_with("PROVISION ") {
                    if let Ok(id) = line["PROVISION ".len()..].trim().parse::<u8>() {
                        let mut engine = pon_engine.lock().unwrap();
                        let omci_frames = engine.build_omci_provisioning(id);
                        drop(engine);
                        for f in &omci_frames {
                            let _ = omci::send_omci_frame(id, f);
                            thread::sleep(Duration::from_millis(20));
                        }
                        let _ = stream.write_all(
                            format!("{{\"status\": \"provisioned\", \"omci_messages\": {}}}\n", omci_frames.len()).as_bytes(),
                        );
                    } else {
                        let _ = stream.write_all(b"{\"error\": \"invalid onu id\"}\n");
                    }
                } else if line.starts_with("AUTH ") {
                    if let Ok(id) = line["AUTH ".len()..].trim().parse::<u8>() {
                        let mut engine = pon_engine.lock().unwrap();
                        let tx1 = engine.next_tx_id();
                        let f1 = omci::OmciMessage::loid_auth_success(tx1);
                        let tx2 = engine.next_tx_id();
                        let f2 = omci::OmciMessage::loid_auth_with_name(tx2, "subGateway");
                        drop(engine);

                        let _ = omci::send_omci_frame(id, &f1);
                        thread::sleep(Duration::from_millis(20));
                        let _ = omci::send_omci_frame(id, &f2);

                        let _ = stream.write_all(
                            format!("{{\"status\": \"auth_sent\", \"onu_id\": {}}}\n", id).as_bytes(),
                        );
                    } else {
                        let _ = stream.write_all(b"{\"error\": \"invalid onu id\"}\n");
                    }
                } else if line == "SYNC_WIFI" {
                    let engine = pon_engine.lock().unwrap();
                    let mut client_lock = mqtt_tx.lock().unwrap();
                    for onu in &engine.active_onus {
                        engine.h3c_coordinator.sync_wifi_mesh(onu, &mut *client_lock);
                    }
                    let _ = stream.write_all(b"{\"status\": \"wifi_synced\"}\n");
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
                } else if line.starts_with("SUBDEV ") {
                    let subdev_line = line["SUBDEV ".len()..].trim();
                    let tokens: Vec<&str> = subdev_line.split_whitespace().collect();
                    if tokens.len() < 2 {
                        let _ = stream.write_all(b"{\"error\": \"usage: SUBDEV <sn> <action> [args...]\"}\n");
                    } else {
                        let sn = tokens[0];
                        let action = tokens[1];
                        let mut client_lock = mqtt_tx.lock().unwrap();

                        let res = match action {
                            "passwd" => {
                                if tokens.len() >= 3 {
                                    let pwd = &subdev_line[sn.len()..].trim()[action.len()..].trim();
                                    let cmd = h3c_subdev::cmd_set_admin_password(pwd);
                                    dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                                } else {
                                    Err("missing password argument")
                                }
                            }
                            "user-passwd" => {
                                if tokens.len() >= 3 {
                                    let pwd = &subdev_line[sn.len()..].trim()[action.len()..].trim();
                                    let cmd = h3c_subdev::cmd_set_user_password(pwd);
                                    dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                                } else {
                                    Err("missing password argument")
                                }
                            }
                            "admin" => {
                                if tokens.len() >= 3 {
                                    match tokens[2] {
                                        "enable" => {
                                            let cmd = h3c_subdev::cmd_enable_admin();
                                            dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                                        }
                                        "disable" => {
                                            let cmd = h3c_subdev::cmd_disable_admin();
                                            dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                                        }
                                        "unlock" => {
                                            let cmd = h3c_subdev::cmd_unlock_admin();
                                            dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                                        }
                                        _ => Err("invalid admin action (enable/disable/unlock)"),
                                    }
                                } else {
                                    Err("missing admin sub-action")
                                }
                            }
                            "telnet" => {
                                if tokens.len() >= 3 {
                                    match tokens[2] {
                                        "enable" => {
                                            let port = tokens.get(3).and_then(|p| p.parse::<u16>().ok()).unwrap_or(23);
                                            let cmd = h3c_subdev::cmd_enable_telnet(port);
                                            dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                                        }
                                        "disable" => {
                                            let cmd = h3c_subdev::cmd_disable_telnet();
                                            dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                                        }
                                        _ => Err("invalid telnet action (enable/disable)"),
                                    }
                                } else {
                                    Err("missing telnet sub-action")
                                }
                            }
                            "tr069" => {
                                if tokens.len() >= 3 {
                                    match tokens[2] {
                                        "disable" => {
                                            let cmd = h3c_subdev::cmd_disable_tr069();
                                            dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                                        }
                                        "set" => {
                                            if tokens.len() >= 4 {
                                                let acs_url = tokens[3];
                                                let vlan = tokens.get(4).and_then(|v| v.parse::<u16>().ok());
                                                let cmd = h3c_subdev::cmd_set_tr069(acs_url, vlan, true);
                                                dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                                            } else {
                                                Err("missing acs url")
                                            }
                                        }
                                        _ => Err("invalid tr069 action (set/disable)"),
                                    }
                                } else {
                                    Err("missing tr069 sub-action")
                                }
                            }
                            "iptv" => {
                                if tokens.len() >= 3 {
                                    match tokens[2] {
                                        "enable" => {
                                            let vlan = tokens.get(3).and_then(|v| v.parse::<u16>().ok()).unwrap_or(43);
                                            let eth_port = tokens.get(4).and_then(|p| p.parse::<u8>().ok()).unwrap_or(2);
                                            let mvlan = tokens.get(5).and_then(|m| m.parse::<u16>().ok());
                                            let cmd = h3c_subdev::cmd_set_iptv(true, vlan, eth_port, mvlan);
                                            dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                                        }
                                        "disable" => {
                                            let cmd = h3c_subdev::cmd_set_iptv(false, 0, 2, None);
                                            dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                                        }
                                        _ => Err("invalid iptv action (enable/disable)"),
                                    }
                                } else {
                                    Err("missing iptv sub-action")
                                }
                            }
                            "internet-vlan" => {
                                if tokens.len() >= 3 {
                                    if let Ok(vlan) = tokens[2].parse::<u16>() {
                                        let mut lan_ports: Vec<u8> = tokens[3..]
                                            .iter()
                                            .filter_map(|p| p.parse::<u8>().ok())
                                            .collect();
                                        if lan_ports.is_empty() {
                                            lan_ports = vec![1, 2, 3, 4];
                                        }
                                        let cmd = h3c_subdev::cmd_set_internet_vlan(vlan, &lan_ports);
                                        dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                                    } else {
                                        Err("invalid vlan id")
                                    }
                                } else {
                                    Err("missing vlan id")
                                }
                            }
                            "wifi" => {
                                if tokens.len() >= 4 {
                                    let ssid = tokens[2];
                                    let pwd = tokens[3];
                                    let enable_5g = tokens
                                        .get(4)
                                        .map(|&v| v != "no" && v != "false" && v != "0")
                                        .unwrap_or(true);
                                    let cmd = h3c_subdev::cmd_set_wifi_local(ssid, pwd, enable_5g);
                                    dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                                } else {
                                    Err("usage: SUBDEV <sn> wifi <ssid> <password> [enable_5g]")
                                }
                            }
                            "mesh" => {
                                if tokens.len() >= 4 {
                                    let ssid = tokens[2];
                                    let pwd = tokens[3];
                                    let ch2 = tokens.get(4).and_then(|c| c.parse::<u8>().ok()).unwrap_or(6);
                                    let ch5 = tokens.get(5).and_then(|c| c.parse::<u8>().ok()).unwrap_or(44);
                                    let bw5 = tokens.get(6).copied().unwrap_or("160MHz");
                                    let roaming = tokens
                                        .get(7)
                                        .map(|&r| r != "false" && r != "0" && r != "no")
                                        .unwrap_or(true);
                                    let json = h3c_subdev::build_wifi_mesh_json(ssid, pwd, ch2, ch5, bw5, roaming);
                                    let topic = h3c_subdev::topic_subdev_config(sn);
                                    if let Some(ref mut client) = *client_lock {
                                        let _ = client.publish(&topic, json.as_bytes());
                                        Ok("dispatched")
                                    } else {
                                        Err("mqtt broker unavailable")
                                    }
                                } else {
                                    Err("usage: SUBDEV <sn> mesh <ssid> <password> [ch2] [ch5] [bw5] [roaming]")
                                }
                            }
                            "reboot" => {
                                let cmd = h3c_subdev::cmd_reboot();
                                dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                            }
                            "factory-reset" => {
                                let cmd = h3c_subdev::cmd_factory_reset();
                                dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                            }
                            "status" => {
                                let cmd = h3c_subdev::cmd_query_status();
                                dispatch_subdev_cmd(&mut client_lock, sn, &cmd)
                            }
                            _ => Err("unknown subdev action"),
                        };

                        let resp = match res {
                            Ok(status) => format!(
                                "{{\"status\": \"{}\", \"sn\": \"{}\", \"action\": \"{}\"}}\n",
                                status, sn, action
                            ),
                            Err(e) => format!("{{\"error\": \"{}\"}}\n", e),
                        };
                        let _ = stream.write_all(resp.as_bytes());
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

    // Auto-register connected sub-gateway H3CT685DF998
    {
        let mut engine = pon_engine.lock().unwrap();
        let vendor = *b"H3TC";
        let sn = [0x68, 0x5D, 0xF9, 0x98];
        let onu = engine.register_onu(vendor, sn);
        let onu_id = onu.onu_id;
        let act_frames = engine.activate_onu(onu_id);
        let omci_frames = engine.build_omci_provisioning(onu_id);
        drop(engine);

        for f in &act_frames {
            let _ = bosa::send_ploam_msg(&f.to_bytes());
            thread::sleep(Duration::from_millis(10));
        }
        for f in &omci_frames {
            let _ = omci::send_omci_frame(onu_id, f);
            thread::sleep(Duration::from_millis(20));
        }
        let _ = bosa::set_fttr_carrier(onu_id as u32, true);
        println!("[+] Sub-gateway H3CT685DF998 registered & activated on fttr{} (State: O5Operation).", onu_id);
    }

    let devices: Arc<Mutex<Vec<SubDevice>>> = Arc::new(Mutex::new(Vec::new()));
    let mqtt_client_arc: Arc<Mutex<Option<MqttClient>>> = Arc::new(Mutex::new(None));

    // Background PLOAM discovery broadcast & reception thread
    {
        let pon_engine_ploam = pon_engine.clone();
        thread::spawn(move || {
            println!("[+] Started background PLOAM discovery & reception loop.");
            let mut last_disc = Instant::now() - Duration::from_secs(5);
            loop {
                // Periodic discovery broadcast (every 3.0s)
                if last_disc.elapsed() >= Duration::from_millis(3000) {
                    let engine = pon_engine_ploam.lock().unwrap();
                    let frames = engine.build_discovery_frames();
                    drop(engine);
                    for frame in &frames {
                        let bytes = frame.to_bytes();
                        let _ = bosa::send_ploam_msg(&bytes);
                    }
                    last_disc = Instant::now();
                }

                // Poll for incoming PLOAM frames
                if let Ok(rx_data) = bosa::recv_ploam_msg() {
                    if rx_data.len() >= 10 {
                        let msg_id = rx_data[1];
                        if msg_id == 0x01 || msg_id == 0x02 { // Serial_Number_ONU
                            let mut vendor = [0u8; 4];
                            let mut sn = [0u8; 4];
                            vendor.copy_from_slice(&rx_data[2..6]);
                            sn.copy_from_slice(&rx_data[6..10]);
                            println!(
                                "[+] PLOAM: Captured upstream ONU serial: Vendor={:?}, SN={:02X?}",
                                String::from_utf8_lossy(&vendor),
                                sn
                            );

                            let mut engine = pon_engine_ploam.lock().unwrap();
                            let onu = engine.register_onu(vendor, sn);
                            let onu_id = onu.onu_id;
                            let act_frames = engine.activate_onu(onu_id);
                            let omci_frames = engine.build_omci_provisioning(onu_id);
                            drop(engine);

                            for f in &act_frames {
                                let _ = bosa::send_ploam_msg(&f.to_bytes());
                                thread::sleep(Duration::from_millis(10));
                            }
                            for f in &omci_frames {
                                let _ = omci::send_omci_frame(onu_id, f);
                                thread::sleep(Duration::from_millis(20));
                            }
                            let _ = bosa::set_fttr_carrier(onu_id as u32, true);
                            println!("[+] ONU {} successfully activated on fttr{} in state O5Operation!", onu_id, onu_id);
                        }
                    }
                }

                thread::sleep(Duration::from_millis(200));
            }
        });
    }

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
        *mqtt_client_arc.lock().unwrap() = None;
        let mut client = match MqttClient::connect("127.0.0.1:1883", "h3c-fttrd-rust") {
            Ok(c) => {
                println!("[+] Connected to local MQTT broker!");
                if let Ok(cloned) = c.try_clone() {
                    *mqtt_client_arc.lock().unwrap() = Some(cloned);
                }
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
