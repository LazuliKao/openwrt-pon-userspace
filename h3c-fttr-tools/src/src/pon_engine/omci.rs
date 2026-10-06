// SPDX-License-Identifier: GPL-2.0-only
//! ITU-T G.988 Baseline OMCI (ONU Management and Control Interface) Engine
//!
//! Synthesizes standard 48-byte baseline OMCI messages to provision:
//! - MIB Reset
//! - T-CONT Allocation
//! - GEM Port Network CTP & GEM Interworking TP
//! - MAC Bridge Service Profile & Port mapping
//! - Extended VLAN Tagging Operation (VLAN 1 bridge for LAN)
//! - Ethernet UNI administrative unlock

pub const OMCI_BASELINE_DEVICE_ID: u8 = 0x0A;

// OMCI Action Types (with AR = 0x40 flag set)
pub const OMCI_ACTION_CREATE: u8 = 0x40 | 0x04;
pub const OMCI_ACTION_DELETE: u8 = 0x40 | 0x05;
pub const OMCI_ACTION_SET: u8 = 0x40 | 0x08;
pub const OMCI_ACTION_GET: u8 = 0x40 | 0x09;
pub const OMCI_ACTION_MIB_RESET: u8 = 0x40 | 0x0F;

// Standard ITU-T G.988 Managed Entity (ME) Class IDs
pub const ME_ONT_DATA: u16 = 2;
pub const ME_PPTP_ETHERNET_UNI: u16 = 11;
pub const ME_MAC_BRIDGE_SERVICE_PROFILE: u16 = 45;
pub const ME_MAC_BRIDGE_PORT_CONFIG_DATA: u16 = 47;
pub const ME_EXT_VLAN_TAGGING_OP: u16 = 171;
pub const ME_TCONT: u16 = 256;
pub const ME_GEM_INTERWORKING_TP: u16 = 266;
pub const ME_GEM_PORT_NETWORK_CTP: u16 = 268;

/// Standard 48-byte Baseline OMCI Message
#[derive(Debug, Clone, Copy)]
pub struct OmciMessage {
    pub transaction_id: u16,
    pub message_type: u8,
    pub device_id: u8,
    pub class_id: u16,
    pub instance_id: u16,
    pub payload: [u8; 32],
}

impl OmciMessage {
    pub fn new(
        transaction_id: u16,
        message_type: u8,
        class_id: u16,
        instance_id: u16,
        payload: [u8; 32],
    ) -> Self {
        Self {
            transaction_id,
            message_type,
            device_id: OMCI_BASELINE_DEVICE_ID,
            class_id,
            instance_id,
            payload,
        }
    }

    /// Serializes to the standard 48-byte ITU-T G.988 frame
    pub fn to_bytes(&self) -> [u8; 48] {
        let mut out = [0u8; 48];
        out[0] = ((self.transaction_id >> 8) & 0xFF) as u8;
        out[1] = (self.transaction_id & 0xFF) as u8;
        out[2] = self.message_type;
        out[3] = self.device_id;
        out[4] = ((self.class_id >> 8) & 0xFF) as u8;
        out[5] = (self.class_id & 0xFF) as u8;
        out[6] = ((self.instance_id >> 8) & 0xFF) as u8;
        out[7] = (self.instance_id & 0xFF) as u8;
        out[8..40].copy_from_slice(&self.payload);
        out[40] = 0x00; // CPCS-UU
        out[41] = 0x00; // CPI
        out[42] = 0x00; // Length MSB
        out[43] = 0x28; // Length LSB (40 payload bytes)

        // Compute CRC-32 (ITU-T G.988)
        let crc = Self::compute_crc32(&out[..44]);
        out[44] = ((crc >> 24) & 0xFF) as u8;
        out[45] = ((crc >> 16) & 0xFF) as u8;
        out[46] = ((crc >> 8) & 0xFF) as u8;
        out[47] = (crc & 0xFF) as u8;
        out
    }

    fn compute_crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFFFFFFu32;
        for &b in bytes {
            crc ^= (b as u32) << 24;
            for _ in 0..8 {
                if (crc & 0x80000000) != 0 {
                    crc = (crc << 1) ^ 0x04C11DB7;
                } else {
                    crc <<= 1;
                }
            }
        }
        !crc
    }

    /// 1. MIB Reset (Resets ONU internal MIB to clean state)
    pub fn mib_reset(tx_id: u16) -> Self {
        Self::new(tx_id, OMCI_ACTION_MIB_RESET, ME_ONT_DATA, 0, [0u8; 32])
    }

    /// 2. Create T-CONT (Alloc-ID = 256 for default data channel)
    pub fn create_tcont(tx_id: u16, tcont_inst: u16, alloc_id: u16) -> Self {
        let mut payload = [0u8; 32];
        payload[0] = ((alloc_id >> 8) & 0xFF) as u8;
        payload[1] = (alloc_id & 0xFF) as u8;
        payload[2] = 0x01; // Policy: strict priority
        Self::new(tx_id, OMCI_ACTION_SET, ME_TCONT, tcont_inst, payload)
    }

    /// 3. Create GEM Port Network CTP (Port ID 256 mapped to T-CONT)
    pub fn create_gem_port(tx_id: u16, gem_port_id: u16, tcont_inst: u16) -> Self {
        let mut payload = [0u8; 32];
        payload[0] = ((gem_port_id >> 8) & 0xFF) as u8;
        payload[1] = (gem_port_id & 0xFF) as u8;
        payload[2] = ((tcont_inst >> 8) & 0xFF) as u8;
        payload[3] = (tcont_inst & 0xFF) as u8;
        payload[4] = 0x03; // Direction: bidirectional
        Self::new(tx_id, OMCI_ACTION_CREATE, ME_GEM_PORT_NETWORK_CTP, gem_port_id, payload)
    }

    /// 4. Create GEM Interworking TP
    pub fn create_gem_interworking_tp(tx_id: u16, gem_port_id: u16) -> Self {
        let mut payload = [0u8; 32];
        payload[0] = ((gem_port_id >> 8) & 0xFF) as u8;
        payload[1] = (gem_port_id & 0xFF) as u8;
        payload[2] = 0x05; // Service type: Ethernet bridge
        payload[3] = 0x00; // MAC Bridge Pointer
        payload[4] = 0x01;
        Self::new(tx_id, OMCI_ACTION_CREATE, ME_GEM_INTERWORKING_TP, gem_port_id, payload)
    }

    /// 5. Create MAC Bridge Service Profile (Bridge instance 1)
    pub fn create_mac_bridge_service_profile(tx_id: u16, bridge_inst: u16) -> Self {
        let mut payload = [0u8; 32];
        payload[0] = 0x00; // Spanning tree disabled
        payload[1] = 0x08; // Learning: enabled
        payload[2] = 0x01; // Port bridging: enabled
        Self::new(tx_id, OMCI_ACTION_CREATE, ME_MAC_BRIDGE_SERVICE_PROFILE, bridge_inst, payload)
    }

    /// 6. Unlock Ethernet Port UNI (Class 11, Admin State = 0)
    pub fn unlock_ethernet_uni(tx_id: u16, uni_inst: u16) -> Self {
        let mut payload = [0u8; 32];
        payload[0] = 0x80; // Attribute mask: Admin state
        payload[1] = 0x00; // Admin state = 0 (Unlocked)
        Self::new(tx_id, OMCI_ACTION_SET, ME_PPTP_ETHERNET_UNI, uni_inst, payload)
    }
}
