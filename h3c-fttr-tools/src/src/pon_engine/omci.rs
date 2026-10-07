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
pub const ME_LOID_AUTHEN: u16 = 65530; // 0xFFFA (China Unicom / H3C LOID Authentication)

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

    /// 7. LOID Authentication Success (ME 65530 / 0xFFFA, Set auth_result = 1)
    pub fn loid_auth_success(tx_id: u16) -> Self {
        let mut payload = [0u8; 32];
        // Attribute mask: In G.988, 2 bytes mask.
        // Attribute 3 (bit 13 = 0x2000): Authentication Result = 1 (Success)
        payload[0] = 0x20;
        payload[1] = 0x00;
        payload[2] = 0x01; // 1 = 认证成功 (Success)
        Self::new(tx_id, OMCI_ACTION_SET, ME_LOID_AUTHEN, 0, payload)
    }

    /// 8. Full LOID Authentication Set with LOID name (ME 65530, LOID = subGateway, auth_result = 1)
    pub fn loid_auth_with_name(tx_id: u16, loid: &str) -> Self {
        let mut payload = [0u8; 32];
        // Attribute mask: 0xA000 (Attribute 1 = LOID, Attribute 3 = Auth Result)
        payload[0] = 0xA0;
        payload[1] = 0x00;
        let loid_bytes = loid.as_bytes();
        let copy_len = loid_bytes.len().min(24);
        payload[2..2 + copy_len].copy_from_slice(&loid_bytes[..copy_len]);
        payload[26] = 0x01; // 1 = 认证成功 (Success)
        Self::new(tx_id, OMCI_ACTION_SET, ME_LOID_AUTHEN, 0, payload)
    }
}

#[repr(C)]
struct SockAddrLl {
    sll_family: u16,
    sll_protocol: u16,
    sll_ifindex: i32,
    sll_hatype: u16,
    sll_pkttype: u8,
    sll_halen: u8,
    sll_addr: [u8; 8],
}

#[cfg(unix)]
extern "C" {
    fn socket(domain: i32, ty: i32, protocol: i32) -> i32;
    fn sendto(s: i32, buf: *const u8, len: usize, flags: i32, to: *const SockAddrLl, tolen: u32) -> isize;
    fn if_nametoindex(ifname: *const std::ffi::c_char) -> u32;
    fn close(fd: i32) -> i32;
}

pub fn send_omci_frame(onu_id: u8, msg: &OmciMessage) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::ffi::CString;

        // 1. Send via molt_omci (virtual netdev)
        let ifname_molt = CString::new("molt_omci").unwrap();
        let ifindex_molt = unsafe { if_nametoindex(ifname_molt.as_ptr()) };
        if ifindex_molt != 0 {
            let sock = unsafe { socket(17 /* AF_PACKET */, 3 /* SOCK_RAW */, (0x0003u16).to_be() as i32) };
            if sock >= 0 {
                let mut sll: SockAddrLl = unsafe { std::mem::zeroed() };
                sll.sll_family = 17;
                sll.sll_ifindex = ifindex_molt as i32;
                sll.sll_halen = 6;
                sll.sll_addr[..6].copy_from_slice(&[0xff; 6]);

                let bytes = msg.to_bytes();
                let _ = unsafe {
                    sendto(
                        sock,
                        bytes.as_ptr(),
                        bytes.len(),
                        0,
                        &sll as *const _,
                        std::mem::size_of::<SockAddrLl>() as u32,
                    )
                };
                unsafe { close(sock); }
            }
        }

        // 2. Also send via physical link (lan1 or eth1) with GEM tag (0x2000 | onu_id)
        let ifname_base = if std::path::Path::new("/sys/class/net/lan1").exists() {
            "lan1"
        } else if std::path::Path::new("/sys/class/net/eth1").exists() {
            "eth1"
        } else {
            return Ok(());
        };

        let c_ifname = CString::new(ifname_base).unwrap();
        let ifindex = unsafe { if_nametoindex(c_ifname.as_ptr()) };
        if ifindex != 0 {
            let sock = unsafe { socket(17 /* AF_PACKET */, 3 /* SOCK_RAW */, (0x0003u16).to_be() as i32) };
            if sock >= 0 {
                let mut sll: SockAddrLl = unsafe { std::mem::zeroed() };
                sll.sll_family = 17;
                sll.sll_ifindex = ifindex as i32;
                sll.sll_halen = 6;
                sll.sll_addr[..6].copy_from_slice(&[0xff; 6]);

                let mut frame = Vec::with_capacity(64);
                frame.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff]); // Dest MAC: broadcast
                frame.extend_from_slice(&[0x1c, 0x94, 0x68, 0x5a, 0xf4, 0x28]); // Src MAC: router MAC
                
                // 802.1Q Tag: TPID 0x8100, TCI = 0x2000 | (onu_id as u16)
                let tci: u16 = 0x2000 | (onu_id as u16);
                frame.extend_from_slice(&0x8100u16.to_be_bytes());
                frame.extend_from_slice(&tci.to_be_bytes());

                // EtherType 0x88B5 (OMCI)
                frame.extend_from_slice(&0x88B5u16.to_be_bytes());

                // 48-byte G.988 OMCI message
                frame.extend_from_slice(&msg.to_bytes());

                let _ = unsafe {
                    sendto(
                        sock,
                        frame.as_ptr(),
                        frame.len(),
                        0,
                        &sll as *const _,
                        std::mem::size_of::<SockAddrLl>() as u32,
                    )
                };
                unsafe { close(sock); }
            }
        }

        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (onu_id, msg);
        Ok(())
    }
}

