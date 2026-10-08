// SPDX-License-Identifier: GPL-2.0-only
use std::env;
use std::fs;
use std::path::Path;
use std::process::{exit, Command};

use h3c_fttr_tools::bosa;
use h3c_fttr_tools::fpga;
use h3c_fttr_tools::ipc;

fn print_usage() {
    eprintln!(
        "Usage: fttrctl <command> [arguments]\n\n\
Commands:\n  \
  load-fpga [path]     Program FPGA bitstream (default: /lib/firmware/FTTR_TOP.sbit)\n  \
  bosa-init            Initialize BOSA optical transceiver registers\n  \
  bridge-setup         Ensure downstream FTTR switch ports (fttr1~16) are bridged to br-lan\n  \
  status [--json|-j]   Display active FTTR optical link and DSA switch ports (table or JSON)\n  \
  discover             Trigger PLOAM discovery sequence on optical link\n  \
  register <sn>        Register and activate sub-gateway by SN (e.g. H3CT685DF998)\n  \
  auth [onu_id]        Send OMCI LOID authentication success to sub-gateway (default: 1)\n  \
  sync-wifi            Push Wi-Fi SSID and credentials to sub-gateways via MQTT\n  \
  provision <onu_id>   Trigger G.988 OMCI provisioning for specific ONU\n  \
  exec <sn> <command>  Dispatch execution command to sub-gateway via MQTT\n  \
  subdev <sn> <action> Manage sub-gateway proprietary features (passwords, telnet, iptv, etc.)\n  \
  help                 Show this help message"
    );
}

fn print_subdev_usage() {
    eprintln!(
        "Usage: fttrctl subdev <sn> <action> [arguments...] [--json|-j]\n\n\
Sub-Gateway Management Actions:\n  \
  passwd <new_pwd>                  Update super administrator (CUAdmin) password and unlock account\n  \
  user-passwd <new_pwd>             Update standard user account password\n  \
  admin <enable|disable|unlock>     Enable, disable, or unlock CUAdmin account\n  \
  telnet <enable|disable> [port]    Enable temporary root Telnet daemon (default port: 23) or disable it\n  \
  tr069 set <acs_url> [vlan]        Configure TR-069 ACS URL and management VLAN\n  \
  tr069 disable                     Completely shut down and disable TR-069\n  \
  iptv enable <vlan> [port] [mvlan] Configure IPTV multi-play VLAN and dedicated LAN port (e.g. 43 2 4094)\n  \
  iptv disable                      Disable IPTV service\n  \
  internet-vlan <vlan> [ports...]   Configure Internet VLAN tagging on sub-gateway LAN ports (e.g. 41 1 2)\n  \
  wifi <ssid> <pwd> [--no-5g]       Configure local Wi-Fi SSIDs and credentials via direct TCAPI\n  \
  mesh <ssid> <pwd> [options]       Broadcast EasyMesh JSON configuration to sub-gateway\n  \
  reboot                            Remote reboot sub-gateway\n  \
  factory-reset                     Restore sub-gateway to factory defaults (tcapi default)\n  \
  status                            Query sub-gateway Account, IPTV, and TR-069 status\n\n\
Examples:\n  \
  fttrctl subdev H3CT685DF998 passwd MyPassw0rd!\n  \
  fttrctl subdev H3CT685DF998 admin unlock\n  \
  fttrctl subdev H3CT685DF998 telnet enable 2323\n  \
  fttrctl subdev H3CT685DF998 iptv enable 43 2 4094\n  \
  fttrctl subdev H3CT685DF998 tr069 disable"
    );
}

fn handle_subdev_command(args: &[String]) {
    let mut is_json = false;
    let filtered_args: Vec<&str> = args
        .iter()
        .map(|s| s.as_str())
        .filter(|&a| {
            if a == "--json" || a == "-j" {
                is_json = true;
                false
            } else {
                true
            }
        })
        .collect();

    // filtered_args[0] is "subdev"
    if filtered_args.len() < 3 {
        print_subdev_usage();
        exit(1);
    }

    let sn = filtered_args[1];
    let action = filtered_args[2];
    let extra_args = &filtered_args[3..];

    let mut processed_extra: Vec<&str> = Vec::new();
    for &arg in extra_args {
        if arg == "--no-5g" {
            processed_extra.push("false");
        } else {
            processed_extra.push(arg);
        }
    }

    let payload = if processed_extra.is_empty() {
        format!("SUBDEV {sn} {action}")
    } else {
        format!("SUBDEV {sn} {action} {}", processed_extra.join(" "))
    };

    match ipc::send_ipc_command(&payload) {
        Ok(resp) => {
            if is_json {
                print!("{resp}");
            } else {
                if resp.contains("\"error\"") {
                    let err_msg = get_field(&resp, "error").unwrap_or("unknown error");
                    eprintln!("[-] Error from sub-gateway daemon: {err_msg}");
                    exit(1);
                } else {
                    println!("[+] Successfully dispatched action '{action}' to sub-gateway {sn} via MQTT.");
                }
            }
        }
        Err(e) => {
            if is_json {
                println!("{{\"error\": \"fttrd IPC error: {}\"}}", e);
            } else {
                eprintln!("[-] Failed to communicate with fttrd daemon: {e}");
            }
            exit(1);
        }
    }
}


fn setup_bridge() {
    println!("[*] Configuring downstream FTTR DSA switch ports (fttr1 ~ fttr16) to br-lan...");
    for id in 1..=16 {
        let ifname = format!("fttr{id}");
        let _ = Command::new("ip").args(["link", "set", &ifname, "up"]).status();
        let _ = Command::new("ip").args(["link", "set", &ifname, "master", "br-lan"]).status();
    }
    println!("[+] Downstream switch ports fttr1 ~ fttr16 configured and bridged to br-lan.");
}

fn get_field<'a>(src: &'a str, key: &str) -> Option<&'a str> {
    let key_pat = format!("\"{}\":", key);
    let start = src.find(&key_pat)?;
    let val_part = src[start + key_pat.len()..].trim_start();
    if val_part.starts_with('"') {
        let after = &val_part[1..];
        let end = after.find('"')?;
        Some(&after[..end])
    } else {
        let end = val_part.find(|c| c == ',' || c == '\n' || c == '}' || c == ']')?;
        Some(val_part[..end].trim())
    }
}

fn print_status_table(json: &str) {
    println!("==========================================================================================");
    println!("                   H3C HM2004-DU FTTR Micro-OLT Subsystem Status                         ");
    println!("==========================================================================================");

    // 1. Optical Link
    let model = get_field(json, "model").unwrap_or("UX3326");
    let tx_wl = get_field(json, "wavelength_tx_nm").unwrap_or("1490");
    let rx_wl = get_field(json, "wavelength_rx_nm").unwrap_or("1310");
    let tx_pwr = get_field(json, "tx_power_dbm").unwrap_or("0.00");
    let bias = get_field(json, "laser_bias_current_ma").unwrap_or("0.0");
    let temp = get_field(json, "temperature_celsius").unwrap_or("0.0");
    let vcc = get_field(json, "vcc_voltage").unwrap_or("0.00");
    let cdr = get_field(json, "cdr_locked").unwrap_or("false");
    let cdr_str = if cdr == "true" { "LOCKED (OK)" } else { "UNLOCKED" };

    println!("[*] Optical Transceiver (BOSA {}):", model);
    println!("    Wavelength: TX {}nm / RX {}nm  |  Rate: 2.488G / 1.244G  |  CDR: {}", tx_wl, rx_wl, cdr_str);
    println!("    TX Power:   +{} dBm  |  Bias: {} mA  |  Temp: {} °C  |  VCC: {} V", tx_pwr, bias, temp, vcc);
    println!();

    // 2. Switch Ports Table
    println!("------------------------------------------------------------------------------------------");
    println!(" Downstream DSA Switch Ports (fttr1 ~ fttr16)");
    println!("------------------------------------------------------------------------------------------");
    println!(" {:<6} {:<10} {:<10} {:<12} {:<8} {:<16} {:<12}",
             "Port", "Interface", "Carrier", "Link Speed", "ONU-ID", "ONU Serial", "Link State");
    println!("------------------------------------------------------------------------------------------");

    let mut active_count = 0;
    if let Some(ports_start) = json.find("\"switch_ports\": [") {
        let rem = &json[ports_start..];
        if let Some(ports_end) = rem.find(']') {
            let ports_block = &rem[..ports_end];
            for line in ports_block.lines() {
                if !line.contains("\"port\":") {
                    continue;
                }
                let port = get_field(line, "port").unwrap_or("?");
                let ifname = get_field(line, "interface").unwrap_or("?");
                let carrier = get_field(line, "carrier").unwrap_or("false");
                let is_up = carrier == "true";
                let status = if is_up { "UP" } else { "DOWN" };
                let speed = if is_up { "2.5 Gbps" } else { "-" };
                let onu_id = if is_up { get_field(line, "onu_id").unwrap_or("-") } else { "-" };
                let onu_sn = if is_up { get_field(line, "onu_sn").unwrap_or("-") } else { "-" };
                let link_state = if is_up { "O5Operation" } else { "Offline" };

                if is_up {
                    active_count += 1;
                }

                println!(" {:<6} {:<10} {:<10} {:<12} {:<8} {:<16} {:<12}",
                         port, ifname, status, speed, onu_id, onu_sn, link_state);
            }
        }
    }
    println!("------------------------------------------------------------------------------------------");
    println!(" Active Downlink Ports: {} / 16", active_count);
    println!();

    // 3. Sub-Gateways Details (if any)
    if let Some(sg_start) = json.find("\"sub_gateways\": [") {
        let rem = &json[sg_start..];
        if let Some(sg_end) = rem.find("  \"telemetry_gateways\":") {
            let sg_block = &rem[..sg_end];
            if sg_block.contains("\"onu_id\":") {
                println!("------------------------------------------------------------------------------------------");
                println!(" Active Sub-Gateway Telemetry & Mesh Details");
                println!("------------------------------------------------------------------------------------------");
                for block in sg_block.split("    {") {
                    if !block.contains("\"onu_id\":") {
                        continue;
                    }
                    let onu_id = get_field(block, "onu_id").unwrap_or("?");
                    let vendor = get_field(block, "vendor").unwrap_or("H3C");
                    let model = get_field(block, "model").unwrap_or("HL202-DU");
                    let sn = get_field(block, "serial_number").unwrap_or("?");
                    let state = get_field(block, "state").unwrap_or("O5Operation");
                    let dist = get_field(block, "fiber_distance_m").unwrap_or("0.0");
                    let rx_pwr = get_field(block, "optical_rx_power_dbm").unwrap_or("0.0");
                    let tx_pwr = get_field(block, "optical_tx_power_dbm").unwrap_or("0.0");
                    let ch2 = get_field(block, "channel_2g").unwrap_or("6");
                    let ch5 = get_field(block, "channel_5g").unwrap_or("44");
                    let bw5 = get_field(block, "bandwidth_5g").unwrap_or("160MHz");

                    println!("[+] Sub-Gateway #{} [{}] - {} {}", onu_id, sn, vendor, model);
                    println!("    Switch Port:    fttr{} (VLAN Tag: {}, State: {})", onu_id, onu_id, state);
                    println!("    Fiber Distance: {} m | Optical RX: {} dBm | Optical TX: {} dBm", dist, rx_pwr, tx_pwr);
                    println!("    Wi-Fi Mesh:     2.4G Ch {} / 5G Ch {} ({})", ch2, ch5, bw5);

                    if block.contains("\"connected_clients\": [") {
                        println!("    Connected Mesh Clients:");
                        if let Some(c_start) = block.find("\"connected_clients\": [") {
                            let c_rem = &block[c_start..];
                            if let Some(c_end) = c_rem.find(']') {
                                let c_block = &c_rem[..c_end];
                                for cline in c_block.split('{') {
                                    if !cline.contains("\"mac\":") {
                                        continue;
                                    }
                                    let mac = get_field(cline, "mac").unwrap_or("?");
                                    let ip = get_field(cline, "ip").unwrap_or("?");
                                    let band = get_field(cline, "band").unwrap_or("5GHz");
                                    let rssi = get_field(cline, "rssi_dbm").unwrap_or("-50");
                                    let rx_rate = get_field(cline, "rx_rate_mbps").unwrap_or("0");
                                    let tx_rate = get_field(cline, "tx_rate_mbps").unwrap_or("0");
                                    println!("      - {} | {} | {} ({} dBm) | {}/{} Mbps",
                                             mac, ip, band, rssi, rx_rate, tx_rate);
                                }
                            }
                        }
                    }
                    println!();
                }
            }
        }
    }
    println!("==========================================================================================");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        exit(1);
    }

    match args[1].as_str() {
        "load-fpga" => {
            let path_str = args
                .get(2)
                .map(|s| s.as_str())
                .unwrap_or("/lib/firmware/FTTR_TOP.sbit");
            let path = Path::new(path_str);
            if let Err(e) = fpga::load_bitstream(path) {
                eprintln!("[-] Error programming FPGA: {e}");
                exit(1);
            }
        }
        "bosa-init" => {
            if let Err(e) = bosa::init_optical_transceiver() {
                eprintln!("[-] Error initializing BOSA: {e}");
                exit(1);
            }
        }
        "bridge-setup" => {
            setup_bridge();
        }
        "status" => {
            let is_json = args.iter().any(|a| a == "--json" || a == "-j");
            match ipc::send_ipc_command("STATUS") {
                Ok(resp) => {
                    if is_json {
                        println!("{resp}");
                    } else {
                        print_status_table(&resp);
                    }
                }
                Err(_) => {
                    if let Ok(content) = fs::read_to_string(ipc::STATUS_FILE) {
                        if is_json {
                            println!("{content}");
                        } else {
                            println!("[-] fttrd daemon is offline. Stored telemetry cache:");
                            print_status_table(&content);
                        }
                    } else {
                        if is_json {
                            println!("{{\"error\": \"fttrd daemon is not running\"}}");
                        } else {
                            println!("[-] fttrd daemon is not running and no status file found.");
                        }
                    }
                }
            }
        }
        "discover" => match ipc::send_ipc_command("DISCOVER") {
            Ok(resp) => println!("{resp}"),
            Err(e) => eprintln!("[-] Failed to communicate with fttrd: {e}"),
        },
        "register" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl register <sn> (e.g. H3CT685DF998)");
                exit(1);
            }
            let sn = &args[2];
            let payload = format!("REGISTER {sn}");
            match ipc::send_ipc_command(&payload) {
                Ok(resp) => println!("{resp}"),
                Err(e) => eprintln!("[-] Failed to communicate with fttrd: {e}"),
            }
        }
        "auth" => {
            let id = args.get(2).map(|s| s.as_str()).unwrap_or("1");
            let payload = format!("AUTH {id}");
            match ipc::send_ipc_command(&payload) {
                Ok(resp) => println!("{resp}"),
                Err(e) => eprintln!("[-] Failed to communicate with fttrd: {e}"),
            }
        }
        "sync-wifi" => {
            match ipc::send_ipc_command("SYNC_WIFI") {
                Ok(resp) => println!("{resp}"),
                Err(e) => eprintln!("[-] Failed to communicate with fttrd: {e}"),
            }
        }
        "provision" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl provision <onu_id>");
                exit(1);
            }
            let id = &args[2];
            let payload = format!("PROVISION {id}");
            match ipc::send_ipc_command(&payload) {
                Ok(resp) => println!("{resp}"),
                Err(e) => eprintln!("[-] Failed to communicate with fttrd: {e}"),
            }
        }
        "exec" => {
            if args.len() < 4 {
                eprintln!("Usage: fttrctl exec <sn> <command>");
                exit(1);
            }
            let sn = &args[2];
            let cmd = &args[3..].join(" ");
            let payload = format!("EXEC {sn} {cmd}");
            match ipc::send_ipc_command(&payload) {
                Ok(resp) => println!("{resp}"),
                Err(e) => eprintln!("[-] Failed to communicate with fttrd: {e}"),
            }
        }
        "subdev" => {
            handle_subdev_command(&args[1..]);
        }
        "read-reg" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl read-reg <addr>");
                exit(1);
            }
            let addr_str = &args[2];
            let addr = u32::from_str_radix(addr_str.trim_start_matches("0x").trim_start_matches("0X"), 16)
                .expect("Invalid hex address");
            match bosa::read_fpga_reg(addr) {
                Ok(val) => println!("FPGA [0x{:08X}] = 0x{:08X}", addr, val),
                Err(e) => eprintln!("[-] Error reading FPGA register: {e}"),
            }
        }
        "write-reg" => {
            if args.len() < 4 {
                eprintln!("Usage: fttrctl write-reg <addr> <val>");
                exit(1);
            }
            let addr = u32::from_str_radix(args[2].trim_start_matches("0x").trim_start_matches("0X"), 16)
                .expect("Invalid hex address");
            let val = u32::from_str_radix(args[3].trim_start_matches("0x").trim_start_matches("0X"), 16)
                .expect("Invalid hex value");
            match bosa::write_fpga_reg(addr, val) {
                Ok(_) => println!("FPGA [0x{:08X}] <= 0x{:08X}", addr, val),
                Err(e) => eprintln!("[-] Error writing FPGA register: {e}"),
            }
        }
        "read-bosa" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl read-bosa <reg>");
                exit(1);
            }
            let reg = u32::from_str_radix(args[2].trim_start_matches("0x").trim_start_matches("0X"), 16)
                .expect("Invalid hex register");
            match bosa::read_bosa_reg(reg) {
                Ok(val) => println!("BOSA [0x{:02X}] = 0x{:02X}", reg, val),
                Err(e) => eprintln!("[-] Error reading BOSA register: {e}"),
            }
        }
        "write-bosa" => {
            if args.len() < 4 {
                eprintln!("Usage: fttrctl write-bosa <reg> <val>");
                exit(1);
            }
            let reg = u32::from_str_radix(args[2].trim_start_matches("0x").trim_start_matches("0X"), 16)
                .expect("Invalid hex register");
            let val = u32::from_str_radix(args[3].trim_start_matches("0x").trim_start_matches("0X"), 16)
                .expect("Invalid hex value");
            match bosa::write_bosa_reg(reg, val) {
                Ok(_) => println!("BOSA [0x{:02X}] <= 0x{:02X}", reg, val),
                Err(e) => eprintln!("[-] Error writing BOSA register: {e}"),
            }
        }
        "help" | "--help" | "-h" => {
            print_usage();
        }
        unknown => {
            eprintln!("[-] Unknown command: {unknown}\n");
            print_usage();
            exit(1);
        }
    }
}
