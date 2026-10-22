use clap::Parser;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use ratatui::prelude::*;
use ratatui::text::Text;
use ratatui::widgets::{Block, Clear, Paragraph, Row, Table, Wrap};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::{Write, stdout};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;

use strum::EnumCount;
use varta_easyblade::SdoRequest;
use varta_easyblade::SdoResponse;

static DEBUG_LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);

fn init_debug_log() {
    let path = "/tmp/varta-tui-debug.log";
    let _ = std::fs::remove_file(path);
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(path) {
        let mut guard = DEBUG_LOG.lock().unwrap();
        *guard = Some(file);
    }
}

fn debug_log(msg: &str) {
    if let Some(ref mut file) = *DEBUG_LOG.lock().unwrap() {
        let timestamp = chrono::Local::now().format("%H:%M:%S.%3f");
        let _ = writeln!(file, "[{}] {}", timestamp, msg);
        let _ = file.flush();
    }
}

#[derive(Debug, Parser)]
struct Args {
    /// The CAN interface to connect to (e.g. 'can0' or 'vcan0').
    #[arg(short, long, default_value_t = String::from("can0"))]
    can_interface: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SelectedTab {
    ModuleInfo,
    MsgBits,
    CellVoltages,
    ErrorHistory,
    DeviceOperation,
    ErrorCounters,
    CellVoltageLimits,
    BatteryVoltage,
    BatteryCurrent,
    FetTemperature,
    CellTemperature,
    CellBalance,
    Impedance,
    Capacity,
    CycleCount,
    ChargeParameters,
    ChargeCurrentParams,
    MaxBatteryTemperature,
    Configuration,
}

/// Value type for a configuration field.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ConfigValue {
    /// Floating point value (voltages, currents) displayed with 3 decimal places.
    Float(f32),
    /// Unsigned integer value (timers, raw counts) displayed as integer.
    UInt(u32),
}

impl ConfigValue {
    fn edit_str(&self) -> String {
        match self {
            ConfigValue::Float(v) => format!("{:.3}", v),
            ConfigValue::UInt(v) => format!("{}", v),
        }
    }
}

/// A single editable configuration field.
#[derive(Debug, Clone)]
struct ConfigField {
    /// Display label (e.g. "Battery Max Charge Voltage")
    label: &'static str,
    /// Current value (float or uint), or None while the SDO read is pending
    value: Option<ConfigValue>,
    /// Unit string (e.g. "V", "s", "mA")
    unit: &'static str,
    /// Editable portion of the value (just the number, e.g. "54.600")
    edit_buffer: String,
    /// Whether this field is currently being edited
    is_focused: bool,
    /// Whether the user has typed into the edit buffer since it was last
    /// filled from the read value (protects in-progress input from updates)
    user_edited: bool,
    /// Status message after last write attempt (cleared on next input)
    status: Option<String>,
}

impl ConfigField {
    /// Create a field whose value has not been read yet (shows "Pending...").
    fn new(label: &'static str, unit: &'static str) -> Self {
        Self {
            label,
            value: None,
            unit,
            edit_buffer: String::new(),
            is_focused: false,
            user_edited: false,
            status: None,
        }
    }

    fn update_value(&mut self, value: ConfigValue) {
        self.value = Some(value);
        self.edit_buffer = value.edit_str();
        self.user_edited = false;
    }

    /// Reset the field to the pending state (e.g. after a write, while the
    /// re-read is in flight).
    fn reset_to_pending(&mut self) {
        self.value = None;
        self.edit_buffer.clear();
        self.user_edited = false;
    }

    fn insert_char(&mut self, ch: char) {
        self.user_edited = true;
        self.edit_buffer.push(ch);
    }

    fn backspace(&mut self) {
        self.user_edited = true;
        self.edit_buffer.pop();
    }

    fn parse_float(&self) -> Option<f32> {
        self.edit_buffer.parse().ok()
    }

    fn parse_uint(&self) -> Option<u32> {
        self.edit_buffer.parse().ok()
    }
}

#[derive(Debug, Default, Clone)]
struct ConfigState {
    fields: Vec<ConfigField>,
    focused_index: usize,
    /// The selected module index the config was last loaded for.
    /// Reset when the user switches modules so stale data isn't shown.
    loaded_for_selected: Option<usize>,
}

fn sdos_for_tab(tab: SelectedTab) -> Vec<SdoRequest> {
    match tab {
        SelectedTab::ModuleInfo => vec![
            SdoRequest::SerialNumber,
            SdoRequest::SoftwareVersion,
            SdoRequest::HardwareVersion,
            SdoRequest::DeviceConfigInfo,
            SdoRequest::DeviceSerialNumberInfo,
            SdoRequest::DeviceDateInfo,
            SdoRequest::DeviceVariantInfo,
            SdoRequest::DeviceControlParam,
        ],
        SelectedTab::MsgBits => vec![],
        SelectedTab::CellVoltages => {
            vec![SdoRequest::CellVoltages, SdoRequest::CellVoltageMinMax]
        },
        SelectedTab::ErrorHistory => vec![SdoRequest::DeviceErrorHistory],
        SelectedTab::DeviceOperation => vec![SdoRequest::DeviceOperationTime],
        SelectedTab::ErrorCounters => vec![SdoRequest::DeviceErrorCounter],
        SelectedTab::CellVoltageLimits => vec![SdoRequest::CellVoltageLimit],
        SelectedTab::BatteryVoltage => {
            vec![SdoRequest::BatteryVoltage, SdoRequest::BatteryVoltageLimit]
        },
        SelectedTab::BatteryCurrent => {
            vec![SdoRequest::BatteryCurrent, SdoRequest::BatteryCurrentLimit]
        },
        SelectedTab::FetTemperature => vec![
            SdoRequest::FetTemperature,
            SdoRequest::FetTemperatureMinMax,
            SdoRequest::FetTemperatureLimit,
        ],
        SelectedTab::CellTemperature => vec![
            SdoRequest::CellTemperature,
            SdoRequest::CellTemperatureMinMax,
            SdoRequest::CellTemperatureLimit,
        ],
        SelectedTab::CellBalance => {
            vec![SdoRequest::CellBalanceStatus, SdoRequest::CellBalanceLimit]
        },
        SelectedTab::Impedance => vec![SdoRequest::CellImpedance],
        SelectedTab::Capacity => {
            vec![SdoRequest::BatteryCapacity, SdoRequest::BatteryCapacityParam]
        },
        SelectedTab::CycleCount => vec![SdoRequest::BatteryCycleCount],
        SelectedTab::ChargeParameters => vec![
            SdoRequest::BatteryChargeVoltage,
            SdoRequest::BatteryChargeCurrent,
            SdoRequest::BatteryChargeTemperature,
        ],
        SelectedTab::ChargeCurrentParams => vec![
            SdoRequest::BatteryChargeCurrentValid,
            SdoRequest::BatteryChargeCurrentMaxNormal,
            SdoRequest::BatteryChargeCurrentMaxLow,
            SdoRequest::BatteryChargeCurrentMaxHigh,
            SdoRequest::BatteryChargeCurrentKeepPower,
            SdoRequest::BatteryChargeCurrentDecreaseStepsize1,
            SdoRequest::BatteryChargeCurrentIncreaseStepsize1,
            SdoRequest::BatteryChargeCurrentDecreaseStepsize2,
            SdoRequest::BatteryChargeCurrentIncreaseStepsize2,
            SdoRequest::BatteryChargeCurrentModifyInterval,
        ],
        SelectedTab::MaxBatteryTemperature => vec![SdoRequest::MasterBatteryTemperature],
        SelectedTab::Configuration => vec![
            SdoRequest::BatteryChargeVoltage,
            SdoRequest::CellVoltageLimit,
            SdoRequest::BatteryCurrentLimit,
            SdoRequest::KeepPowerTimer,
        ],
    }
}

impl SelectedTab {
    fn title(&self) -> &'static str {
        match self {
            SelectedTab::ModuleInfo => "Module Info",
            SelectedTab::MsgBits => "Message Bits",
            SelectedTab::CellVoltages => "Cell Voltages",
            SelectedTab::ErrorHistory => "Error History",
            SelectedTab::DeviceOperation => "Device Operation",
            SelectedTab::ErrorCounters => "Error Counters",
            SelectedTab::CellVoltageLimits => "Cell Voltage Limits",
            SelectedTab::BatteryVoltage => "Battery Voltage",
            SelectedTab::BatteryCurrent => "Battery Current",
            SelectedTab::FetTemperature => "FET Temperature",
            SelectedTab::CellTemperature => "Cell Temperature",
            SelectedTab::CellBalance => "Cell Balance",
            SelectedTab::Impedance => "Impedance",
            SelectedTab::Capacity => "Capacity",
            SelectedTab::CycleCount => "Cycle Count",
            SelectedTab::ChargeParameters => "Charge Parameters",
            SelectedTab::ChargeCurrentParams => "Charge Current",
            SelectedTab::MaxBatteryTemperature => "Max Battery Temperature",
            SelectedTab::Configuration => "Configuration",
        }
    }

    fn cycle(&self, right: bool) -> Self {
        let tabs = [
            SelectedTab::ModuleInfo,
            SelectedTab::MsgBits,
            SelectedTab::CellVoltages,
            SelectedTab::ErrorHistory,
            SelectedTab::DeviceOperation,
            SelectedTab::ErrorCounters,
            SelectedTab::CellVoltageLimits,
            SelectedTab::BatteryVoltage,
            SelectedTab::BatteryCurrent,
            SelectedTab::FetTemperature,
            SelectedTab::CellTemperature,
            SelectedTab::CellBalance,
            SelectedTab::Impedance,
            SelectedTab::Capacity,
            SelectedTab::CycleCount,
            SelectedTab::ChargeParameters,
            SelectedTab::ChargeCurrentParams,
            SelectedTab::MaxBatteryTemperature,
            SelectedTab::Configuration,
        ];
        let idx = tabs.iter().position(|t| *t == *self).unwrap();
        let next = if right {
            (idx + 1) % tabs.len()
        } else {
            (idx as i32 - 1 + tabs.len() as i32) as usize % tabs.len()
        };
        tabs[next]
    }
}

fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn cleanup_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), LeaveAlternateScreen);
}

fn format_last_seen(last_seen: SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(last_seen)
        .format("%Y-%m-%d %H:%M:%S.%3f")
        .to_string()
}

fn yes_no(v: bool) -> &'static str {
    if v { "Yes" } else { "No" }
}

fn format_fet_status(msgs: Option<&varta_easyblade::MsgBits>) -> String {
    match msgs {
        Some(m) => format!(
            "{}{}{}",
            if m.info_bit_2_chgfet_closed { "C" } else { "-" },
            if m.info_bit_3_dsgfet_closed { "D" } else { "-" },
            if m.info_bit_4_bypass_fet_on { "B" } else { "-" },
        ),
        None => "---".to_string(),
    }
}

#[derive(Default, Clone)]
enum SaveState {
    #[default]
    Idle,
    Saving {
        file_path: String,
    },
    Saved(String),
}

impl SaveState {
    fn is_active(&self) -> bool {
        !matches!(self, SaveState::Idle)
    }
}

const TOTAL_SDOS: f64 = SdoRequest::COUNT as f64;

fn sdo_completion(eb: &varta_easyblade::VartaEasyblade) -> f64 {
    let mut count = 0.0;
    if eb.sdo.serial_number.is_some() {
        count += 1.0;
    }
    if eb.sdo.software_version.is_some() {
        count += 1.0;
    }
    if eb.sdo.hardware_version.is_some() {
        count += 1.0;
    }
    if eb.sdo.device_config_info.is_some() {
        count += 1.0;
    }
    if eb.sdo.device_serial_number_info.is_some() {
        count += 1.0;
    }
    if eb.sdo.device_date_info.is_some() {
        count += 1.0;
    }
    if eb.sdo.device_variant_info.is_some() {
        count += 1.0;
    }
    if eb.sdo.device_control_param.is_some() {
        count += 1.0;
    }
    if eb.sdo.device_operation_time.is_some() {
        count += 1.0;
    }
    if eb.sdo.device_errors.is_some() {
        count += 1.0;
    }
    if eb.sdo.device_error_counter.is_some() {
        count += 1.0;
    }
    if eb.sdo.cell_voltages.is_some() {
        count += 1.0;
    }
    if eb.sdo.cell_voltage_min_max.is_some() {
        count += 1.0;
    }
    if eb.sdo.cell_voltage_limit.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_voltage.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_voltage_limit.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_current.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_current_limit.is_some() {
        count += 1.0;
    }
    if eb.sdo.fet_temperature.is_some() {
        count += 1.0;
    }
    if eb.sdo.fet_temperature_min_max.is_some() {
        count += 1.0;
    }
    if eb.sdo.fet_temperature_limit.is_some() {
        count += 1.0;
    }
    if eb.sdo.cell_temperature.is_some() {
        count += 1.0;
    }
    if eb.sdo.cell_temperature_min_max.is_some() {
        count += 1.0;
    }
    if eb.sdo.cell_temperature_limit.is_some() {
        count += 1.0;
    }
    if eb.sdo.cell_balance_status.is_some() {
        count += 1.0;
    }
    if eb.sdo.cell_balance_limit.is_some() {
        count += 1.0;
    }
    if eb.sdo.cell_impedance.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_capacity.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_capacity_param.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_cycle_count.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_charge_voltage.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_charge_current.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_charge_current_valid.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_charge_current_max_normal.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_charge_current_max_low.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_charge_current_max_high.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_charge_current_keep_power.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_charge_current_decrease_stepsize1.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_charge_current_increase_stepsize1.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_charge_current_decrease_stepsize2.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_charge_current_increase_stepsize2.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_charge_current_modify_interval.is_some() {
        count += 1.0;
    }
    if eb.sdo.battery_charge_temperature.is_some() {
        count += 1.0;
    }
    if eb.sdo.master_battery_temperature.is_some() {
        count += 1.0;
    }
    if eb.sdo.keep_power_timer.is_some() {
        count += 1.0;
    }
    count
}

fn easyblade_to_json(eb: &varta_easyblade::VartaEasyblade) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("node_id".into(), json!(eb.node_id));
    map.insert("serial_number".into(), json!(eb.sdo.serial_number));

    // PDO data
    if let Some(ref v) = eb.pdo.voltage {
        map.insert("pdo_voltage".into(), json!(v));
    }
    if let Some(ref v) = eb.pdo.current {
        map.insert("pdo_current".into(), json!(v));
    }
    if let Some(ref v) = eb.pdo.soc {
        map.insert("pdo_soc".into(), json!(v));
    }
    if let Some(ref v) = eb.pdo.soh {
        map.insert("pdo_soh".into(), json!(v));
    }
    if let Some(ref v) = eb.pdo.msg_bits {
        map.insert("pdo_msg_bits".into(), json!(v));
    }

    if let Some(ref v) = eb.sdo.software_version {
        map.insert("software_version".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.hardware_version {
        map.insert("hardware_version".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.device_config_info {
        map.insert("device_config_info".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.device_serial_number_info {
        map.insert("device_serial_number_info".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.device_date_info {
        map.insert("device_date_info".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.device_variant_info {
        map.insert("device_variant_info".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.device_control_param {
        map.insert("device_control_param".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.device_operation_time {
        map.insert("device_operation_time".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.device_errors {
        map.insert(
            "device_error_history".into(),
            json!(
                v.values
                    .iter()
                    .map(|e| format!("{:?}", e))
                    .collect::<Vec<_>>()
            ),
        );
    }
    if let Some(ref v) = eb.sdo.device_error_counter {
        map.insert("device_error_counter".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.cell_voltages {
        map.insert("cell_voltages".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.cell_voltage_min_max {
        map.insert("cell_voltage_min_max".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.cell_voltage_limit {
        map.insert("cell_voltage_limit".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_voltage {
        map.insert("battery_voltage".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_voltage_limit {
        map.insert("battery_voltage_limit".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_current {
        map.insert("battery_current".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_current_limit {
        map.insert("battery_current_limit".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.fet_temperature {
        map.insert("fet_temperature".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.fet_temperature_min_max {
        map.insert("fet_temperature_min_max".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.fet_temperature_limit {
        map.insert("fet_temperature_limit".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.cell_temperature {
        map.insert("cell_temperature".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.cell_temperature_min_max {
        map.insert("cell_temperature_min_max".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.cell_temperature_limit {
        map.insert("cell_temperature_limit".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.cell_balance_status {
        map.insert("cell_balance_status".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.cell_balance_limit {
        map.insert("cell_balance_limit".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.cell_impedance {
        map.insert("cell_impedance".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_capacity {
        map.insert("battery_capacity".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_capacity_param {
        map.insert("battery_capacity_param".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_cycle_count {
        map.insert("battery_cycle_count".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_charge_voltage {
        map.insert("battery_charge_voltage".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_charge_current {
        map.insert("battery_charge_current".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_charge_current_valid {
        map.insert("battery_charge_current_valid".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_charge_current_max_normal {
        map.insert("battery_charge_current_max_normal".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_charge_current_max_low {
        map.insert("battery_charge_current_max_low".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_charge_current_max_high {
        map.insert("battery_charge_current_max_high".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_charge_current_keep_power {
        map.insert("battery_charge_current_keep_power".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_charge_current_decrease_stepsize1 {
        map.insert("battery_charge_current_decrease_stepsize1".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_charge_current_increase_stepsize1 {
        map.insert("battery_charge_current_increase_stepsize1".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_charge_current_decrease_stepsize2 {
        map.insert("battery_charge_current_decrease_stepsize2".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_charge_current_increase_stepsize2 {
        map.insert("battery_charge_current_increase_stepsize2".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_charge_current_modify_interval {
        map.insert("battery_charge_current_modify_interval".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.battery_charge_temperature {
        map.insert("battery_charge_temperature".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.master_battery_temperature {
        map.insert("master_battery_temperature".into(), json!(v));
    }
    if let Some(ref v) = eb.sdo.keep_power_timer {
        map.insert("keep_power_timer".into(), json!(v));
    }

    serde_json::Value::Object(map)
}

fn try_complete_save(varta: &varta_easyblade::Varta, selected: usize, save_state: &mut SaveState) {
    if let SaveState::Saving { ref file_path } = *save_state {
        let eb = varta.get_easyblade_by_index(selected);
        if let Some(data) = eb {
            if sdo_completion(data) >= TOTAL_SDOS {
                let json_value = easyblade_to_json(data);
                let json_string = serde_json::to_string_pretty(&json_value).unwrap();
                let path = file_path.clone();
                if let Ok(mut file) = fs::File::create(&path) {
                    if let Err(e) = file.write_all(json_string.as_bytes()) {
                        eprintln!("Error writing file: {}", e);
                    }
                } else {
                    eprintln!("Error creating file: {}", path);
                }
                *save_state = SaveState::Saved(path);
            }
        } else {
            *save_state = SaveState::Idle;
        }
    }
}

fn update_config_state(
    config_state: &mut ConfigState,
    varta: &varta_easyblade::Varta,
    selected: usize,
) {
    // Reset config when the selected module changes
    if config_state.loaded_for_selected != Some(selected) {
        config_state.fields.clear();
        config_state.focused_index = 0;
        config_state.loaded_for_selected = Some(selected);
    }

    let eb = match varta.get_easyblade_by_index(selected) {
        Some(eb) => eb,
        None => {
            config_state.fields.clear();
            return;
        },
    };

    // Always show all configuration fields; values are pending until read.
    if config_state.fields.is_empty() {
        config_state.fields = vec![
            // Field 0: Battery Max Charge Voltage (0x3000:02)
            ConfigField::new("Battery Max Charge Voltage", "V"),
            // Field 1: Battery Keep Power Voltage (0x3000:03)
            ConfigField::new("Battery Keep Power Voltage", "V"),
            // Field 2: Keep Power Timer (0x3d00:0c)
            ConfigField::new("Keep Power Timer", "s"),
            // Field 3: Single Cell Max Charge Voltage (0x2104:02)
            ConfigField::new("Single Cell Max Charge Voltage", "V"),
            // Field 4: Battery Charge Current Fully Charged End (0x2304:0a)
            ConfigField::new("Charge Current Fully Charged End", "A"),
        ];
        config_state.fields[0].is_focused = true;
    }

    // Update values from module data as the SDO reads complete
    // (but not while a field is being edited)
    // A focused field is skipped to avoid clobbering user input, unless the
    // user hasn't typed anything into it yet (e.g. initial focus on a
    // not-yet-read value).
    let editable = |field: &ConfigField| !field.is_focused || !field.user_edited;

    // Fields 0 & 1: from BatteryChargeVoltage (0x3000)
    if let Some(ref charge_voltage) = eb.sdo.battery_charge_voltage {
        if let Some(field) = config_state.fields.get_mut(0)
            && editable(field)
        {
            field.update_value(ConfigValue::Float(charge_voltage.charge_max_voltage_v));
        }
        if let Some(field) = config_state.fields.get_mut(1)
            && editable(field)
        {
            field.update_value(ConfigValue::Float(
                charge_voltage.charge_keep_power_voltage_v,
            ));
        }
    }
    // Field 2: Keep Power Timer
    if let Some(ref kpt) = eb.sdo.keep_power_timer
        && let Some(field) = config_state.fields.get_mut(2)
        && editable(field)
    {
        field.update_value(ConfigValue::UInt(kpt.value));
    }
    // Field 3: Single Cell Max Charge Voltage
    if let Some(ref limit) = eb.sdo.cell_voltage_limit
        && let Some(field) = config_state.fields.get_mut(3)
        && editable(field)
    {
        field.update_value(ConfigValue::Float(limit.max_charge_voltage_v));
    }
    // Field 4: Battery Charge Current Fully Charged End
    if let Some(ref current_limit) = eb.sdo.battery_current_limit
        && let Some(field) = config_state.fields.get_mut(4)
        && editable(field)
    {
        field.update_value(ConfigValue::Float(
            current_limit.charge_current_fully_charged_end_ma as f32 / 1000.0,
        ));
    }
}

fn draw_popup(f: &mut Frame, area: Rect, save_state: &SaveState, completion: f64) {
    f.render_widget(Clear, area);

    let popup_block = Block::bordered()
        .title(" Save Module Data ")
        .style(Style::new().bg(Color::Rgb(50, 50, 50)));
    f.render_widget(&popup_block, area);
    let inner = popup_block.inner(area);

    let lines: Vec<Line> = match save_state {
        SaveState::Saving { .. } => {
            let percentage = (completion / TOTAL_SDOS * 100.0).round() as u16;
            let clamped_percentage = percentage.min(100);
            let bar_width: usize = 40;
            let filled = (clamped_percentage as f64 / 100.0 * bar_width as f64).round() as usize;
            let bar_str = format!(
                "{}{}",
                "█".repeat(filled),
                " ".repeat(bar_width.saturating_sub(filled))
            );
            let bar = format!("[{}] {}%", bar_str, clamped_percentage);
            vec![
                Line::from(""),
                Line::from(bar).style(Style::new().fg(Color::Green)),
                Line::from(format!(
                    "Reading SDOs: {}/{}",
                    completion.min(TOTAL_SDOS) as u32,
                    TOTAL_SDOS as u32
                )),
                Line::from("Press 'q' to cancel"),
                Line::from(""),
            ]
        },
        SaveState::Saved(path) => {
            vec![
                Line::from(""),
                Line::from("Data saved successfully!"),
                Line::from(""),
                Line::from(format!("  {}", path)),
                Line::from(""),
                Line::from("Press Enter to dismiss"),
            ]
        },
        SaveState::Idle => vec![],
    };

    let text_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height,
    };
    let text = Paragraph::new(lines).wrap(Wrap { trim: true });
    f.render_widget(text, text_area);
}

fn draw_frame(
    f: &mut Frame,
    varta: &varta_easyblade::Varta,
    selected: usize,
    tab: SelectedTab,
    save_state: &SaveState,
    config_state: &ConfigState,
) {
    let area = f.area();

    // Middle pane: sized just right to show all the modules.
    let middle_len = (varta.easyblade_count() + 3).min(area.height as usize);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Length(middle_len as u16),
            Constraint::Min(5),
        ])
        .split(area);

    // Top: master info
    let top_block = Block::bordered().title(" Master ");
    let top_inner = top_block.inner(layout[0]);
    let master = &varta.master;
    let last_seen = match master.last_seen {
        Some(t) => format_last_seen(t),
        None => String::from("none"),
    };
    if master
        .last_seen
        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs() <= 10)
        .unwrap_or(false)
    {
        let info = format!(
            "{:>7.2} V  {:>7.2} A  SOC: {:>6}   (Last seen {})\n\
             Charge Request: {:>7.2} V, {:>7.2} A  Status: {:>6}\n\
             FET Temp: {:>6.1}  Cell Temp: {:>6.1}\n\
             Capacity: {:>7.2} Ah remaining / {:>7.2} Ah full ({:>7.2} Ah design)",
            master.voltage.unwrap_or(0.0),
            master.current.unwrap_or(0.0),
            master
                .soc
                .map_or("-----".to_string(), |v| format!("{:.1}%", v)),
            last_seen,
            master.charge_voltage_request.unwrap_or(0.0),
            master.charge_current_request.unwrap_or(0.0),
            master
                .battery_status
                .map_or("-----".to_string(), |v| format!("{v:#x}")),
            master.max_battery_fet_temp.unwrap_or(0.0),
            master.max_battery_cell_temp.unwrap_or(0.0),
            master.master_remaining_capacity.unwrap_or(0.0),
            master.master_full_charge_capacity.unwrap_or(0.0),
            master.master_design_capacity.unwrap_or(0.0),
        );
        let text = Paragraph::new(info).wrap(Wrap { trim: true });
        f.render_widget(text, top_inner);
    } else {
        let text = Paragraph::new("No master data");
        f.render_widget(text, top_inner);
    }

    // Middle: module table
    let middle_block = Block::bordered().title(" EasyBlade Modules ");
    f.render_widget(&middle_block, layout[1]);
    let middle_inner = middle_block.inner(layout[1]);

    let header = Row::new(["Node ID", "Serial", "Voltage", "Current", "SOC", "FET", "Last Seen"])
        .style(Style::new().add_modifier(Modifier::BOLD));

    let mut rows = Vec::new();
    for (idx, eb) in varta
        .easyblades
        .iter()
        .filter_map(|e| e.as_ref())
        .enumerate()
    {
        let voltage = eb
            .pdo
            .voltage
            .map_or("----".to_string(), |v| format!("{v:.2} V"));
        let current = eb
            .pdo
            .current
            .map_or("----".to_string(), |c| format!("{c:.2} A"));
        let soc = eb
            .pdo
            .soc
            .map_or("----".to_string(), |v| format!("{:.1}%", v));
        let fet = format_fet_status(eb.pdo.msg_bits.as_ref());
        let last_seen = format_last_seen(eb.last_seen);
        let row = Row::new([
            format!("{}", eb.node_id),
            eb.sdo
                .serial_number
                .as_ref()
                .map_or("----".to_string(), |v| format!("{}", v.value)),
            voltage,
            current,
            soc,
            fet,
            last_seen,
        ]);
        if idx == selected {
            rows.push(row.style(Style::new().add_modifier(Modifier::REVERSED)));
        } else {
            rows.push(row);
        }
    }

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(8),
            Constraint::Percentage(12),
            Constraint::Percentage(15),
            Constraint::Percentage(15),
            Constraint::Percentage(11),
            Constraint::Percentage(11),
            Constraint::Percentage(28),
        ],
    )
    .header(header)
    .column_spacing(1);

    f.render_widget(table, middle_inner);

    // Bottom: tabbed pane
    let bottom_block = Block::bordered().title(" Detail ");
    f.render_widget(&bottom_block, layout[2]);
    let bottom_inner = bottom_block.inner(layout[2]);

    let tab_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3)])
        .split(bottom_inner);

    let tab_titles: Vec<SelectedTab> = vec![
        SelectedTab::ModuleInfo,
        SelectedTab::MsgBits,
        SelectedTab::CellVoltages,
        SelectedTab::ErrorHistory,
        SelectedTab::DeviceOperation,
        SelectedTab::ErrorCounters,
        SelectedTab::CellVoltageLimits,
        SelectedTab::BatteryVoltage,
        SelectedTab::BatteryCurrent,
        SelectedTab::FetTemperature,
        SelectedTab::CellTemperature,
        SelectedTab::CellBalance,
        SelectedTab::Impedance,
        SelectedTab::Capacity,
        SelectedTab::CycleCount,
        SelectedTab::ChargeParameters,
        SelectedTab::ChargeCurrentParams,
        SelectedTab::MaxBatteryTemperature,
        SelectedTab::Configuration,
    ];

    // Build tab labels and calculate widths
    let separator = " ";
    let tab_labels: Vec<String> = tab_titles
        .iter()
        .map(|t| {
            if *t == tab {
                format!("[{}]", t.title())
            } else {
                format!(" {} ", t.title())
            }
        })
        .collect();
    let tab_widths: Vec<usize> = tab_labels.iter().map(|l| l.len()).collect();

    let available_width = tab_layout[0].width as usize;

    // Calculate total width including separators
    let total_width: usize =
        tab_widths.iter().sum::<usize>() + separator.len() * (tab_titles.len() - 1);

    // Calculate scroll offset to keep selected tab visible
    let selected_idx = tab_titles.iter().position(|t| *t == tab).unwrap();
    let (scroll_start, scroll_end) = if total_width > available_width {
        // Find the byte offset of the start and end of each tab in the full string
        let mut tab_positions: Vec<(usize, usize)> = Vec::new();
        let mut offset = 0;
        for w in &tab_widths {
            let start = offset;
            let end = offset + w;
            tab_positions.push((start, end));
            offset += w + separator.len();
        }

        let sel_start = tab_positions[selected_idx].0;
        let sel_end = tab_positions[selected_idx].1;

        // Try to center the selected tab, but ensure it's fully visible
        let mut candidate_start = sel_start.saturating_sub(available_width / 2);

        // Make sure the selected tab fits
        if candidate_start + available_width < sel_end {
            candidate_start = sel_end.saturating_sub(available_width);
        }

        // Clamp to valid range
        let max_start = total_width.saturating_sub(available_width);
        let candidate_start = candidate_start.min(max_start);

        (candidate_start, candidate_start + available_width)
    } else {
        (0, total_width)
    };

    // Build the full indicator string
    let full_indicator = tab_labels.join(separator);
    let indicator = if scroll_start > 0 || scroll_end < full_indicator.len() {
        let chars: Vec<char> = full_indicator.chars().collect();
        let mut byte_pos = 0;
        let mut start_char = None;
        let mut end_char = chars.len();
        for (i, c) in chars.iter().enumerate() {
            if byte_pos >= scroll_start && start_char.is_none() {
                start_char = Some(i);
            }
            let next_pos = byte_pos + c.len_utf8();
            if next_pos > scroll_end {
                end_char = i;
                break;
            }
            byte_pos = next_pos;
        }
        let start = start_char.unwrap_or(0);
        chars[start..end_char].iter().collect()
    } else {
        full_indicator
    };

    let tabs_text = Paragraph::new(indicator).style(Style::new().fg(Color::White));
    f.render_widget(tabs_text, tab_layout[0]);

    let content_area = tab_layout[1];
    let eb = varta.get_easyblade_by_index(selected);

    match tab {
        SelectedTab::ModuleInfo => {
            if let Some(eb) = eb {
                let info = format!(
                    "Node ID:              {}\n\
                     Serial Number:        {}\n\
                     Software Version:     {}\n\
                     Hardware Version:     {}\n\
                     Voltage:              {}\n\
                     Current:              {}\n\
            SOC:                  {}\n\
                      SOH:                  {}\n\
                      Last Seen:            {}\n",
                    eb.node_id,
                    eb.sdo
                        .serial_number
                        .as_ref()
                        .map_or("N/A".to_string(), |v| format!("{}", v.value)),
                    eb.sdo
                        .software_version
                        .as_ref()
                        .map(|v| v.value.as_str())
                        .unwrap_or("N/A"),
                    eb.sdo
                        .hardware_version
                        .as_ref()
                        .map(|v| v.value.as_str())
                        .unwrap_or("N/A"),
                    eb.pdo
                        .voltage
                        .map_or("N/A".to_string(), |v| format!("{:.2} V", v)),
                    eb.pdo
                        .current
                        .map_or("N/A".to_string(), |c| format!("{:.2} A", c)),
                    eb.pdo
                        .soc
                        .map_or("N/A".to_string(), |v| format!("{:.1}%", v)),
                    eb.pdo
                        .soh
                        .map_or("N/A".to_string(), |v| format!("{:.1}%", v)),
                    format_last_seen(eb.last_seen),
                );
                let text = Paragraph::new(info).wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            } else {
                let text = Paragraph::new("No module selected");
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::CellVoltages => {
            if let Some(eb) = eb {
                if let (Some(voltages), Some(min_max)) =
                    (&eb.sdo.cell_voltages, &eb.sdo.cell_voltage_min_max)
                {
                    let mut lines: String = voltages
                        .values
                        .iter()
                        .enumerate()
                        .map(|(i, v)| format!("Cell {:>2}: {:.3} V\n", i + 1, v))
                        .collect();
                    lines.push_str(&format!(
                        "\nMin Cell Voltage: {:.3} V\n\
                         Max Cell Voltage: {:.3} V",
                        min_max.min_voltage_v, min_max.max_voltage_v,
                    ));
                    let text = Paragraph::new(lines).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },
        SelectedTab::ErrorHistory => {
            if let Some(eb) = eb {
                if let Some(ref errors) = eb.sdo.device_errors {
                    let error_text: Text = errors
                        .values
                        .iter()
                        .enumerate()
                        .map(|(i, e)| format!("[{:#03}] {:?}\n", i + 1, e))
                        .collect();
                    let text = Paragraph::new(error_text).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("No error history").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::DeviceOperation => {
            if let Some(eb) = eb {
                if let Some(v) = &eb.sdo.device_operation_time {
                    let info = format!(
                        "Operation Minutes (by temp range):\n\
                         <0°C:       {:>3} min\n\
                         0-40°C:     {:>3} min\n\
                         40-60°C:    {:>3} min\n\
                         60-80°C:    {:>3} min\n\
                         >80°C:      {:>3} min\n\
                         Operation Hours (by temp range):\n\
                         <0°C:       {:>10} h\n\
                         0-40°C:     {:>10} h\n\
                         40-60°C:    {:>10} h\n\
                         60-80°C:    {:>10} h\n\
                         >80°C:      {:>10} h\n",
                        v.minutes_below_zero,
                        v.minutes_zero_to_40,
                        v.minutes_40_to_60,
                        v.minutes_60_to_80,
                        v.minutes_above_80,
                        v.hours_below_zero,
                        v.hours_zero_to_40,
                        v.hours_40_to_60,
                        v.hours_60_to_80,
                        v.hours_above_80,
                    );
                    let text = Paragraph::new(info).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::ErrorCounters => {
            if let Some(eb) = eb {
                if let Some(ref counters) = eb.sdo.device_error_counter {
                    let lines = format!(
                        "over_temp_laden_zellen:                          {:>6}\n\
                         under_temp_laden_zellen:                         {:>6}\n\
                         over_temp_laden_fet:                             {:>6}\n\
                         over_temp_entladen_zellen:                       {:>6}\n\
                         under_temp_entladen_zellen:                      {:>6}\n\
                         over_temp_entladen_fet:                          {:>6}\n\
                         over_temp_clamp:                                 {:>6}\n\
                         over_voltage:                                    {:>6}\n\
                         under_voltage:                                   {:>6}\n\
                         deep_low_voltage:                                {:>6}\n\
                         cell_disbalance:                                 {:>6}\n\
                         akku_pack_spn_min_error:                         {:>6}\n\
                         akku_pack_spn_max_alarm:                         {:>6}\n\
                         akku_pack_fused_spn_diff_error:                  {:>6}\n\
                         akku_pwr_spn_diff_error:                         {:>6}\n\
                         akku_pwr_spn_min_error:                          {:>6}\n\
                         akku_pwr_spn_max_error:                          {:>6}\n\
                         akku_netz_spn_min_error:                         {:>6}\n\
                         akku_netz_spn_max_alarm:                         {:>6}\n\
                         akku_rekuperation_spn_max_alarm:                 {:>6}\n\
                         i_charge_sc:                                     {:>6}\n\
                         i_charge_occ_1:                                  {:>6}\n\
                         i_charge_occ_2:                                  {:>6}\n\
                         i_charge_occ_3:                                  {:>6}\n\
                         i_discharge_sc:                                  {:>6}\n\
                         i_discharge_ocd_1:                               {:>6}\n\
                         i_discharge_ocd_2:                               {:>6}\n\
                         i_discharge_ocd_3:                               {:>6}\n\
                         i_akku_diff_error:                               {:>6}\n\
                         scnd_spn_min_error:                              {:>6}\n\
                         scnd_spn_max_error:                              {:>6}\n\
                         scnd_uc_fet_enable:                              {:>6}\n\
                         scnd_current_sense_ein:                          {:>6}\n\
                         scnd_voltage_sense_ein:                          {:>6}\n\
                         scnd_temp_cell_sense_ein:                        {:>6}\n\
                         scnd_temp_fet_sense_ein:                         {:>6}\n\
                         scnd_pyro_fuse_eject_sense_ein:                  {:>6}\n\
                         scnd_i_discharge_fet_error:                      {:>6}\n\
                         scnd_i_charge_fet_error:                         {:>6}\n\
                         scnd_voltage_error:                              {:>6}\n\
                         adc_spn_min_scale:                               {:>6}\n\
                         adc_spn_max_scale:                               {:>6}\n\
                         adc_temp_zellen_min_scale:                       {:>6}\n\
                         adc_temp_zellen_max_scale:                       {:>6}\n\
                         adc_temp_fet_min_scale:                          {:>6}\n\
                         adc_temp_fet_max_scale:                          {:>6}\n\
                         adc_temp_clamp_min_scale:                        {:>6}\n\
                         adc_temp_clamp_max_scale:                        {:>6}\n\
                         adc_i_charge_min_scale:                          {:>6}\n\
                         adc_i_charge_max_scale:                          {:>6}\n\
                         adc_i_discharge_min_scale:                       {:>6}\n\
                         adc_i_discharge_max_scale:                       {:>6}\n\
                         i_discharge_fet_error:                           {:>6}\n\
                         i_charge_fet_error:                              {:>6}\n\
                         i_discharge_charge_fet_error:                    {:>6}\n\
                         temp_discharge_error_lock:                       {:>6}\n\
                         temp_charge_error_lock:                          {:>6}\n\
                         over_charge_current_alarm_recuperation:          {:>6}\n\
                         over_charge_cell_voltage_alarm_recuperation:     {:>6}\n\
                         v24_spn_min_error:                               {:>6}\n\
                         v24_spn_max_error:                               {:>6}\n\
                         can_network_not_conf_node_id:                    {:>6}\n\
                         can_network_double_node_id:                      {:>6}\n\
                         parameter_configuration_error:                   {:>6}\n",
                        counters.over_temp_laden_zellen,
                        counters.under_temp_laden_zellen,
                        counters.over_temp_laden_fet,
                        counters.over_temp_entladen_zellen,
                        counters.under_temp_entladen_zellen,
                        counters.over_temp_entladen_fet,
                        counters.over_temp_clamp,
                        counters.over_voltage,
                        counters.under_voltage,
                        counters.deep_low_voltage,
                        counters.cell_disbalance,
                        counters.akku_pack_spn_min_error,
                        counters.akku_pack_spn_max_alarm,
                        counters.akku_pack_fused_spn_diff_error,
                        counters.akku_pwr_spn_diff_error,
                        counters.akku_pwr_spn_min_error,
                        counters.akku_pwr_spn_max_error,
                        counters.akku_netz_spn_min_error,
                        counters.akku_netz_spn_max_alarm,
                        counters.akku_rekuperation_spn_max_alarm,
                        counters.i_charge_sc,
                        counters.i_charge_occ_1,
                        counters.i_charge_occ_2,
                        counters.i_charge_occ_3,
                        counters.i_discharge_sc,
                        counters.i_discharge_ocd_1,
                        counters.i_discharge_ocd_2,
                        counters.i_discharge_ocd_3,
                        counters.i_akku_diff_error,
                        counters.scnd_spn_min_error,
                        counters.scnd_spn_max_error,
                        counters.scnd_uc_fet_enable,
                        counters.scnd_current_sense_ein,
                        counters.scnd_voltage_sense_ein,
                        counters.scnd_temp_cell_sense_ein,
                        counters.scnd_temp_fet_sense_ein,
                        counters.scnd_pyro_fuse_eject_sense_ein,
                        counters.scnd_i_discharge_fet_error,
                        counters.scnd_i_charge_fet_error,
                        counters.scnd_voltage_error,
                        counters.adc_spn_min_scale,
                        counters.adc_spn_max_scale,
                        counters.adc_temp_zellen_min_scale,
                        counters.adc_temp_zellen_max_scale,
                        counters.adc_temp_fet_min_scale,
                        counters.adc_temp_fet_max_scale,
                        counters.adc_temp_clamp_min_scale,
                        counters.adc_temp_clamp_max_scale,
                        counters.adc_i_charge_min_scale,
                        counters.adc_i_charge_max_scale,
                        counters.adc_i_discharge_min_scale,
                        counters.adc_i_discharge_max_scale,
                        counters.i_discharge_fet_error,
                        counters.i_charge_fet_error,
                        counters.i_discharge_charge_fet_error,
                        counters.temp_discharge_error_lock,
                        counters.temp_charge_error_lock,
                        counters.over_charge_current_alarm_recuperation,
                        counters.over_charge_cell_voltage_alarm_recuperation,
                        counters.v24_spn_min_error,
                        counters.v24_spn_max_error,
                        counters.can_network_not_conf_node_id,
                        counters.can_network_double_node_id,
                        counters.parameter_configuration_error,
                    );
                    let text = Paragraph::new(lines).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::CellVoltageLimits => {
            if let Some(eb) = eb {
                if let Some(limit) = &eb.sdo.cell_voltage_limit {
                    let info = format!(
                        "Over Voltage Error:                  {:.3} V\n\
                         Max Charge Voltage:                  {:.3} V\n\
                         Fully Charged Voltage:               {:.3} V\n\
                         Near Fully Charged Voltage:          {:.3} V\n\
                         Fully Charged Reset Voltage:         {:.3} V\n\
                         EDV Reset Voltage:                   {:.3} V\n\
                         Near Empty Voltage Warning:          {:.3} V\n\
                         Near Empty Voltage EDV1:             {:.3} V\n\
                         Empty Voltage EDV0:                  {:.3} V\n\
                         Min Error Reset Voltage:             {:.3} V\n\
                         EDV OFF Voltage:                     {:.3} V\n\
                         Under Voltage Error:                 {:.3} V\n\
                         Deep Low Voltage Error:              {:.3} V\n\
                         Max Charge Voltage (no password):    {:.3} V\n",
                        limit.over_voltage_error_v,
                        limit.max_charge_voltage_v,
                        limit.fully_charged_voltage_v,
                        limit.near_fully_charged_voltage_v,
                        limit.fully_charged_reset_voltage_v,
                        limit.edv_reset_voltage_v,
                        limit.near_empty_voltage_warning_v,
                        limit.near_empty_voltage_edv1_v,
                        limit.empty_voltage_edv0_v,
                        limit.min_error_reset_voltage_v,
                        limit.edv_off_voltage_v,
                        limit.under_voltage_error_v,
                        limit.deep_low_voltage_error_v,
                        limit.max_charge_voltage_no_password_v,
                    );
                    let text = Paragraph::new(info).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::BatteryVoltage => {
            if let Some(eb) = eb {
                if let (Some(voltage), Some(limit)) =
                    (&eb.sdo.battery_voltage, &eb.sdo.battery_voltage_limit)
                {
                    let info = format!(
                        "SumOfCell Voltage:       {:.3} V\n\
                         Internal Connector:      {:.3} V\n\
                         External Connector:      {:.3} V\n\
                         Internal-External MinΔ:  {:.3} V\n",
                        voltage.sum_of_cell_voltage_v,
                        voltage.internal_connector_voltage_v,
                        voltage.external_connector_voltage_v,
                        limit.internal_external_min_delta_v,
                    );
                    let text = Paragraph::new(info).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::BatteryCurrent => {
            if let Some(eb) = eb {
                if let (Some(current), Some(limit)) =
                    (&eb.sdo.battery_current, &eb.sdo.battery_current_limit)
                {
                    let info = format!(
                        "Fast Current:           {:>10.2} A\n\
                         Weighted Avg Current:   {:>10.2} A\n\
                         Integrated Current:     {:>10.2} A\n\
                         Average 1s Current:     {:>10.2} A\n\
                         Average 10s Current:    {:>10.2} A\n\
                         Discharge SC Error:     {:>10.2} A\n",
                        current.fast_current_a,
                        current.weighted_avg_current_a,
                        current.integrated_current_a,
                        current.average_1s_current_a,
                        current.average_10s_current_a,
                        limit.discharge_sc_error_a,
                    );
                    let text = Paragraph::new(info).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::FetTemperature => {
            if let Some(eb) = eb {
                if let (Some(temp), Some(min_max), Some(limit)) = (
                    &eb.sdo.fet_temperature,
                    &eb.sdo.fet_temperature_min_max,
                    &eb.sdo.fet_temperature_limit,
                ) {
                    let info = format!(
                        "FET Temperature 1:    {:>7.1} °C\n\
                         FET Temperature 2:    {:>7.1} °C\n\
                         Min FET Temperature:  {:>7.1} °C\n\
                         Max FET Temperature:  {:>7.1} °C\n\
                         Discharge Over Temp:  {:>7.1} °C\n",
                        temp.temperature_1_c,
                        temp.temperature_2_c,
                        min_max.min_temperature_c,
                        min_max.max_temperature_c,
                        limit.discharge_over_temp_c,
                    );
                    let text = Paragraph::new(info).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::CellTemperature => {
            if let Some(eb) = eb {
                if let (Some(temp), Some(min_max), Some(limit)) = (
                    &eb.sdo.cell_temperature,
                    &eb.sdo.cell_temperature_min_max,
                    &eb.sdo.cell_temperature_limit,
                ) {
                    let info = format!(
                        "Cell Temperature 1:   {:>7.1} °C\n\
                         Cell Temperature 2:   {:>7.1} °C\n\
                         Cell Temperature 3:   {:>7.1} °C\n\
                         Cell Temperature 4:   {:>7.1} °C\n\
                         Cell Temperature 5:   {:>7.1} °C\n\
                         Cell Temperature 6:   {:>7.1} °C\n\
                         Min Cell Temperature: {:>7.1} °C\n\
                         Max Cell Temperature: {:>7.1} °C\n\
                         Discharge Over Temp:  {:>7.1} °C\n",
                        temp.temperature_1_c,
                        temp.temperature_2_c,
                        temp.temperature_3_c,
                        temp.temperature_4_c,
                        temp.temperature_5_c,
                        temp.temperature_6_c,
                        min_max.min_temperature_c,
                        min_max.max_temperature_c,
                        limit.discharge_over_temp_c,
                    );
                    let text = Paragraph::new(info).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::CellBalance => {
            if let Some(eb) = eb {
                if let (Some(status), Some(limit)) =
                    (&eb.sdo.cell_balance_status, &eb.sdo.cell_balance_limit)
                {
                    let info = format!(
                        "Balance Status Register:      {:#05x}\n\
                         Balance FET Active:           {:#05x}\n\
                         Balance FET Active Persistent:{:#05x}\n\
                         Balance Start Diff Voltage:   {:.3} V\n",
                        status.balance_status_register,
                        status.balance_fet_active,
                        status.balance_fet_active_persistent,
                        limit.balance_start_diff_voltage_v,
                    );
                    let text = Paragraph::new(info).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::Impedance => {
            if let Some(eb) = eb {
                if let Some(ref imp) = eb.sdo.cell_impedance {
                    let mut lines = String::new();
                    for (i, v) in imp.cell_impedances_mohm.iter().enumerate() {
                        lines.push_str(&format!("Cell {:>2} Impedance: {:>6} mΩ\n", i + 1, v));
                    }
                    lines.push_str(&format!("Low Temp Factor:  {:>6}\n", imp.low_temp_factor));
                    lines.push_str(&format!("High Temp Factor: {:>6}\n", imp.high_temp_factor));
                    let text = Paragraph::new(lines).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::Capacity => {
            if let Some(eb) = eb {
                if let Some(v) = &eb.sdo.battery_capacity {
                    let info = format!(
                        "Design Capacity:           {:>10.2} Ah\n\
                         Full Charge Capacity:      {:>10.2} Ah\n\
                         Remaining Capacity:        {:>10.2} Ah\n\
                         SOC:                       {:>10} %\n\
                         SOH:                       {:>10} %\n\
                         Total Discharged Capacity: {:>10.2} Ah\n\
                         Total Charged Capacity:    {:>10.2} Ah\n",
                        v.design_capacity_ah,
                        v.full_charge_capacity_ah,
                        v.remaining_capacity_ah,
                        format!("{:.1}", v.soc_percent),
                        format!("{:.1}", v.soh_percent),
                        v.total_discharged_capacity_ah,
                        v.total_charged_capacity_ah,
                    );
                    let text = Paragraph::new(info).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::CycleCount => {
            if let Some(eb) = eb {
                if let Some(v) = &eb.sdo.battery_cycle_count {
                    let info = format!(
                        "Discharge Cycles:                    {:>10}\n\
                         Discharge Learning Cycles:           {:>10}\n\
                         Discharge Cycles After Learning:     {:>10}\n\
                         Charge Cycles Completed:             {:>10}\n\
                         Charge Cycles Started:               {:>10}\n\
                         Discharge Use Detect:                {:>10}\n\
                         Charge Use Low Temperature:          {:>10}\n\
                         Charge Use Normal Temperature:       {:>10}\n\
                         Charge Use High Temperature:         {:>10}\n",
                        v.discharge_cycles,
                        v.discharge_learning_cycles,
                        v.discharge_cycles_after_learning,
                        v.charge_cycles_completed,
                        v.charge_cycles_started,
                        v.discharge_use_detect,
                        v.charge_use_low_temperature,
                        v.charge_use_normal_temperature,
                        v.charge_use_high_temperature,
                    );
                    let text = Paragraph::new(info).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::ChargeParameters => {
            if let Some(eb) = eb {
                if let (Some(voltage), Some(current), Some(temp)) = (
                    &eb.sdo.battery_charge_voltage,
                    &eb.sdo.battery_charge_current,
                    &eb.sdo.battery_charge_temperature,
                ) {
                    let info = format!(
                        "Charge Voltage Valid:    {:.3} V\n\
                         Charge Max Voltage:      {:.3} V\n\
                         Charge Keep Power Volt:  {:.3} V\n\
                         Charge Current Valid:    {:.3} A\n\
                         Charge Max Current N:    {:.3} A\n\
                         Charge Max Current Low:  {:.3} A\n\
                         Charge Max Current High: {:.3} A\n\
                         Charge Keep Power Curr:  {:.3} A\n\
                         Charge Temp Min Low:     {:>7.1} °C\n\
                         Charge Temp Min Normal:  {:>7.1} °C\n\
                         Charge Temp Max Normal:  {:>7.1} °C\n\
                         Charge Temp Max High:    {:>7.1} °C\n",
                        voltage.charge_voltage_valid_v,
                        voltage.charge_max_voltage_v,
                        voltage.charge_keep_power_voltage_v,
                        current.charge_current_valid_a,
                        current.charge_max_current_n_a,
                        current.charge_max_current_low_a,
                        current.charge_max_current_high_a,
                        current.charge_keep_power_current_a,
                        temp.temp_min_low_c,
                        temp.temp_min_normal_c,
                        temp.temp_max_normal_c,
                        temp.temp_max_high_c,
                    );
                    let text = Paragraph::new(info).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::ChargeCurrentParams => {
            if let Some(eb) = eb {
                let sdo = &eb.sdo;
                let mut lines = String::from("Object 0x3100 Battery Charge Current Parameter\n\n");
                let add = |lines: &mut String,
                           label: &str,
                           raw: Option<u32>,
                           human: Option<f32>,
                           unit: &str| {
                    match (raw, human) {
                        (Some(r), Some(h)) => {
                            lines.push_str(&format!("{}: {:>8} ({:.3} {})\n", label, r, h, unit));
                        },
                        _ => lines.push_str(&format!("{}: {:>8}\n", label, "pending")),
                    }
                };
                add(
                    &mut lines,
                    "Charge Current Valid",
                    sdo.battery_charge_current_valid.as_ref().map(|v| v.value),
                    sdo.battery_charge_current_valid.as_ref().map(|v| v.value_a),
                    "A",
                );
                add(
                    &mut lines,
                    "Max Current Normal Temp",
                    sdo.battery_charge_current_max_normal
                        .as_ref()
                        .map(|v| v.value),
                    sdo.battery_charge_current_max_normal
                        .as_ref()
                        .map(|v| v.value_a),
                    "A",
                );
                add(
                    &mut lines,
                    "Max Current Low Temp",
                    sdo.battery_charge_current_max_low.as_ref().map(|v| v.value),
                    sdo.battery_charge_current_max_low
                        .as_ref()
                        .map(|v| v.value_a),
                    "A",
                );
                add(
                    &mut lines,
                    "Max Current High Temp",
                    sdo.battery_charge_current_max_high
                        .as_ref()
                        .map(|v| v.value),
                    sdo.battery_charge_current_max_high
                        .as_ref()
                        .map(|v| v.value_a),
                    "A",
                );
                add(
                    &mut lines,
                    "Keep Power Current",
                    sdo.battery_charge_current_keep_power
                        .as_ref()
                        .map(|v| v.value),
                    sdo.battery_charge_current_keep_power
                        .as_ref()
                        .map(|v| v.value_a),
                    "A",
                );
                add(
                    &mut lines,
                    "Decrease Stepsize 1",
                    sdo.battery_charge_current_decrease_stepsize1
                        .as_ref()
                        .map(|v| v.value as u32),
                    sdo.battery_charge_current_decrease_stepsize1
                        .as_ref()
                        .map(|v| v.value_a),
                    "A",
                );
                add(
                    &mut lines,
                    "Increase Stepsize 1",
                    sdo.battery_charge_current_increase_stepsize1
                        .as_ref()
                        .map(|v| v.value as u32),
                    sdo.battery_charge_current_increase_stepsize1
                        .as_ref()
                        .map(|v| v.value_a),
                    "A",
                );
                add(
                    &mut lines,
                    "Decrease Stepsize 2",
                    sdo.battery_charge_current_decrease_stepsize2
                        .as_ref()
                        .map(|v| v.value as u32),
                    sdo.battery_charge_current_decrease_stepsize2
                        .as_ref()
                        .map(|v| v.value_a),
                    "A",
                );
                add(
                    &mut lines,
                    "Increase Stepsize 2",
                    sdo.battery_charge_current_increase_stepsize2
                        .as_ref()
                        .map(|v| v.value as u32),
                    sdo.battery_charge_current_increase_stepsize2
                        .as_ref()
                        .map(|v| v.value_a),
                    "A",
                );
                add(
                    &mut lines,
                    "Modify Interval Time",
                    sdo.battery_charge_current_modify_interval
                        .as_ref()
                        .map(|v| v.value),
                    sdo.battery_charge_current_modify_interval
                        .as_ref()
                        .map(|v| v.value_ms),
                    "ms",
                );
                let text = Paragraph::new(lines).wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::MaxBatteryTemperature => {
            if let Some(eb) = eb {
                if let Some(v) = &eb.sdo.master_battery_temperature {
                    let info = format!(
                        "Master Max FET Temperature:  {:>7.1} °C\n\
                         Master Max Cell Temperature: {:>7.1} °C\n",
                        v.max_fet_temperature_c, v.max_cell_temperature_c,
                    );
                    let text = Paragraph::new(info).wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::MsgBits => {
            if let Some(eb) = eb {
                if let Some(m) = &eb.pdo.msg_bits {
                    let left_text = format!(
                        "=== Info ===\n\
                         Empty:              {}\n\
                         Almost Empty:       {}\n\
                         CHG FET Closed:     {}\n\
                         DSG FET Closed:     {}\n\
                         Bypass FET On:      {}\n\
                         Fully Charged:      {}\n\
                         === Warnings ===\n\
                         Low Voltage:        {}\n\
                         Low SOC:            {}\n\
                         Reserve SOC:        {}\n\
                         O/U Temp Discharge: {}\n\
                         O/U Temp Charge:    {}\n\
                         Max Chg Recup:      {}\n\
                         CAN Network Fail:   {}\n\
                         Deactivation Enb:   {}\n\
                         Node ID Proc Enb:   {}\n\
                         Unknown:            {}\n",
                        yes_no(m.info_bit_0_empty),
                        yes_no(m.info_bit_1_almost_empty),
                        yes_no(m.info_bit_2_chgfet_closed),
                        yes_no(m.info_bit_3_dsgfet_closed),
                        yes_no(m.info_bit_4_bypass_fet_on),
                        yes_no(m.info_bit_6_fully_charged),
                        yes_no(m.warn_bit_0_low_voltage),
                        yes_no(m.warn_bit_1_low_soc),
                        yes_no(m.warn_bit_2_reserve_soc),
                        yes_no(m.warn_bit_3_over_or_under_temp_discharge),
                        yes_no(m.warn_bit_4_over_or_under_temp_charge),
                        yes_no(m.warn_bit_7_max_charge_condition_recuperation),
                        yes_no(m.warn_bit_11_can_network_failure),
                        yes_no(m.warn_bit_12_set_deactivation_enable),
                        yes_no(m.warn_bit_14_set_node_id_process_enable),
                        yes_no(m.warn_bit_15_unknown),
                    );
                    let right_text = format!(
                        "=== Errors ===\n\
                         Error Lock Discharge:   {}\n\
                         Error Lock Charge:      {}\n\
                         Over Charge Recup:      {}\n\
                         Short Circuit Charge:   {}\n\
                         Short Circuit Discharge:{}\n\
                         Max Voltage Alarm:      {}\n\
                         Discharge FET Error:    {}\n\
                         Charge FET Error:       {}\n\
                         Max Charge Current:     {}\n\
                         Max Discharge Current:  {}\n\
                         Under Charge Alarm:     {}\n\
                         Over Charge Alarm:      {}\n\
                         O/U Temp Charge:        {}\n\
                         O/U Temp Discharge:     {}\n\
                         Module Defect:          {}\n\
                         Unknown:                {}\n\
                         === Charge ===\n\
                         Voltage Enabled:        {}\n\
                         Voltage Keep Power:     {}\n\
                         Current Enable:         {}\n\
                         Current Keep Power:     {}\n\
                         Current Low Temp:       {}\n\
                         Current Normal Temp:    {}\n\
                         Current High Temp:      {}\n\
                         Max Current Request:    {}\n\
                         Max Cell Volt Request:  {}\n\
                         Master Chgr Output Off: {}\n\
                         FET Disable Temp Cells: {}\n\
                         Charging Ready:         {}\n\
                         Supply Cond Ready:      {}\n",
                        yes_no(m.error_bit_0_error_lock_flag_discharge),
                        yes_no(m.error_bit_1_error_lock_flag_charge),
                        yes_no(m.error_bit_2_over_charge_condition_recuperation),
                        yes_no(m.error_bit_3_shortcircuit_charge_alarm),
                        yes_no(m.error_bit_4_shortcircuit_discharge_alarm),
                        yes_no(m.error_bit_5_max_voltage_alarm),
                        yes_no(m.error_bit_6_discharge_fet_error),
                        yes_no(m.error_bit_7_charge_fet_error),
                        yes_no(m.error_bit_8_max_charge_current_alarm),
                        yes_no(m.error_bit_9_max_discharge_current_alarm),
                        yes_no(m.error_bit_10_under_charge_alarm),
                        yes_no(m.error_bit_11_over_charge_alarm),
                        yes_no(m.error_bit_12_over_under_temp_charge),
                        yes_no(m.error_bit_13_over_under_temp_discharge),
                        yes_no(m.error_bit_14_module_defect),
                        yes_no(m.error_bit_15_uknown),
                        yes_no(m.charge_bit_0_charge_voltage_enabled),
                        yes_no(m.charge_bit_1_charge_voltage_keep_power),
                        yes_no(m.charge_bit_4_charge_current_enable),
                        yes_no(m.charge_bit_5_charge_current_keep_power),
                        yes_no(m.charge_bit_6_charge_current_low_temp_range),
                        yes_no(m.charge_bit_7_charge_current_normal_temp_range),
                        yes_no(m.charge_bit_8_charge_current_high_temp_range),
                        yes_no(m.charge_bit_10_charge_max_charge_current_request),
                        yes_no(m.charge_bit_11_charge_max_charge_cell_voltage_request),
                        yes_no(m.charge_bit_12_charge_master_set_charger_output_off),
                        yes_no(m.charge_bit_13_charge_fet_disable_temp_range_cells),
                        yes_no(m.charge_bit_14_master_charger_control_charging_ready),
                        yes_no(m.charge_bit_15_charger_supply_conditions_ready),
                    );

                    let col_layout = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .split(content_area);

                    let left = Paragraph::new(left_text).wrap(Wrap { trim: true });
                    let right = Paragraph::new(right_text).wrap(Wrap { trim: true });
                    f.render_widget(left, col_layout[0]);
                    f.render_widget(right, col_layout[1]);
                } else {
                    let text = Paragraph::new("Pending...").wrap(Wrap { trim: true });
                    f.render_widget(text, content_area);
                }
            } else {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },

        SelectedTab::Configuration => {
            if config_state.fields.is_empty() {
                let text = Paragraph::new("No module selected").wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            } else {
                let mut lines: Vec<Line> = Vec::new();
                for field in config_state.fields.iter() {
                    let mut spans: Vec<Span> = Vec::new();
                    spans.push(Span::raw(format!("{}: ", field.label)));

                    if field.is_focused {
                        spans.push(Span::styled(
                            format!("{} {}", field.edit_buffer, field.unit),
                            Style::new().add_modifier(Modifier::REVERSED),
                        ));
                    } else if let Some(value) = &field.value {
                        spans.push(Span::raw(format!("{} {}", value.edit_str(), field.unit)));
                    } else {
                        spans.push(Span::styled(
                            "Pending...".to_string(),
                            Style::new().fg(Color::Gray),
                        ));
                    }

                    lines.push(Line::from(spans));

                    if let Some(ref status) = field.status {
                        let status_style = if status.starts_with("OK") {
                            Style::new().fg(Color::Green)
                        } else {
                            Style::new().fg(Color::Red)
                        };
                        lines.push(Line::from(Span::styled(
                            format!("    {}", status),
                            status_style,
                        )));
                    }
                }

                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Tab: next field  Enter: write value  Backspace: delete",
                    Style::new().fg(Color::Gray),
                )));

                let text = Paragraph::new(lines).wrap(Wrap { trim: true });
                f.render_widget(text, content_area);
            }
        },
    }

    if save_state.is_active() {
        let popup_width = 60;
        let popup_height = 10;
        let popup_area = Rect::new(
            area.width / 2 - popup_width / 2,
            area.height / 2 - popup_height / 2,
            popup_width,
            popup_height,
        );
        let eb_ref = varta.get_easyblade_by_index(selected);
        let completion = eb_ref.map(sdo_completion).unwrap_or(TOTAL_SDOS);
        draw_popup(f, popup_area, save_state, completion);
    }
}

struct VartaSdoTask {
    sdo_request_tx: tokio::sync::mpsc::UnboundedSender<(u8, SdoRequest)>,
    cancellation_token: tokio_util::sync::CancellationToken,
}

fn sdo_request_name(request: &SdoRequest) -> &'static str {
    match request {
        SdoRequest::SerialNumber => "SerialNumber",
        SdoRequest::SoftwareVersion => "SoftwareVersion",
        SdoRequest::HardwareVersion => "HardwareVersion",
        SdoRequest::DeviceErrorHistory => "DeviceErrorHistory",
        SdoRequest::CellVoltages => "CellVoltages",
        SdoRequest::DeviceConfigInfo => "DeviceConfigInfo",
        SdoRequest::DeviceSerialNumberInfo => "DeviceSerialNumberInfo",
        SdoRequest::DeviceDateInfo => "DeviceDateInfo",
        SdoRequest::DeviceVariantInfo => "DeviceVariantInfo",
        SdoRequest::DeviceControlParam => "DeviceControlParam",
        SdoRequest::DeviceOperationTime => "DeviceOperationTime",
        SdoRequest::DeviceErrorCounter => "DeviceErrorCounter",
        SdoRequest::CellVoltageMinMax => "CellVoltageMinMax",
        SdoRequest::CellVoltageLimit => "CellVoltageLimit",
        SdoRequest::BatteryVoltage => "BatteryVoltage",
        SdoRequest::BatteryVoltageLimit => "BatteryVoltageLimit",
        SdoRequest::BatteryCurrent => "BatteryCurrent",
        SdoRequest::BatteryCurrentLimit => "BatteryCurrentLimit",
        SdoRequest::FetTemperature => "FetTemperature",
        SdoRequest::FetTemperatureMinMax => "FetTemperatureMinMax",
        SdoRequest::FetTemperatureLimit => "FetTemperatureLimit",
        SdoRequest::CellTemperature => "CellTemperature",
        SdoRequest::CellTemperatureMinMax => "CellTemperatureMinMax",
        SdoRequest::CellTemperatureLimit => "CellTemperatureLimit",
        SdoRequest::CellBalanceStatus => "CellBalanceStatus",
        SdoRequest::CellBalanceLimit => "CellBalanceLimit",
        SdoRequest::CellImpedance => "CellImpedance",
        SdoRequest::BatteryCapacity => "BatteryCapacity",
        SdoRequest::BatteryCapacityParam => "BatteryCapacityParam",
        SdoRequest::BatteryCycleCount => "BatteryCycleCount",
        SdoRequest::BatteryChargeVoltage => "BatteryChargeVoltage",
        SdoRequest::BatteryChargeCurrent => "BatteryChargeCurrent",
        SdoRequest::BatteryChargeCurrentValid => "BatteryChargeCurrentValid",
        SdoRequest::BatteryChargeCurrentMaxNormal => "BatteryChargeCurrentMaxNormal",
        SdoRequest::BatteryChargeCurrentMaxLow => "BatteryChargeCurrentMaxLow",
        SdoRequest::BatteryChargeCurrentMaxHigh => "BatteryChargeCurrentMaxHigh",
        SdoRequest::BatteryChargeCurrentKeepPower => "BatteryChargeCurrentKeepPower",
        SdoRequest::BatteryChargeCurrentDecreaseStepsize1 => {
            "BatteryChargeCurrentDecreaseStepsize1"
        },
        SdoRequest::BatteryChargeCurrentIncreaseStepsize1 => {
            "BatteryChargeCurrentIncreaseStepsize1"
        },
        SdoRequest::BatteryChargeCurrentDecreaseStepsize2 => {
            "BatteryChargeCurrentDecreaseStepsize2"
        },
        SdoRequest::BatteryChargeCurrentIncreaseStepsize2 => {
            "BatteryChargeCurrentIncreaseStepsize2"
        },
        SdoRequest::BatteryChargeCurrentModifyInterval => "BatteryChargeCurrentModifyInterval",
        SdoRequest::BatteryChargeTemperature => "BatteryChargeTemperature",
        SdoRequest::MasterBatteryTemperature => "MasterBatteryTemperature",
        SdoRequest::KeepPowerTimer => "KeepPowerTimer",
    }
}

async fn varta_sdo_task(
    can_interface: &str,
    mut sdo_request_rx: tokio::sync::mpsc::UnboundedReceiver<(u8, SdoRequest)>,
    sdo_response_tx: tokio::sync::mpsc::UnboundedSender<SdoResponse>,
    cancellation_token: tokio_util::sync::CancellationToken,
) {
    debug_log("[SDO] Varta SDO task started");
    loop {
        tokio::select! {
            biased;
            _ = cancellation_token.cancelled() => {
                debug_log("[SDO] Varta SDO task cancelled");
                break;
            }
            request = sdo_request_rx.recv() => {
                let (node_id, sdo_request) = match request {
                    Some(r) => r,
                    None => {
                        debug_log("[SDO] Request channel closed");
                        break;
                    },
                };

                let mut sdo = match varta_easyblade::SdoSession::new(can_interface, node_id) {
                    Ok(s) => s,
                    Err(e) => {
                        debug_log(&format!("[SDO:{}] Failed to open socket: {}", node_id, e));
                        continue;
                    },
                };

                let start = std::time::Instant::now();
                match sdo.read(sdo_request).await {
                    Ok(response) => {
                        let elapsed = start.elapsed();
                        debug_log(&format!(
                            "[SDO:{}] {} ({}) -> {} in {}ms",
                            node_id,
                            sdo_request_name(&sdo_request),
                            sdo_request,
                            response,
                            elapsed.as_millis(),
                        ));
                        let _ = sdo_response_tx.send(response);
                    },
                    Err(e) => {
                        let elapsed = start.elapsed();
                        debug_log(&format!(
                            "[SDO:{}] SDO read ({} {}) failed in {}ms: {}",
                            node_id,
                            sdo_request_name(&sdo_request),
                            sdo_request,
                            elapsed.as_millis(),
                            e,
                        ));
                    },
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

fn spawn_varta_sdo_task(
    can_interface: &str,
    sdo_response_tx: tokio::sync::mpsc::UnboundedSender<SdoResponse>,
) -> VartaSdoTask {
    debug_log(&format!(
        "[SDO] Spawning Varta SDO task on {}",
        can_interface
    ));
    let (sdo_request_tx, sdo_request_rx) = tokio::sync::mpsc::unbounded_channel();
    let cancellation_token = tokio_util::sync::CancellationToken::new();
    let cancellation_token_clone = cancellation_token.clone();
    let can_interface = can_interface.to_string();

    tokio::spawn(async move {
        varta_sdo_task(
            &can_interface,
            sdo_request_rx,
            sdo_response_tx,
            cancellation_token_clone,
        )
        .await;
    });

    VartaSdoTask { sdo_request_tx, cancellation_token }
}

fn burst_initial_sdos(node_id: u8, tx: &tokio::sync::mpsc::UnboundedSender<(u8, SdoRequest)>) {
    debug_log(&format!(
        "[BURST:{}] Sending {} SDO requests",
        node_id,
        SdoRequest::COUNT
    ));
    tx.send((node_id, SdoRequest::SerialNumber)).ok();
    tx.send((node_id, SdoRequest::SoftwareVersion)).ok();
    tx.send((node_id, SdoRequest::HardwareVersion)).ok();
    tx.send((node_id, SdoRequest::DeviceErrorHistory)).ok();
    tx.send((node_id, SdoRequest::CellVoltages)).ok();
    tx.send((node_id, SdoRequest::DeviceConfigInfo)).ok();
    tx.send((node_id, SdoRequest::DeviceSerialNumberInfo)).ok();
    tx.send((node_id, SdoRequest::DeviceDateInfo)).ok();
    tx.send((node_id, SdoRequest::DeviceVariantInfo)).ok();
    tx.send((node_id, SdoRequest::DeviceControlParam)).ok();
    tx.send((node_id, SdoRequest::DeviceOperationTime)).ok();
    tx.send((node_id, SdoRequest::DeviceErrorCounter)).ok();
    tx.send((node_id, SdoRequest::CellVoltageMinMax)).ok();
    tx.send((node_id, SdoRequest::CellVoltageLimit)).ok();
    tx.send((node_id, SdoRequest::BatteryVoltage)).ok();
    tx.send((node_id, SdoRequest::BatteryVoltageLimit)).ok();
    tx.send((node_id, SdoRequest::BatteryCurrent)).ok();
    tx.send((node_id, SdoRequest::BatteryCurrentLimit)).ok();
    tx.send((node_id, SdoRequest::FetTemperature)).ok();
    tx.send((node_id, SdoRequest::FetTemperatureMinMax)).ok();
    tx.send((node_id, SdoRequest::FetTemperatureLimit)).ok();
    tx.send((node_id, SdoRequest::CellTemperature)).ok();
    tx.send((node_id, SdoRequest::CellTemperatureMinMax)).ok();
    tx.send((node_id, SdoRequest::CellTemperatureLimit)).ok();
    tx.send((node_id, SdoRequest::CellBalanceStatus)).ok();
    tx.send((node_id, SdoRequest::CellBalanceLimit)).ok();
    tx.send((node_id, SdoRequest::CellImpedance)).ok();
    tx.send((node_id, SdoRequest::BatteryCapacity)).ok();
    tx.send((node_id, SdoRequest::BatteryCapacityParam)).ok();
    tx.send((node_id, SdoRequest::BatteryCycleCount)).ok();
    tx.send((node_id, SdoRequest::BatteryChargeVoltage)).ok();
    tx.send((node_id, SdoRequest::BatteryChargeCurrent)).ok();
    tx.send((node_id, SdoRequest::BatteryChargeCurrentValid))
        .ok();
    tx.send((node_id, SdoRequest::BatteryChargeCurrentMaxNormal))
        .ok();
    tx.send((node_id, SdoRequest::BatteryChargeCurrentMaxLow))
        .ok();
    tx.send((node_id, SdoRequest::BatteryChargeCurrentMaxHigh))
        .ok();
    tx.send((node_id, SdoRequest::BatteryChargeCurrentKeepPower))
        .ok();
    tx.send((node_id, SdoRequest::BatteryChargeCurrentDecreaseStepsize1))
        .ok();
    tx.send((node_id, SdoRequest::BatteryChargeCurrentIncreaseStepsize1))
        .ok();
    tx.send((node_id, SdoRequest::BatteryChargeCurrentDecreaseStepsize2))
        .ok();
    tx.send((node_id, SdoRequest::BatteryChargeCurrentIncreaseStepsize2))
        .ok();
    tx.send((node_id, SdoRequest::BatteryChargeCurrentModifyInterval))
        .ok();
    tx.send((node_id, SdoRequest::BatteryChargeTemperature))
        .ok();
    tx.send((node_id, SdoRequest::MasterBatteryTemperature))
        .ok();
    tx.send((node_id, SdoRequest::KeepPowerTimer)).ok();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    init_debug_log();
    debug_log(&format!(
        "Starting varta-tui on interface {}",
        args.can_interface
    ));

    let mut varta = varta_easyblade::Varta::new(&args.can_interface).await?;
    let (sdo_response_tx, mut sdo_response_rx) = tokio::sync::mpsc::unbounded_channel();

    let mut terminal = setup_terminal()?;

    let (tx, mut rx) = mpsc::channel::<Event>(16);
    let handle = tokio::task::spawn_blocking(move || {
        while crossterm::event::poll(Duration::from_secs(10)).is_ok() {
            if let Ok(event) = crossterm::event::read()
                && tx.blocking_send(event).is_err()
            {
                break;
            }
        }
    });

    let mut selected = 0;
    let mut selected_tab = SelectedTab::ModuleInfo;
    let mut expire_timer = Box::pin(tokio::time::sleep(varta.next_expiry_delay()));
    let varta_sdo_task = spawn_varta_sdo_task(&args.can_interface, sdo_response_tx);
    let mut save_state = SaveState::Idle;
    let mut config_state = ConfigState::default();

    let mut last_redraw: Option<tokio::time::Instant> = None;

    loop {
        let count = varta.easyblade_count();
        if selected >= count {
            selected = count.saturating_sub(1);
        }

        // Update config fields from current module data
        update_config_state(&mut config_state, &varta, selected);

        let should_redraw = match last_redraw {
            None => true,
            Some(t) => tokio::time::Instant::now() - t > tokio::time::Duration::from_millis(100),
        };
        if should_redraw {
            let current_tab = selected_tab;
            let current_save_state = save_state.clone();
            let current_config_state = config_state.clone();
            terminal.draw(|f| {
                draw_frame(
                    f,
                    &varta,
                    selected,
                    current_tab,
                    &current_save_state,
                    &current_config_state,
                )
            })?;
            last_redraw = Some(tokio::time::Instant::now());
        }

        tokio::select! {
            result = varta.process_socketcan_msg() => {
                match result {
                    Ok(Some(node_id)) => {
                        debug_log(&format!("[CAN] New module detected: node_id={}", node_id));
                        burst_initial_sdos(node_id, &varta_sdo_task.sdo_request_tx);
                    },
                    Ok(None) => {},
                    Err(e) => {
                        debug_log(&format!("[CAN] Error processing message: {}", e));
                    },
                }
                // ... And while we're here, process a bunch more CAN frames, if available.
                match varta.process_socketcan_messages(32) {
                    Ok(Some(node_id)) => {
                        debug_log(&format!("[CAN] New module detected: node_id={}", node_id));
                        burst_initial_sdos(node_id, &varta_sdo_task.sdo_request_tx);
                    },
                    Ok(None) => {},
                    Err(e) => {
                        debug_log(&format!("[CAN] Error processing message: {}", e));
                    },
                }
                expire_timer = Box::pin(tokio::time::sleep(varta.next_expiry_delay()));
            }

            _ = expire_timer.as_mut() => {
                let before = varta.easyblade_count();
                varta.expire_missing_modules();
                let after = varta.easyblade_count();
                if before != after {
                    debug_log(&format!("[EXPIRY] Modules expired: {} -> {}", before, after));
                }
                expire_timer = Box::pin(tokio::time::sleep(varta.next_expiry_delay()));
            }
            response = sdo_response_rx.recv() => {
                if let Some(resp) = response {
                    match resp {
                        varta_easyblade::SdoResponse::SerialNumber { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.serial_number = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::SoftwareVersion { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.software_version = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::HardwareVersion { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.hardware_version = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::DeviceErrorHistory { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.device_errors = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::CellVoltages { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.cell_voltages = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::DeviceConfigInfo { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.device_config_info = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::DeviceSerialNumberInfo { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.device_serial_number_info = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::DeviceDateInfo { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.device_date_info = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::DeviceVariantInfo { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.device_variant_info = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::DeviceControlParam { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.device_control_param = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::DeviceOperationTime { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.device_operation_time = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::DeviceErrorCounter { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.device_error_counter = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::CellVoltageMinMax { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.cell_voltage_min_max = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::CellVoltageLimit { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.cell_voltage_limit = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryVoltage { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_voltage = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryVoltageLimit { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_voltage_limit = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryCurrent { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_current = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryCurrentLimit { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_current_limit = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::FetTemperature { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.fet_temperature = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::FetTemperatureMinMax { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.fet_temperature_min_max = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::FetTemperatureLimit { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.fet_temperature_limit = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::CellTemperature { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.cell_temperature = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::CellTemperatureMinMax { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.cell_temperature_min_max = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::CellTemperatureLimit { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.cell_temperature_limit = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::CellBalanceStatus { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.cell_balance_status = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::CellBalanceLimit { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.cell_balance_limit = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::CellImpedance { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.cell_impedance = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryCapacity { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_capacity = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryCapacityParam { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_capacity_param = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryCycleCount { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_cycle_count = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryChargeVoltage { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_charge_voltage = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryChargeCurrent { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_charge_current = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryChargeCurrentValid {
                            node_id,
                            value,
                        } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_charge_current_valid = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryChargeCurrentMaxNormal {
                            node_id,
                            value,
                        } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_charge_current_max_normal = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryChargeCurrentMaxLow {
                            node_id,
                            value,
                        } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_charge_current_max_low = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryChargeCurrentMaxHigh {
                            node_id,
                            value,
                        } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_charge_current_max_high = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryChargeCurrentKeepPower {
                            node_id,
                            value,
                        } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_charge_current_keep_power = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryChargeCurrentDecreaseStepsize1 {
                            node_id,
                            value,
                        } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_charge_current_decrease_stepsize1 = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryChargeCurrentIncreaseStepsize1 {
                            node_id,
                            value,
                        } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_charge_current_increase_stepsize1 = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryChargeCurrentDecreaseStepsize2 {
                            node_id,
                            value,
                        } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_charge_current_decrease_stepsize2 = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryChargeCurrentIncreaseStepsize2 {
                            node_id,
                            value,
                        } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_charge_current_increase_stepsize2 = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryChargeCurrentModifyInterval {
                            node_id,
                            value,
                        } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_charge_current_modify_interval = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::BatteryChargeTemperature { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.battery_charge_temperature = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::MasterBatteryTemperature { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.master_battery_temperature = Some(value);
                            }
                        },
                        varta_easyblade::SdoResponse::KeepPowerTimer { node_id, value } => {
                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                eb.sdo.keep_power_timer = Some(value);
                            }
                        },

                    }
                }
                try_complete_save(&varta, selected, &mut save_state);
            }
            event = rx.recv() => {
                if let Some(Event::Key(key)) = event
                    && key.kind == KeyEventKind::Press
                {
                    match key.code {
                        KeyCode::Char('q') => {
                            if save_state.is_active() {
                                save_state = SaveState::Idle;
                            } else {
                                varta_sdo_task.cancellation_token.cancel();
                                break;
                            }
                        },
                        KeyCode::Enter if save_state.is_active() => {
                            save_state = SaveState::Idle;
                        },
                        KeyCode::Char('s') if !save_state.is_active() && count > 0 => {
                            let eb = varta.get_easyblade_by_index(selected);
                            if let Some(eb) = eb {
                                let serial = eb.sdo.serial_number.as_ref().map(|s| s.value).unwrap_or(0);
                                let file_path = format!("/tmp/varta-easyblade-{}.json", serial);
                                save_state = SaveState::Saving { file_path };
                                try_complete_save(&varta, selected, &mut save_state);
                            }
                        },
                        KeyCode::Up if count > 0 && !save_state.is_active() => {
                            selected = selected.saturating_sub(1);
                        }
                        KeyCode::Down if selected + 1 < count && !save_state.is_active() => {
                            selected += 1;
                        }
                        KeyCode::Left if !save_state.is_active() => {
                            selected_tab = selected_tab.cycle(false);
                        }
                        KeyCode::Right if !save_state.is_active() => {
                            selected_tab = selected_tab.cycle(true);
                        }
                        KeyCode::Char('r') | KeyCode::Char('R') if !save_state.is_active() && count > 0 && key.modifiers.contains(KeyModifiers::CONTROL) => {
                            let eb = varta.get_easyblade_by_index(selected);
                            if let Some(eb) = eb {
                                let node_id = eb.node_id;
                                burst_initial_sdos(node_id, &varta_sdo_task.sdo_request_tx);
                            }
                        }
                        KeyCode::Char('r') | KeyCode::Char('R') if !save_state.is_active() && count > 0 => {
                            let eb = varta.get_easyblade_by_index(selected);
                            if let Some(eb) = eb {
                                let node_id = eb.node_id;
                                let sdos = sdos_for_tab(selected_tab);
                                for sdo in sdos {
                                    let _ = varta_sdo_task.sdo_request_tx.send((node_id, sdo));
                                }
                            }
                        }
                        // Configuration tab: Tab to cycle fields
                        KeyCode::Tab if selected_tab == SelectedTab::Configuration && !config_state.fields.is_empty() => {
                            config_state.focused_index = (config_state.focused_index + 1) % config_state.fields.len();
                            for (i, field) in config_state.fields.iter_mut().enumerate() {
                                field.is_focused = i == config_state.focused_index;
                                field.user_edited = false;
                                field.status = None;
                            }
                        }
                        // Configuration tab: Enter to write value
                        KeyCode::Enter if selected_tab == SelectedTab::Configuration => {
                            let idx = config_state.focused_index;
                            if idx >= config_state.fields.len() {
                                continue;
                            }
                            let node_id = if let Some(eb) = varta.get_easyblade_by_index(selected) {
                                eb.node_id
                            } else {
                                config_state.fields[idx].status = Some("No module selected".to_string());
                                continue;
                            };
                            if config_state.fields[idx].value.is_none() {
                                config_state.fields[idx]
                                    .status = Some("Value not read yet".to_string());
                                continue;
                            }
                            let mut sdo_session = match varta_easyblade::SdoSession::new(&args.can_interface, node_id) {
                                Ok(s) => s,
                                Err(e) => {
                                    debug_log(&format!("failed to create SdoSession: {}", e));
                                    config_state.fields[idx].status = Some(format!("Error: {}", e));
                                    continue;
                                }
                            };

                            // Dispatch to the correct write method based on field index
                            let result = match idx {
                                // Field 0: Battery Max Charge Voltage (0x3000:02)
                                0 => {
                                    let value = match config_state.fields[idx].parse_float() {
                                        Some(v) => v,
                                        None => {
                                            config_state.fields[idx].status = Some("Invalid value".to_string());
                                            continue;
                                        }
                                    };
                                    sdo_session.sdo_write_battery_charge_max_voltage(value).await
                                }
                                // Field 1: Battery Keep Power Voltage (0x3000:03)
                                1 => {
                                    let value = match config_state.fields[idx].parse_float() {
                                        Some(v) => v,
                                        None => {
                                            config_state.fields[idx].status = Some("Invalid value".to_string());
                                            continue;
                                        }
                                    };
                                    sdo_session.sdo_write_battery_charge_keep_power_voltage(value).await
                                }
                                // Field 2: Keep Power Timer (0x3d00:0c)
                                2 => {
                                    let value = match config_state.fields[idx].parse_uint() {
                                        Some(v) => v,
                                        None => {
                                            config_state.fields[idx].status = Some("Invalid value".to_string());
                                            continue;
                                        }
                                    };
                                    sdo_session.sdo_write_keep_power_timer(value).await
                                }
                                // Field 3: Single Cell Max Charge Voltage (0x2104:02)
                                3 => {
                                    let value_v = match config_state.fields[idx].parse_float() {
                                        Some(v) => v,
                                        None => {
                                            config_state.fields[idx].status = Some("Invalid value".to_string());
                                            continue;
                                        }
                                    };
                                    let value_mv = (value_v * 1000.0) as u32;
                                    sdo_session.sdo_write_single_cell_max_charge_voltage(value_mv).await
                                }
                                // Field 4: Battery Charge Current Fully Charged End (0x2304:0a)
                                4 => {
                                    let value_a = match config_state.fields[idx].parse_float() {
                                        Some(v) => v,
                                        None => {
                                            config_state.fields[idx].status = Some("Invalid value".to_string());
                                            continue;
                                        }
                                    };
                                    let value_ma = (value_a * 1000.0) as u16;
                                    sdo_session.sdo_write_battery_charge_current_fully_charged_end(value_ma).await
                                }
                                _ => {
                                    config_state.fields[idx].status = Some("Unknown field".to_string());
                                    continue;
                                }
                            };

                            match result {
                                Ok(changed) => {
                                    let msg = if changed { "OK - value changed" } else { "OK - already set" };
                                    config_state.fields[idx].status = Some(msg.to_string());
                                    debug_log(&format!(
                                        "[CONFIG:{}] Field {} written: {}",
                                        node_id, config_state.fields[idx].label, msg,
                                    ));
                                    config_state.fields[idx].is_focused = false;
                                    // Show the field as pending until the re-read confirms
                                    config_state.fields[idx].reset_to_pending();
                                    // Drop the cached value of the SDO we just wrote (so the
                                    // stale value doesn't repopulate the field) and re-read
                                    // just that one SDO.
                                    let re_read = match idx {
                                        // Fields 0 & 1 both come from 0x3000
                                        0 | 1 => {
                                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                                eb.sdo.battery_charge_voltage = None;
                                            }
                                            SdoRequest::BatteryChargeVoltage
                                        }
                                        2 => {
                                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                                eb.sdo.keep_power_timer = None;
                                            }
                                            SdoRequest::KeepPowerTimer
                                        }
                                        3 => {
                                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                                eb.sdo.cell_voltage_limit = None;
                                            }
                                            SdoRequest::CellVoltageLimit
                                        }
                                        4 => {
                                            if let Some(Some(eb)) = varta.easyblades.get_mut(node_id as usize) {
                                                eb.sdo.battery_current_limit = None;
                                            }
                                            SdoRequest::BatteryCurrentLimit
                                        }
                                        _ => unreachable!("unknown config field"),
                                    };
                                    let _ = varta_sdo_task.sdo_request_tx.send((node_id, re_read));
                                },
                                Err(e) => {
                                    debug_log(&format!(
                                        "[CONFIG:{}] Field {} write failed: {}",
                                        node_id, config_state.fields[idx].label, e,
                                    ));
                                    config_state.fields[idx].status = Some(format!("Error: {}", e));
                                },
                            }
                        }

                        // Configuration tab: character input for edit buffer
                        KeyCode::Char(c) if selected_tab == SelectedTab::Configuration => {
                            if let Some(field) = config_state.fields.get_mut(config_state.focused_index) {
                                field.status = None;
                                // Some terminals send backspace as BS (\x08) instead of KeyCode::Backspace
                                if c == '\x08' {
                                    field.backspace();
                                } else if c.is_ascii_digit() || c == '.' || c == '-' {
                                    field.insert_char(c);
                                }
                            }
                        }
                        KeyCode::Backspace if selected_tab == SelectedTab::Configuration => {
                            if let Some(field) = config_state.fields.get_mut(config_state.focused_index) {
                                field.status = None;
                                field.backspace();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    handle.abort();
    cleanup_terminal();
    Ok(())
}
