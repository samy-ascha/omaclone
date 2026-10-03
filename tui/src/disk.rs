//! Read the internal SSD and the Thunderbolt link. USB sticks are not clone disks.

use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disk {
    pub name: String,
    pub model: String,
    pub serial: String,
    pub bytes: u64,
    pub parts: Vec<Part>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usb {
    pub name: String,
    pub model: String,
    pub bytes: u64,
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub clx: Option<String>,
    pub e2e: Option<String>,
    pub addr: Option<String>,
    pub oper: Option<String>,
    /// Dropped packets plus receive and transmit errors on thunderbolt0.
    pub drops: Option<u64>,
}

impl Link {
    pub fn offline() -> Self {
        Self {
            clx: None,
            e2e: None,
            addr: None,
            oper: None,
            drops: None,
        }
    }

    /// Address, link up, and both packet-dropping options off.
    pub fn passes_frames(&self) -> bool {
        self.addr.as_ref().is_some_and(|addr| !addr.is_empty())
            && Self::flag_off(self.clx.as_deref())
            && Self::flag_off(self.e2e.as_deref())
            && self.oper.as_deref() == Some("up")
    }

    /// Why a copy must not start. None when `passes_frames` is true.
    pub fn copy_block(&self) -> Option<&'static str> {
        if self.passes_frames() {
            None
        } else if self.clx.is_some() && !Self::flag_off(self.clx.as_deref())
            || self.e2e.is_some() && !Self::flag_off(self.e2e.as_deref())
        {
            Some("traffic will not pass. Press r on both laptops.")
        } else {
            Some("Thunderbolt is not ready.")
        }
    }

    fn flag_off(value: Option<&str>) -> bool {
        matches!(value, Some("N" | "n" | "0"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inventory {
    pub product: String,
    pub disk: Result<Disk, String>,
    pub usb: Vec<Usb>,
    pub link: Link,
}

pub fn human_bytes(bytes: u64) -> String {
    let gib = bytes as f64 / 1024.0 / 1024.0 / 1024.0;
    format!("{gib:.1} GiB")
}

pub fn load_inventory() -> Inventory {
    let mut inventory = read_sysfs(Path::new("/sys"), Path::new("/dev/disk/by-label"));
    if let Ok(disk) = inventory.disk.as_mut() {
        if let Some(model) = lsblk_model(&disk.name) {
            if !model.is_empty() {
                disk.model = model;
            }
        }
    }
    inventory.link = read_link();
    inventory.product = read_product();
    inventory
}

pub fn read_sysfs(sys: &Path, by_label: &Path) -> Inventory {
    let block = sys.join("block");
    let mut internal = Vec::new();
    let mut usb = Vec::new();
    let entries = match fs::read_dir(&block) {
        Ok(entries) => entries,
        Err(err) => {
            return Inventory {
                product: String::new(),
                disk: Err(format!("cannot read {}: {err}", block.display())),
                usb,
                link: Link::offline(),
            };
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if skip_name(&name) {
            continue;
        }
        let bytes = read_sectors(&path.join("size")).saturating_mul(512);
        if bytes == 0 {
            continue;
        }
        let model = read_trim(&path.join("device/model")).unwrap_or_default();
        // Kernel 7 exposes device/serial. SystemRescue 6.18 leaves that file
        // empty and puts the SCSI serial in VPD page 0x80.
        let serial = read_trim(&path.join("device/serial"))
            .or_else(|| read_vpd80(&path.join("device/vpd_pg80")))
            .unwrap_or_default();
        let removable = read_trim(&path.join("removable")).unwrap_or_default() == "1";
        if removable {
            usb.push(Usb {
                name,
                model,
                bytes,
                label: None,
            });
            continue;
        }
        if serial.is_empty() {
            continue;
        }
        internal.push(Disk {
            name,
            model,
            serial,
            bytes,
            parts: read_parts(&path),
        });
    }
    attach_labels(&mut usb, by_label);
    usb.sort_by(|a, b| a.name.cmp(&b.name));
    internal.sort_by(|a, b| a.name.cmp(&b.name));
    let disk = match internal.len() {
        0 => Err("No internal SSD with a serial was found.".to_string()),
        1 => Ok(internal.remove(0)),
        n => Err(format!(
            "{n} internal disks have serials. Refusing to guess.\n{}",
            internal
                .iter()
                .map(|disk| format!("{}  {}  {}", disk.name, disk.model, disk.serial))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    };
    Inventory {
        product: String::new(),
        disk,
        usb,
        link: Link::offline(),
    }
}

pub fn read_link() -> Link {
    Link {
        clx: read_trim(Path::new("/sys/module/thunderbolt/parameters/clx")),
        e2e: read_trim(Path::new("/sys/module/thunderbolt_net/parameters/e2e")),
        oper: read_trim(Path::new("/sys/class/net/thunderbolt0/operstate")),
        addr: thunderbolt_addr(),
        drops: iface_drops(),
    }
}

fn iface_drops() -> Option<u64> {
    let stats = Path::new("/sys/class/net/thunderbolt0/statistics");
    let rx = read_counter(&stats.join("rx_dropped"))?;
    let tx = read_counter(&stats.join("tx_dropped"))?;
    let rx_err = read_counter(&stats.join("rx_errors")).unwrap_or(0);
    let tx_err = read_counter(&stats.join("tx_errors")).unwrap_or(0);
    Some(
        rx.saturating_add(tx)
            .saturating_add(rx_err)
            .saturating_add(tx_err),
    )
}

fn read_counter(path: &Path) -> Option<u64> {
    read_trim(path)?.parse().ok()
}

/// How long a new drop stays on the cable screen.
pub const DROP_KEEP: std::time::Duration = std::time::Duration::from_secs(15);

/// Turns the kernel's lifetime counter into drops that age out.
#[derive(Clone, Debug)]
pub struct DropWatch {
    last: Option<u64>,
    hits: Vec<(std::time::Instant, u64)>,
}

impl DropWatch {
    pub fn new() -> Self {
        Self {
            last: None,
            hits: Vec::new(),
        }
    }

    /// `None` when the interface is missing. The first sample is a baseline, so
    /// drops that already happened show as zero.
    pub fn observe(&mut self, counter: Option<u64>, now: std::time::Instant) -> Option<u64> {
        let Some(counter) = counter else {
            self.last = None;
            self.hits.clear();
            return None;
        };
        if let Some(last) = self.last {
            if counter > last {
                self.hits.push((now, counter - last));
            }
        }
        self.last = Some(counter);
        self.hits
            .retain(|(at, _)| now.saturating_duration_since(*at) < DROP_KEEP);
        Some(self.hits.iter().map(|(_, count)| *count).sum())
    }
}

/// Reload Falcon Ridge with the parameters that actually pass frames.
/// The other laptop has to do this too. A one-sided reload drops the handshake.
pub fn reload_thunderbolt() -> Result<(), String> {
    let _ = modprobe(&["-r", "thunderbolt_net"]);
    if Path::new("/sys/module/thunderbolt").exists() {
        modprobe(&["-r", "thunderbolt"])?;
    }
    if Path::new("/sys/module/thunderbolt").exists() {
        return Err("thunderbolt is still loaded".to_string());
    }
    std::thread::sleep(std::time::Duration::from_secs(1));
    modprobe(&["thunderbolt", "clx=0"])?;
    modprobe(&["thunderbolt_net", "e2e=0"])?;
    let link = read_link();
    let clx_off = matches!(link.clx.as_deref(), Some("N" | "n" | "0"));
    let e2e_off = matches!(link.e2e.as_deref(), Some("N" | "n" | "0"));
    if clx_off && e2e_off {
        Ok(())
    } else {
        Err(format!(
            "clx is {}, e2e is {}",
            link.clx.as_deref().unwrap_or("missing"),
            link.e2e.as_deref().unwrap_or("missing")
        ))
    }
}

fn modprobe(args: &[&str]) -> Result<(), String> {
    let output = Command::new("modprobe")
        .args(args)
        .output()
        .map_err(|err| format!("modprobe failed: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        let line = err.lines().map(str::trim).find(|line| !line.is_empty());
        Err(line.unwrap_or("modprobe failed").to_string())
    }
}

pub fn parse_lsblk_models(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let name = quoted_field(line, "NAME")?;
            let model = quoted_field(line, "MODEL")?;
            Some((name, model))
        })
        .collect()
}

fn lsblk_model(name: &str) -> Option<String> {
    let output = Command::new("lsblk")
        .args(["-dn", "-P", "-o", "NAME,MODEL"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_lsblk_models(&text)
        .into_iter()
        .find(|(disk, _)| disk == name)
        .map(|(_, model)| model)
}

fn quoted_field(line: &str, key: &str) -> Option<String> {
    let marker = format!("{key}=\"");
    let start = line.find(&marker)? + marker.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn attach_labels(usb: &mut [Usb], by_label: &Path) {
    let Ok(entries) = fs::read_dir(by_label) else {
        return;
    };
    for entry in entries.flatten() {
        let label = entry.file_name().to_string_lossy().to_string();
        let Ok(target) = fs::read_link(entry.path()) else {
            continue;
        };
        let dev = target
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let disk = parent_disk(&dev);
        if let Some(usb) = usb.iter_mut().find(|usb| usb.name == disk) {
            usb.label = Some(label);
        }
    }
}

fn parent_disk(name: &str) -> String {
    if let Some(index) = name.rfind('p')
        && name.starts_with("nvme")
        && name[index + 1..].chars().all(|ch| ch.is_ascii_digit())
        && index > 0
    {
        return name[..index].to_string();
    }
    name.trim_end_matches(|ch: char| ch.is_ascii_digit())
        .to_string()
}

fn read_parts(disk: &Path) -> Vec<Part> {
    let mut parts = Vec::new();
    let Ok(entries) = fs::read_dir(disk) else {
        return parts;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.join("partition").exists() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        parts.push(Part {
            name,
            bytes: read_sectors(&path.join("size")).saturating_mul(512),
        });
    }
    parts.sort_by(|a, b| a.name.cmp(&b.name));
    parts
}

fn read_sectors(path: &Path) -> u64 {
    read_trim(path)
        .and_then(|text| text.parse().ok())
        .unwrap_or(0)
}

fn read_product() -> String {
    read_trim(Path::new("/sys/class/dmi/id/product_name"))
        .unwrap_or_else(|| "unknown board".to_string())
}

fn read_vpd80(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() < 4 || bytes[1] != 0x80 {
        return None;
    }
    let len = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
    let end = 4usize.checked_add(len)?;
    let data = bytes.get(4..end)?;
    let text = String::from_utf8_lossy(data).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn read_trim(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn thunderbolt_addr() -> Option<String> {
    let output = Command::new("ip")
        .args(["-4", "-br", "addr", "show", "dev", "thunderbolt0"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .find(|field| field.contains('.'))
        .map(|field| field.split('/').next().unwrap_or(field).to_string())
}

fn skip_name(name: &str) -> bool {
    name.starts_with("loop")
        || name.starts_with("ram")
        || name.starts_with("zram")
        || name.starts_with("dm-")
        || name.starts_with("sr")
        || name.starts_with("nbd")
        || name.starts_with("fd")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn touch(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn internal_serial_is_kept_and_usb_is_not() {
        let root = std::env::temp_dir().join(format!("omaclone-sys-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let sys = root.join("sys");
        let sda = sys.join("block/sda");
        touch(&sda.join("size"), "490234752\n");
        touch(&sda.join("removable"), "0\n");
        touch(&sda.join("device/model"), "APPLE SSD SM0256\n");
        fs::write(
            sda.join("device/vpd_pg80"),
            b"\x00\x80\x00\x14S2Z5NY0H998813      ",
        )
        .unwrap();
        touch(&sda.join("sda1/partition"), "1\n");
        touch(&sda.join("sda1/size"), "4194304\n");
        let sdb = sys.join("block/sdb");
        touch(&sdb.join("size"), "122880000\n");
        touch(&sdb.join("removable"), "1\n");
        touch(&sdb.join("device/model"), "PHILIPS USB\n");
        touch(&sdb.join("device/serial"), "usbserial\n");
        touch(&sys.join("block/zram0/size"), "100\n");
        touch(&sys.join("block/zram0/removable"), "0\n");
        touch(&sys.join("block/dm-0/size"), "100\n");
        touch(&sys.join("block/dm-0/removable"), "0\n");
        touch(&sys.join("block/dm-0/device/serial"), "mapper\n");
        let labels = root.join("by-label");
        fs::create_dir_all(&labels).unwrap();
        symlink("../../sdb1", labels.join("RESCUE1302")).unwrap();

        let inventory = read_sysfs(&sys, &labels);
        let disk = inventory.disk.unwrap();
        assert_eq!(disk.serial, "S2Z5NY0H998813");
        assert_eq!(disk.bytes, 490234752 * 512);
        assert_eq!(disk.parts.len(), 1);
        assert_eq!(inventory.usb.len(), 1);
        assert_eq!(inventory.usb[0].label.as_deref(), Some("RESCUE1302"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn two_internal_disks_are_an_error() {
        let root = std::env::temp_dir().join(format!("omaclone-sys2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let sys = root.join("sys");
        for (name, serial) in [("sda", "ONE"), ("sdb", "TWO")] {
            let disk = sys.join("block").join(name);
            touch(&disk.join("size"), "100\n");
            touch(&disk.join("removable"), "0\n");
            touch(&disk.join("device/model"), "SSD\n");
            touch(&disk.join("device/serial"), serial);
        }
        let inventory = read_sysfs(&sys, &root.join("missing"));
        assert!(inventory.disk.is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn lsblk_model_quotes() {
        let rows = parse_lsblk_models("NAME=\"sda\" MODEL=\"APPLE SSD SM0256G\"\n");
        assert_eq!(
            rows,
            vec![("sda".to_string(), "APPLE SSD SM0256G".to_string())]
        );
    }

    #[test]
    fn nvme_partition_maps_to_the_disk() {
        assert_eq!(parent_disk("nvme0n1p1"), "nvme0n1");
        assert_eq!(parent_disk("sda1"), "sda");
    }

    #[test]
    fn earlier_drops_expire() {
        let mut watch = DropWatch::new();
        let start = std::time::Instant::now();
        assert_eq!(watch.observe(Some(6), start), Some(0));
        assert_eq!(watch.observe(Some(8), start), Some(2));
        assert_eq!(watch.observe(Some(8), start + DROP_KEEP), Some(0));
        assert_eq!(watch.observe(None, start + DROP_KEEP), None);
    }
}
