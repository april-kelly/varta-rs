pub mod varta;
pub use varta::FrameEvent;
pub use varta::MasterPdo;
pub use varta::ModulePdo;
pub use varta::PdoUpdate;
pub use varta::SdoSession;
pub use varta::Varta;

pub mod varta_easyblade;
pub use varta_easyblade::BatteryCapacity;
pub use varta_easyblade::BatteryCapacityParam;
pub use varta_easyblade::BatteryChargeCurrent;
pub use varta_easyblade::BatteryChargeTemperature;
pub use varta_easyblade::BatteryChargeVoltage;
pub use varta_easyblade::BatteryCurrent;
pub use varta_easyblade::BatteryCurrentLimit;
pub use varta_easyblade::BatteryCycleCount;
pub use varta_easyblade::BatteryVoltage;
pub use varta_easyblade::BatteryVoltageLimit;
pub use varta_easyblade::CellBalanceLimit;
pub use varta_easyblade::CellBalanceStatus;
pub use varta_easyblade::CellImpedance;
pub use varta_easyblade::CellTemperature;
pub use varta_easyblade::CellTemperatureLimit;
pub use varta_easyblade::CellTemperatureMinMax;
pub use varta_easyblade::CellVoltageLimit;
pub use varta_easyblade::CellVoltageMinMax;
pub use varta_easyblade::CellVoltages;
pub use varta_easyblade::DeviceConfigInfo;
pub use varta_easyblade::DeviceControlParam;
pub use varta_easyblade::DeviceDateInfo;
pub use varta_easyblade::DeviceError;
pub use varta_easyblade::DeviceErrorCounterInfo;
pub use varta_easyblade::DeviceErrorHistory;
pub use varta_easyblade::DeviceOperationTime;
pub use varta_easyblade::DeviceSerialNumberInfo;
pub use varta_easyblade::DeviceVariantInfo;
pub use varta_easyblade::FetTemperature;
pub use varta_easyblade::FetTemperatureLimit;
pub use varta_easyblade::FetTemperatureMinMax;
pub use varta_easyblade::HardwareVersion;
pub use varta_easyblade::KeepPowerTimer;
pub use varta_easyblade::MasterBatteryTemperature;
pub use varta_easyblade::MasterInfo;
pub use varta_easyblade::MsgBits;

pub use varta_easyblade::Pdo;
pub use varta_easyblade::SdoRequest;
pub use varta_easyblade::SdoResponse;
pub use varta_easyblade::SerialNumber;
pub use varta_easyblade::SoftwareVersion;
pub use varta_easyblade::VartaEasyblade;

#[rustfmt::skip]
#[allow(clippy::too_many_arguments)]
mod varta_easyblade_can_messages {
    include!(concat!(env!("OUT_DIR"), "/varta_easyblade_can_messages.rs"));
}

/// Maximum number of EasyBlade modules supported.
pub const MAX_MODULES: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    ZenCan(#[from] zencan_client::SdoClientError),

    #[error(transparent)]
    Dbc(#[from] varta_easyblade_can_messages::CanError),

    #[error("IO Error on {can_interface}: {e}")]
    Io {
        can_interface: String,
        #[source]
        e: std::io::Error,
    },
}
