//! Stage machine for one laptop. The other laptop runs the same binary and picks the other role.

use crate::disk::{DropWatch, Inventory};
use crate::wifi::Network;

/// Development machine internal SSD. It may be read as a source. It is never a target.
pub const CONTROLLER_SERIAL: &str = "S2Z5NY0H998813";

/// The protected disk must not be opened for writing.
pub fn controller_is_target(role: Role, serial: &str) -> bool {
    role == Role::Target && serial == CONTROLLER_SERIAL
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Source,
    Target,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyView {
    Idle,
    Waiting,
    Copying,
    Done,
    Stopped,
    Failed,
}

impl CopyView {
    pub fn busy(self) -> bool {
        matches!(self, Self::Waiting | Self::Copying)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Role,
    Disk,
    Cable,
    Confirm,
    Ready,
    Wifi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiPhase {
    List,
    Ssid,
    Password,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiJob {
    Scan,
    Connect,
    Disconnect,
}

#[derive(Clone, Debug)]
pub struct WifiUi {
    pub return_to: Stage,
    pub phase: WifiPhase,
    pub networks: Vec<Network>,
    pub cursor: usize,
    pub ssid: String,
    pub password: String,
    pub hidden: bool,
    pub reveal: bool,
    pub note: String,
    pub connected: String,
    pub iface: String,
    pub pending: Option<WifiJob>,
}

impl WifiUi {
    fn new() -> Self {
        Self {
            return_to: Stage::Role,
            phase: WifiPhase::List,
            networks: Vec::new(),
            cursor: 0,
            ssid: String::new(),
            password: String::new(),
            hidden: false,
            reveal: false,
            note: String::new(),
            connected: String::new(),
            iface: String::new(),
            pending: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Up,
    Down,
    Tab,
}

#[derive(Clone, Debug)]
pub struct App {
    pub stage: Stage,
    pub role: Option<Role>,
    pub inventory: Inventory,
    pub typed: String,
    pub mismatch: bool,
    pub confirmed: bool,
    pub wifi: WifiUi,
    pub pulse: u8,
    pub reload: bool,
    pub link_note: String,
    pub copy_view: CopyView,
    pub copy_note: String,
    pub copy_done: u64,
    pub copy_total: u64,
    pub copy_hash: String,
    pub copy_peer: String,
    pub copy_launch: bool,
    pub copy_abort: bool,
    pub copy_reset: bool,
    pub speed_bits: Option<u64>,
    pub speed_testing: bool,
    pub speed_again: bool,
    pub speed_note: String,
    pub speed_addr: Option<String>,
    pub recent_drops: Option<u64>,
    drops_watch: DropWatch,
}

impl App {
    pub fn new(inventory: Inventory) -> Self {
        let mut app = Self {
            stage: Stage::Role,
            role: None,
            inventory,
            typed: String::new(),
            mismatch: false,
            confirmed: false,
            wifi: WifiUi::new(),
            pulse: 0,
            reload: false,
            link_note: String::new(),
            copy_view: CopyView::Idle,
            copy_note: String::new(),
            copy_done: 0,
            copy_total: 0,
            copy_hash: String::new(),
            copy_peer: String::new(),
            copy_launch: false,
            copy_abort: false,
            copy_reset: false,
            speed_bits: None,
            speed_testing: false,
            speed_again: false,
            speed_note: String::new(),
            speed_addr: None,
            recent_drops: None,
            drops_watch: DropWatch::new(),
        };
        app.observe_drops(std::time::Instant::now());
        app
    }

    pub fn observe_drops(&mut self, now: std::time::Instant) {
        self.recent_drops = self.drops_watch.observe(self.inventory.link.drops, now);
    }

    pub fn serial(&self) -> Option<&str> {
        self.inventory
            .disk
            .as_ref()
            .ok()
            .map(|disk| disk.serial.as_str())
    }

    /// Returns true when the process should exit.
    pub fn on_key(&mut self, key: Key) -> bool {
        let typing = self.stage == Stage::Confirm
            || (self.stage == Stage::Wifi
                && matches!(self.wifi.phase, WifiPhase::Ssid | WifiPhase::Password));
        if matches!(key, Key::Char('q' | 'Q')) && !typing {
            if self.copy_view.busy() {
                self.copy_abort = true;
                return false;
            }
            return true;
        }
        if matches!(key, Key::Char('w' | 'W'))
            && !typing
            && !self.copy_view.busy()
            && matches!(
                self.stage,
                Stage::Role | Stage::Disk | Stage::Cable | Stage::Ready
            )
        {
            self.open_wifi();
            return false;
        }
        match (self.stage, key) {
            (Stage::Role, Key::Char('s' | 'S')) => {
                self.role = Some(Role::Source);
                self.stage = Stage::Disk;
            }
            (Stage::Role, Key::Char('t' | 'T')) => {
                self.role = Some(Role::Target);
                self.stage = Stage::Disk;
            }
            (Stage::Role, Key::Esc) => return true,
            (Stage::Disk, Key::Enter) if self.inventory.disk.is_ok() => {
                self.stage = Stage::Cable;
            }
            (Stage::Disk, Key::Esc) => {
                self.role = None;
                self.stage = Stage::Role;
            }
            (Stage::Cable, Key::Char('t' | 'T')) => {
                self.speed_again = true;
                self.speed_testing = true;
                self.speed_bits = None;
                self.speed_note.clear();
            }
            (Stage::Cable, Key::Char('r' | 'R')) => {
                self.link_note = "Reloading drivers.".to_string();
                self.reload = true;
            }
            (Stage::Cable, Key::Enter) => {
                self.typed.clear();
                self.mismatch = false;
                self.stage = if self.role == Some(Role::Target) {
                    Stage::Confirm
                } else {
                    Stage::Ready
                };
            }
            (Stage::Cable, Key::Esc) => self.stage = Stage::Disk,
            (Stage::Confirm, Key::Esc) => {
                self.typed.clear();
                self.mismatch = false;
                self.stage = Stage::Cable;
            }
            (Stage::Confirm, Key::Backspace) => {
                self.typed.pop();
                self.mismatch = false;
            }
            (Stage::Confirm, Key::Char(ch)) if ch.is_ascii_alphanumeric() => {
                self.typed.push(ch.to_ascii_uppercase());
                self.mismatch = false;
            }
            (Stage::Confirm, Key::Enter) => self.try_confirm(),
            (Stage::Ready, Key::Enter) => self.request_copy(),
            (Stage::Ready, Key::Esc) => {
                if self.copy_view.busy() {
                    self.copy_abort = true;
                } else {
                    self.reset_copy();
                    self.confirmed = false;
                    self.stage = if self.role == Some(Role::Target) {
                        Stage::Confirm
                    } else {
                        Stage::Cable
                    };
                }
            }
            (Stage::Wifi, key) => self.on_wifi_key(key),
            _ => {}
        }
        false
    }

    fn open_wifi(&mut self) {
        self.wifi.return_to = self.stage;
        self.wifi.phase = WifiPhase::List;
        self.wifi.password.clear();
        self.wifi.reveal = false;
        self.wifi.note = "Scanning.".to_string();
        self.wifi.pending = Some(WifiJob::Scan);
        self.stage = Stage::Wifi;
    }

    fn on_wifi_key(&mut self, key: Key) {
        match (self.wifi.phase, key) {
            (WifiPhase::List, Key::Esc) => {
                self.stage = self.wifi.return_to;
            }
            (WifiPhase::List, Key::Up | Key::Char('k')) => self.move_wifi(-1),
            (WifiPhase::List, Key::Down | Key::Char('j')) => self.move_wifi(1),
            (WifiPhase::List, Key::Char('s' | 'S')) => {
                self.wifi.note = "Scanning.".to_string();
                self.wifi.pending = Some(WifiJob::Scan);
            }
            (WifiPhase::List, Key::Char('h' | 'H')) => {
                self.wifi.ssid.clear();
                self.wifi.password.clear();
                self.wifi.hidden = true;
                self.wifi.note.clear();
                self.wifi.phase = WifiPhase::Ssid;
            }
            (WifiPhase::List, Key::Enter) => self.choose_network(),
            (WifiPhase::List, Key::Char('d' | 'D')) => {
                let active = !self.wifi.connected.is_empty()
                    || self.wifi.networks.iter().any(|net| net.in_use);
                if active {
                    self.wifi.note = "Disconnecting.".to_string();
                    self.wifi.pending = Some(WifiJob::Disconnect);
                } else {
                    self.wifi.note = "Not connected.".to_string();
                }
            }
            (WifiPhase::Ssid, Key::Esc) => {
                self.wifi.phase = WifiPhase::List;
                self.wifi.note.clear();
            }
            (WifiPhase::Ssid, Key::Backspace) => {
                self.wifi.ssid.pop();
            }
            (WifiPhase::Ssid, Key::Char(ch)) => self.wifi.ssid.push(ch),
            (WifiPhase::Ssid, Key::Enter) => {
                if self.wifi.ssid.is_empty() {
                    self.wifi.note = "Type the network name.".to_string();
                } else {
                    self.wifi.password.clear();
                    self.wifi.reveal = false;
                    self.wifi.note.clear();
                    self.wifi.phase = WifiPhase::Password;
                }
            }
            (WifiPhase::Password, Key::Esc) => {
                self.wifi.password.clear();
                self.wifi.phase = if self.wifi.hidden {
                    WifiPhase::Ssid
                } else {
                    WifiPhase::List
                };
            }
            (WifiPhase::Password, Key::Backspace) => {
                self.wifi.password.pop();
            }
            (WifiPhase::Password, Key::Tab) => self.wifi.reveal = !self.wifi.reveal,
            (WifiPhase::Password, Key::Char(ch)) => self.wifi.password.push(ch),
            (WifiPhase::Password, Key::Enter) => {
                self.wifi.note = "Connecting.".to_string();
                self.wifi.pending = Some(WifiJob::Connect);
            }
            _ => {}
        }
    }

    /// A joined network returns to the list, with every list key working.
    pub fn wifi_connected(&mut self, ssid: &str, ip: &str) {
        self.wifi.phase = WifiPhase::List;
        self.wifi.password.clear();
        self.wifi.reveal = false;
        self.wifi.note.clear();
        self.wifi.connected = if ip.is_empty() {
            format!("Connected  {ssid}")
        } else {
            format!("Connected  {ssid}  {ip}")
        };
        if let Some(index) = self.wifi.networks.iter().position(|net| net.ssid == ssid) {
            for (i, network) in self.wifi.networks.iter_mut().enumerate() {
                network.in_use = i == index;
            }
            self.wifi.cursor = index;
        }
    }

    pub fn wifi_disconnected(&mut self) {
        self.wifi.phase = WifiPhase::List;
        self.wifi.note.clear();
        self.wifi.connected.clear();
        for network in &mut self.wifi.networks {
            network.in_use = false;
        }
    }

    fn move_wifi(&mut self, delta: isize) {
        let len = self.wifi.networks.len() as isize;
        if len == 0 {
            return;
        }
        let next = self.wifi.cursor as isize + delta;
        self.wifi.cursor = next.rem_euclid(len) as usize;
    }

    fn choose_network(&mut self) {
        let Some(network) = self.wifi.networks.get(self.wifi.cursor).cloned() else {
            self.wifi.note = "No networks. Press s to scan, or h for a hidden name.".to_string();
            return;
        };
        if network.hidden() {
            self.wifi.ssid.clear();
            self.wifi.password.clear();
            self.wifi.hidden = true;
            self.wifi.note.clear();
            self.wifi.phase = WifiPhase::Ssid;
            return;
        }
        let open = network.open();
        self.wifi.ssid = network.ssid;
        self.wifi.password.clear();
        self.wifi.hidden = false;
        self.wifi.reveal = false;
        if open {
            self.wifi.note = "Connecting.".to_string();
            self.wifi.pending = Some(WifiJob::Connect);
        } else {
            self.wifi.note.clear();
            self.wifi.phase = WifiPhase::Password;
        }
    }

    fn request_copy(&mut self) {
        if self.copy_view.busy() || self.copy_view == CopyView::Done {
            return;
        }
        self.copy_note.clear();
        self.copy_hash.clear();
        self.copy_peer.clear();
        self.copy_done = 0;
        self.copy_total = 0;
        match self.copy_gate() {
            Ok(()) => {
                self.copy_view = CopyView::Waiting;
                self.copy_note = "Waiting for the other laptop.".to_string();
                if self.role == Some(Role::Source) {
                    self.copy_total = self
                        .inventory
                        .disk
                        .as_ref()
                        .map(|disk| disk.bytes)
                        .unwrap_or(0);
                }
                self.copy_launch = true;
            }
            Err(msg) => {
                self.copy_view = CopyView::Idle;
                self.copy_note = format!("error: {msg}");
            }
        }
    }

    fn copy_gate(&self) -> Result<(), String> {
        let disk = self.inventory.disk.as_ref().map_err(|err| err.clone())?;
        if disk.serial.is_empty() {
            return Err("this disk has no serial.".to_string());
        }
        if self.role == Some(Role::Target) && disk.serial == CONTROLLER_SERIAL {
            return Err("this disk is not a clone endpoint.".to_string());
        }
        if disk.bytes == 0 {
            return Err("this disk size is missing.".to_string());
        }
        if self.role.is_none() {
            return Err("no role selected.".to_string());
        }
        if let Some(block) = self.inventory.link.copy_block() {
            return Err(block.to_string());
        }
        Ok(())
    }

    fn reset_copy(&mut self) {
        self.copy_view = CopyView::Idle;
        self.copy_note.clear();
        self.copy_hash.clear();
        self.copy_peer.clear();
        self.copy_done = 0;
        self.copy_total = 0;
        self.copy_launch = false;
        self.copy_abort = false;
        self.copy_reset = true;
    }

    fn try_confirm(&mut self) {
        match self.serial() {
            Some(serial) if self.typed == serial => {
                self.confirmed = true;
                self.mismatch = false;
                self.stage = Stage::Ready;
            }
            _ => self.mismatch = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::{Disk, Inventory, Link, Part, Usb};
    use crate::wifi::Network;

    fn app() -> App {
        App::new(Inventory {
            product: "MacBookPro11,4".to_string(),
            disk: Ok(Disk {
                name: "sda".to_string(),
                model: "APPLE SSD SM0256G".to_string(),
                serial: "S29CNYDG898371".to_string(),
                bytes: 251_000_193_024,
                parts: vec![Part {
                    name: "sda1".to_string(),
                    bytes: 100,
                }],
            }),
            usb: vec![Usb {
                name: "sdb".to_string(),
                model: "PHILIPS".to_string(),
                bytes: 1,
                label: Some("RESCUE1302".to_string()),
            }],
            link: Link::offline(),
        })
    }

    #[test]
    fn source_reaches_ready_without_a_serial() {
        let mut app = app();
        assert!(!app.on_key(Key::Char('s')));
        assert_eq!(app.stage, Stage::Disk);
        assert!(!app.on_key(Key::Enter));
        assert!(!app.on_key(Key::Enter));
        assert_eq!(app.stage, Stage::Ready);
        assert!(!app.confirmed);
    }

    #[test]
    fn target_stays_put_until_the_serial_matches() {
        let mut app = app();
        app.on_key(Key::Char('t'));
        app.on_key(Key::Enter);
        app.on_key(Key::Enter);
        assert_eq!(app.stage, Stage::Confirm);
        for ch in "s29cnydg89837".chars() {
            app.on_key(Key::Char(ch));
        }
        app.on_key(Key::Enter);
        assert!(app.mismatch);
        assert_eq!(app.stage, Stage::Confirm);
        app.on_key(Key::Char('1'));
        app.on_key(Key::Enter);
        assert!(app.confirmed);
        assert_eq!(app.stage, Stage::Ready);
    }

    #[test]
    fn q_during_confirm_is_a_character() {
        let mut app = app();
        app.stage = Stage::Confirm;
        app.role = Some(Role::Target);
        assert!(!app.on_key(Key::Char('q')));
        assert_eq!(app.typed, "Q");
    }

    #[test]
    fn no_disk_cannot_continue() {
        let mut app = app();
        app.inventory.disk = Err("missing".to_string());
        app.on_key(Key::Char('s'));
        app.on_key(Key::Enter);
        assert_eq!(app.stage, Stage::Disk);
    }

    #[test]
    fn wifi_is_outside_the_clone_steps() {
        let mut app = app();
        app.on_key(Key::Char('s'));
        app.on_key(Key::Char('w'));
        assert_eq!(app.stage, Stage::Wifi);
        assert_eq!(app.wifi.return_to, Stage::Disk);
        assert_eq!(app.wifi.pending, Some(WifiJob::Scan));
        app.wifi.pending = None;
        app.on_key(Key::Esc);
        assert_eq!(app.stage, Stage::Disk);
        assert_eq!(app.role, Some(Role::Source));
    }

    #[test]
    fn hidden_network_asks_for_the_name_then_the_password() {
        let mut app = app();
        app.open_wifi();
        app.wifi.pending = None;
        app.on_key(Key::Char('h'));
        assert_eq!(app.wifi.phase, WifiPhase::Ssid);
        for ch in "S&L".chars() {
            app.on_key(Key::Char(ch));
        }
        app.on_key(Key::Enter);
        assert_eq!(app.wifi.phase, WifiPhase::Password);
        app.on_key(Key::Char('q'));
        assert_eq!(app.stage, Stage::Wifi);
        assert_eq!(app.wifi.password, "q");
        app.on_key(Key::Enter);
        assert_eq!(app.wifi.pending, Some(WifiJob::Connect));
        assert!(app.wifi.hidden);
        assert_eq!(app.wifi.ssid, "S&L");
    }

    #[test]
    fn a_joined_network_returns_to_the_list() {
        let mut app = app();
        app.stage = Stage::Wifi;
        app.wifi.phase = WifiPhase::Password;
        app.wifi.password = "secret".to_string();
        app.wifi.networks = vec![
            Network {
                ssid: "Other".to_string(),
                signal: 10,
                security: "WPA2".to_string(),
                in_use: false,
            },
            Network {
                ssid: "S&L".to_string(),
                signal: 70,
                security: "WPA2".to_string(),
                in_use: false,
            },
        ];
        app.wifi_connected("S&L", "192.168.2.13");
        assert_eq!(app.wifi.phase, WifiPhase::List);
        assert!(app.wifi.password.is_empty());
        assert_eq!(app.wifi.connected, "Connected  S&L  192.168.2.13");
        assert!(!app.wifi.networks[0].in_use);
        assert!(app.wifi.networks[1].in_use);
        assert_eq!(app.wifi.cursor, 1);
        app.on_key(Key::Char('j'));
        assert_eq!(app.wifi.cursor, 0);
        app.on_key(Key::Char('d'));
        assert_eq!(app.wifi.pending, Some(WifiJob::Disconnect));
        app.wifi.pending = None;
        app.wifi_disconnected();
        assert!(app.wifi.connected.is_empty());
        assert!(app.wifi.networks.iter().all(|net| !net.in_use));
        app.on_key(Key::Char('d'));
        assert_eq!(app.wifi.note, "Not connected.");
        assert!(app.wifi.pending.is_none());
    }

    #[test]
    fn ready_enter_waits_until_thunderbolt_can_pass_frames() {
        let mut app = app();
        app.on_key(Key::Char('s'));
        app.on_key(Key::Enter);
        app.on_key(Key::Enter);
        assert_eq!(app.stage, Stage::Ready);
        app.on_key(Key::Enter);
        assert!(!app.copy_launch);
        assert!(app.copy_note.contains("Thunderbolt is not ready"));
        app.inventory.link = Link {
            clx: Some("N".to_string()),
            e2e: Some("N".to_string()),
            addr: Some("169.254.251.250".to_string()),
            oper: Some("up".to_string()),
            drops: Some(0),
        };
        app.on_key(Key::Enter);
        assert!(app.copy_launch);
        assert_eq!(app.copy_view, CopyView::Waiting);
    }

    #[test]
    fn the_controller_disk_can_be_a_source_but_not_a_target() {
        let mut app = app();
        app.inventory.disk.as_mut().unwrap().serial = CONTROLLER_SERIAL.to_string();
        app.inventory.link = Link {
            clx: Some("N".to_string()),
            e2e: Some("0".to_string()),
            addr: Some("169.254.1.1".to_string()),
            oper: Some("up".to_string()),
            drops: Some(0),
        };
        app.stage = Stage::Ready;
        app.role = Some(Role::Source);
        app.on_key(Key::Enter);
        assert!(app.copy_launch);
        assert!(!app.copy_note.contains("not a clone endpoint"));

        app.copy_launch = false;
        app.copy_note.clear();
        app.copy_view = CopyView::Idle;
        app.role = Some(Role::Target);
        app.on_key(Key::Enter);
        assert!(!app.copy_launch);
        assert!(app.copy_note.contains("not a clone endpoint"));
    }

    #[test]
    fn q_and_esc_abort_a_running_copy() {
        let mut app = app();
        app.stage = Stage::Ready;
        app.role = Some(Role::Target);
        app.copy_view = CopyView::Copying;
        assert!(!app.on_key(Key::Char('q')));
        assert!(app.copy_abort);
        assert_eq!(app.stage, Stage::Ready);
        app.copy_abort = false;
        app.on_key(Key::Char('w'));
        assert_eq!(app.stage, Stage::Ready);
        app.on_key(Key::Esc);
        assert!(app.copy_abort);
        assert_eq!(app.stage, Stage::Ready);
    }

    #[test]
    fn a_finished_copy_does_not_start_again_on_enter() {
        let mut app = app();
        app.stage = Stage::Ready;
        app.role = Some(Role::Source);
        app.copy_view = CopyView::Done;
        app.inventory.link = Link {
            clx: Some("N".to_string()),
            e2e: Some("N".to_string()),
            addr: Some("169.254.1.1".to_string()),
            oper: Some("up".to_string()),
            drops: Some(0),
        };
        app.on_key(Key::Enter);
        assert!(!app.copy_launch);
        assert_eq!(app.copy_view, CopyView::Done);
    }

    #[test]
    fn w_on_the_serial_screen_is_a_character() {
        let mut app = app();
        app.stage = Stage::Confirm;
        app.role = Some(Role::Target);
        app.on_key(Key::Char('w'));
        assert_eq!(app.stage, Stage::Confirm);
        assert_eq!(app.typed, "W");
    }
}
