use socketcan::{CanFilter, EmbeddedFrame, SocketOptions};
use zencan_client::common::{SocketCanReceiver, SocketCanSender};

use crate::Error;
use crate::MAX_MODULES;
use crate::MasterInfo;
use crate::SdoRequest;
use crate::SdoResponse;
use crate::VartaEasyblade;
use crate::varta_easyblade;
use crate::varta_easyblade_can_messages;
use varta_easyblade::BatteryCapacity;
use varta_easyblade::BatteryCapacityParam;
use varta_easyblade::BatteryChargeCurrent;
use varta_easyblade::BatteryChargeCurrentDecreaseStepsize1;
use varta_easyblade::BatteryChargeCurrentDecreaseStepsize2;
use varta_easyblade::BatteryChargeCurrentIncreaseStepsize1;
use varta_easyblade::BatteryChargeCurrentIncreaseStepsize2;
use varta_easyblade::BatteryChargeCurrentKeepPower;
use varta_easyblade::BatteryChargeCurrentMaxHigh;
use varta_easyblade::BatteryChargeCurrentMaxLow;
use varta_easyblade::BatteryChargeCurrentMaxNormal;
use varta_easyblade::BatteryChargeCurrentModifyInterval;
use varta_easyblade::BatteryChargeCurrentValid;
use varta_easyblade::BatteryChargeTemperature;
use varta_easyblade::BatteryChargeVoltage;
use varta_easyblade::BatteryCurrent;
use varta_easyblade::BatteryCurrentLimit;
use varta_easyblade::BatteryCycleCount;
use varta_easyblade::BatteryVoltage;
use varta_easyblade::BatteryVoltageLimit;
use varta_easyblade::CellBalanceLimit;
use varta_easyblade::CellBalanceStatus;
use varta_easyblade::CellImpedance;
use varta_easyblade::CellTemperature;
use varta_easyblade::CellTemperatureLimit;
use varta_easyblade::CellTemperatureMinMax;
use varta_easyblade::CellVoltageLimit;
use varta_easyblade::CellVoltageMinMax;
use varta_easyblade::CellVoltages;
use varta_easyblade::DeviceConfigInfo;
use varta_easyblade::DeviceControlParam;
use varta_easyblade::DeviceDateInfo;
use varta_easyblade::DeviceErrorCounterInfo;
use varta_easyblade::DeviceErrorHistory;
use varta_easyblade::DeviceOperationTime;
use varta_easyblade::DeviceSerialNumberInfo;
use varta_easyblade::DeviceVariantInfo;
use varta_easyblade::FetTemperature;
use varta_easyblade::FetTemperatureLimit;
use varta_easyblade::FetTemperatureMinMax;
use varta_easyblade::HardwareVersion;
use varta_easyblade::KeepPowerTimer;
use varta_easyblade::MasterBatteryTemperature;
use varta_easyblade::MsgBits;
use varta_easyblade::Sdo;
use varta_easyblade::SerialNumber;
use varta_easyblade::SoftwareVersion;

/// A master (pack-level) PDO message, as broadcast by the master module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MasterPdo {
    /// `MasterPackInfo1`: pack voltage and current.
    PackInfo1,
    /// `MasterChargeControl`: charge voltage/current request and battery status.
    ChargeControl,
    /// `MasterPackInfo2`: max FET/cell temperature and design capacity.
    PackInfo2,
    /// `MasterPackInfo3`: full-charge and remaining capacity (SOC is derived from these).
    PackInfo3,
    /// `MasterErrorMsgs`: info/warning/error/charge bits.
    ErrorMsgs,
}

/// A per-module PDO message, as broadcast by each Easyblade module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModulePdo {
    /// `PACKnn_PackInfo1`: module voltage and current.
    PackInfo1,
    /// `PACKnn_Info2`: module temperatures and charge requests (currently not stored).
    Info2,
    /// `PACKnn_Info3`: module capacities (SOC and SOH are derived from these).
    Info3,
    /// `PACKnn_MSGS`: module info/warning/error/charge bits.
    Msgs,
}

/// Which PDO a received CAN frame carried, i.e. which part of [`Varta`] it updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PdoUpdate {
    /// The frame updated [`Varta::master`].
    Master(MasterPdo),
    /// The frame updated `Varta::easyblades[node_id]`.
    Module { node_id: u8, pdo: ModulePdo },
}

impl PdoUpdate {
    /// The module this update belongs to, or `None` for master updates.
    pub fn node_id(&self) -> Option<u8> {
        match self {
            PdoUpdate::Master(_) => None,
            PdoUpdate::Module { node_id, .. } => Some(*node_id),
        }
    }
}

/// The outcome of processing one CAN frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameEvent {
    /// `Some(node_id)` if the frame is the first one seen from that module
    /// (either at startup or after the module was expired).
    pub new_module: Option<u8>,
    /// Which PDO the frame carried.
    pub update: PdoUpdate,
}

// `classify_message` takes the node ID straight from the message name (the DBC
// defines PACK01..=PACK15) and `handle_frame` indexes `easyblades` with it.
const _: () = assert!(MAX_MODULES > 15, "MAX_MODULES must cover node IDs 1..=15");

/// Maps a decoded CAN message to the part of [`Varta`] it updates.
fn classify_message(msg: &varta_easyblade_can_messages::Messages) -> PdoUpdate {
    use varta_easyblade_can_messages::Messages as M;

    let (node_id, pdo) = match msg {
        M::MasterPackInfo1(_) => return PdoUpdate::Master(MasterPdo::PackInfo1),
        M::MasterChargeControl(_) => return PdoUpdate::Master(MasterPdo::ChargeControl),
        M::MasterPackInfo2(_) => return PdoUpdate::Master(MasterPdo::PackInfo2),
        M::MasterPackInfo3(_) => return PdoUpdate::Master(MasterPdo::PackInfo3),
        M::MasterErrorMsgs(_) => return PdoUpdate::Master(MasterPdo::ErrorMsgs),
        M::Pack01PackInfo1(_) => (1, ModulePdo::PackInfo1),
        M::Pack01Info2(_) => (1, ModulePdo::Info2),
        M::Pack01Info3(_) => (1, ModulePdo::Info3),
        M::Pack01Msgs(_) => (1, ModulePdo::Msgs),
        M::Pack02PackInfo1(_) => (2, ModulePdo::PackInfo1),
        M::Pack02Info2(_) => (2, ModulePdo::Info2),
        M::Pack02Info3(_) => (2, ModulePdo::Info3),
        M::Pack02Msgs(_) => (2, ModulePdo::Msgs),
        M::Pack03PackInfo1(_) => (3, ModulePdo::PackInfo1),
        M::Pack03Info2(_) => (3, ModulePdo::Info2),
        M::Pack03Info3(_) => (3, ModulePdo::Info3),
        M::Pack03Msgs(_) => (3, ModulePdo::Msgs),
        M::Pack04PackInfo1(_) => (4, ModulePdo::PackInfo1),
        M::Pack04Info2(_) => (4, ModulePdo::Info2),
        M::Pack04Info3(_) => (4, ModulePdo::Info3),
        M::Pack04Msgs(_) => (4, ModulePdo::Msgs),
        M::Pack05PackInfo1(_) => (5, ModulePdo::PackInfo1),
        M::Pack05Info2(_) => (5, ModulePdo::Info2),
        M::Pack05Info3(_) => (5, ModulePdo::Info3),
        M::Pack05Msgs(_) => (5, ModulePdo::Msgs),
        M::Pack06PackInfo1(_) => (6, ModulePdo::PackInfo1),
        M::Pack06Info2(_) => (6, ModulePdo::Info2),
        M::Pack06Info3(_) => (6, ModulePdo::Info3),
        M::Pack06Msgs(_) => (6, ModulePdo::Msgs),
        M::Pack07PackInfo1(_) => (7, ModulePdo::PackInfo1),
        M::Pack07Info2(_) => (7, ModulePdo::Info2),
        M::Pack07Info3(_) => (7, ModulePdo::Info3),
        M::Pack07Msgs(_) => (7, ModulePdo::Msgs),
        M::Pack08PackInfo1(_) => (8, ModulePdo::PackInfo1),
        M::Pack08Info2(_) => (8, ModulePdo::Info2),
        M::Pack08Info3(_) => (8, ModulePdo::Info3),
        M::Pack08Msgs(_) => (8, ModulePdo::Msgs),
        M::Pack09PackInfo1(_) => (9, ModulePdo::PackInfo1),
        M::Pack09Info2(_) => (9, ModulePdo::Info2),
        M::Pack09Info3(_) => (9, ModulePdo::Info3),
        M::Pack09Msgs(_) => (9, ModulePdo::Msgs),
        M::Pack10PackInfo1(_) => (10, ModulePdo::PackInfo1),
        M::Pack10Info2(_) => (10, ModulePdo::Info2),
        M::Pack10Info3(_) => (10, ModulePdo::Info3),
        M::Pack10Msgs(_) => (10, ModulePdo::Msgs),
        M::Pack11PackInfo1(_) => (11, ModulePdo::PackInfo1),
        M::Pack11Info2(_) => (11, ModulePdo::Info2),
        M::Pack11Info3(_) => (11, ModulePdo::Info3),
        M::Pack11Msgs(_) => (11, ModulePdo::Msgs),
        M::Pack12PackInfo1(_) => (12, ModulePdo::PackInfo1),
        M::Pack12Info2(_) => (12, ModulePdo::Info2),
        M::Pack12Info3(_) => (12, ModulePdo::Info3),
        M::Pack12Msgs(_) => (12, ModulePdo::Msgs),
        M::Pack13PackInfo1(_) => (13, ModulePdo::PackInfo1),
        M::Pack13Info2(_) => (13, ModulePdo::Info2),
        M::Pack13Info3(_) => (13, ModulePdo::Info3),
        M::Pack13Msgs(_) => (13, ModulePdo::Msgs),
        M::Pack14PackInfo1(_) => (14, ModulePdo::PackInfo1),
        M::Pack14Info2(_) => (14, ModulePdo::Info2),
        M::Pack14Info3(_) => (14, ModulePdo::Info3),
        M::Pack14Msgs(_) => (14, ModulePdo::Msgs),
        M::Pack15PackInfo1(_) => (15, ModulePdo::PackInfo1),
        M::Pack15Info2(_) => (15, ModulePdo::Info2),
        M::Pack15Info3(_) => (15, ModulePdo::Info3),
        M::Pack15Msgs(_) => (15, ModulePdo::Msgs),
    };

    PdoUpdate::Module { node_id, pdo }
}

#[derive(Debug)]
pub struct Varta {
    pub socketcan_interface: socketcan::tokio::AsyncCanSocket<socketcan::CanSocket>,
    pub canbus_interface: String,
    pub master: MasterInfo,
    pub easyblades: [Option<VartaEasyblade>; MAX_MODULES],
}

// Public API
impl Varta {
    pub async fn new(canbus_interface: &str) -> Result<Self, Error> {
        let socketcan_interface =
            socketcan::tokio::CanSocket::open(canbus_interface).map_err(|e| Error::Io {
                can_interface: String::from(canbus_interface),
                e,
            })?;

        socketcan_interface
            .set_filters(&[
                CanFilter::new(0x180, 0x7f0), // module info1: voltage & current for each module
                CanFilter::new(0x280, 0x7f0), // module info2: temps & charge requests for each module
                CanFilter::new(0x380, 0x7f0), // module info3: capacities for each module
                CanFilter::new(0x480, 0x7f0), // module msgs: FET status for each module
                CanFilter::new(0x264, 0x7ff), // master charge control
                CanFilter::new(0x19b, 0x7ff), // master info1: voltage & current for pack
                CanFilter::new(0x29b, 0x7ff), // master info2: temps & design capacity
                CanFilter::new(0x39b, 0x7ff), // master info3: full & remaining capacity
            ])
            .map_err(|e| Error::Io {
                can_interface: String::from(canbus_interface),
                e,
            })?;

        let varta = Self {
            socketcan_interface,
            canbus_interface: String::from(canbus_interface),
            master: MasterInfo::default(),
            easyblades: [const { None }; MAX_MODULES],
        };
        Ok(varta)
    }

    fn handle_frame(&mut self, can_frame: socketcan::frame::CanFrame) -> Result<FrameEvent, Error> {
        let msg = crate::varta_easyblade_can_messages::Messages::from_can_message(
            can_frame.id(),
            can_frame.data(),
        )?;

        let update = classify_message(&msg);

        let mut new_module: Option<u8> = None;

        if let Some(node_id) = update.node_id() {
            let was_new = self.easyblades[node_id as usize].is_none();
            let easyblade = self.get_or_init_easyblade(node_id);
            if was_new {
                new_module = Some(node_id);
            }
            easyblade.last_seen = std::time::SystemTime::now();
            match msg {
                varta_easyblade_can_messages::Messages::Pack01PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack02PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack03PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack04PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack05PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack06PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack07PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack08PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack09PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack10PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack11PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack12PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack13PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack14PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack15PackInfo1(m) => {
                    Self::update_easyblade_voltage_current(easyblade, m.voltage(), m.current());
                },
                varta_easyblade_can_messages::Messages::Pack01Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack02Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack03Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack04Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack05Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack06Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack07Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack08Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack09Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack10Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack11Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack12Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack13Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack14Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack15Info3(m) => {
                    Self::update_easyblade_soc_soh(
                        easyblade,
                        m.battery_full_cap(),
                        m.battery_rem_cap(),
                        m.battery_design_cap(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack01Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack02Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_16_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack03Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack04Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack05Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack06Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack07Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack08Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack09Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack10Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack11Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack12Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack13Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack14Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                varta_easyblade_can_messages::Messages::Pack15Msgs(m) => {
                    Self::update_easyblade_pack_msgs(
                        easyblade,
                        m.info_bit_0_empty(),
                        m.info_bit_1_almost_empty(),
                        m.info_bit_2_chgfet_closed(),
                        m.info_bit_3_dsgfet_closed(),
                        m.info_bit_4_bypass_fet_on(),
                        m.info_bit_6_fully_charged(),
                        m.warn_bit_0_low_voltage(),
                        m.warn_bit_1_low_soc(),
                        m.warn_bit_2_reserve_soc(),
                        m.warn_bit_3_over_or_under_temp_discharge(),
                        m.warn_bit_4_over_or_under_temp_charge(),
                        m.warn_bit_7_max_charge_condition_recuperation(),
                        m.warn_bit_11_can_network_failure(),
                        m.warn_bit_12_set_deactivation_enable(),
                        m.warn_bit_14_set_node_id_process_enable(),
                        m.warn_bit_15_unknown(),
                        m.error_bit_0_error_lock_flag_discharge(),
                        m.error_bit_1_error_lock_flag_charge(),
                        m.error_bit_2_over_charge_condition_recuperation(),
                        m.error_bit_3_shortcircuit_charge_alarm(),
                        m.error_bit_4_shortcircuit_discharge_alarm(),
                        m.error_bit_5_max_voltage_alarm(),
                        m.error_bit_6_discharge_fet_error(),
                        m.error_bit_7_charge_fet_error(),
                        m.error_bit_8_max_charge_current_alarm(),
                        m.error_bit_9_max_discharge_current_alarm(),
                        m.error_bit_10_under_charge_alarm(),
                        m.error_bit_11_over_charge_alarm(),
                        m.error_bit_12_over_under_temp_charge(),
                        m.error_bit_13_over_under_temp_discharge(),
                        m.error_bit_14_module_defect(),
                        m.error_bit_15_uknown(),
                        m.charge_bit_0_charge_voltage_enabled(),
                        m.charge_bit_1_charge_voltage_keep_power(),
                        m.charge_bit_4_charge_current_enable(),
                        m.charge_bit_5_charge_current_keep_power(),
                        m.charge_bit_6_charge_current_low_temp_range(),
                        m.charge_bit_7_charge_current_normal_temp_range(),
                        m.charge_bit_8_charge_current_high_temp_range(),
                        m.charge_bit_10_charge_max_charge_current_request(),
                        m.charge_bit_11_charge_max_charge_cell_voltage_request(),
                        m.charge_bit_12_charge_master_set_charger_output_off(),
                        m.charge_bit_13_charge_fet_disable_temp_range_cells(),
                        m.charge_bit_14_master_charger_control_charging_ready(),
                        m.charge_bit_15_charger_supply_conditions_ready(),
                    );
                },
                _ => {},
            }
        }

        match msg {
            varta_easyblade_can_messages::Messages::MasterPackInfo1(m) => {
                self.master.voltage = Some(m.voltage());
                self.master.current = Some(m.current());
                self.master.last_seen = Some(std::time::SystemTime::now());
            },
            varta_easyblade_can_messages::Messages::MasterChargeControl(m) => {
                self.master.charge_voltage_request = Some(m.charge_voltage_request());
                self.master.charge_current_request = Some(m.charge_current_request());
                self.master.battery_status = Some(m.battery_status());
                self.master.last_seen = Some(std::time::SystemTime::now());
            },
            varta_easyblade_can_messages::Messages::MasterPackInfo2(m) => {
                self.master.max_battery_fet_temp = Some(m.max_battery_fet_temp());
                self.master.max_battery_cell_temp = Some(m.max_battery_cell_temp());
                self.master.master_design_capacity = Some(m.master_design_capacity());
                self.master.last_seen = Some(std::time::SystemTime::now());
            },
            varta_easyblade_can_messages::Messages::MasterPackInfo3(m) => {
                let full = m.master_full_charge_capacity();
                let remaining = m.master_remaining_capacity();
                self.master.master_full_charge_capacity = Some(full);
                self.master.master_remaining_capacity = Some(remaining);
                self.master.soc = if full > 0.0 {
                    Some((remaining / full) * 100.0)
                } else {
                    None
                };
                self.master.last_seen = Some(std::time::SystemTime::now());
            },
            varta_easyblade_can_messages::Messages::MasterErrorMsgs(m) => {
                self.master.master_msgs = Some(MsgBits {
                    info_bit_0_empty: m.info_bit_0_empty(),
                    info_bit_1_almost_empty: m.info_bit_1_almost_empty(),
                    info_bit_2_chgfet_closed: m.info_bit_2_chgfet_closed(),
                    info_bit_3_dsgfet_closed: m.info_bit_3_dsgfet_closed(),
                    info_bit_4_bypass_fet_on: m.info_bit_4_bypass_fet_on(),
                    info_bit_6_fully_charged: m.info_bit_6_fully_charged(),
                    warn_bit_0_low_voltage: m.warn_bit_0_low_voltage(),
                    warn_bit_1_low_soc: m.warn_bit_1_low_soc(),
                    warn_bit_2_reserve_soc: m.warn_bit_2_reserve_soc(),
                    warn_bit_3_over_or_under_temp_discharge: m
                        .warn_bit_3_over_or_under_temp_discharge(),
                    warn_bit_4_over_or_under_temp_charge: m.warn_bit_4_over_or_under_temp_charge(),
                    warn_bit_7_max_charge_condition_recuperation: m
                        .warn_bit_7_max_charge_condition_recuperation(),
                    warn_bit_11_can_network_failure: m.warn_bit_11_can_network_failure(),
                    warn_bit_12_set_deactivation_enable: m.warn_bit_12_set_deactivation_enable(),
                    warn_bit_14_set_node_id_process_enable: m
                        .warn_bit_14_set_node_id_process_enable(),
                    warn_bit_15_unknown: m.warn_bit_15_unknown(),
                    error_bit_0_error_lock_flag_discharge: m
                        .error_bit_0_error_lock_flag_discharge(),
                    error_bit_1_error_lock_flag_charge: m.error_bit_1_error_lock_flag_charge(),
                    error_bit_2_over_charge_condition_recuperation: m
                        .error_bit_2_over_charge_condition_recuperation(),
                    error_bit_3_shortcircuit_charge_alarm: m
                        .error_bit_3_shortcircuit_charge_alarm(),
                    error_bit_4_shortcircuit_discharge_alarm: m
                        .error_bit_4_shortcircuit_discharge_alarm(),
                    error_bit_5_max_voltage_alarm: m.error_bit_5_max_voltage_alarm(),
                    error_bit_6_discharge_fet_error: m.error_bit_6_discharge_fet_error(),
                    error_bit_7_charge_fet_error: m.error_bit_7_charge_fet_error(),
                    error_bit_8_max_charge_current_alarm: m.error_bit_8_max_charge_current_alarm(),
                    error_bit_9_max_discharge_current_alarm: m
                        .error_bit_9_max_discharge_current_alarm(),
                    error_bit_10_under_charge_alarm: m.error_bit_10_under_charge_alarm(),
                    error_bit_11_over_charge_alarm: m.error_bit_11_over_charge_alarm(),
                    error_bit_12_over_under_temp_charge: m.error_bit_12_over_under_temp_charge(),
                    error_bit_13_over_under_temp_discharge: m
                        .error_bit_13_over_under_temp_discharge(),
                    error_bit_14_module_defect: m.error_bit_14_module_defect(),
                    error_bit_15_uknown: m.error_bit_15_uknown(),
                    charge_bit_0_charge_voltage_enabled: m.charge_bit_0_charge_voltage_enabled(),
                    charge_bit_1_charge_voltage_keep_power: m
                        .charge_bit_1_charge_voltage_keep_power(),
                    charge_bit_4_charge_current_enable: m.charge_bit_4_charge_current_enable(),
                    charge_bit_5_charge_current_keep_power: m
                        .charge_bit_5_charge_current_keep_power(),
                    charge_bit_6_charge_current_low_temp_range: m
                        .charge_bit_6_charge_current_low_temp_range(),
                    charge_bit_7_charge_current_normal_temp_range: m
                        .charge_bit_7_charge_current_normal_temp_range(),
                    charge_bit_8_charge_current_high_temp_range: m
                        .charge_bit_8_charge_current_high_temp_range(),
                    charge_bit_10_charge_max_charge_current_request: m
                        .charge_bit_10_charge_max_charge_current_request(),
                    charge_bit_11_charge_max_charge_cell_voltage_request: m
                        .charge_bit_11_charge_max_charge_cell_voltage_request(),
                    charge_bit_12_charge_master_set_charger_output_off: m
                        .charge_bit_12_charge_master_set_charger_output_off(),
                    charge_bit_13_charge_fet_disable_temp_range_cells: m
                        .charge_bit_13_charge_fet_disable_temp_range_cells(),
                    charge_bit_14_master_charger_control_charging_ready: m
                        .charge_bit_14_master_charger_control_charging_ready(),
                    charge_bit_15_charger_supply_conditions_ready: m
                        .charge_bit_15_charger_supply_conditions_ready(),
                });
                self.master.last_seen = Some(std::time::SystemTime::now());
            },
            _ => {},
        }

        self.expire_missing_modules();

        Ok(FrameEvent { new_module, update })
    }

    // Process up to `max_to_process` CAN messages. Returns early if
    // it discovers a new node, or if there are no more CAN frames
    // to process.
    pub fn process_socketcan_messages(
        &mut self,
        max_to_process: usize,
    ) -> Result<Option<u8>, Error> {
        for _ in 0..max_to_process {
            match self.socketcan_interface.try_read_frame() {
                Ok(frame) => {
                    if let Some(new_node) = self.handle_frame(frame)?.new_module {
                        return Ok(Some(new_node));
                    }
                },
                Err(e) => {
                    return Err(Error::Io {
                        can_interface: self.canbus_interface.clone(),
                        e,
                    });
                },
            }
        }
        println!("read all {max_to_process}!");
        Ok(None)
    }

    /// Wait for and process one CAN frame, reporting which PDO it carried
    /// (and therefore which part of `self` it updated).
    pub async fn process_socketcan_frame(&mut self) -> Result<FrameEvent, Error> {
        let can_frame = self
            .socketcan_interface
            .read_frame()
            .await
            .map_err(|e| Error::Io {
                can_interface: self.canbus_interface.clone(),
                e,
            })?;
        self.handle_frame(can_frame)
    }

    /// Wait for and process one CAN frame. Returns the node ID if the frame
    /// revealed a new module. See [`Self::process_socketcan_frame`] for a
    /// variant that also reports what the frame updated.
    pub async fn process_socketcan_msg(&mut self) -> Result<Option<u8>, Error> {
        Ok(self.process_socketcan_frame().await?.new_module)
    }

    /// Expire any modules that have not reported in the last 10 seconds.
    pub fn expire_missing_modules(&mut self) {
        let now = std::time::SystemTime::now();
        for entry in self.easyblades.iter_mut() {
            if entry
                .as_ref()
                .and_then(|eb| now.duration_since(eb.last_seen).ok())
                .map(|d| d.as_secs() > 10)
                .unwrap_or(false)
            {
                entry.take();
            }
        }
    }

    /// Returns the duration until the oldest module should be expired, or 10s if no modules exist.
    pub fn next_expiry_delay(&self) -> std::time::Duration {
        let now = std::time::SystemTime::now();
        let oldest_last_seen = self
            .easyblades
            .iter()
            .filter_map(|e| e.as_ref())
            .map(|e| e.last_seen)
            .min();

        match oldest_last_seen {
            Some(last_seen) => {
                let expiry = last_seen
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .checked_add(std::time::Duration::from_secs(10))
                    .unwrap();
                let now_since_epoch = now.duration_since(std::time::UNIX_EPOCH).unwrap();
                expiry.saturating_sub(now_since_epoch)
            },
            None => std::time::Duration::from_secs(10),
        }
    }

    pub fn get_or_init_easyblade(&mut self, node_id: u8) -> &mut VartaEasyblade {
        if self.easyblades[node_id as usize].is_none() {
            self.easyblades[node_id as usize] = Some(VartaEasyblade {
                node_id,
                last_seen: std::time::SystemTime::now(),
                sdo: crate::varta_easyblade::SdoData {
                    serial_number: None,
                    software_version: None,
                    hardware_version: None,
                    cell_voltages: None,
                    device_errors: None,
                    device_config_info: None,
                    device_serial_number_info: None,
                    device_date_info: None,
                    device_variant_info: None,
                    device_control_param: None,
                    device_operation_time: None,
                    device_error_counter: None,
                    cell_voltage_min_max: None,
                    cell_voltage_limit: None,
                    battery_voltage: None,
                    battery_voltage_limit: None,
                    battery_current: None,
                    battery_current_limit: None,
                    fet_temperature: None,
                    fet_temperature_min_max: None,
                    fet_temperature_limit: None,
                    cell_temperature: None,
                    cell_temperature_min_max: None,
                    cell_temperature_limit: None,
                    cell_balance_status: None,
                    cell_balance_limit: None,
                    cell_impedance: None,
                    battery_capacity: None,
                    battery_capacity_param: None,
                    battery_cycle_count: None,
                    battery_charge_voltage: None,
                    battery_charge_current: None,
                    battery_charge_current_valid: None,
                    battery_charge_current_max_normal: None,
                    battery_charge_current_max_low: None,
                    battery_charge_current_max_high: None,
                    battery_charge_current_keep_power: None,
                    battery_charge_current_decrease_stepsize1: None,
                    battery_charge_current_increase_stepsize1: None,
                    battery_charge_current_decrease_stepsize2: None,
                    battery_charge_current_increase_stepsize2: None,
                    battery_charge_current_modify_interval: None,
                    battery_charge_temperature: None,
                    master_battery_temperature: None,
                    keep_power_timer: None,
                },
                pdo: crate::varta_easyblade::Pdo {
                    voltage: None,
                    current: None,
                    soc: None,
                    soh: None,
                    msg_bits: None,
                },
            });
        }
        self.easyblades[node_id as usize].as_mut().unwrap()
    }

    /// Create an SDO session for the given node ID.
    /// The session owns its own CAN socket and can be used independently
    /// from this Varta instance (e.g., in a separate tokio task).
    pub fn sdo_client(&self, node_id: u8) -> Result<SdoSession, Error> {
        let (tx, rx) =
            zencan_client::open_socketcan(&self.canbus_interface).map_err(|e| Error::Io {
                can_interface: self.canbus_interface.clone(),
                e,
            })?;
        Ok(SdoSession::from_socket(node_id, tx, rx))
    }

    /// Returns the easyblade at the given index (0-based) among active modules.
    pub fn get_easyblade_by_index(&self, index: usize) -> Option<&VartaEasyblade> {
        self.easyblades.iter().filter_map(|e| e.as_ref()).nth(index)
    }

    /// Returns the total number of active easyblade modules.
    pub fn easyblade_count(&self) -> usize {
        self.easyblades.iter().filter_map(|e| e.as_ref()).count()
    }
}

/// SDO session for a specific CAN node.
/// Owns its own CAN socket; create via [`Varta::sdo_client()`].
/// Can be used in a separate task while [`Varta`] processes PDOs.
pub struct SdoSession {
    node_id: u8,
    sdo_client: zencan_client::SdoClient<SocketCanSender, SocketCanReceiver>,
}

impl SdoSession {
    /// Create a new SDO session for the given CAN interface and node ID.
    /// Opens its own CAN socket. Useful when you don't have a [`Varta`] instance
    /// (e.g., in a standalone tokio task).
    pub fn new(canbus_interface: &str, node_id: u8) -> Result<Self, Error> {
        let (tx, rx) = zencan_client::open_socketcan(canbus_interface).map_err(|e| Error::Io {
            can_interface: String::from(canbus_interface),
            e,
        })?;
        Ok(Self::from_socket(node_id, tx, rx))
    }

    fn from_socket(node_id: u8, sender: SocketCanSender, receiver: SocketCanReceiver) -> Self {
        let sdo_client = zencan_client::SdoClient::new_std(node_id, sender, receiver);
        Self { node_id, sdo_client }
    }

    /// Read an SDO object from the node. Returns the response value.
    pub async fn read(&mut self, request: SdoRequest) -> Result<SdoResponse, String> {
        match request {
            SdoRequest::SerialNumber => {
                let bytes = self
                    .sdo_client
                    .upload(SerialNumber::INDEX, SerialNumber::SUBINDEX)
                    .await
                    .map_err(|e| e.to_string())?;
                if bytes.len() < 2 {
                    return Err("Serial number data too short".to_string());
                }
                Ok(SdoResponse::SerialNumber {
                    node_id: self.node_id,
                    value: SerialNumber {
                        value: u16::from_le_bytes([bytes[0], bytes[1]]),
                    },
                })
            },
            SdoRequest::SoftwareVersion => {
                let sw = self
                    .sdo_client
                    .upload(SoftwareVersion::INDEX, SoftwareVersion::SUBINDEX)
                    .await
                    .map_err(|e| e.to_string())?;
                let fw = self
                    .sdo_client
                    .upload(SoftwareVersion::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                let sw_str = String::from_utf8_lossy(&sw).trim_matches('\0').to_string();
                let fw_str = String::from_utf8_lossy(&fw).trim_matches('\0').to_string();
                Ok(SdoResponse::SoftwareVersion {
                    node_id: self.node_id,
                    value: SoftwareVersion { value: format!("{}{}", sw_str, fw_str) },
                })
            },
            SdoRequest::HardwareVersion => {
                let bytes = self
                    .sdo_client
                    .upload(HardwareVersion::INDEX, HardwareVersion::SUBINDEX)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::HardwareVersion {
                    node_id: self.node_id,
                    value: HardwareVersion {
                        value: String::from_utf8_lossy(&bytes)
                            .trim_matches('\0')
                            .to_string(),
                    },
                })
            },
            SdoRequest::DeviceErrorHistory => {
                let highest_subindex = self
                    .sdo_client
                    .read_u8(DeviceErrorHistory::INDEX, DeviceErrorHistory::SUBINDEX)
                    .await
                    .map_err(|e| e.to_string())?;
                if highest_subindex != 16 {
                    return Err(format!(
                        "Expected 16 error entries, got {}",
                        highest_subindex
                    ));
                }
                let mut errors = Vec::new();
                for sub_index in 1..=16u8 {
                    let val = self
                        .sdo_client
                        .read_u8(DeviceErrorHistory::INDEX, sub_index)
                        .await
                        .map_err(|e| e.to_string())?;
                    let e: varta_easyblade::DeviceError =
                        match varta_easyblade::DeviceError::try_from(val) {
                            Ok(e) => e,
                            Err(_) => varta_easyblade::DeviceError::Unknown,
                        };
                    errors.push(e);
                }
                Ok(SdoResponse::DeviceErrorHistory {
                    node_id: self.node_id,
                    value: DeviceErrorHistory { values: errors },
                })
            },
            SdoRequest::CellVoltages => {
                let highest_subindex = self
                    .sdo_client
                    .read_u8(CellVoltages::INDEX, 0x00)
                    .await
                    .map_err(|e| e.to_string())?;
                if highest_subindex != 16 {
                    return Err(format!(
                        "Expected 16 cell voltage entries, got {}",
                        highest_subindex
                    ));
                }
                let mut cell_voltages: Vec<f32> = Vec::new();
                for sub_index in 1..=14u8 {
                    let val = self
                        .sdo_client
                        .read_u32(CellVoltages::INDEX, sub_index)
                        .await
                        .map_err(|e| e.to_string())?;
                    cell_voltages.push((val as f32) / 1000.0);
                }
                Ok(SdoResponse::CellVoltages {
                    node_id: self.node_id,
                    value: CellVoltages { values: cell_voltages },
                })
            },
            SdoRequest::DeviceConfigInfo => {
                let c1 = self
                    .sdo_client
                    .upload(DeviceConfigInfo::INDEX, DeviceConfigInfo::SUBINDEX)
                    .await
                    .map_err(|e| e.to_string())?;
                let c2 = self
                    .sdo_client
                    .upload(DeviceConfigInfo::INDEX, DeviceConfigInfo::SUBINDEX + 1)
                    .await
                    .map_err(|e| e.to_string())?;
                let c3 = self
                    .sdo_client
                    .upload(DeviceConfigInfo::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::DeviceConfigInfo {
                    node_id: self.node_id,
                    value: DeviceConfigInfo {
                        config_1: String::from_utf8_lossy(&c1).trim_matches('\0').to_string(),
                        config_2: String::from_utf8_lossy(&c2).trim_matches('\0').to_string(),
                        config_3: String::from_utf8_lossy(&c3).trim_matches('\0').to_string(),
                    },
                })
            },
            SdoRequest::DeviceSerialNumberInfo => {
                let s1 = self
                    .sdo_client
                    .read_u32(
                        DeviceSerialNumberInfo::INDEX,
                        DeviceSerialNumberInfo::SUBINDEX,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let s2 = self
                    .sdo_client
                    .read_u32(
                        DeviceSerialNumberInfo::INDEX,
                        DeviceSerialNumberInfo::SUBINDEX + 1,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let s3 = self
                    .sdo_client
                    .read_u32(DeviceSerialNumberInfo::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::DeviceSerialNumberInfo {
                    node_id: self.node_id,
                    value: DeviceSerialNumberInfo {
                        serial_part_1: s1,
                        serial_part_2: s2,
                        serial_part_3: s3,
                    },
                })
            },
            SdoRequest::DeviceDateInfo => {
                let d1 = self
                    .sdo_client
                    .read_u16(DeviceDateInfo::INDEX, DeviceDateInfo::SUBINDEX)
                    .await
                    .map_err(|e| e.to_string())?;
                let d2 = self
                    .sdo_client
                    .read_u16(DeviceDateInfo::INDEX, DeviceDateInfo::SUBINDEX + 1)
                    .await
                    .map_err(|e| e.to_string())?;
                let d3 = self
                    .sdo_client
                    .read_u16(DeviceDateInfo::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::DeviceDateInfo {
                    node_id: self.node_id,
                    value: DeviceDateInfo { year: d1, month: d2, day: d3 },
                })
            },
            SdoRequest::DeviceVariantInfo => {
                let s1 = self
                    .sdo_client
                    .read_u8(DeviceVariantInfo::INDEX, DeviceVariantInfo::SUBINDEX)
                    .await
                    .map_err(|e| e.to_string())?;
                let s2 = self
                    .sdo_client
                    .read_u8(DeviceVariantInfo::INDEX, DeviceVariantInfo::SUBINDEX + 1)
                    .await
                    .map_err(|e| e.to_string())?;
                let s3 = self
                    .sdo_client
                    .read_u8(DeviceVariantInfo::INDEX, DeviceVariantInfo::SUBINDEX + 2)
                    .await
                    .map_err(|e| e.to_string())?;
                let s4 = self
                    .sdo_client
                    .read_u16(DeviceVariantInfo::INDEX, DeviceVariantInfo::SUBINDEX + 3)
                    .await
                    .map_err(|e| e.to_string())?;
                let s5 = self
                    .sdo_client
                    .read_u16(DeviceVariantInfo::INDEX, DeviceVariantInfo::SUBINDEX + 4)
                    .await
                    .map_err(|e| e.to_string())?;
                let s6 = self
                    .sdo_client
                    .read_u16(DeviceVariantInfo::INDEX, DeviceVariantInfo::SUBINDEX + 5)
                    .await
                    .map_err(|e| e.to_string())?;
                let s7 = self
                    .sdo_client
                    .read_u8(DeviceVariantInfo::INDEX, 0x07)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::DeviceVariantInfo {
                    node_id: self.node_id,
                    value: DeviceVariantInfo {
                        variant_1: s1,
                        variant_2: s2,
                        variant_3: s3,
                        variant_4: s4,
                        variant_5: s5,
                        variant_6: s6,
                        variant_7: s7,
                    },
                })
            },
            SdoRequest::DeviceControlParam => {
                let val = self
                    .sdo_client
                    .read_u16(DeviceControlParam::INDEX, DeviceControlParam::SUBINDEX)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::DeviceControlParam {
                    node_id: self.node_id,
                    value: DeviceControlParam { value: val },
                })
            },
            SdoRequest::DeviceOperationTime => {
                let m1 = self
                    .sdo_client
                    .read_u8(DeviceOperationTime::INDEX, DeviceOperationTime::SUBINDEX)
                    .await
                    .map_err(|e| e.to_string())?;
                let m2 = self
                    .sdo_client
                    .read_u8(
                        DeviceOperationTime::INDEX,
                        DeviceOperationTime::SUBINDEX + 1,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let m3 = self
                    .sdo_client
                    .read_u8(
                        DeviceOperationTime::INDEX,
                        DeviceOperationTime::SUBINDEX + 2,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let m4 = self
                    .sdo_client
                    .read_u8(
                        DeviceOperationTime::INDEX,
                        DeviceOperationTime::SUBINDEX + 3,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let m5 = self
                    .sdo_client
                    .read_u8(
                        DeviceOperationTime::INDEX,
                        DeviceOperationTime::SUBINDEX + 4,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let h1 = self
                    .sdo_client
                    .read_u32(
                        DeviceOperationTime::INDEX,
                        DeviceOperationTime::SUBINDEX + 5,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let h2 = self
                    .sdo_client
                    .read_u32(
                        DeviceOperationTime::INDEX,
                        DeviceOperationTime::SUBINDEX + 6,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let h3 = self
                    .sdo_client
                    .read_u32(
                        DeviceOperationTime::INDEX,
                        DeviceOperationTime::SUBINDEX + 7,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let h4 = self
                    .sdo_client
                    .read_u32(
                        DeviceOperationTime::INDEX,
                        DeviceOperationTime::SUBINDEX + 8,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let h5 = self
                    .sdo_client
                    .read_u32(DeviceOperationTime::INDEX, 0x0A)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::DeviceOperationTime {
                    node_id: self.node_id,
                    value: DeviceOperationTime {
                        minutes_below_zero: m1,
                        minutes_zero_to_40: m2,
                        minutes_40_to_60: m3,
                        minutes_60_to_80: m4,
                        minutes_above_80: m5,
                        hours_below_zero: h1,
                        hours_zero_to_40: h2,
                        hours_40_to_60: h3,
                        hours_60_to_80: h4,
                        hours_above_80: h5,
                    },
                })
            },
            SdoRequest::DeviceErrorCounter => {
                let over_temp_laden_zellen = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let under_temp_laden_zellen = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                let over_temp_laden_fet = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                let over_temp_entladen_zellen = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x04)
                    .await
                    .map_err(|e| e.to_string())?;
                let under_temp_entladen_zellen = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x05)
                    .await
                    .map_err(|e| e.to_string())?;
                let over_temp_entladen_fet = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x06)
                    .await
                    .map_err(|e| e.to_string())?;
                let over_temp_clamp = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x07)
                    .await
                    .map_err(|e| e.to_string())?;
                let over_voltage = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x08)
                    .await
                    .map_err(|e| e.to_string())?;
                let under_voltage = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x09)
                    .await
                    .map_err(|e| e.to_string())?;
                let deep_low_voltage = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x0a)
                    .await
                    .map_err(|e| e.to_string())?;
                let cell_disbalance = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x0b)
                    .await
                    .map_err(|e| e.to_string())?;
                let akku_pack_spn_min_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x0c)
                    .await
                    .map_err(|e| e.to_string())?;
                let akku_pack_spn_max_alarm = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x0d)
                    .await
                    .map_err(|e| e.to_string())?;
                let akku_pack_fused_spn_diff_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x0e)
                    .await
                    .map_err(|e| e.to_string())?;
                let akku_pwr_spn_diff_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x0f)
                    .await
                    .map_err(|e| e.to_string())?;
                let akku_pwr_spn_min_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x10)
                    .await
                    .map_err(|e| e.to_string())?;
                let akku_pwr_spn_max_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x11)
                    .await
                    .map_err(|e| e.to_string())?;
                let akku_netz_spn_min_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x12)
                    .await
                    .map_err(|e| e.to_string())?;
                let akku_netz_spn_max_alarm = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x13)
                    .await
                    .map_err(|e| e.to_string())?;
                let akku_rekuperation_spn_max_alarm = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x14)
                    .await
                    .map_err(|e| e.to_string())?;
                let i_charge_sc = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x15)
                    .await
                    .map_err(|e| e.to_string())?;
                let i_charge_occ_1 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x16)
                    .await
                    .map_err(|e| e.to_string())?;
                let i_charge_occ_2 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x17)
                    .await
                    .map_err(|e| e.to_string())?;
                let i_charge_occ_3 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x18)
                    .await
                    .map_err(|e| e.to_string())?;
                let i_discharge_sc = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x19)
                    .await
                    .map_err(|e| e.to_string())?;
                let i_discharge_ocd_1 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x1a)
                    .await
                    .map_err(|e| e.to_string())?;
                let i_discharge_ocd_2 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x1b)
                    .await
                    .map_err(|e| e.to_string())?;
                let i_discharge_ocd_3 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x1c)
                    .await
                    .map_err(|e| e.to_string())?;
                let i_akku_diff_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x1d)
                    .await
                    .map_err(|e| e.to_string())?;
                let scnd_spn_min_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x1e)
                    .await
                    .map_err(|e| e.to_string())?;
                let scnd_spn_max_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x1f)
                    .await
                    .map_err(|e| e.to_string())?;
                let scnd_uc_fet_enable = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x20)
                    .await
                    .map_err(|e| e.to_string())?;
                let scnd_current_sense_ein = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x21)
                    .await
                    .map_err(|e| e.to_string())?;
                let scnd_voltage_sense_ein = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x22)
                    .await
                    .map_err(|e| e.to_string())?;
                let scnd_temp_cell_sense_ein = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x23)
                    .await
                    .map_err(|e| e.to_string())?;
                let scnd_temp_fet_sense_ein = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x24)
                    .await
                    .map_err(|e| e.to_string())?;
                let scnd_pyro_fuse_eject_sense_ein = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x25)
                    .await
                    .map_err(|e| e.to_string())?;
                let scnd_i_discharge_fet_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x26)
                    .await
                    .map_err(|e| e.to_string())?;
                let scnd_i_charge_fet_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x27)
                    .await
                    .map_err(|e| e.to_string())?;
                let scnd_voltage_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x28)
                    .await
                    .map_err(|e| e.to_string())?;
                let adc_spn_min_scale = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x29)
                    .await
                    .map_err(|e| e.to_string())?;
                let adc_spn_max_scale = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x2a)
                    .await
                    .map_err(|e| e.to_string())?;
                let adc_temp_zellen_min_scale = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x2b)
                    .await
                    .map_err(|e| e.to_string())?;
                let adc_temp_zellen_max_scale = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x2c)
                    .await
                    .map_err(|e| e.to_string())?;
                let adc_temp_fet_min_scale = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x2d)
                    .await
                    .map_err(|e| e.to_string())?;
                let adc_temp_fet_max_scale = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x2e)
                    .await
                    .map_err(|e| e.to_string())?;
                let adc_temp_clamp_min_scale = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x2f)
                    .await
                    .map_err(|e| e.to_string())?;
                let adc_temp_clamp_max_scale = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x30)
                    .await
                    .map_err(|e| e.to_string())?;
                let adc_i_charge_min_scale = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x31)
                    .await
                    .map_err(|e| e.to_string())?;
                let adc_i_charge_max_scale = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x32)
                    .await
                    .map_err(|e| e.to_string())?;
                let adc_i_discharge_min_scale = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x33)
                    .await
                    .map_err(|e| e.to_string())?;
                let adc_i_discharge_max_scale = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x34)
                    .await
                    .map_err(|e| e.to_string())?;
                let i_discharge_fet_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x35)
                    .await
                    .map_err(|e| e.to_string())?;
                let i_charge_fet_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x36)
                    .await
                    .map_err(|e| e.to_string())?;
                let i_discharge_charge_fet_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x37)
                    .await
                    .map_err(|e| e.to_string())?;
                let temp_discharge_error_lock = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x38)
                    .await
                    .map_err(|e| e.to_string())?;
                let temp_charge_error_lock = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x39)
                    .await
                    .map_err(|e| e.to_string())?;
                let over_charge_current_alarm_recuperation = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x3a)
                    .await
                    .map_err(|e| e.to_string())?;
                let over_charge_cell_voltage_alarm_recuperation = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x3b)
                    .await
                    .map_err(|e| e.to_string())?;
                let v24_spn_min_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x3c)
                    .await
                    .map_err(|e| e.to_string())?;
                let v24_spn_max_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x3d)
                    .await
                    .map_err(|e| e.to_string())?;
                let can_network_not_conf_node_id = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x3e)
                    .await
                    .map_err(|e| e.to_string())?;
                let can_network_double_node_id = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x3f)
                    .await
                    .map_err(|e| e.to_string())?;
                let parameter_configuration_error = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x40)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_01 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x41)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_02 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x42)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_03 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x43)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_04 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x44)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_05 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x45)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_06 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x46)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_07 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x47)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_08 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x48)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_09 = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x49)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_0a = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x4a)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_0b = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x4b)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_0c = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x4c)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_0d = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x4d)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_0e = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x4e)
                    .await
                    .map_err(|e| e.to_string())?;
                let reserved_0f = self
                    .sdo_client
                    .read_u16(DeviceErrorCounterInfo::INDEX, 0x4f)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::DeviceErrorCounter {
                    node_id: self.node_id,
                    value: DeviceErrorCounterInfo {
                        over_temp_laden_zellen,
                        under_temp_laden_zellen,
                        over_temp_laden_fet,
                        over_temp_entladen_zellen,
                        under_temp_entladen_zellen,
                        over_temp_entladen_fet,
                        over_temp_clamp,
                        over_voltage,
                        under_voltage,
                        deep_low_voltage,
                        cell_disbalance,
                        akku_pack_spn_min_error,
                        akku_pack_spn_max_alarm,
                        akku_pack_fused_spn_diff_error,
                        akku_pwr_spn_diff_error,
                        akku_pwr_spn_min_error,
                        akku_pwr_spn_max_error,
                        akku_netz_spn_min_error,
                        akku_netz_spn_max_alarm,
                        akku_rekuperation_spn_max_alarm,
                        i_charge_sc,
                        i_charge_occ_1,
                        i_charge_occ_2,
                        i_charge_occ_3,
                        i_discharge_sc,
                        i_discharge_ocd_1,
                        i_discharge_ocd_2,
                        i_discharge_ocd_3,
                        i_akku_diff_error,
                        scnd_spn_min_error,
                        scnd_spn_max_error,
                        scnd_uc_fet_enable,
                        scnd_current_sense_ein,
                        scnd_voltage_sense_ein,
                        scnd_temp_cell_sense_ein,
                        scnd_temp_fet_sense_ein,
                        scnd_pyro_fuse_eject_sense_ein,
                        scnd_i_discharge_fet_error,
                        scnd_i_charge_fet_error,
                        scnd_voltage_error,
                        adc_spn_min_scale,
                        adc_spn_max_scale,
                        adc_temp_zellen_min_scale,
                        adc_temp_zellen_max_scale,
                        adc_temp_fet_min_scale,
                        adc_temp_fet_max_scale,
                        adc_temp_clamp_min_scale,
                        adc_temp_clamp_max_scale,
                        adc_i_charge_min_scale,
                        adc_i_charge_max_scale,
                        adc_i_discharge_min_scale,
                        adc_i_discharge_max_scale,
                        i_discharge_fet_error,
                        i_charge_fet_error,
                        i_discharge_charge_fet_error,
                        temp_discharge_error_lock,
                        temp_charge_error_lock,
                        over_charge_current_alarm_recuperation,
                        over_charge_cell_voltage_alarm_recuperation,
                        v24_spn_min_error,
                        v24_spn_max_error,
                        can_network_not_conf_node_id,
                        can_network_double_node_id,
                        parameter_configuration_error,
                        reserved_01,
                        reserved_02,
                        reserved_03,
                        reserved_04,
                        reserved_05,
                        reserved_06,
                        reserved_07,
                        reserved_08,
                        reserved_09,
                        reserved_0a,
                        reserved_0b,
                        reserved_0c,
                        reserved_0d,
                        reserved_0e,
                        reserved_0f,
                    },
                })
            },
            SdoRequest::CellVoltageMinMax => {
                let min = self
                    .sdo_client
                    .read_u32(CellVoltageMinMax::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let max = self
                    .sdo_client
                    .read_u32(CellVoltageMinMax::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::CellVoltageMinMax {
                    node_id: self.node_id,
                    value: CellVoltageMinMax {
                        min_voltage_v: min as f32 / 1000.0,
                        max_voltage_v: max as f32 / 1000.0,
                    },
                })
            },
            SdoRequest::CellVoltageLimit => {
                let sub1 = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let sub2 = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                let sub3 = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                let sub4 = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x04)
                    .await
                    .map_err(|e| e.to_string())?;
                let sub5 = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x05)
                    .await
                    .map_err(|e| e.to_string())?;
                let sub6 = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x06)
                    .await
                    .map_err(|e| e.to_string())?;
                let sub7 = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x07)
                    .await
                    .map_err(|e| e.to_string())?;
                let sub8 = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x08)
                    .await
                    .map_err(|e| e.to_string())?;
                let sub9 = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x09)
                    .await
                    .map_err(|e| e.to_string())?;
                let suba = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x0a)
                    .await
                    .map_err(|e| e.to_string())?;
                let subb = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x0b)
                    .await
                    .map_err(|e| e.to_string())?;
                let subc = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x0c)
                    .await
                    .map_err(|e| e.to_string())?;
                let subd = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x0d)
                    .await
                    .map_err(|e| e.to_string())?;
                let sube = self
                    .sdo_client
                    .read_u32(CellVoltageLimit::INDEX, 0x0e)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::CellVoltageLimit {
                    node_id: self.node_id,
                    value: CellVoltageLimit {
                        over_voltage_error_v: sub1 as f32 / 1000.0,
                        max_charge_voltage_v: sub2 as f32 / 1000.0,
                        fully_charged_voltage_v: sub3 as f32 / 1000.0,
                        near_fully_charged_voltage_v: sub4 as f32 / 1000.0,
                        fully_charged_reset_voltage_v: sub5 as f32 / 1000.0,
                        edv_reset_voltage_v: sub6 as f32 / 1000.0,
                        near_empty_voltage_warning_v: sub7 as f32 / 1000.0,
                        near_empty_voltage_edv1_v: sub8 as f32 / 1000.0,
                        empty_voltage_edv0_v: sub9 as f32 / 1000.0,
                        min_error_reset_voltage_v: suba as f32 / 1000.0,
                        edv_off_voltage_v: subb as f32 / 1000.0,
                        under_voltage_error_v: subc as f32 / 1000.0,
                        deep_low_voltage_error_v: subd as f32 / 1000.0,
                        max_charge_voltage_no_password_v: sube as f32 / 1000.0,
                    },
                })
            },
            SdoRequest::BatteryVoltage => {
                let v1 = self
                    .sdo_client
                    .read_u32(BatteryVoltage::INDEX, BatteryVoltage::SUBINDEX)
                    .await
                    .map_err(|e| e.to_string())?;
                let v2 = self
                    .sdo_client
                    .read_u32(BatteryVoltage::INDEX, BatteryVoltage::SUBINDEX + 1)
                    .await
                    .map_err(|e| e.to_string())?;
                let v3 = self
                    .sdo_client
                    .read_u32(BatteryVoltage::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryVoltage {
                    node_id: self.node_id,
                    value: BatteryVoltage {
                        sum_of_cell_voltage_v: v1 as f32 / 1000.0,
                        internal_connector_voltage_v: v2 as f32 / 1000.0,
                        external_connector_voltage_v: v3 as f32 / 1000.0,
                    },
                })
            },
            SdoRequest::BatteryVoltageLimit => {
                let raw = self
                    .sdo_client
                    .read_u32(BatteryVoltageLimit::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryVoltageLimit {
                    node_id: self.node_id,
                    value: BatteryVoltageLimit {
                        internal_external_min_delta_v: raw as f32 / 1000.0,
                    },
                })
            },
            SdoRequest::BatteryCurrent => {
                let c1 = self
                    .sdo_client
                    .read_i32(BatteryCurrent::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let c2 = self
                    .sdo_client
                    .read_i32(BatteryCurrent::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                let c3 = self
                    .sdo_client
                    .read_i32(BatteryCurrent::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                let c4 = self
                    .sdo_client
                    .read_i32(BatteryCurrent::INDEX, 0x04)
                    .await
                    .map_err(|e| e.to_string())?;
                let c5 = self
                    .sdo_client
                    .read_i32(BatteryCurrent::INDEX, 0x05)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryCurrent {
                    node_id: self.node_id,
                    value: BatteryCurrent {
                        fast_current_a: c1 as f32 / 1000.0,
                        weighted_avg_current_a: c2 as f32 / 1000.0,
                        integrated_current_a: c3 as f32 / 1000.0,
                        average_1s_current_a: c4 as f32 / 1000.0,
                        average_10s_current_a: c5 as f32 / 1000.0,
                    },
                })
            },
            SdoRequest::BatteryCurrentLimit => {
                let raw = self
                    .sdo_client
                    .read_i32(BatteryCurrentLimit::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let fully_charged_end = self
                    .sdo_client
                    .read_i32(BatteryCurrentLimit::INDEX, 0x0a)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryCurrentLimit {
                    node_id: self.node_id,
                    value: BatteryCurrentLimit {
                        discharge_sc_error_a: raw as f32 / 1000.0,
                        charge_current_fully_charged_end_ma: fully_charged_end,
                    },
                })
            },
            SdoRequest::FetTemperature => {
                let t1 = self
                    .sdo_client
                    .read_i32(FetTemperature::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let t2 = self
                    .sdo_client
                    .read_i32(FetTemperature::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::FetTemperature {
                    node_id: self.node_id,
                    value: FetTemperature {
                        temperature_1_c: t1 as f32 / 10.0,
                        temperature_2_c: t2 as f32 / 10.0,
                    },
                })
            },
            SdoRequest::FetTemperatureMinMax => {
                let min = self
                    .sdo_client
                    .read_i32(FetTemperatureMinMax::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let max = self
                    .sdo_client
                    .read_i32(FetTemperatureMinMax::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::FetTemperatureMinMax {
                    node_id: self.node_id,
                    value: FetTemperatureMinMax {
                        min_temperature_c: min as f32 / 10.0,
                        max_temperature_c: max as f32 / 10.0,
                    },
                })
            },
            SdoRequest::FetTemperatureLimit => {
                let raw = self
                    .sdo_client
                    .read_i32(FetTemperatureLimit::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::FetTemperatureLimit {
                    node_id: self.node_id,
                    value: FetTemperatureLimit { discharge_over_temp_c: raw as f32 / 10.0 },
                })
            },
            SdoRequest::CellTemperature => {
                let t1 = self
                    .sdo_client
                    .read_i32(CellTemperature::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let t2 = self
                    .sdo_client
                    .read_i32(CellTemperature::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                let t3 = self
                    .sdo_client
                    .read_i32(CellTemperature::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                let t4 = self
                    .sdo_client
                    .read_i32(CellTemperature::INDEX, 0x04)
                    .await
                    .map_err(|e| e.to_string())?;
                let t5 = self
                    .sdo_client
                    .read_i32(CellTemperature::INDEX, 0x05)
                    .await
                    .map_err(|e| e.to_string())?;
                let t6 = self
                    .sdo_client
                    .read_i32(CellTemperature::INDEX, 0x06)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::CellTemperature {
                    node_id: self.node_id,
                    value: CellTemperature {
                        temperature_1_c: t1 as f32 / 10.0,
                        temperature_2_c: t2 as f32 / 10.0,
                        temperature_3_c: t3 as f32 / 10.0,
                        temperature_4_c: t4 as f32 / 10.0,
                        temperature_5_c: t5 as f32 / 10.0,
                        temperature_6_c: t6 as f32 / 10.0,
                    },
                })
            },
            SdoRequest::CellTemperatureMinMax => {
                let min = self
                    .sdo_client
                    .read_i32(CellTemperatureMinMax::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let max = self
                    .sdo_client
                    .read_i32(CellTemperatureMinMax::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::CellTemperatureMinMax {
                    node_id: self.node_id,
                    value: CellTemperatureMinMax {
                        min_temperature_c: min as f32 / 10.0,
                        max_temperature_c: max as f32 / 10.0,
                    },
                })
            },
            SdoRequest::CellTemperatureLimit => {
                let raw = self
                    .sdo_client
                    .read_i32(CellTemperatureLimit::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::CellTemperatureLimit {
                    node_id: self.node_id,
                    value: CellTemperatureLimit { discharge_over_temp_c: raw as f32 / 10.0 },
                })
            },
            SdoRequest::CellBalanceStatus => {
                let s1 = self
                    .sdo_client
                    .read_u16(CellBalanceStatus::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let s2 = self
                    .sdo_client
                    .read_u16(CellBalanceStatus::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                let s3 = self
                    .sdo_client
                    .read_u16(CellBalanceStatus::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::CellBalanceStatus {
                    node_id: self.node_id,
                    value: CellBalanceStatus {
                        balance_status_register: s1,
                        balance_fet_active: s2,
                        balance_fet_active_persistent: s3,
                    },
                })
            },
            SdoRequest::CellBalanceLimit => {
                let raw = self
                    .sdo_client
                    .read_u32(CellBalanceLimit::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::CellBalanceLimit {
                    node_id: self.node_id,
                    value: CellBalanceLimit {
                        balance_start_diff_voltage_v: raw as f32 / 1000.0,
                    },
                })
            },
            SdoRequest::CellImpedance => {
                let mut raw = [0u16; 18];
                for i in 0..18u8 {
                    raw[i as usize] = self
                        .sdo_client
                        .read_u16(CellImpedance::INDEX, i + 1)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                let mut cell_impedances_mohm = [0u16; 16];
                cell_impedances_mohm.copy_from_slice(&raw[..16]);
                Ok(SdoResponse::CellImpedance {
                    node_id: self.node_id,
                    value: CellImpedance {
                        cell_impedances_mohm,
                        low_temp_factor: raw[16],
                        high_temp_factor: raw[17],
                    },
                })
            },
            SdoRequest::BatteryCapacity => {
                let design = self
                    .sdo_client
                    .read_u32(BatteryCapacity::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let fcc = self
                    .sdo_client
                    .read_u32(BatteryCapacity::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                let remain = self
                    .sdo_client
                    .read_u32(BatteryCapacity::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                let soc = self
                    .sdo_client
                    .read_u8(BatteryCapacity::INDEX, 0x04)
                    .await
                    .map_err(|e| e.to_string())?;
                let soh = self
                    .sdo_client
                    .read_u8(BatteryCapacity::INDEX, 0x05)
                    .await
                    .map_err(|e| e.to_string())?;
                let discharged = self
                    .sdo_client
                    .read_u32(BatteryCapacity::INDEX, 0x06)
                    .await
                    .map_err(|e| e.to_string())?;
                let charged = self
                    .sdo_client
                    .read_u32(BatteryCapacity::INDEX, 0x07)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryCapacity {
                    node_id: self.node_id,
                    value: BatteryCapacity {
                        design_capacity_ah: design as f32 / 1000.0,
                        full_charge_capacity_ah: fcc as f32 / 1000.0,
                        remaining_capacity_ah: remain as f32 / 1000.0,
                        soc_percent: soc as f32,
                        soh_percent: soh as f32,
                        total_discharged_capacity_ah: discharged as f32 / 1000.0,
                        total_charged_capacity_ah: charged as f32 / 1000.0,
                    },
                })
            },
            SdoRequest::BatteryCapacityParam => {
                let val = self
                    .sdo_client
                    .read_u8(BatteryCapacityParam::INDEX, BatteryCapacityParam::SUBINDEX)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryCapacityParam {
                    node_id: self.node_id,
                    value: BatteryCapacityParam { value: val },
                })
            },
            SdoRequest::BatteryCycleCount => {
                let c1 = self
                    .sdo_client
                    .read_u32(BatteryCycleCount::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let c2 = self
                    .sdo_client
                    .read_u32(BatteryCycleCount::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                let c3 = self
                    .sdo_client
                    .read_u32(BatteryCycleCount::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                let c4 = self
                    .sdo_client
                    .read_u32(BatteryCycleCount::INDEX, 0x04)
                    .await
                    .map_err(|e| e.to_string())?;
                let c5 = self
                    .sdo_client
                    .read_u32(BatteryCycleCount::INDEX, 0x05)
                    .await
                    .map_err(|e| e.to_string())?;
                let c6 = self
                    .sdo_client
                    .read_u32(BatteryCycleCount::INDEX, 0x06)
                    .await
                    .map_err(|e| e.to_string())?;
                let c7 = self
                    .sdo_client
                    .read_u32(BatteryCycleCount::INDEX, 0x07)
                    .await
                    .map_err(|e| e.to_string())?;
                let c8 = self
                    .sdo_client
                    .read_u32(BatteryCycleCount::INDEX, 0x08)
                    .await
                    .map_err(|e| e.to_string())?;
                let c9 = self
                    .sdo_client
                    .read_u32(BatteryCycleCount::INDEX, 0x09)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryCycleCount {
                    node_id: self.node_id,
                    value: BatteryCycleCount {
                        discharge_cycles: c1,
                        discharge_learning_cycles: c2,
                        discharge_cycles_after_learning: c3,
                        charge_cycles_completed: c4,
                        charge_cycles_started: c5,
                        discharge_use_detect: c6,
                        charge_use_low_temperature: c7,
                        charge_use_normal_temperature: c8,
                        charge_use_high_temperature: c9,
                    },
                })
            },
            SdoRequest::BatteryChargeVoltage => {
                let v1 = self
                    .sdo_client
                    .read_u32(BatteryChargeVoltage::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let v2 = self
                    .sdo_client
                    .read_u32(BatteryChargeVoltage::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                let v3 = self
                    .sdo_client
                    .read_u32(BatteryChargeVoltage::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryChargeVoltage {
                    node_id: self.node_id,
                    value: BatteryChargeVoltage {
                        charge_voltage_valid_v: v1 as f32 / 1000.0,
                        charge_max_voltage_v: v2 as f32 / 1000.0,
                        charge_keep_power_voltage_v: v3 as f32 / 1000.0,
                    },
                })
            },
            SdoRequest::BatteryChargeCurrent => {
                let c1 = self
                    .sdo_client
                    .read_u32(BatteryChargeCurrent::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let c2 = self
                    .sdo_client
                    .read_u32(BatteryChargeCurrent::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                let c3 = self
                    .sdo_client
                    .read_u32(BatteryChargeCurrent::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                let c4 = self
                    .sdo_client
                    .read_u32(BatteryChargeCurrent::INDEX, 0x04)
                    .await
                    .map_err(|e| e.to_string())?;
                let c5 = self
                    .sdo_client
                    .read_u32(BatteryChargeCurrent::INDEX, 0x05)
                    .await
                    .map_err(|e| e.to_string())?;
                let t1 = self
                    .sdo_client
                    .read_u16(BatteryChargeCurrent::INDEX, 0x06)
                    .await
                    .map_err(|e| e.to_string())?;
                let t2 = self
                    .sdo_client
                    .read_u16(BatteryChargeCurrent::INDEX, 0x07)
                    .await
                    .map_err(|e| e.to_string())?;
                let t3 = self
                    .sdo_client
                    .read_u16(BatteryChargeCurrent::INDEX, 0x08)
                    .await
                    .map_err(|e| e.to_string())?;
                let t4 = self
                    .sdo_client
                    .read_u16(BatteryChargeCurrent::INDEX, 0x09)
                    .await
                    .map_err(|e| e.to_string())?;
                let c10 = self
                    .sdo_client
                    .read_u32(BatteryChargeCurrent::INDEX, 0x0a)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryChargeCurrent {
                    node_id: self.node_id,
                    value: BatteryChargeCurrent {
                        charge_current_valid_a: c1 as f32 / 1000.0,
                        charge_max_current_n_a: c2 as f32 / 1000.0,
                        charge_max_current_low_a: c3 as f32 / 1000.0,
                        charge_max_current_high_a: c4 as f32 / 1000.0,
                        charge_keep_power_current_a: c5 as f32 / 1000.0,
                        charge_temp_min_low_c: t1 as f32 / 10.0,
                        charge_temp_min_normal_c: t2 as f32 / 10.0,
                        charge_temp_max_normal_c: t3 as f32 / 10.0,
                        charge_temp_max_high_c: t4 as f32 / 10.0,
                        charge_current_config: c10,
                    },
                })
            },
            SdoRequest::BatteryChargeCurrentValid => {
                let v = self
                    .sdo_client
                    .read_u32(
                        BatteryChargeCurrentValid::INDEX,
                        BatteryChargeCurrentValid::SUBINDEX,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryChargeCurrentValid {
                    node_id: self.node_id,
                    value: BatteryChargeCurrentValid { value: v, value_a: v as f32 / 1000.0 },
                })
            },
            SdoRequest::BatteryChargeCurrentMaxNormal => {
                let v = self
                    .sdo_client
                    .read_u32(
                        BatteryChargeCurrentMaxNormal::INDEX,
                        BatteryChargeCurrentMaxNormal::SUBINDEX,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryChargeCurrentMaxNormal {
                    node_id: self.node_id,
                    value: BatteryChargeCurrentMaxNormal { value: v, value_a: v as f32 / 1000.0 },
                })
            },
            SdoRequest::BatteryChargeCurrentMaxLow => {
                let v = self
                    .sdo_client
                    .read_u32(
                        BatteryChargeCurrentMaxLow::INDEX,
                        BatteryChargeCurrentMaxLow::SUBINDEX,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryChargeCurrentMaxLow {
                    node_id: self.node_id,
                    value: BatteryChargeCurrentMaxLow { value: v, value_a: v as f32 / 1000.0 },
                })
            },
            SdoRequest::BatteryChargeCurrentMaxHigh => {
                let v = self
                    .sdo_client
                    .read_u32(
                        BatteryChargeCurrentMaxHigh::INDEX,
                        BatteryChargeCurrentMaxHigh::SUBINDEX,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryChargeCurrentMaxHigh {
                    node_id: self.node_id,
                    value: BatteryChargeCurrentMaxHigh { value: v, value_a: v as f32 / 1000.0 },
                })
            },
            SdoRequest::BatteryChargeCurrentKeepPower => {
                let v = self
                    .sdo_client
                    .read_u32(
                        BatteryChargeCurrentKeepPower::INDEX,
                        BatteryChargeCurrentKeepPower::SUBINDEX,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryChargeCurrentKeepPower {
                    node_id: self.node_id,
                    value: BatteryChargeCurrentKeepPower { value: v, value_a: v as f32 / 1000.0 },
                })
            },
            SdoRequest::BatteryChargeCurrentDecreaseStepsize1 => {
                let v = self
                    .sdo_client
                    .read_u16(
                        BatteryChargeCurrentDecreaseStepsize1::INDEX,
                        BatteryChargeCurrentDecreaseStepsize1::SUBINDEX,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryChargeCurrentDecreaseStepsize1 {
                    node_id: self.node_id,
                    value: BatteryChargeCurrentDecreaseStepsize1 {
                        value: v,
                        value_a: v as f32 / 1000.0,
                    },
                })
            },
            SdoRequest::BatteryChargeCurrentIncreaseStepsize1 => {
                let v = self
                    .sdo_client
                    .read_u16(
                        BatteryChargeCurrentIncreaseStepsize1::INDEX,
                        BatteryChargeCurrentIncreaseStepsize1::SUBINDEX,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryChargeCurrentIncreaseStepsize1 {
                    node_id: self.node_id,
                    value: BatteryChargeCurrentIncreaseStepsize1 {
                        value: v,
                        value_a: v as f32 / 1000.0,
                    },
                })
            },
            SdoRequest::BatteryChargeCurrentDecreaseStepsize2 => {
                let v = self
                    .sdo_client
                    .read_u16(
                        BatteryChargeCurrentDecreaseStepsize2::INDEX,
                        BatteryChargeCurrentDecreaseStepsize2::SUBINDEX,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryChargeCurrentDecreaseStepsize2 {
                    node_id: self.node_id,
                    value: BatteryChargeCurrentDecreaseStepsize2 {
                        value: v,
                        value_a: v as f32 / 1000.0,
                    },
                })
            },
            SdoRequest::BatteryChargeCurrentIncreaseStepsize2 => {
                let v = self
                    .sdo_client
                    .read_u16(
                        BatteryChargeCurrentIncreaseStepsize2::INDEX,
                        BatteryChargeCurrentIncreaseStepsize2::SUBINDEX,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryChargeCurrentIncreaseStepsize2 {
                    node_id: self.node_id,
                    value: BatteryChargeCurrentIncreaseStepsize2 {
                        value: v,
                        value_a: v as f32 / 1000.0,
                    },
                })
            },
            SdoRequest::BatteryChargeCurrentModifyInterval => {
                let v = self
                    .sdo_client
                    .read_u32(
                        BatteryChargeCurrentModifyInterval::INDEX,
                        BatteryChargeCurrentModifyInterval::SUBINDEX,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryChargeCurrentModifyInterval {
                    node_id: self.node_id,
                    value: BatteryChargeCurrentModifyInterval { value: v, value_ms: v as f32 },
                })
            },
            SdoRequest::BatteryChargeTemperature => {
                let t1 = self
                    .sdo_client
                    .read_i32(BatteryChargeTemperature::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let t2 = self
                    .sdo_client
                    .read_i32(BatteryChargeTemperature::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                let t3 = self
                    .sdo_client
                    .read_i32(BatteryChargeTemperature::INDEX, 0x03)
                    .await
                    .map_err(|e| e.to_string())?;
                let t4 = self
                    .sdo_client
                    .read_i32(BatteryChargeTemperature::INDEX, 0x04)
                    .await
                    .map_err(|e| e.to_string())?;
                let t5 = self
                    .sdo_client
                    .read_i32(BatteryChargeTemperature::INDEX, 0x05)
                    .await
                    .map_err(|e| e.to_string())?;
                let t6 = self
                    .sdo_client
                    .read_i32(BatteryChargeTemperature::INDEX, 0x06)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::BatteryChargeTemperature {
                    node_id: self.node_id,
                    value: BatteryChargeTemperature {
                        temp_min_low_c: t1 as f32 / 10.0,
                        temp_min_normal_c: t2 as f32 / 10.0,
                        temp_max_normal_c: t3 as f32 / 10.0,
                        temp_max_high_c: t4 as f32 / 10.0,
                        temp_5_c: t5 as f32 / 10.0,
                        temp_6_c: t6 as f32 / 10.0,
                    },
                })
            },
            SdoRequest::MasterBatteryTemperature => {
                let t1 = self
                    .sdo_client
                    .read_i32(MasterBatteryTemperature::INDEX, 0x01)
                    .await
                    .map_err(|e| e.to_string())?;
                let t2 = self
                    .sdo_client
                    .read_i32(MasterBatteryTemperature::INDEX, 0x02)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::MasterBatteryTemperature {
                    node_id: self.node_id,
                    value: MasterBatteryTemperature {
                        max_fet_temperature_c: t1 as f32 / 10.0,
                        max_cell_temperature_c: t2 as f32 / 10.0,
                    },
                })
            },
            SdoRequest::KeepPowerTimer => {
                let raw = self
                    .sdo_client
                    .read_u32(KeepPowerTimer::INDEX, KeepPowerTimer::SUBINDEX)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(SdoResponse::KeepPowerTimer {
                    node_id: self.node_id,
                    value: KeepPowerTimer { value: raw },
                })
            },
        }
    }

    /// Read the serial number of the node.
    pub async fn read_serial_number(&mut self) -> Result<SerialNumber, String> {
        match self.read(SdoRequest::SerialNumber).await? {
            SdoResponse::SerialNumber { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the software version of the node.
    pub async fn read_software_version(&mut self) -> Result<SoftwareVersion, String> {
        match self.read(SdoRequest::SoftwareVersion).await? {
            SdoResponse::SoftwareVersion { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the hardware version of the node.
    pub async fn read_hardware_version(&mut self) -> Result<HardwareVersion, String> {
        match self.read(SdoRequest::HardwareVersion).await? {
            SdoResponse::HardwareVersion { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the device error history.
    pub async fn read_device_error_history(&mut self) -> Result<DeviceErrorHistory, String> {
        match self.read(SdoRequest::DeviceErrorHistory).await? {
            SdoResponse::DeviceErrorHistory { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the cell voltages.
    pub async fn read_cell_voltages(&mut self) -> Result<CellVoltages, String> {
        match self.read(SdoRequest::CellVoltages).await? {
            SdoResponse::CellVoltages { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the device config info.
    pub async fn read_device_config_info(&mut self) -> Result<DeviceConfigInfo, String> {
        match self.read(SdoRequest::DeviceConfigInfo).await? {
            SdoResponse::DeviceConfigInfo { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the device serial number info.
    pub async fn read_device_serial_number_info(
        &mut self,
    ) -> Result<DeviceSerialNumberInfo, String> {
        match self.read(SdoRequest::DeviceSerialNumberInfo).await? {
            SdoResponse::DeviceSerialNumberInfo { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the device date info.
    pub async fn read_device_date_info(&mut self) -> Result<DeviceDateInfo, String> {
        match self.read(SdoRequest::DeviceDateInfo).await? {
            SdoResponse::DeviceDateInfo { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the device variant info.
    pub async fn read_device_variant_info(&mut self) -> Result<DeviceVariantInfo, String> {
        match self.read(SdoRequest::DeviceVariantInfo).await? {
            SdoResponse::DeviceVariantInfo { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the device control parameter.
    pub async fn read_device_control_param(&mut self) -> Result<DeviceControlParam, String> {
        match self.read(SdoRequest::DeviceControlParam).await? {
            SdoResponse::DeviceControlParam { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the device operation time.
    pub async fn read_device_operation_time(&mut self) -> Result<DeviceOperationTime, String> {
        match self.read(SdoRequest::DeviceOperationTime).await? {
            SdoResponse::DeviceOperationTime { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the device error counter.
    pub async fn read_device_error_counter(&mut self) -> Result<DeviceErrorCounterInfo, String> {
        match self.read(SdoRequest::DeviceErrorCounter).await? {
            SdoResponse::DeviceErrorCounter { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the cell voltage min/max.
    pub async fn read_cell_voltage_min_max(&mut self) -> Result<CellVoltageMinMax, String> {
        match self.read(SdoRequest::CellVoltageMinMax).await? {
            SdoResponse::CellVoltageMinMax { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the cell voltage limit.
    pub async fn read_cell_voltage_limit(&mut self) -> Result<CellVoltageLimit, String> {
        match self.read(SdoRequest::CellVoltageLimit).await? {
            SdoResponse::CellVoltageLimit { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the battery voltage.
    pub async fn read_battery_voltage(&mut self) -> Result<BatteryVoltage, String> {
        match self.read(SdoRequest::BatteryVoltage).await? {
            SdoResponse::BatteryVoltage { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the battery voltage limit.
    pub async fn read_battery_voltage_limit(&mut self) -> Result<BatteryVoltageLimit, String> {
        match self.read(SdoRequest::BatteryVoltageLimit).await? {
            SdoResponse::BatteryVoltageLimit { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the battery current.
    pub async fn read_battery_current(&mut self) -> Result<BatteryCurrent, String> {
        match self.read(SdoRequest::BatteryCurrent).await? {
            SdoResponse::BatteryCurrent { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the battery current limit.
    pub async fn read_battery_current_limit(&mut self) -> Result<BatteryCurrentLimit, String> {
        match self.read(SdoRequest::BatteryCurrentLimit).await? {
            SdoResponse::BatteryCurrentLimit { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the FET temperature.
    pub async fn read_fet_temperature(&mut self) -> Result<FetTemperature, String> {
        match self.read(SdoRequest::FetTemperature).await? {
            SdoResponse::FetTemperature { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the FET temperature min/max.
    pub async fn read_fet_temperature_min_max(&mut self) -> Result<FetTemperatureMinMax, String> {
        match self.read(SdoRequest::FetTemperatureMinMax).await? {
            SdoResponse::FetTemperatureMinMax { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the FET temperature limit.
    pub async fn read_fet_temperature_limit(&mut self) -> Result<FetTemperatureLimit, String> {
        match self.read(SdoRequest::FetTemperatureLimit).await? {
            SdoResponse::FetTemperatureLimit { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the cell temperature.
    pub async fn read_cell_temperature(&mut self) -> Result<CellTemperature, String> {
        match self.read(SdoRequest::CellTemperature).await? {
            SdoResponse::CellTemperature { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the cell temperature min/max.
    pub async fn read_cell_temperature_min_max(&mut self) -> Result<CellTemperatureMinMax, String> {
        match self.read(SdoRequest::CellTemperatureMinMax).await? {
            SdoResponse::CellTemperatureMinMax { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the cell temperature limit.
    pub async fn read_cell_temperature_limit(&mut self) -> Result<CellTemperatureLimit, String> {
        match self.read(SdoRequest::CellTemperatureLimit).await? {
            SdoResponse::CellTemperatureLimit { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the cell balance status.
    pub async fn read_cell_balance_status(&mut self) -> Result<CellBalanceStatus, String> {
        match self.read(SdoRequest::CellBalanceStatus).await? {
            SdoResponse::CellBalanceStatus { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the cell balance limit.
    pub async fn read_cell_balance_limit(&mut self) -> Result<CellBalanceLimit, String> {
        match self.read(SdoRequest::CellBalanceLimit).await? {
            SdoResponse::CellBalanceLimit { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the cell impedance.
    pub async fn read_cell_impedance(&mut self) -> Result<CellImpedance, String> {
        match self.read(SdoRequest::CellImpedance).await? {
            SdoResponse::CellImpedance { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the battery capacity.
    pub async fn read_battery_capacity(&mut self) -> Result<BatteryCapacity, String> {
        match self.read(SdoRequest::BatteryCapacity).await? {
            SdoResponse::BatteryCapacity { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the battery capacity parameter.
    pub async fn read_battery_capacity_param(&mut self) -> Result<BatteryCapacityParam, String> {
        match self.read(SdoRequest::BatteryCapacityParam).await? {
            SdoResponse::BatteryCapacityParam { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the battery cycle count.
    pub async fn read_battery_cycle_count(&mut self) -> Result<BatteryCycleCount, String> {
        match self.read(SdoRequest::BatteryCycleCount).await? {
            SdoResponse::BatteryCycleCount { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the battery charge voltage.
    pub async fn read_battery_charge_voltage(&mut self) -> Result<BatteryChargeVoltage, String> {
        match self.read(SdoRequest::BatteryChargeVoltage).await? {
            SdoResponse::BatteryChargeVoltage { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the battery charge current.
    pub async fn read_battery_charge_current(&mut self) -> Result<BatteryChargeCurrent, String> {
        match self.read(SdoRequest::BatteryChargeCurrent).await? {
            SdoResponse::BatteryChargeCurrent { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the battery charge temperature.
    pub async fn read_battery_charge_temperature(
        &mut self,
    ) -> Result<BatteryChargeTemperature, String> {
        match self.read(SdoRequest::BatteryChargeTemperature).await? {
            SdoResponse::BatteryChargeTemperature { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Read the master battery temperature.
    pub async fn read_master_battery_temperature(
        &mut self,
    ) -> Result<MasterBatteryTemperature, String> {
        match self.read(SdoRequest::MasterBatteryTemperature).await? {
            SdoResponse::MasterBatteryTemperature { value, .. } => Ok(value),
            _ => unreachable!(),
        }
    }

    /// Unlock codes required to write configuration SDOs (from VARTA).
    /// Different unlock codes for different OD indexes.
    pub const CONFIG_UNLOCK_CODE_032F: u16 = 0x032f;
    pub const CONFIG_UNLOCK_CODE_0717: u16 = 0x0717;

    /// Save code to persist configuration to EEPROM (from VARTA handbook).
    const CONFIG_SAVE_CODE: u16 = 0x1c2b;

    /// Unlock configuration writes by writing the unlock code to 0x2010:01.
    async fn sdo_unlock(&mut self, unlock_code: u16) -> Result<(), String> {
        self.sdo_client
            .write_u16(0x2010, 0x01, unlock_code)
            .await
            .map_err(|e| e.to_string())
    }

    /// Save configuration to EEPROM by writing the save code to 0x2010:01.
    async fn sdo_save(&mut self) -> Result<(), String> {
        self.sdo_client
            .write_u16(0x2010, 0x01, Self::CONFIG_SAVE_CODE)
            .await
            .map_err(|e| e.to_string())
    }

    /// Write the Battery Charge Max Voltage Parameter (0x3000sub02) if it differs from the target.
    ///
    /// `battery_charge_max_voltage` is in volts. The value is stored internally in millivolts.
    /// Returns `true` if the value was changed, `false` if it already matched.
    pub async fn sdo_write_battery_charge_max_voltage(
        &mut self,
        battery_charge_max_voltage: f32,
    ) -> Result<bool, String> {
        let current_raw = self
            .sdo_client
            .read_u32(0x3000, 0x02)
            .await
            .map_err(|e| e.to_string())?;
        let current_v = current_raw as f32 / 1000.0;

        // Compare with a small tolerance to account for floating-point rounding
        if (current_v - battery_charge_max_voltage).abs() < 0.000_001 {
            return Ok(false);
        }

        // Unlock, write, save
        self.sdo_unlock(crate::varta::SdoSession::CONFIG_UNLOCK_CODE_032F)
            .await?;

        let target_raw = (battery_charge_max_voltage * 1000.0) as u32;
        self.sdo_client
            .write_u32(0x3000, 0x02, target_raw)
            .await
            .map_err(|e| e.to_string())?;

        // Verify the write
        let verify_raw = self
            .sdo_client
            .read_u32(0x3000, 0x02)
            .await
            .map_err(|e| e.to_string())?;
        if verify_raw != target_raw {
            return Err(format!(
                "Write verification failed: expected {}, got {}",
                target_raw, verify_raw
            ));
        }

        self.sdo_save().await?;
        Ok(true)
    }

    /// Write the Battery Charge Voltage Keep Power Parameter (0x3000sub03).
    ///
    /// `keep_power_voltage` is in volts. The value is stored internally in millivolts.
    /// Returns `true` if the value was changed, `false` if it already matched.
    pub async fn sdo_write_battery_charge_keep_power_voltage(
        &mut self,
        keep_power_voltage: f32,
    ) -> Result<bool, String> {
        let current_raw = self
            .sdo_client
            .read_u32(0x3000, 0x03)
            .await
            .map_err(|e| e.to_string())?;
        let current_v = current_raw as f32 / 1000.0;

        if (current_v - keep_power_voltage).abs() < 0.001 {
            return Ok(false);
        }

        self.sdo_unlock(crate::varta::SdoSession::CONFIG_UNLOCK_CODE_032F)
            .await?;

        let target_raw = (keep_power_voltage * 1000.0) as u32;
        self.sdo_client
            .write_u32(0x3000, 0x03, target_raw)
            .await
            .map_err(|e| e.to_string())?;

        let verify_raw = self
            .sdo_client
            .read_u32(0x3000, 0x03)
            .await
            .map_err(|e| e.to_string())?;
        if verify_raw != target_raw {
            return Err(format!(
                "Write verification failed: expected {}, got {}",
                target_raw, verify_raw
            ));
        }

        self.sdo_save().await?;
        Ok(true)
    }

    /// Write the Keep Power Timer / Charger Standby to OFF Delay (0x3d00sub0c).
    ///
    /// `timer_seconds` is in seconds. 0xFFFFFFFF disables automatic shutdown.
    /// Returns `true` if the value was changed, `false` if it already matched.
    pub async fn sdo_write_keep_power_timer(&mut self, timer_seconds: u32) -> Result<bool, String> {
        let current_raw = self
            .sdo_client
            .read_u32(0x3d00, 0x0c)
            .await
            .map_err(|e| e.to_string())?;

        if current_raw == timer_seconds {
            return Ok(false);
        }

        self.sdo_unlock(crate::varta::SdoSession::CONFIG_UNLOCK_CODE_032F)
            .await?;

        self.sdo_client
            .write_u32(0x3d00, 0x0c, timer_seconds)
            .await
            .map_err(|e| e.to_string())?;

        let verify_raw = self
            .sdo_client
            .read_u32(0x3d00, 0x0c)
            .await
            .map_err(|e| e.to_string())?;
        if verify_raw != timer_seconds {
            return Err(format!(
                "Write verification failed: expected {}, got {}",
                timer_seconds, verify_raw
            ));
        }

        self.sdo_save().await?;
        Ok(true)
    }

    /// Write the Single Cell Max Charge Voltage (0x2104sub02).
    ///
    /// `voltage_mv` is in millivolts.
    /// Returns `true` if the value was changed, `false` if it already matched.
    pub async fn sdo_write_single_cell_max_charge_voltage(
        &mut self,
        voltage_mv: u32,
    ) -> Result<bool, String> {
        let current_raw = self
            .sdo_client
            .read_u32(0x2104, 0x02)
            .await
            .map_err(|e| e.to_string())?;

        if current_raw == voltage_mv {
            return Ok(false);
        }

        self.sdo_unlock(crate::varta::SdoSession::CONFIG_UNLOCK_CODE_032F)
            .await?;

        self.sdo_client
            .write_u32(0x2104, 0x02, voltage_mv)
            .await
            .map_err(|e| e.to_string())?;

        let verify_raw = self
            .sdo_client
            .read_u32(0x2104, 0x02)
            .await
            .map_err(|e| e.to_string())?;
        if verify_raw != voltage_mv {
            return Err(format!(
                "Write verification failed: expected {}, got {}",
                voltage_mv, verify_raw
            ));
        }

        self.sdo_save().await?;
        Ok(true)
    }

    /// Write the Battery Charge Current Fully Charged End (0x2304sub0a).
    ///
    /// `current_ma` is in milliamperes.
    /// Returns `true` if the value was changed, `false` if it already matched.
    pub async fn sdo_write_battery_charge_current_fully_charged_end(
        &mut self,
        current_ma: u16,
    ) -> Result<bool, String> {
        let current_raw = self
            .sdo_client
            .read_u16(0x2304, 0x0a)
            .await
            .map_err(|e| e.to_string())?;

        if current_raw == current_ma {
            return Ok(false);
        }

        self.sdo_unlock(crate::varta::SdoSession::CONFIG_UNLOCK_CODE_032F)
            .await?;

        self.sdo_client
            .write_u16(0x2304, 0x0a, current_ma)
            .await
            .map_err(|e| e.to_string())?;

        let verify_raw = self
            .sdo_client
            .read_u16(0x2304, 0x0a)
            .await
            .map_err(|e| e.to_string())?;
        if verify_raw != current_ma {
            return Err(format!(
                "Write verification failed: expected {}, got {}",
                current_ma, verify_raw
            ));
        }

        self.sdo_save().await?;
        Ok(true)
    }
}

// Private API
impl Varta {
    fn update_easyblade_voltage_current(
        easyblade: &mut VartaEasyblade,
        voltage: f32,
        current: f32,
    ) {
        easyblade.pdo.voltage = Some(voltage);
        easyblade.pdo.current = Some(current);
        easyblade.last_seen = std::time::SystemTime::now();
    }

    fn update_easyblade_soc_soh(
        easyblade: &mut VartaEasyblade,
        full_cap: f32,
        rem_cap: f32,
        design_cap: f32,
    ) {
        easyblade.pdo.soc = if full_cap > 0.0 {
            Some((rem_cap / full_cap) * 100.0)
        } else {
            None
        };
        easyblade.pdo.soh = if design_cap > 0.0 {
            Some((full_cap / design_cap) * 100.0)
        } else {
            None
        };
        easyblade.last_seen = std::time::SystemTime::now();
    }

    #[allow(clippy::too_many_arguments)]
    fn update_easyblade_pack_msgs(
        easyblade: &mut VartaEasyblade,
        info_bit_0_empty: bool,
        info_bit_1_almost_empty: bool,
        info_bit_2_chgfet_closed: bool,
        info_bit_3_dsgfet_closed: bool,
        info_bit_4_bypass_fet_on: bool,
        info_bit_6_fully_charged: bool,
        warn_bit_0_low_voltage: bool,
        warn_bit_1_low_soc: bool,
        warn_bit_2_reserve_soc: bool,
        warn_bit_3_over_or_under_temp_discharge: bool,
        warn_bit_4_over_or_under_temp_charge: bool,
        warn_bit_7_max_charge_condition_recuperation: bool,
        warn_bit_11_can_network_failure: bool,
        warn_bit_12_set_deactivation_enable: bool,
        warn_bit_14_set_node_id_process_enable: bool,
        warn_bit_15_unknown: bool,
        error_bit_0_error_lock_flag_discharge: bool,
        error_bit_1_error_lock_flag_charge: bool,
        error_bit_2_over_charge_condition_recuperation: bool,
        error_bit_3_shortcircuit_charge_alarm: bool,
        error_bit_4_shortcircuit_discharge_alarm: bool,
        error_bit_5_max_voltage_alarm: bool,
        error_bit_6_discharge_fet_error: bool,
        error_bit_7_charge_fet_error: bool,
        error_bit_8_max_charge_current_alarm: bool,
        error_bit_9_max_discharge_current_alarm: bool,
        error_bit_10_under_charge_alarm: bool,
        error_bit_11_over_charge_alarm: bool,
        error_bit_12_over_under_temp_charge: bool,
        error_bit_13_over_under_temp_discharge: bool,
        error_bit_14_module_defect: bool,
        error_bit_15_uknown: bool,
        charge_bit_0_charge_voltage_enabled: bool,
        charge_bit_1_charge_voltage_keep_power: bool,
        charge_bit_4_charge_current_enable: bool,
        charge_bit_5_charge_current_keep_power: bool,
        charge_bit_6_charge_current_low_temp_range: bool,
        charge_bit_7_charge_current_normal_temp_range: bool,
        charge_bit_8_charge_current_high_temp_range: bool,
        charge_bit_10_charge_max_charge_current_request: bool,
        charge_bit_11_charge_max_charge_cell_voltage_request: bool,
        charge_bit_12_charge_master_set_charger_output_off: bool,
        charge_bit_13_charge_fet_disable_temp_range_cells: bool,
        charge_bit_14_master_charger_control_charging_ready: bool,
        charge_bit_15_charger_supply_conditions_ready: bool,
    ) {
        easyblade.pdo.msg_bits = Some(MsgBits {
            info_bit_0_empty,
            info_bit_1_almost_empty,
            info_bit_2_chgfet_closed,
            info_bit_3_dsgfet_closed,
            info_bit_4_bypass_fet_on,
            info_bit_6_fully_charged,
            warn_bit_0_low_voltage,
            warn_bit_1_low_soc,
            warn_bit_2_reserve_soc,
            warn_bit_3_over_or_under_temp_discharge,
            warn_bit_4_over_or_under_temp_charge,
            warn_bit_7_max_charge_condition_recuperation,
            warn_bit_11_can_network_failure,
            warn_bit_12_set_deactivation_enable,
            warn_bit_14_set_node_id_process_enable,
            warn_bit_15_unknown,
            error_bit_0_error_lock_flag_discharge,
            error_bit_1_error_lock_flag_charge,
            error_bit_2_over_charge_condition_recuperation,
            error_bit_3_shortcircuit_charge_alarm,
            error_bit_4_shortcircuit_discharge_alarm,
            error_bit_5_max_voltage_alarm,
            error_bit_6_discharge_fet_error,
            error_bit_7_charge_fet_error,
            error_bit_8_max_charge_current_alarm,
            error_bit_9_max_discharge_current_alarm,
            error_bit_10_under_charge_alarm,
            error_bit_11_over_charge_alarm,
            error_bit_12_over_under_temp_charge,
            error_bit_13_over_under_temp_discharge,
            error_bit_14_module_defect,
            error_bit_15_uknown,
            charge_bit_0_charge_voltage_enabled,
            charge_bit_1_charge_voltage_keep_power,
            charge_bit_4_charge_current_enable,
            charge_bit_5_charge_current_keep_power,
            charge_bit_6_charge_current_low_temp_range,
            charge_bit_7_charge_current_normal_temp_range,
            charge_bit_8_charge_current_high_temp_range,
            charge_bit_10_charge_max_charge_current_request,
            charge_bit_11_charge_max_charge_cell_voltage_request,
            charge_bit_12_charge_master_set_charger_output_off,
            charge_bit_13_charge_fet_disable_temp_range_cells,
            charge_bit_14_master_charger_control_charging_ready,
            charge_bit_15_charger_supply_conditions_ready,
        });
        easyblade.last_seen = std::time::SystemTime::now();
    }
}
