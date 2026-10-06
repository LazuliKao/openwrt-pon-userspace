# SPDX-License-Identifier: GPL-2.0-only

include $(TOPDIR)/rules.mk

PKG_NAME:=h3c-fttr-tools
PKG_VERSION:=0.1.0
PKG_RELEASE:=1
PKG_URL:=https://github.com/pbs05/openwrt-pon-userspace/tree/main/h3c-fttr-tools

PKG_LICENSE:=GPL-2.0-only
PKG_BUILD_DEPENDS:=rust/host
PKG_BUILD_PARALLEL:=1

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/rust/rust-package.mk

define Package/h3c-fttr-tools
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=PON
  TITLE:=H3C HM2004-DU FTTR Micro-OLT utilities and controller daemon
  DEPENDS:=$(RUST_ARCH_DEPENDS) +kmod-h3c-fmcs +mosquitto-client
endef

define Package/h3c-fttr-tools/description
 Pure-Rust userspace FPGA programmer, optical BOSA management, and FTTR controller daemon for H3C HM2004-DU.
 Manages downlinked stock HL202-DU sub-gateways over optical bridge and MQTT.
endef

define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
	$(CP) ./src/Cargo.toml ./src/Cargo.lock $(PKG_BUILD_DIR)/
	$(CP) ./src/src $(PKG_BUILD_DIR)/
endef

define Package/h3c-fttr-tools/install
	$(INSTALL_DIR) $(1)/usr/sbin
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/fttrctl $(1)/usr/sbin/
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/fttrd $(1)/usr/sbin/
	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) ./files/fttr.init $(1)/etc/init.d/fttr
endef

$(eval $(call RustBinPackage,h3c-fttr-tools))
$(eval $(call BuildPackage,h3c-fttr-tools))
