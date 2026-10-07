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
  bridge-setup         Ensure downstream FTTR VLANs (eth1.1~eth1.16) are bridged to br-lan\n  \
  status               Display active FTTR optical link and downlinked sub-gateways\n  \
  discover             Trigger PLOAM discovery sequence on optical link\n  \
  provision <onu_id>   Trigger G.988 OMCI provisioning for specific ONU\n  \
  exec <sn> <command>  Dispatch execution command to sub-gateway via MQTT\n  \
  help                 Show this help message"
    );
}

fn setup_bridge() {
    let eth1_path = Path::new("/sys/class/net/eth1");
    if !eth1_path.exists() {
        println!("[*] Physical interface eth1 not present, skipping VLAN eth1.1~eth1.16 bridge (control plane is active).");
        return;
    }

    println!("[*] Configuring downstream FTTR network bridges...");
    let _ = Command::new("ip").args(["link", "set", "eth1", "up"]).status();

    for id in 1..=16 {
        let ifname = format!("eth1.{id}");
        let _ = Command::new("ip")
            .args(["link", "add", "link", "eth1", "name", &ifname, "type", "vlan", "id", &id.to_string()])
            .status();
        let _ = Command::new("ip").args(["link", "set", &ifname, "up"]).status();
        let _ = Command::new("ip").args(["link", "set", &ifname, "master", "br-lan"]).status();
    }
    println!("[+] Downstream VLANs eth1.1 ~ eth1.16 configured and bridged to br-lan.");
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
        "status" => match ipc::send_ipc_command("STATUS") {
            Ok(resp) => println!("{resp}"),
            Err(_) => {
                if let Ok(content) = fs::read_to_string(ipc::STATUS_FILE) {
                    println!("{content}");
                } else {
                    println!("[-] fttrd daemon is not running and no status file found.");
                }
            }
        },
        "discover" => match ipc::send_ipc_command("DISCOVER") {
            Ok(resp) => println!("{resp}"),
            Err(e) => eprintln!("[-] Failed to communicate with fttrd: {e}"),
        },
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
