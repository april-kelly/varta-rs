use clap::{Parser, Subcommand};
use serde_json::{Map, Value, json};
use std::fs;
use std::io::Write;
use std::time::Duration;
use varta_easyblade::{DeviceError, SdoSession, Varta};

#[derive(Debug, Parser)]
#[command(name = "varta-cli")]
struct Args {
    /// The CAN interface to connect to (e.g. 'can0' or 'vcan0').
    #[arg(short, long, default_value_t = String::from("can0"))]
    can_interface: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Listen for CAN PDO packets for 3 seconds and print discovered serial numbers.
    Scan,
    /// Read all SDOs from the module with the given serial number and output JSON.
    Read {
        /// Serial number of the module to read.
        serial_number: u16,
        /// Write output to a file instead of stdout.
        #[arg(short, long)]
        output: Option<String>,
    },
}

fn device_error_to_string(err: &DeviceError) -> String {
    match err {
        DeviceError::None => "None".to_string(),
        DeviceError::CellOverTempWhileCharging => "CellOverTempWhileCharging".to_string(),
        DeviceError::CellUnderTempWhileCharging => "CellUnderTempWhileCharging".to_string(),
        DeviceError::ChargeFetOverTempWhileCharging => "ChargeFetOverTempWhileCharging".to_string(),
        DeviceError::CellOverTempWhileDischarging => "CellOverTempWhileDischarging".to_string(),
        DeviceError::CellUnderTempWhileDischarging => "CellUnderTempWhileDischarging".to_string(),
        DeviceError::DischargeFetOverTempWhileDischarging => {
            "DischargeFetOverTempWhileDischarging".to_string()
        },
        DeviceError::NotUsed0x07 => "NotUsed0x07".to_string(),
        DeviceError::CellOverVoltageWhileCharging => "CellOverVoltageWhileCharging".to_string(),
        DeviceError::CellUnderVoltageWhileDischarging => {
            "CellUnderVoltageWhileDischarging".to_string()
        },
        DeviceError::CellVoltageTooLow => "CellVoltageTooLow".to_string(),
        DeviceError::SevereCellUnbalance => "SevereCellUnbalance".to_string(),
        DeviceError::NotUsed0x0c => "NotUsed0x0c".to_string(),
        DeviceError::PackOverVoltage => "PackOverVoltage".to_string(),
        DeviceError::NotUsed0x0e => "NotUsed0x0e".to_string(),
        DeviceError::VoltageSumDifference => "VoltageSumDifference".to_string(),
        DeviceError::ModuleUnderVoltage => "ModuleUnderVoltage".to_string(),
        DeviceError::NotUsed0x11 => "NotUsed0x11".to_string(),
        DeviceError::NotUsed0x12 => "NotUsed0x12".to_string(),
        DeviceError::NotUsed0x13 => "NotUsed0x13".to_string(),
        DeviceError::NotUsed0x14 => "NotUsed0x14".to_string(),
        DeviceError::ShortCircuitWhileCharging => "ShortCircuitWhileCharging".to_string(),
        DeviceError::OverCurrent65AWhileCharging => "OverCurrent65AWhileCharging".to_string(),
        DeviceError::OverCurrent85AWhileCharging => "OverCurrent85AWhileCharging".to_string(),
        DeviceError::NotUsed0x18 => "NotUsed0x18".to_string(),
        DeviceError::ShortCircuitWhileDischarging => "ShortCircuitWhileDischarging".to_string(),
        DeviceError::OverCurrent65AWhileDischarging => "OverCurrent65AWhileDischarging".to_string(),
        DeviceError::OverCurrent85AWhileDischarging => "OverCurrent85AWhileDischarging".to_string(),
        DeviceError::OverCurrentWhileDischarging => "OverCurrentWhileDischarging".to_string(),
        DeviceError::CurrentMeasurementError => "CurrentMeasurementError".to_string(),
        DeviceError::NotUsed0x1e => "NotUsed0x1e".to_string(),
        DeviceError::NotUsed0x1f => "NotUsed0x1f".to_string(),
        DeviceError::NotUsed0x20 => "NotUsed0x20".to_string(),
        DeviceError::NotUsed0x21 => "NotUsed0x21".to_string(),
        DeviceError::NotUsed0x22 => "NotUsed0x22".to_string(),
        DeviceError::NotUsed0x23 => "NotUsed0x23".to_string(),
        DeviceError::NotUsed0x24 => "NotUsed0x24".to_string(),
        DeviceError::NotUsed0x25 => "NotUsed0x25".to_string(),
        DeviceError::NotUsed0x26 => "NotUsed0x26".to_string(),
        DeviceError::NotUsed0x27 => "NotUsed0x27".to_string(),
        DeviceError::NotUsed0x28 => "NotUsed0x28".to_string(),
        DeviceError::AdcMinScale => "AdcMinScale".to_string(),
        DeviceError::AdcMaxScale => "AdcMaxScale".to_string(),
        DeviceError::CellTempSensorHighFailure => "CellTempSensorHighFailure".to_string(),
        DeviceError::CellTempSensorLowFailure => "CellTempSensorLowFailure".to_string(),
        DeviceError::FetTempSensorHighFailure => "FetTempSensorHighFailure".to_string(),
        DeviceError::FetTempSensorLowFailure => "FetTempSensorLowFailure".to_string(),
        DeviceError::NotUsed0x2f => "NotUsed0x2f".to_string(),
        DeviceError::NotUsed0x30 => "NotUsed0x30".to_string(),
        DeviceError::NotUsed0x31 => "NotUsed0x31".to_string(),
        DeviceError::CurrentSensorHighFailure => "CurrentSensorHighFailure".to_string(),
        DeviceError::NotUsed0x33 => "NotUsed0x33".to_string(),
        DeviceError::CurrentSensorHighFailureWhileDischarging => {
            "CurrentSensorHighFailureWhileDischarging".to_string()
        },
        DeviceError::DischargeFetShorted => "DischargeFetShorted".to_string(),
        DeviceError::ChargeFetShorted => "ChargeFetShorted".to_string(),
        DeviceError::NotUsed0x37 => "NotUsed0x37".to_string(),
        DeviceError::TempDischargeErrorLock => "TempDischargeErrorLock".to_string(),
        DeviceError::TempChargeErrorLock => "TempChargeErrorLock".to_string(),
        DeviceError::OverCurrentWhileRecuperating => "OverCurrentWhileRecuperating".to_string(),
        DeviceError::CellOverVoltageWhileRecuperating => {
            "CellOverVoltageWhileRecuperating".to_string()
        },
        DeviceError::V24UnderVoltage => "V24UnderVoltage".to_string(),
        DeviceError::V24OverVoltage => "V24OverVoltage".to_string(),
        DeviceError::CanNodeIdNotAssigned => "CanNodeIdNotAssigned".to_string(),
        DeviceError::CanNodeIdDuplicate => "CanNodeIdDuplicate".to_string(),
        DeviceError::ParameterConfigError => "ParameterConfigError".to_string(),
        DeviceError::AnalogFrontEndCommunicationErorr => {
            "AnalogFrontEndCommunicationErorr".to_string()
        },
        DeviceError::AnalogFrontEndSelftestError => "AnalogFrontEndSelftestError".to_string(),
        DeviceError::AnalogFrontEndFullScaleError => "AnalogFrontEndFullScaleError".to_string(),
        DeviceError::TempMuxSelftestError => "TempMuxSelftestError".to_string(),
        DeviceError::Unknown => "Unknown".to_string(),
    }
}

struct SdoData {
    software_version: Option<Value>,
    hardware_version: Option<Value>,
    device_config_info: Option<Value>,
    device_serial_number_info: Option<Value>,
    device_date_info: Option<Value>,
    device_variant_info: Option<Value>,
    device_control_param: Option<Value>,
    device_operation_time: Option<Value>,
    device_error_history: Option<Value>,
    device_error_counter: Option<Value>,
    cell_voltages: Option<Value>,
    cell_voltage_min_max: Option<Value>,
    cell_voltage_limit: Option<Value>,
    battery_voltage: Option<Value>,
    battery_voltage_limit: Option<Value>,
    battery_current: Option<Value>,
    battery_current_limit: Option<Value>,
    fet_temperature: Option<Value>,
    fet_temperature_min_max: Option<Value>,
    fet_temperature_limit: Option<Value>,
    cell_temperature: Option<Value>,
    cell_temperature_min_max: Option<Value>,
    cell_temperature_limit: Option<Value>,
    cell_balance_status: Option<Value>,
    cell_balance_limit: Option<Value>,
    cell_impedance: Option<Value>,
    battery_capacity: Option<Value>,
    battery_capacity_param: Option<Value>,
    battery_cycle_count: Option<Value>,
    battery_charge_voltage: Option<Value>,
    battery_charge_current: Option<Value>,
    battery_charge_temperature: Option<Value>,
    master_battery_temperature: Option<Value>,
}

fn ok_value<T>(result: Result<T, String>, transform: impl Fn(T) -> Value) -> Value {
    match result {
        Ok(v) => transform(v),
        Err(e) => json!({"error": e}),
    }
}

async fn read_serial_number(sdo: &mut SdoSession) -> Result<u16, String> {
    Ok(sdo.read_serial_number().await?.value)
}

async fn read_all_sdos(sdo: &mut SdoSession) -> SdoData {
    SdoData {
        software_version: Some(ok_value(sdo.read_software_version().await, |v| json!(v))),
        hardware_version: Some(ok_value(sdo.read_hardware_version().await, |v| json!(v))),
        device_config_info: Some(ok_value(sdo.read_device_config_info().await, |v| json!(v))),
        device_serial_number_info: Some(ok_value(
            sdo.read_device_serial_number_info().await,
            |v| json!(v),
        )),
        device_date_info: Some(ok_value(sdo.read_device_date_info().await, |v| json!(v))),
        device_variant_info: Some(ok_value(sdo.read_device_variant_info().await, |v| json!(v))),
        device_control_param: Some(ok_value(sdo.read_device_control_param().await, |v| {
            json!(v)
        })),
        device_operation_time: Some(ok_value(sdo.read_device_operation_time().await, |v| {
            json!(v)
        })),
        device_error_history: Some(ok_value(sdo.read_device_error_history().await, |errors| {
            json!(
                errors
                    .values
                    .iter()
                    .map(device_error_to_string)
                    .collect::<Vec<_>>()
            )
        })),
        device_error_counter: Some(ok_value(sdo.read_device_error_counter().await, |v| {
            json!(v)
        })),
        cell_voltages: Some(ok_value(sdo.read_cell_voltages().await, |v| json!(v))),
        cell_voltage_min_max: Some(ok_value(sdo.read_cell_voltage_min_max().await, |v| {
            json!(v)
        })),
        cell_voltage_limit: Some(ok_value(sdo.read_cell_voltage_limit().await, |v| json!(v))),
        battery_voltage: Some(ok_value(sdo.read_battery_voltage().await, |v| json!(v))),
        battery_voltage_limit: Some(ok_value(sdo.read_battery_voltage_limit().await, |v| {
            json!(v)
        })),
        battery_current: Some(ok_value(sdo.read_battery_current().await, |v| json!(v))),
        battery_current_limit: Some(ok_value(sdo.read_battery_current_limit().await, |v| {
            json!(v)
        })),
        fet_temperature: Some(ok_value(sdo.read_fet_temperature().await, |v| json!(v))),
        fet_temperature_min_max: Some(ok_value(sdo.read_fet_temperature_min_max().await, |v| {
            json!(v)
        })),
        fet_temperature_limit: Some(ok_value(sdo.read_fet_temperature_limit().await, |v| {
            json!(v)
        })),
        cell_temperature: Some(ok_value(sdo.read_cell_temperature().await, |v| json!(v))),
        cell_temperature_min_max: Some(ok_value(sdo.read_cell_temperature_min_max().await, |v| {
            json!(v)
        })),
        cell_temperature_limit: Some(ok_value(sdo.read_cell_temperature_limit().await, |v| {
            json!(v)
        })),
        cell_balance_status: Some(ok_value(sdo.read_cell_balance_status().await, |v| json!(v))),
        cell_balance_limit: Some(ok_value(sdo.read_cell_balance_limit().await, |v| json!(v))),
        cell_impedance: Some(ok_value(sdo.read_cell_impedance().await, |v| json!(v))),
        battery_capacity: Some(ok_value(sdo.read_battery_capacity().await, |v| json!(v))),
        battery_capacity_param: Some(ok_value(sdo.read_battery_capacity_param().await, |v| {
            json!(v)
        })),
        battery_cycle_count: Some(ok_value(sdo.read_battery_cycle_count().await, |v| json!(v))),
        battery_charge_voltage: Some(ok_value(sdo.read_battery_charge_voltage().await, |v| {
            json!(v)
        })),
        battery_charge_current: Some(ok_value(sdo.read_battery_charge_current().await, |v| {
            json!(v)
        })),
        battery_charge_temperature: Some(ok_value(
            sdo.read_battery_charge_temperature().await,
            |v| json!(v),
        )),
        master_battery_temperature: Some(ok_value(
            sdo.read_master_battery_temperature().await,
            |v| json!(v),
        )),
    }
}

fn sdo_data_to_json(node_id: u8, serial_number: u16, data: &SdoData) -> Value {
    let mut map = Map::new();
    map.insert("node_id".into(), json!(node_id));
    map.insert("serial_number".into(), json!(serial_number));

    if let Some(v) = &data.software_version {
        map.insert("software_version".into(), v.clone());
    }
    if let Some(v) = &data.hardware_version {
        map.insert("hardware_version".into(), v.clone());
    }
    if let Some(v) = &data.device_config_info {
        map.insert("device_config_info".into(), v.clone());
    }
    if let Some(v) = &data.device_serial_number_info {
        map.insert("device_serial_number_info".into(), v.clone());
    }
    if let Some(v) = &data.device_date_info {
        map.insert("device_date_info".into(), v.clone());
    }
    if let Some(v) = &data.device_variant_info {
        map.insert("device_variant_info".into(), v.clone());
    }
    if let Some(v) = &data.device_control_param {
        map.insert("device_control_param".into(), v.clone());
    }
    if let Some(v) = &data.device_operation_time {
        map.insert("device_operation_time".into(), v.clone());
    }
    if let Some(v) = &data.device_error_history {
        map.insert("device_error_history".into(), v.clone());
    }
    if let Some(v) = &data.device_error_counter {
        map.insert("device_error_counter".into(), v.clone());
    }
    if let Some(v) = &data.cell_voltages {
        map.insert("cell_voltages".into(), v.clone());
    }
    if let Some(v) = &data.cell_voltage_min_max {
        map.insert("cell_voltage_min_max".into(), v.clone());
    }
    if let Some(v) = &data.cell_voltage_limit {
        map.insert("cell_voltage_limit".into(), v.clone());
    }
    if let Some(v) = &data.battery_voltage {
        map.insert("battery_voltage".into(), v.clone());
    }
    if let Some(v) = &data.battery_voltage_limit {
        map.insert("battery_voltage_limit".into(), v.clone());
    }
    if let Some(v) = &data.battery_current {
        map.insert("battery_current".into(), v.clone());
    }
    if let Some(v) = &data.battery_current_limit {
        map.insert("battery_current_limit".into(), v.clone());
    }
    if let Some(v) = &data.fet_temperature {
        map.insert("fet_temperature".into(), v.clone());
    }
    if let Some(v) = &data.fet_temperature_min_max {
        map.insert("fet_temperature_min_max".into(), v.clone());
    }
    if let Some(v) = &data.fet_temperature_limit {
        map.insert("fet_temperature_limit".into(), v.clone());
    }
    if let Some(v) = &data.cell_temperature {
        map.insert("cell_temperature".into(), v.clone());
    }
    if let Some(v) = &data.cell_temperature_min_max {
        map.insert("cell_temperature_min_max".into(), v.clone());
    }
    if let Some(v) = &data.cell_temperature_limit {
        map.insert("cell_temperature_limit".into(), v.clone());
    }
    if let Some(v) = &data.cell_balance_status {
        map.insert("cell_balance_status".into(), v.clone());
    }
    if let Some(v) = &data.cell_balance_limit {
        map.insert("cell_balance_limit".into(), v.clone());
    }
    if let Some(v) = &data.cell_impedance {
        map.insert("cell_impedance".into(), v.clone());
    }
    if let Some(v) = &data.battery_capacity {
        map.insert("battery_capacity".into(), v.clone());
    }
    if let Some(v) = &data.battery_capacity_param {
        map.insert("battery_capacity_param".into(), v.clone());
    }
    if let Some(v) = &data.battery_cycle_count {
        map.insert("battery_cycle_count".into(), v.clone());
    }
    if let Some(v) = &data.battery_charge_voltage {
        map.insert("battery_charge_voltage".into(), v.clone());
    }
    if let Some(v) = &data.battery_charge_current {
        map.insert("battery_charge_current".into(), v.clone());
    }
    if let Some(v) = &data.battery_charge_temperature {
        map.insert("battery_charge_temperature".into(), v.clone());
    }
    if let Some(v) = &data.master_battery_temperature {
        map.insert("master_battery_temperature".into(), v.clone());
    }

    Value::Object(map)
}

async fn run_scan(can_interface: &str) {
    let mut varta = match Varta::new(can_interface).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error opening CAN interface {}: {}", can_interface, e);
            std::process::exit(1);
        },
    };

    let scan_duration = Duration::from_secs(3);
    let start = std::time::Instant::now();

    while start.elapsed() < scan_duration {
        let remaining = scan_duration.saturating_sub(start.elapsed());
        match tokio::time::timeout(remaining, varta.process_socketcan_msg()).await {
            Ok(Ok(_)) => {},
            Ok(Err(e)) => {
                eprintln!("Error reading CAN: {}", e);
                break;
            },
            Err(_) => break,
        }
    }

    let detected: Vec<u8> = varta
        .easyblades
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| entry.as_ref().map(|_| idx as u8))
        .collect();

    if detected.is_empty() {
        println!("No modules detected on CAN bus.");
        return;
    }

    println!(
        "Detected {} module(s), reading serial numbers...",
        detected.len()
    );

    for node_id in detected {
        let mut sdo = match varta.sdo_client(node_id) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  Node {}: cannot open CAN: {}", node_id, e);
                continue;
            },
        };

        match read_serial_number(&mut sdo).await {
            Ok(sn) => println!("  Node {}: serial {}", node_id, sn),
            Err(e) => eprintln!("  Node {}: error reading serial: {}", node_id, e),
        }
    }
}

async fn run_read(can_interface: &str, target_serial: u16, output: Option<String>) {
    let mut varta = match Varta::new(can_interface).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error opening CAN interface {}: {}", can_interface, e);
            std::process::exit(1);
        },
    };

    println!("Scanning for module serial {}...", target_serial);
    let scan_duration = Duration::from_secs(5);
    let start = std::time::Instant::now();

    let mut target_node: Option<u8> = None;

    while start.elapsed() < scan_duration {
        let remaining = scan_duration.saturating_sub(start.elapsed());
        match tokio::time::timeout(remaining, varta.process_socketcan_msg()).await {
            Ok(Ok(_)) => {},
            Ok(Err(e)) => {
                eprintln!("Error reading CAN: {}", e);
                return;
            },
            Err(_) => break,
        }

        if target_node.is_none() {
            for (idx, entry) in varta.easyblades.iter().enumerate() {
                if let Some(eb) = entry
                    && eb.sdo.serial_number.as_ref().map(|s| s.value) == Some(target_serial)
                {
                    target_node = Some(idx as u8);
                    break;
                }
            }
        }

        if target_node.is_none() {
            for (idx, entry) in varta.easyblades.iter().enumerate() {
                if entry.is_some() && target_node.is_none() {
                    let node_id = idx as u8;
                    let mut sdo = match varta.sdo_client(node_id) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    if let Ok(sn) = read_serial_number(&mut sdo).await
                        && sn == target_serial
                    {
                        target_node = Some(node_id);
                        break;
                    }
                }
            }
        }

        if target_node.is_some() {
            break;
        }
    }

    let node_id = match target_node {
        Some(n) => n,
        None => {
            eprintln!(
                "Module with serial number {} not found on CAN bus.",
                target_serial
            );
            std::process::exit(1);
        },
    };

    println!(
        "Found module serial {} on node {}, reading all SDOs...",
        target_serial, node_id
    );

    let mut sdo = match varta.sdo_client(node_id) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error opening CAN for SDO read: {}", e);
            std::process::exit(1);
        },
    };

    let data = read_all_sdos(&mut sdo).await;
    let json_value = sdo_data_to_json(node_id, target_serial, &data);
    let json_string = serde_json::to_string_pretty(&json_value).unwrap();

    match output {
        Some(path) => {
            let mut file = fs::File::create(&path).unwrap_or_else(|e| {
                eprintln!("Error creating file {}: {}", path, e);
                std::process::exit(1);
            });
            file.write_all(json_string.as_bytes()).unwrap_or_else(|e| {
                eprintln!("Error writing to file {}: {}", path, e);
                std::process::exit(1);
            });
            println!("Output written to {}", path);
        },
        None => {
            println!("{}", json_string);
        },
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    match args.command {
        Command::Scan => {
            run_scan(&args.can_interface).await;
        },
        Command::Read { serial_number, output } => {
            run_read(&args.can_interface, serial_number, output).await;
        },
    }
}
