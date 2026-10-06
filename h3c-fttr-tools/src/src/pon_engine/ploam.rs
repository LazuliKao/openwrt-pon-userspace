// SPDX-License-Identifier: GPL-2.0-only
//! ITU-T G.984.3 GPON PLOAM (Physical Layer OAM) Protocol Engine
//!
//! Handles ONU discovery, activation state machine (O1 -> O5), ranging,
//! and equalization delay calculation for all downstream ONUs.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnuState {
    O1Initial,
    O2Standby,
    O3SerialNumber,
    O4Ranging,
    O5Operation,
    O6IntermittentLODS,
    O7EmergencyStop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GponOnu {
    pub onu_id: u8,
    pub vendor_id: [u8; 4],
    pub serial_number: [u8; 4],
    pub state: OnuState,
    pub eq_delay: u32,
    pub is_h3c_device: bool,
}

impl GponOnu {
    pub fn new(onu_id: u8, vendor_id: [u8; 4], serial_number: [u8; 4]) -> Self {
        // H3C vendor ID is typically b"H3TC" or b"ALCL" (Alcatel/Nokia derived)
        let is_h3c_device = &vendor_id == b"H3TC" || &vendor_id == b"ALCL";
        Self {
            onu_id,
            vendor_id,
            serial_number,
            state: OnuState::O1Initial,
            eq_delay: 0,
            is_h3c_device,
        }
    }

    pub fn vendor_str(&self) -> String {
        String::from_utf8_lossy(&self.vendor_id).to_string()
    }

    pub fn sn_hex(&self) -> String {
        format!(
            "{:02X}{:02X}{:02X}{:02X}",
            self.serial_number[0],
            self.serial_number[1],
            self.serial_number[2],
            self.serial_number[3]
        )
    }

    pub fn full_sn_str(&self) -> String {
        format!("{}{}", self.vendor_str(), self.sn_hex())
    }
}

// ITU-T G.984.3 Standard Downstream PLOAM Message IDs
pub const PLOAM_UPSTREAM_OVERHEAD: u8 = 0x01;
pub const PLOAM_SERIAL_NUMBER_MASK: u8 = 0x02;
pub const PLOAM_ASSIGN_ONU_ID: u8 = 0x03;
pub const PLOAM_RANGING_REQUEST: u8 = 0x04;
pub const PLOAM_ASSIGN_DELAY: u8 = 0x05;
pub const PLOAM_DEACTIVATE_ONU_ID: u8 = 0x06;
pub const PLOAM_DISABLE_SERIAL_NUMBER: u8 = 0x07;
pub const PLOAM_ENCRYPTED_PORT_ID: u8 = 0x08;
pub const PLOAM_REQUEST_PASSWORD: u8 = 0x09;

// ITU-T G.984.3 Standard Upstream PLOAM Message IDs
pub const PLOAM_UPSTREAM_SERIAL_NUMBER: u8 = 0x01;
pub const PLOAM_UPSTREAM_PASSWORD: u8 = 0x02;
pub const PLOAM_UPSTREAM_DYING_GASP: u8 = 0x03;
pub const PLOAM_UPSTREAM_ACK: u8 = 0x04;

/// Standard 13-byte G.984.3 Downstream PLOAM message format
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct PloamFrame {
    pub onu_id: u8,        // 0x00-0xFF (0xFF = broadcast)
    pub message_id: u8,    // PLOAM message ID
    pub payload: [u8; 10], // 10 bytes parameter payload
    pub crc: u8,           // CRC-8 check
}

impl PloamFrame {
    pub fn new(onu_id: u8, message_id: u8, payload: [u8; 10]) -> Self {
        let mut frame = Self {
            onu_id,
            message_id,
            payload,
            crc: 0,
        };
        frame.crc = frame.compute_crc8();
        frame
    }

    pub fn compute_crc8(&self) -> u8 {
        let mut crc = 0u8;
        let bytes = [
            self.onu_id,
            self.message_id,
            self.payload[0],
            self.payload[1],
            self.payload[2],
            self.payload[3],
            self.payload[4],
            self.payload[5],
            self.payload[6],
            self.payload[7],
            self.payload[8],
            self.payload[9],
        ];

        for &b in &bytes {
            crc ^= b;
            for _ in 0..8 {
                if (crc & 0x80) != 0 {
                    crc = (crc << 1) ^ 0x07;
                } else {
                    crc <<= 1;
                }
            }
        }
        crc
    }

    pub fn to_bytes(&self) -> [u8; 13] {
        let mut out = [0u8; 13];
        out[0] = self.onu_id;
        out[1] = self.message_id;
        out[2..12].copy_from_slice(&self.payload);
        out[12] = self.crc;
        out
    }

    /// 1. Upstream Overhead message (broadcast)
    pub fn upstream_overhead() -> Self {
        let mut payload = [0u8; 10];
        // Standard G.984.3 Upstream Overhead defaults
        payload[0] = 0x00; // Guard bits
        payload[1] = 0x20; // Preamble length
        payload[2] = 0x10; // Delimiter length
        payload[3] = 0xAA; // Preamble pattern byte 1
        payload[4] = 0xAA; // Preamble pattern byte 2
        payload[5] = 0xAB; // Delimiter pattern byte 1
        payload[6] = 0x59; // Delimiter pattern byte 2
        payload[7] = 0x00; // Power level mode
        Self::new(0xFF, PLOAM_UPSTREAM_OVERHEAD, payload)
    }

    /// 2. Serial Number Mask message (broadcast to trigger ONU discovery)
    pub fn serial_number_mask() -> Self {
        // Mask length 0 triggers wildcard response from all undiscovered ONUs
        let payload = [0u8; 10];
        Self::new(0xFF, PLOAM_SERIAL_NUMBER_MASK, payload)
    }

    /// 3. Assign ONU-ID message
    pub fn assign_onu_id(onu_id: u8, vendor: [u8; 4], sn: [u8; 4]) -> Self {
        let mut payload = [0u8; 10];
        payload[0] = onu_id;
        payload[1..5].copy_from_slice(&vendor);
        payload[5..9].copy_from_slice(&sn);
        Self::new(0xFF, PLOAM_ASSIGN_ONU_ID, payload)
    }

    /// 4. Ranging Request message
    pub fn ranging_request(onu_id: u8) -> Self {
        let payload = [0u8; 10];
        Self::new(onu_id, PLOAM_RANGING_REQUEST, payload)
    }

    /// 5. Assign Delay (EqD) message to transition ONU to O5 (Operation State)
    pub fn assign_delay(onu_id: u8, delay_bits: u32) -> Self {
        let mut payload = [0u8; 10];
        payload[0] = ((delay_bits >> 24) & 0xFF) as u8;
        payload[1] = ((delay_bits >> 16) & 0xFF) as u8;
        payload[2] = ((delay_bits >> 8) & 0xFF) as u8;
        payload[3] = (delay_bits & 0xFF) as u8;
        Self::new(onu_id, PLOAM_ASSIGN_DELAY, payload)
    }
}
