// SPDX-License-Identifier: GPL-2.0-only
//! Modular GPON Micro-OLT Protocol Engine
//!
//! Provides decoupled protocol processing:
//! - `ploam`: ITU-T G.984.3 O1-O5 discovery & ranging state machine
//! - `omci`: ITU-T G.988 MIB baseline provisioning
//! - `h3c_mesh`: Vendor-specific H3C HL202-DU Mesh & Telemetry coordinator

pub mod h3c_mesh;
pub mod omci;
pub mod ploam;

use self::h3c_mesh::H3cMeshCoordinator;
use self::omci::OmciMessage;
use self::ploam::{GponOnu, OnuState, PloamFrame};
use crate::mqtt::MqttClient;

pub struct PonEngine {
    pub active_onus: Vec<GponOnu>,
    next_onu_id: u8,
    omci_tx_id: u16,
    pub h3c_coordinator: H3cMeshCoordinator,
}

impl PonEngine {
    pub fn new() -> Self {
        Self {
            active_onus: Vec::new(),
            next_onu_id: 1,
            omci_tx_id: 1,
            h3c_coordinator: H3cMeshCoordinator::new(),
        }
    }

    /// Generates initial PLOAM downstream frames to discover sub-gateways on the ODN
    pub fn build_discovery_frames(&self) -> Vec<PloamFrame> {
        vec![
            PloamFrame::upstream_overhead(),
            PloamFrame::serial_number_mask(),
        ]
    }

    /// Processes an ONU discovered on the PON link (from upstream PLOAM or auto-ranging)
    pub fn register_onu(&mut self, vendor: [u8; 4], sn: [u8; 4]) -> GponOnu {
        // Check if already registered
        for onu in &self.active_onus {
            if onu.vendor_id == vendor && onu.serial_number == sn {
                return onu.clone();
            }
        }

        let id = self.next_onu_id;
        self.next_onu_id = (self.next_onu_id % 16) + 1;

        let mut onu = GponOnu::new(id, vendor, sn);
        println!(
            "[+] PON Engine: New ONU discovered! ID={}, Vendor={}, SN={}, Is_H3C={}",
            onu.onu_id,
            onu.vendor_str(),
            onu.sn_hex(),
            onu.is_h3c_device
        );

        onu.state = OnuState::O4Ranging;
        self.active_onus.push(onu.clone());
        onu
    }

    /// Generates PLOAM sequence to bring the ONU from O4 to O5 (Operation)
    pub fn activate_onu(&mut self, onu_id: u8) -> Vec<PloamFrame> {
        if let Some(onu) = self.active_onus.iter_mut().find(|o| o.onu_id == onu_id) {
            println!("[+] PLOAM: Transitioning ONU {} to State O5 (Operation)...", onu_id);
            onu.state = OnuState::O5Operation;
            vec![
                PloamFrame::assign_onu_id(onu_id, onu.vendor_id, onu.serial_number),
                PloamFrame::ranging_request(onu_id),
                PloamFrame::assign_delay(onu_id, 0x00001000), // EqD
            ]
        } else {
            Vec::new()
        }
    }

    /// Generates standard ITU-T G.988 OMCI provisioning sequence to unlock bridging
    pub fn build_omci_provisioning(&mut self, onu_id: u8) -> Vec<OmciMessage> {
        let mut frames = Vec::new();

        println!("[+] OMCI: Generating G.988 provisioning sequence for ONU {}...", onu_id);

        let tcont_inst = onu_id as u16;
        let gem_port_id = 256 + (onu_id as u16);
        let alloc_id = 256 + (onu_id as u16);

        // 1. MIB Reset
        frames.push(OmciMessage::mib_reset(self.next_tx_id()));
        // 2. LOID Authentication (ME 65530 / 0xFFFA - China Unicom / H3C standard)
        frames.push(OmciMessage::loid_auth_success(self.next_tx_id()));
        frames.push(OmciMessage::loid_auth_with_name(self.next_tx_id(), "subGateway"));
        // 3. Provision T-CONT
        frames.push(OmciMessage::create_tcont(self.next_tx_id(), tcont_inst, alloc_id));
        // 4. Provision GEM Port
        frames.push(OmciMessage::create_gem_port(self.next_tx_id(), gem_port_id, tcont_inst));
        // 5. Provision GEM Interworking TP
        frames.push(OmciMessage::create_gem_interworking_tp(self.next_tx_id(), gem_port_id));
        // 6. Provision MAC Bridge
        frames.push(OmciMessage::create_mac_bridge_service_profile(self.next_tx_id(), 1));
        // 7. Unlock Ethernet Port
        frames.push(OmciMessage::unlock_ethernet_uni(self.next_tx_id(), 1));

        frames
    }

    /// Performs vendor-specific setup if the ONU is an H3C HL202-DU
    pub fn handle_post_activation(&self, onu: &GponOnu, mqtt: &mut Option<MqttClient>) {
        if onu.is_h3c_device {
            println!("[*] Applying H3C HL202-DU optimizations...");
            self.h3c_coordinator.optimize_bridge_port(onu.onu_id);
            self.h3c_coordinator.sync_wifi_mesh(onu, mqtt);
        } else {
            println!(
                "[*] Generic GPON ONU (Vendor: {}) activated as standard bridge port fttr{}",
                onu.vendor_str(),
                onu.onu_id
            );
        }
    }

    pub fn next_tx_id(&mut self) -> u16 {
        let id = self.omci_tx_id;
        self.omci_tx_id = self.omci_tx_id.wrapping_add(1);
        if self.omci_tx_id == 0 {
            self.omci_tx_id = 1;
        }
        id
    }
}
