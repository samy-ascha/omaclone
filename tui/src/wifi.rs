//! Wi-Fi is a rescue-system helper. It is not part of the clone path.
//!
//! These MacBooks need brcmfmac loaded with roamoff=1 and feature_disable
//! bits 13 (firmware supplicant) and 19 (SAE). `nmcli device wifi connect`
//! and `--ask` start WPS on this access point, so the profile is stored
//! and brought up by name. Regulatory domain NL is required for the 5 GHz
//! DFS channel this network uses.

use std::fs;
use std::process::Command;

/// Bit 13 is FWSUP, bit 19 is SAE.
const FEATURE_DISABLE: &str = "532480";
const PROFILE: &str = "omaclone-wifi";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub ssid: String,
    pub signal: u32,
    pub security: String,
    pub in_use: bool,
}

impl Network {
    pub fn hidden(&self) -> bool {
        self.ssid.is_empty()
    }

    pub fn label(&self) -> String {
        if self.hidden() {
            "(hidden)".to_string()
        } else {
            self.ssid.clone()
        }
    }

    pub fn open(&self) -> bool {
        let security = self.security.trim();
        security.is_empty() || security == "--" || security.eq_ignore_ascii_case("open")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinPlan {
    pub delete: Vec<String>,
    pub add: Vec<String>,
    pub modify: Vec<String>,
    pub up: Vec<String>,
}

pub fn driver_needs_reload(roamoff: Option<&str>) -> bool {
    roamoff != Some("1")
}

pub fn parse_wifi_list(text: &str) -> Vec<Network> {
    let mut found: Vec<Network> = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let fields = split_nmcli(line);
        if fields.len() < 4 {
            continue;
        }
        let network = Network {
            in_use: fields[0].contains('*'),
            ssid: fields[1].clone(),
            signal: fields[2].trim().parse().unwrap_or(0),
            security: fields[3].clone(),
        };
        if let Some(existing) = found.iter_mut().find(|item| item.ssid == network.ssid) {
            existing.in_use |= network.in_use;
            if network.signal > existing.signal {
                existing.signal = network.signal;
                existing.security = network.security;
            }
        } else {
            found.push(network);
        }
    }
    found.sort_by(|a, b| {
        b.in_use
            .cmp(&a.in_use)
            .then(b.signal.cmp(&a.signal))
            .then(a.ssid.cmp(&b.ssid))
    });
    found
}

pub fn plan_join(iface: &str, ssid: &str, password: &str, hidden: bool) -> JoinPlan {
    let mut add = vec![
        "connection".to_string(),
        "add".to_string(),
        "type".to_string(),
        "wifi".to_string(),
        "ifname".to_string(),
        iface.to_string(),
        "con-name".to_string(),
        PROFILE.to_string(),
        "ssid".to_string(),
        ssid.to_string(),
    ];
    if !password.is_empty() {
        add.extend([
            "wifi-sec.key-mgmt".to_string(),
            "wpa-psk".to_string(),
            "wifi-sec.psk".to_string(),
            password.to_string(),
            "wifi-sec.psk-flags".to_string(),
            "0".to_string(),
            "wifi-sec.wps-method".to_string(),
            "0".to_string(),
        ]);
    }
    let mut modify = vec![
        "connection".to_string(),
        "modify".to_string(),
        PROFILE.to_string(),
        "802-11-wireless.cloned-mac-address".to_string(),
        "permanent".to_string(),
        "802-11-wireless.mac-address-randomization".to_string(),
        "1".to_string(),
    ];
    if hidden {
        modify.push("802-11-wireless.hidden".to_string());
        modify.push("yes".to_string());
    }
    JoinPlan {
        delete: vec![
            "connection".to_string(),
            "delete".to_string(),
            PROFILE.to_string(),
        ],
        add,
        modify,
        up: vec![
            "--wait".to_string(),
            "45".to_string(),
            "connection".to_string(),
            "up".to_string(),
            PROFILE.to_string(),
        ],
    }
}

pub fn scan() -> Result<Vec<Network>, String> {
    ensure_driver()?;
    let iface = wait_iface()?;
    let _ = run("nmcli", &["device", "set", &iface, "managed", "yes"]);
    let output = run(
        "nmcli",
        &[
            "-t",
            "-f",
            "IN-USE,SSID,SIGNAL,SECURITY",
            "device",
            "wifi",
            "list",
            "--rescan",
            "yes",
        ],
    )?;
    Ok(parse_wifi_list(&output))
}

pub fn join(iface: &str, ssid: &str, password: &str, hidden: bool) -> Result<WifiLink, String> {
    if ssid.is_empty() {
        return Err("The network name is empty.".to_string());
    }
    ensure_driver()?;
    let _ = wait_iface();
    let iface = if iface.is_empty() {
        wifi_iface()?
    } else {
        iface.to_string()
    };
    let plan = plan_join(&iface, ssid, password, hidden);
    let _ = run("nmcli", &plan.delete);
    run("nmcli", &plan.add).map_err(|err| redact(&err, password))?;
    run("nmcli", &plan.modify).map_err(|err| redact(&err, password))?;
    run("nmcli", &plan.up).map_err(|err| redact(&err, password))?;
    let mut link = snapshot();
    if link.ssid.is_empty() {
        link.ssid = ssid.to_string();
    }
    Ok(link)
}

pub fn disconnect(iface: &str) -> Result<(), String> {
    let iface = if iface.is_empty() {
        wifi_iface()?
    } else {
        iface.to_string()
    };
    // device disconnect also stops the profile from coming straight back.
    run("nmcli", &["device", "disconnect", &iface]).map(|_| ())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiLink {
    pub iface: String,
    pub ssid: String,
    pub ip: String,
}

impl WifiLink {
    pub fn connected(&self) -> bool {
        !self.ssid.is_empty()
    }
}

pub fn snapshot() -> WifiLink {
    let iface = wifi_iface().unwrap_or_default();
    if iface.is_empty() {
        return WifiLink {
            iface: String::new(),
            ssid: String::new(),
            ip: String::new(),
        };
    }
    let addr = Command::new("ip")
        .args(["-4", "-br", "addr", "show", "dev", &iface])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default();
    let ip = addr
        .split_whitespace()
        .find(|field| field.contains('.'))
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    WifiLink {
        ssid: active_ssid(&iface),
        iface,
        ip,
    }
}

pub fn wifi_iface() -> Result<String, String> {
    let output = run(
        "nmcli",
        &["-t", "-f", "DEVICE,TYPE,STATE", "device", "status"],
    )?;
    let mut found = String::new();
    for line in output.lines() {
        let fields = split_nmcli(line);
        if fields.len() >= 3
            && fields[1] == "wifi"
            && fields[2] != "unavailable"
            && !fields[0].starts_with("p2p")
        {
            found = fields[0].clone();
            break;
        }
    }
    if found.is_empty() {
        Err("No Wi-Fi device.".to_string())
    } else {
        Ok(found)
    }
}

fn wait_iface() -> Result<String, String> {
    let mut last = "No Wi-Fi device.".to_string();
    for _ in 0..16 {
        match wifi_iface() {
            Ok(iface) => return Ok(iface),
            Err(err) => last = err,
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(last)
}

fn ensure_driver() -> Result<(), String> {
    let roamoff = fs::read_to_string("/sys/module/brcmfmac/parameters/roamoff")
        .ok()
        .map(|text| text.trim().to_string());
    if driver_needs_reload(roamoff.as_deref()) {
        // Radio off first. brcmfmac_wcc holds the module, and a busy
        // rmmod must not be treated as a successful reload.
        let _ = run("nmcli", &["radio", "wifi", "off"]);
        std::thread::sleep(std::time::Duration::from_secs(1));
        let _ = run("modprobe", &["-r", "brcmfmac_wcc"]);
        let _ = run("modprobe", &["-r", "brcmfmac"]);
        run(
            "modprobe",
            &[
                "brcmfmac",
                "roamoff=1",
                &format!("feature_disable={FEATURE_DISABLE}"),
            ],
        )?;
        let now = fs::read_to_string("/sys/module/brcmfmac/parameters/roamoff").unwrap_or_default();
        if now.trim() != "1" {
            return Err("brcmfmac roamoff did not stick.".to_string());
        }
    }
    run("nmcli", &["radio", "wifi", "on"])?;
    run("iw", &["reg", "set", "NL"])?;
    Ok(())
}

fn active_ssid(iface: &str) -> String {
    let Ok(output) = run("iw", &["dev", iface, "link"]) else {
        return String::new();
    };
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("SSID:") {
            return rest.trim().to_string();
        }
    }
    String::new()
}

fn run(cmd: &str, args: &[impl AsRef<str>]) -> Result<String, String> {
    let output = Command::new(cmd)
        .args(args.iter().map(AsRef::as_ref))
        .output()
        .map_err(|err| format!("{cmd} failed: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        let out = String::from_utf8_lossy(&output.stdout);
        let text = if err.trim().is_empty() { out } else { err };
        let line = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .last()
            .unwrap_or("command failed");
        Err(format!("{cmd}: {line}"))
    }
}

fn redact(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, "••••")
    }
}

fn split_nmcli(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                current.push(next);
            }
        } else if ch == ':' {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    fields.push(current);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_unescapes_dedupes_and_sorts() {
        let text = "\
 :Ziggo:40:WPA2
*:S\\&L:74:WPA2
 :S\\&L:20:WPA2
 ::30:WPA2
 ::10:WPA2
 :foo\\:bar:50:WPA2
";
        let nets = parse_wifi_list(text);
        assert_eq!(nets[0].ssid, "S&L");
        assert!(nets[0].in_use);
        assert_eq!(nets[0].signal, 74);
        assert!(nets.iter().any(|net| net.ssid == "foo:bar"));
        assert_eq!(nets.iter().filter(|net| net.hidden()).count(), 1);
    }

    #[test]
    fn join_plan_stores_the_psk_and_does_not_use_wps() {
        let plan = plan_join("wlp3s0", "S&L", "secret!", true);
        let add = plan.add.join(" ");
        assert!(add.contains("ssid S&L"));
        assert!(add.contains("wifi-sec.psk secret!"));
        assert!(add.contains("wifi-sec.wps-method 0"));
        assert!(!add.contains("device wifi connect"));
        assert!(plan.modify.iter().any(|arg| arg == "yes"));
        assert_eq!(plan.up.last().map(String::as_str), Some("omaclone-wifi"));
    }

    #[test]
    fn open_network_has_no_psk_arguments() {
        let plan = plan_join("wlp3s0", "cafe", "", false);
        assert!(!plan.add.iter().any(|arg| arg.contains("psk")));
        assert!(!plan.modify.iter().any(|arg| arg == "yes"));
    }

    #[test]
    fn reload_only_when_roamoff_is_missing() {
        assert!(driver_needs_reload(None));
        assert!(driver_needs_reload(Some("0")));
        assert!(!driver_needs_reload(Some("1")));
    }
}
