// SPDX-License-Identifier: GPL-2.0-only
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

const SYSFS_RELOAD_PATH: &str = "/sys/devices/platform/fmcs/reload_fpga";
const GPIO_CLK: u32 = 29;
const GPIO_DATA: u32 = 30;

pub fn is_driver_loaded() -> bool {
    Path::new("/dev/fmcs_mci").exists() || Path::new(SYSFS_RELOAD_PATH).exists()
}

pub fn load_bitstream(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Bitstream file not found: {}", path.display()),
        ));
    }

    // Fast path 1: Check if FPGA is already programmed and active
    if let Ok(ts) = crate::bosa::read_fpga_reg(0x00000008) {
        if ts != 0 && ts != 0xFFFFFFFF {
            println!("[+] FPGA bitstream already active (timestamp: 0x{:08X}), skipping reload.", ts);
            return Ok(());
        }
    }

    // Fast path 2: Trigger high-speed kernel bitstream loader via sysfs
    if Path::new(SYSFS_RELOAD_PATH).exists() {
        println!("[+] Found active h3c-fmcs driver, triggering high-speed kernel reload...");
        let mut f = OpenOptions::new().write(true).open(SYSFS_RELOAD_PATH)?;
        f.write_all(b"1\n")?;
        println!("[+] FPGA bitstream reload successfully triggered via kernel driver.");
        return Ok(());
    }

    println!("[*] In-kernel loader not available; performing user-space Passive Serial programming...");
    export_gpio(GPIO_CLK)?;
    export_gpio(GPIO_DATA)?;
    sleep(Duration::from_millis(50));

    set_gpio_direction(GPIO_CLK, "out")?;
    set_gpio_direction(GPIO_DATA, "out")?;

    let mut clk_file = OpenOptions::new()
        .write(true)
        .open(format!("/sys/class/gpio/gpio{GPIO_CLK}/value"))?;
    let mut data_file = OpenOptions::new()
        .write(true)
        .open(format!("/sys/class/gpio/gpio{GPIO_DATA}/value"))?;

    let mut file = File::open(path)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    println!(
        "[*] Clocking {} bytes into FPGA via GPIO {}/{}...",
        buffer.len(),
        GPIO_CLK,
        GPIO_DATA
    );

    for byte in buffer {
        for bit in (0..8).rev() {
            let bit_val = if (byte >> bit) & 1 == 1 { b"1" } else { b"0" };
            let _ = data_file.write_all(bit_val);
            let _ = clk_file.write_all(b"1");
            let _ = clk_file.write_all(b"0");
        }
    }

    println!("[+] User-space FPGA programming complete!");
    Ok(())
}

fn export_gpio(pin: u32) -> io::Result<()> {
    let path = format!("/sys/class/gpio/gpio{pin}");
    if !Path::new(&path).exists() {
        if let Ok(mut f) = OpenOptions::new().write(true).open("/sys/class/gpio/export") {
            let _ = f.write_all(pin.to_string().as_bytes());
        }
    }
    Ok(())
}

fn set_gpio_direction(pin: u32, dir: &str) -> io::Result<()> {
    let path = format!("/sys/class/gpio/gpio{pin}/direction");
    let mut f = OpenOptions::new().write(true).open(path)?;
    f.write_all(dir.as_bytes())
}
