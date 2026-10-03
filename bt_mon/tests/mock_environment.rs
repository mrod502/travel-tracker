//! Integration tests for what a monitor is built with: the adapter it reports
//! and the cadence it advertises on.
//!
//! These exercise the public surface the application uses (`MonitorConfig`,
//! `create_mock_monitor_with_config`) rather than the module internals. The
//! mock backend is the only radio available here, so it stands in for the real
//! ones on the two behaviours that are shared: an adapter selector resolves to
//! one named adapter, and the scan interval decides how often the node hears
//! from the radio.

#![cfg(feature = "mock")]

use bt_mon::backends::mock::{MockConfig, SimulatedAdvertiser};
use bt_mon::{adapter_matches, create_mock_monitor_with_config, DeviceEvent, DeviceMonitor};
use futures::StreamExt;
use std::time::Duration;

/// Events the radio produces within `within`, with the stream opened first so
/// nothing published during the scan is missed.
async fn hear_from(monitor: &impl DeviceMonitor, within: Duration) -> Vec<DeviceEvent> {
    let mut events = monitor.device_events().await.expect("event stream");
    monitor.start_scan().await.expect("scan starts");

    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let Ok(maybe) = tokio::time::timeout_at(deadline, events.next()).await else {
            break;
        };
        match maybe {
            Some(event) => seen.push(event),
            None => break,
        }
    }

    monitor.stop_scan().await.ok();
    seen
}

fn beacon(id: &str, name: &str, interval_ms: u64) -> MockConfig {
    MockConfig::default()
        .with_scan_delay_ms(0)
        .with_advertisers(vec![SimulatedAdvertiser::new(id, name)])
        .with_advertise_interval_ms(interval_ms)
}

#[tokio::test]
async fn the_advertisement_reaches_the_consumer_as_bytes_too() {
    // `raw_payload` is what `store_raw_payload` gates, so the bytes have to
    // arrive on the event and not only in the parsed fields.
    let advertiser = SimulatedAdvertiser::new("F0:EE:00:00:01:02", "Payload Beacon");
    let monitor = create_mock_monitor_with_config(
        MockConfig::default()
            .with_scan_delay_ms(0)
            .with_advertisers(vec![advertiser.clone()])
            .with_advertise_interval_ms(40),
    )
    .await
    .expect("mock monitor");

    let seen = hear_from(&monitor, Duration::from_millis(300)).await;

    let advertised: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            DeviceEvent::DeviceAdded { device } => Some(device),
            _ => None,
        })
        .collect();
    assert!(
        !advertised.is_empty(),
        "a beacon should have been discovered, saw {} events",
        seen.len()
    );

    for device in advertised {
        let raw = device
            .raw_payload
            .as_deref()
            .expect("an advertised device carries its advertisement bytes");
        assert_eq!(
            raw,
            advertiser.advertisement_bytes().as_slice(),
            "the payload is the beacon's own, unchanged"
        );
    }
}

#[tokio::test]
async fn the_advertisement_bytes_are_advertisement_structures() {
    // Length-Type-Data, as the air puts them: the stored hex has to be
    // walkable by anything that reads advertisement structures later.
    let advertiser = SimulatedAdvertiser::new("AA:BB:CC:DD:EE:FF", "Structure Beacon");
    let bytes = advertiser.advertisement_bytes();

    let mut kinds = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let length = bytes[cursor] as usize;
        assert!(length >= 1, "a structure always carries a type");
        assert!(
            cursor + 1 + length <= bytes.len(),
            "the last structure overruns the payload"
        );
        kinds.push(bytes[cursor + 1]);
        cursor += 1 + length;
    }

    assert_eq!(
        kinds,
        vec![0x01, 0x09, 0xFF],
        "flags, name, manufacturer data"
    );
}

#[tokio::test]
async fn the_scan_interval_decides_how_often_the_radio_is_heard() {
    // The two monitors differ only in the interval, so a difference in what the
    // node hears is attributable to the setting rather than to the harness.
    let chatty = create_mock_monitor_with_config(beacon(
        "F0:EE:00:00:00:01",
        "Fast Beacon",
        /* interval_ms */ 30,
    ))
    .await
    .expect("mock monitor");
    let heard_chatty = hear_from(&chatty, Duration::from_millis(400)).await;

    let leisurely = create_mock_monitor_with_config(beacon(
        "F0:EE:00:00:00:01",
        "Fast Beacon",
        /* interval_ms */ 400,
    ))
    .await
    .expect("mock monitor");
    let heard_leisurely = hear_from(&leisurely, Duration::from_millis(400)).await;

    assert!(
        heard_chatty.len() >= 3,
        "a 30 ms beacon should be heard repeatedly, heard {}",
        heard_chatty.len()
    );
    assert!(
        heard_chatty.len() > heard_leisurely.len(),
        "a 30 ms beacon must be heard more often than a 400 ms one ({} vs {})",
        heard_chatty.len(),
        heard_leisurely.len()
    );
}

#[tokio::test]
async fn the_selected_adapter_is_the_one_the_monitor_reports() {
    let monitor = create_mock_monitor_with_config(
        MockConfig::default()
            .with_scan_delay_ms(0)
            .with_adapter_name("hci1"),
    )
    .await
    .expect("mock monitor");

    let info = monitor.adapter_info().await.expect("adapter info");
    assert!(info.contains("hci1"), "got: {info}");
    assert!(
        adapter_matches(&info, "hci1"),
        "the reported adapter has to satisfy its own selector: {info}"
    );
}
