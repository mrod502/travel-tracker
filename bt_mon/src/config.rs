//! Options a monitor is built with: which radio to use, and how to exercise it.
//!
//! Both of these have to reach the backend that opens the adapter, which is why
//! they are constructor arguments rather than methods on [`DeviceMonitor`]: by
//! the time a monitor exists, the adapter is already chosen and a scan is
//! already configured. Accepting them after the fact is how a setting comes to
//! be read off the command line, printed back at the operator, and then ignored.
//!
//! [`DeviceMonitor`]: crate::monitor::DeviceMonitor

use std::time::Duration;

/// The interval a monitor re-arms its scan on when nothing else is stated.
pub const DEFAULT_SCAN_INTERVAL: Duration = Duration::from_millis(1_000);

/// How a monitor selects and drives its adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorConfig {
    /// Which adapter to open, matched with [`adapter_matches`].
    ///
    /// `None` takes the first adapter the system reports, which is a fine answer
    /// for a machine with one radio and a bad one for a machine with two — hence
    /// the setting existing at all.
    pub adapter: Option<String>,

    /// How long a scan runs before the monitor re-arms it, and the interval the
    /// simulated radio advertises on.
    ///
    /// Scanning is continuous: this is not a duty cycle. Every interval the
    /// monitor issues a fresh stop/start so a controller-side scan that has gone
    /// stale (adapter sleep, `bluetoothd` restart, a stack that expires discovery
    /// on its own) cannot leave a running node silently deaf. Zero disables
    /// re-arming, which is what a unit test driving the scan by hand wants.
    pub scan_interval: Duration,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            adapter: None,
            scan_interval: DEFAULT_SCAN_INTERVAL,
        }
    }
}

impl MonitorConfig {
    /// The default configuration: the first adapter, re-armed every second.
    pub fn new() -> Self {
        Self::default()
    }

    /// Select an adapter by id, name, or MAC address.
    pub fn with_adapter(mut self, selector: impl Into<String>) -> Self {
        self.adapter = Some(selector.into());
        self
    }

    /// Set how often the scan is re-armed; see [`MonitorConfig::scan_interval`].
    pub fn with_scan_interval(mut self, interval: Duration) -> Self {
        self.scan_interval = interval;
        self
    }

    /// Whether `description` is the adapter this configuration asks for.
    ///
    /// A configuration with no selector matches every adapter, so the caller
    /// keeps its existing "first adapter" behaviour.
    pub fn matches_adapter(&self, description: &str) -> bool {
        match self.adapter.as_deref() {
            None => true,
            Some(selector) => adapter_matches(description, selector),
        }
    }
}

/// Match an adapter against what the operator asked for.
///
/// `description` is whatever the backend says identifies the adapter — on BlueZ
/// the D-Bus adapter id followed by a modalias (`hci0 (usb:1d6b:0003)`), for the
/// mock backend the configured name, and on other platforms a description this
/// crate does not define. Matching is therefore deliberately narrow:
///
/// * case-insensitive, whitespace-trimmed equality with the whole description;
/// * equality with its first field (up to a space or a parenthesis), which is the
///   part that carries `hci0` on BlueZ;
/// * a substring match only for a selector that looks like a MAC address, since
///   an address can appear anywhere in a description and is unambiguous by
///   itself.
///
/// Prefix matching is excluded on purpose: `hci` would match both `hci0` and
/// `hci1`, and an operator who meant one of them should be told to say which
/// rather than have one picked.
pub fn adapter_matches(description: &str, selector: &str) -> bool {
    let description = description.trim().to_lowercase();
    let selector = selector.trim().to_lowercase();

    if description.is_empty() || selector.is_empty() {
        return false;
    }

    if description == selector {
        return true;
    }

    if first_field(&description) == selector {
        return true;
    }

    selector.contains(':') && description.contains(&selector)
}

/// The part of a description before the first space or parenthesis.
fn first_field(description: &str) -> &str {
    description
        .find([' ', '\t', '('])
        .map(|end| &description[..end])
        .unwrap_or(description)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(description: &str, selector: &str) -> bool {
        adapter_matches(description, selector)
    }

    #[test]
    fn an_adapter_selector_matches_the_bluez_id_column() {
        // What `Central::adapter_info()` yields on BlueZ: "<id> (<modalias>)".
        assert!(matches("hci0 (usb:1d6b:0002)", "hci0"));
        assert!(matches("hci1 (usb:1d6b:0002)", "hci1"));
    }

    #[test]
    fn a_selector_is_case_insensitive_and_may_be_padded() {
        assert!(matches("hci0 (usb:1d6b:0002)", " HCI0 "));
        assert!(matches("MockAdapter 'dongle'", "mockadapter"));
    }

    #[test]
    fn the_whole_description_matches_too() {
        assert!(matches("hci0", "hci0"));
        assert!(matches("hci0 (usb:1d6b:0002)", "hci0 (usb:1d6b:0002)"));
    }

    #[test]
    fn a_mac_address_matches_wherever_it_appears() {
        assert!(matches("eth0 [aa:bb:cc:dd:ee:ff]", "AA:BB:CC:DD:EE:FF"));
    }

    #[test]
    fn a_prefix_of_an_adapter_id_is_not_a_match() {
        // `hci` would silently pick one of two adapters.
        assert!(!matches("hci0 (usb:1d6b:0002)", "hci"));
        assert!(!matches("hci0 (usb:1d6b:0002)", "hc"));
    }

    #[test]
    fn a_different_adapter_does_not_match() {
        assert!(!matches("hci1 (usb:1d6b:0002)", "hci0"));
        assert!(!matches("MockAdapter", "hci0"));
    }

    #[test]
    fn an_empty_selector_or_description_matches_nothing() {
        assert!(!matches("hci0", ""));
        assert!(!matches("", "hci0"));
    }

    #[test]
    fn a_config_without_a_selector_matches_every_adapter() {
        let config = MonitorConfig::default();

        assert!(config.matches_adapter("hci0 (usb:1d6b:0002)"));
        assert!(config.matches_adapter("anything at all"));
    }

    #[test]
    fn a_config_with_a_selector_matches_only_that_adapter() {
        let config = MonitorConfig::new().with_adapter("hci1");

        assert!(config.matches_adapter("hci1 (usb:1d6b:0002)"));
        assert!(!config.matches_adapter("hci0 (usb:1d6b:0002)"));
    }

    #[test]
    fn the_default_rearm_interval_is_the_documented_one() {
        assert_eq!(
            MonitorConfig::default().scan_interval,
            DEFAULT_SCAN_INTERVAL
        );
        assert_eq!(
            MonitorConfig::new()
                .with_scan_interval(Duration::from_secs(5))
                .scan_interval,
            Duration::from_secs(5)
        );
    }
}
