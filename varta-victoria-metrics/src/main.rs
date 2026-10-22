use clap::Parser;
use reqwest::Client;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use varta_easyblade::{MasterInfo, MasterPdo, ModulePdo, PdoUpdate, Varta, VartaEasyblade};

/// How often to retry reading the serial number of modules that don't have one yet.
const SERIAL_NUMBER_RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// Upper bound on a single HTTP request to Victoria Metrics.
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum number of metric points waiting to be sent. If Victoria Metrics can't
/// keep up (or is down) the queue fills up and newer points are dropped.
const SEND_QUEUE_CAPACITY: usize = 4096;

/// Maximum number of metric points per POST to Victoria Metrics.
const MAX_POINTS_PER_REQUEST: usize = 512;

/// Minimum time between "queue full" warnings.
const DROP_WARNING_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Parser)]
#[command(name = "varta-victoria-metrics")]
struct Args {
    /// Victoria Metrics HTTP URL
    #[arg(long, default_value_t = String::from("http://localhost:8428"))]
    vm_url: String,

    /// Used as identifying label for all metrics posted to Victora Metrics.
    #[arg(long)]
    battery_name: String,

    /// The CAN interface to connect to (e.g. 'can0' or 'vcan0').
    #[arg(long, default_value_t = String::from("can0"))]
    canbus: String,
}

#[derive(Debug)]
struct MetricPoint {
    metric: String,
    value: f64,
    timestamp: i64,
}

impl MetricPoint {
    /// One line of the JSON line format accepted by `/api/v1/import`.
    fn to_import_line(&self, battery_name: &str) -> serde_json::Value {
        serde_json::json!({
            "metric": {
                "__name__": self.metric,
                "battery_name": battery_name,
            },
            "values": [self.value],
            "timestamps": [self.timestamp],
            "timestamp": self.timestamp,
        })
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Posts `points` to Victoria Metrics in a single request.
async fn send_metrics(
    client: &Client,
    vm_url: &str,
    battery_name: &str,
    points: &[MetricPoint],
) -> Result<(), String> {
    let mut import_body = Vec::new();
    for p in points {
        serde_json::to_writer(&mut import_body, &p.to_import_line(battery_name))
            .map_err(|e| e.to_string())?;
        import_body.push(b'\n');
    }

    client
        .post(format!("{}/api/v1/import", vm_url))
        .header("Content-Type", "application/json")
        .body(import_body)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Forwards queued metric points to Victoria Metrics until the queue is closed.
///
/// Each request carries whatever has been queued since the previous one: just the
/// points of a single CAN frame while Victoria Metrics keeps up, more if it falls
/// behind. The CAN loop never waits for HTTP.
async fn send_metrics_task(
    client: Client,
    vm_url: String,
    battery_name: String,
    mut rx: mpsc::Receiver<MetricPoint>,
) {
    let mut batch = Vec::with_capacity(MAX_POINTS_PER_REQUEST);
    while rx.recv_many(&mut batch, MAX_POINTS_PER_REQUEST).await > 0 {
        if let Err(e) = send_metrics(&client, &vm_url, &battery_name, &batch).await {
            eprintln!("Error sending {} metric(s): {}", batch.len(), e);
        }
        batch.clear();
    }
}

/// Producer side of the metrics queue. Never blocks the CAN loop: if the queue is
/// full the point is dropped and a (rate limited) warning is printed.
struct MetricQueue {
    tx: mpsc::Sender<MetricPoint>,
    dropped: u64,
    last_drop_warning: Option<Instant>,
}

impl MetricQueue {
    fn new(tx: mpsc::Sender<MetricPoint>) -> Self {
        Self { tx, dropped: 0, last_drop_warning: None }
    }

    fn push(&mut self, point: MetricPoint) {
        match self.tx.try_send(point) {
            Ok(()) => {},
            Err(TrySendError::Full(_)) => {
                self.dropped += 1;
                let now = Instant::now();
                if self
                    .last_drop_warning
                    .is_none_or(|t| now - t >= DROP_WARNING_INTERVAL)
                {
                    eprintln!(
                        "Metrics queue full, {} metric(s) dropped so far; is Victoria Metrics up?",
                        self.dropped
                    );
                    self.last_drop_warning = Some(now);
                }
            },
            Err(TrySendError::Closed(_)) => {
                eprintln!("Metrics sender stopped unexpectedly");
                std::process::exit(1);
            },
        }
    }
}

fn flag_to_f32(flag: bool) -> f32 {
    if flag { 1.0 } else { 0.0 }
}

/// Metric points for the master values refreshed by `pdo`.
fn master_metrics(master: &MasterInfo, pdo: MasterPdo, ts: i64, points: &mut Vec<MetricPoint>) {
    let mut push = |metric: &str, value: Option<f32>| {
        if let Some(value) = value {
            points.push(MetricPoint {
                metric: metric.to_string(),
                value: value as f64,
                timestamp: ts,
            });
        }
    };

    match pdo {
        MasterPdo::PackInfo1 => {
            push("varta_master_voltage", master.voltage);
            push("varta_master_current", master.current);
        },
        MasterPdo::ChargeControl => {
            push(
                "varta_master_charge_voltage_request",
                master.charge_voltage_request,
            );
            push(
                "varta_master_charge_current_request",
                master.charge_current_request,
            );
        },
        MasterPdo::PackInfo2 => {
            push("varta_master_max_fet_temp", master.max_battery_fet_temp);
            push("varta_master_max_cell_temp", master.max_battery_cell_temp);
        },
        MasterPdo::PackInfo3 => {
            push(
                "varta_master_full_charge_capacity",
                master.master_full_charge_capacity,
            );
            push(
                "varta_master_remaining_capacity",
                master.master_remaining_capacity,
            );
            push("varta_master_soc", master.soc);
        },
        // The master's info/warning/error bits are not exported.
        MasterPdo::ErrorMsgs => {},
    }
}

/// Metric points for the module values refreshed by `pdo`.
///
/// Module metrics are named after the module's serial number, which is only known
/// once it has been read via SDO; until then nothing is sent for the module.
fn module_metrics(eb: &VartaEasyblade, pdo: ModulePdo, ts: i64, points: &mut Vec<MetricPoint>) {
    let Some(serial_number) = &eb.sdo.serial_number else {
        return;
    };

    let metric_header = format!("varta_module_{:4}", serial_number.value);
    let mut push = |suffix: &str, value: Option<f32>| {
        if let Some(value) = value {
            points.push(MetricPoint {
                metric: metric_header.clone() + suffix,
                value: value as f64,
                timestamp: ts,
            });
        }
    };

    match pdo {
        ModulePdo::PackInfo1 => {
            push("_voltage", eb.pdo.voltage);
            push("_current", eb.pdo.current);
        },
        ModulePdo::Info3 => {
            push("_soc", eb.pdo.soc);
            push("_soh", eb.pdo.soh);
        },
        ModulePdo::Msgs => {
            if let Some(m) = &eb.pdo.msg_bits {
                push("_charge_fet", Some(flag_to_f32(m.info_bit_2_chgfet_closed)));
                push(
                    "_discharge_fet",
                    Some(flag_to_f32(m.info_bit_3_dsgfet_closed)),
                );
                push("_bypass_fet", Some(flag_to_f32(m.info_bit_4_bypass_fet_on)));
                push(
                    "_fully_charged",
                    Some(flag_to_f32(m.info_bit_6_fully_charged)),
                );
            }
        },
        // Module temperatures and charge requests are not tracked by the library.
        ModulePdo::Info2 => {},
    }
}

/// Metric points for the part of `varta` that `update` just refreshed, stamped
/// with the frame's receive time `ts`.
fn metrics_for_update(varta: &Varta, update: PdoUpdate, ts: i64) -> Vec<MetricPoint> {
    let mut points = Vec::new();
    match update {
        PdoUpdate::Master(pdo) => master_metrics(&varta.master, pdo, ts, &mut points),
        PdoUpdate::Module { node_id, pdo } => {
            if let Some(eb) = &varta.easyblades[node_id as usize] {
                module_metrics(eb, pdo, ts, &mut points);
            }
        },
    }
    points
}

async fn try_read_serial_number(varta: &mut varta_easyblade::Varta, canbus: &str, node_id: u8) {
    if varta.easyblades[node_id as usize].is_none() {
        return;
    }

    let mut sdo = match varta_easyblade::SdoSession::new(canbus, node_id) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to open CAN interface {}: {}", canbus, e);
            return;
        },
    };

    let Ok(serial_number) = sdo.read_serial_number().await else {
        return;
    };

    if let Some(eb) = &mut varta.easyblades[node_id as usize] {
        eb.sdo.serial_number = Some(serial_number);
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let client = match Client::builder().timeout(HTTP_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error creating HTTP client: {}", e);
            std::process::exit(1);
        },
    };

    let mut varta = match Varta::new(&args.canbus).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error opening CAN interface {}: {}", args.canbus, e);
            std::process::exit(1);
        },
    };

    eprintln!(
        "Connected to CAN interface {}, streaming to {}",
        args.canbus, args.vm_url
    );

    let (metrics_tx, metrics_rx) = mpsc::channel(SEND_QUEUE_CAPACITY);
    tokio::spawn(send_metrics_task(
        client,
        args.vm_url.clone(),
        args.battery_name.clone(),
        metrics_rx,
    ));
    let mut metrics = MetricQueue::new(metrics_tx);

    let mut serial_number_retry = tokio::time::interval(SERIAL_NUMBER_RETRY_INTERVAL);
    let mut expire_timer = Box::pin(tokio::time::sleep(varta.next_expiry_delay()));

    loop {
        tokio::select! {
            result = varta.process_socketcan_frame() => {
                let event = match result {
                    Ok(event) => event,
                    Err(e) => {
                        eprintln!("Error reading CAN: {}", e);
                        std::process::exit(1);
                    },
                };
                let received_at = now_ms();

                if let Some(node_id) = event.new_module {
                    // Found a new node, try to read its serial number via SDO.
                    try_read_serial_number(&mut varta, &args.canbus, node_id).await;
                }

                // Send whatever this frame updated, right away.
                for point in metrics_for_update(&varta, event.update, received_at) {
                    metrics.push(point);
                }

                expire_timer = Box::pin(tokio::time::sleep(varta.next_expiry_delay()));
            },

            _ = serial_number_retry.tick() => {
                // Try to read the serial number for any Easyblades that don't have it yet.
                for node_id in 0..varta_easyblade::MAX_MODULES {
                    if let Some(eb) = &mut varta.easyblades[node_id]
                        && eb.sdo.serial_number.is_none() {
                        try_read_serial_number(&mut varta, &args.canbus, node_id as u8).await;
                    }
                }
            },

            _ = expire_timer.as_mut() => {
                varta.expire_missing_modules();
                expire_timer = Box::pin(tokio::time::sleep(varta.next_expiry_delay()));
            }

        }
    }
}
