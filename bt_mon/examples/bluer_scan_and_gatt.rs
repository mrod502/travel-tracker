//! Example: scan and GATT I/O on the BlueZ (bluer) backend
//!
//! Scans, prints what the radio reports, and — given a device — connects, discovers
//! services, reads a characteristic and streams notifications. It is the runnable form of
//! the paths that cannot be exercised without a radio: discovery re-arming, the per-device
//! event stream, `services_resolved` after `discover_services`, BlueZ's `is_primary`, the
//! write-with-response flag, and the notification session
//! (`Characteristic1.StartNotify`) behind `subscribe`.
//!
//! # Running the example
//!
//! ```bash
//! # Scan for 10 seconds on the default adapter
//! cargo run --example bluer_scan_and_gatt --features bluer --no-default-features
//!
//! # On a named adapter, for 30 seconds
//! cargo run --example bluer_scan_and_gatt --features bluer --no-default-features -- \
//!     --adapter hci1 --scan 30
//!
//! # Connect and print the service table (with is_primary, which BlueZ answers)
//! cargo run --example bluer_scan_and_gatt --features bluer --no-default-features -- \
//!     --device dev_F4_FE_FB_CC_78_58
//!
//! # Read the Battery Level characteristic
//! cargo run --example bluer_scan_and_gatt --features bluer --no-default-features -- \
//!     --device dev_F4_FE_FB_CC_78_58 --read 00002a19-0000-1000-8000-00805f9b34fb
//!
//! # Subscribe and print frames until the interval elapses
//! cargo run --example bluer_scan_and_gatt --features bluer --no-default-features -- \
//!     --device dev_F4_FE_FB_CC_78_58 --notify 00002a37-0000-1000-8000-00805f9b34fb
//! ```
//!
//! Pass the `--device` value exactly as this example printed it. On BlueZ the identifier
//! carries the adapter it was heard on, and `DeviceId::from_platform_id` is what turns
//! `hci0/dev_F4_FE_FB_CC_78_58` into a stable id — hashing the raw string yourself would
//! make one peripheral two devices on a node with a second dongle.
//!
//! # What to look at
//!
//! * `raw_payload` on a discovered device is re-framed from BlueZ's typed properties and
//!   labelled `Reconstructed`: order by AD type, and an AD type missing from it is
//!   something BlueZ did not report, not proof the device omitted it.
//! * The health line at the end distinguishes a quiet room from a deaf node:
//!   `discovering`, `rearm_attempts`/`rearm_failures`, `event_stream_opens`, `last_error`.
//!   Toggle the adapter off (`rfkill block bluetooth`) during a scan and watch
//!   `discovering` go false within one scan interval instead of the scan merely going quiet.

use bt_mon::{
    create_bluer_monitor_with_config, CharacteristicUuid, DeviceEvent, DeviceId, DeviceMonitor,
    GattClient, MonitorConfig, RawPayloadSource,
};
use futures_util::stream::StreamExt;
use log::{debug, info, warn};
use std::time::Duration;

struct Options {
    adapter: Option<String>,
    scan: Duration,
    device: Option<DeviceId>,
    read: Option<CharacteristicUuid>,
    notify: Option<CharacteristicUuid>,
}

const USAGE: &str = "\
usage: bluer_scan_and_gatt [--adapter NAME] [--scan SECONDS] [--device ID] \
[--read UUID] [--notify UUID]";

/// Take the value that follows `flag`, advancing past it.
fn take_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}

fn parse_args() -> Result<Options, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut options = Options {
        adapter: None,
        scan: Duration::from_secs(10),
        device: None,
        read: None,
        notify: None,
    };

    let mut index = 0;
    while index < args.len() {
        // Every flag takes exactly one value, consumed here rather than carried as
        // half-parsed state into the next iteration.
        match args[index].as_str() {
            "--adapter" => options.adapter = Some(take_value(&args, &mut index, "--adapter")?),
            "--scan" => {
                let raw = take_value(&args, &mut index, "--scan")?;
                let seconds: u64 = raw
                    .parse()
                    .map_err(|_| format!("--scan wants a number of seconds, not {raw}"))?;
                options.scan = Duration::from_secs(seconds);
            }
            "--device" => {
                let raw = take_value(&args, &mut index, "--device")?;
                options.device = Some(DeviceId::from_platform_id(&raw));
            }
            "--read" => {
                let raw = take_value(&args, &mut index, "--read")?;
                options.read =
                    Some(CharacteristicUuid::parse_str(&raw).map_err(|e| e.to_string())?);
            }
            "--notify" => {
                let raw = take_value(&args, &mut index, "--notify")?;
                options.notify =
                    Some(CharacteristicUuid::parse_str(&raw).map_err(|e| e.to_string())?);
            }
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
        index += 1;
    }

    if (options.read.is_some() || options.notify.is_some()) && options.device.is_none() {
        return Err(format!("--read and --notify need --device\n{USAGE}"));
    }
    Ok(options)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            // `--help` and a real mistake both arrive here; only the mistake is a failure.
            eprintln!("{message}");
            if message.starts_with("usage:") {
                return Ok(());
            }
            return Err(message.into());
        }
    };

    // One entry point for both cases: with no `--adapter` the config simply names
    // nothing, and BlueZ's default adapter is chosen.
    let mut config = MonitorConfig::new();
    if let Some(selector) = &options.adapter {
        config = config.with_adapter(selector.clone());
    }
    let monitor = create_bluer_monitor_with_config(config).await?;

    if !monitor.is_powered().await? {
        warn!("Bluetooth adapter is not powered on.");
        return Err("Bluetooth adapter not powered".into());
    }
    info!("Adapter: {}", monitor.adapter_info().await?);

    scan_and_report(&monitor, options.scan).await?;

    if let Some(device_id) = &options.device {
        gatt_session(&monitor, device_id, &options).await?;
    }

    // Health last, because it is what makes a quiet run interpretable: zero events with
    // `discovering: true` is an empty room, zero events with it false is a radio problem.
    if let Some(health) = monitor.health() {
        info!(
            "health: powered={} requested_scan={} discovering={} rearm={}/{} failures \
             stream_opens={} last_error={:?}",
            health.adapter_powered,
            health.scanning_requested,
            health.discovering,
            health.rearm_failures,
            health.rearm_attempts,
            health.event_stream_opens,
            health.last_error,
        );
    }

    Ok(())
}

/// Scan with the event stream open, so reports are seen as they arrive rather than only in
/// the cache afterwards.
async fn scan_and_report(
    monitor: &impl GattClient,
    for_duration: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut events = monitor.device_events().await?;
    monitor.start_scan().await?;
    info!("Scanning for {}s...", for_duration.as_secs());

    let mut added = 0usize;
    let mut updated = 0usize;
    let mut advertisements = 0usize;
    let mut removed = 0usize;
    let report_deadline = tokio::time::sleep(for_duration);
    tokio::pin!(report_deadline);

    loop {
        tokio::select! {
            biased;
            _ = &mut report_deadline => break,
            event = events.next() => {
                let Some(event) = event else {
                    warn!("The event stream ended; the monitor will not report again until it re-opens it");
                    break;
                };
                match event {
                    DeviceEvent::DeviceAdded { device } => {
                        added += 1;
                        info!(
                            "added  {} {} rssi={:?}",
                            device.id,
                            device.name.as_deref().unwrap_or("(unnamed)"),
                            device.rssi,
                        );
                        report_advertisement(&device);
                    }
                    DeviceEvent::DeviceUpdated { device, changed_fields } => {
                        updated += 1;
                        info!("updated {} {:?}", device.id, changed_fields);
                    }
                    // Presence evidence and nothing else: a stationary beacon keeps
                    // producing these, which is the point — a swallowed report looks
                    // exactly like a device that left.
                    DeviceEvent::Advertisement { .. } => advertisements += 1,
                    DeviceEvent::DeviceRemoved { id } => {
                        removed += 1;
                        info!("removed {}", id);
                    }
                }
            }
        }
    }

    monitor.stop_scan().await?;
    info!("Scan stopped");
    let devices = monitor.devices().await?;
    info!(
        "{added} added, {updated} updated, {advertisements} unchanged advertisements, \
         {removed} removed; {n} devices in cache",
        n = devices.len()
    );

    for device in devices.iter().take(20) {
        debug!(
            "cache  {} {} rssi={:?} resolved={}",
            device.id,
            device.name.as_deref().unwrap_or("(unnamed)"),
            device.rssi,
            device.services_resolved,
        );
    }
    Ok(())
}

/// The advertisement contents a scan produced, with the provenance of the payload bytes.
fn report_advertisement(device: &bt_mon::BluetoothDevice) {
    for (company_id, data) in &device.manufacturer_data {
        info!("    manufacturer 0x{:04x} {:02x?}", company_id, data);
    }
    for (uuid, data) in &device.service_data {
        info!("    service-data {} {:02x?}", uuid, data);
    }
    match (&device.raw_payload, device.raw_payload_source) {
        (Some(payload), Some(source)) => {
            let label = match source {
                RawPayloadSource::Captured => "captured",
                RawPayloadSource::Reconstructed => {
                    "reconstructed from BlueZ's properties; order is by AD type, and an \
                     absent type means BlueZ did not report it"
                }
            };
            info!("    raw payload ({label}): {:02x?}", payload);
        }
        // No payload and no source: this backend never exposes the bytes. Read it as
        // "not available here", never as "the device advertised nothing".
        _ => debug!("    raw payload: not exposed by this backend"),
    }
}

async fn gatt_session(
    monitor: &impl GattClient,
    device_id: &DeviceId,
    options: &Options,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Connecting to {}...", device_id);
    monitor.connect(device_id).await?;

    let services = monitor.discover_services(device_id).await?;
    let resolved = monitor.device(device_id).await?.services_resolved;
    info!("Service table (services_resolved={resolved}):");
    for service in &services {
        info!(
            "  {} primary={} ({} characteristics)",
            service.uuid,
            service.is_primary,
            service.characteristics.len()
        );
        for characteristic in &service.characteristics {
            debug!(
                "    {} read={} write={} write_without_response={} notify={} indicate={}",
                characteristic.uuid,
                characteristic.properties.read,
                characteristic.properties.write,
                characteristic.properties.write_without_response,
                characteristic.properties.notify,
                characteristic.properties.indicate,
            );
        }
    }

    if let Some(uuid) = &options.read {
        match monitor.read_characteristic(device_id, uuid).await {
            Ok(value) => info!("read {uuid}: {:02x?}", value),
            Err(error) => warn!("read {uuid} failed: {error}"),
        }
    }

    if let Some(uuid) = &options.notify {
        monitor.subscribe(device_id, uuid).await?;
        info!("Subscribed to {uuid}; frames arrive on the device's stream");
        let mut frames = monitor.notifications(device_id).await?;
        let deadline = tokio::time::sleep(options.scan);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                biased;
                _ = &mut deadline => break,
                frame = frames.next() => {
                    let Some(frame) = frame else { break };
                    info!(
                        "{} on {:?}: {:02x?}",
                        frame.characteristic, frame.service, frame.value
                    );
                }
            }
        }

        // Ending the BlueZ session is what writes the CCCD back to "off".
        monitor.unsubscribe(device_id, uuid).await?;
        info!("Unsubscribed from {uuid}");
    }

    monitor.disconnect(device_id).await?;
    info!("Disconnected");
    Ok(())
}
