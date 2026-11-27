//! What the machine is made of: the processor, the graphics cards and the installed packages.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Processor {
    /// e.g. "AMD Ryzen 7 5800X 8-Core Processor".
    pub model: String,
    pub cores: usize,
    pub threads: usize,
    /// The highest clock in MHz (0 when unknown).
    pub max_mhz: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphicsCard {
    /// e.g. "NVIDIA GeForce RTX 3050 6GB".
    pub name: String,
    /// The kernel driver, with its version where the driver reports one ("nvidia 620.70.06").
    pub driver: String,
    /// The PCI device directory in /sys.
    pub device: PathBuf,
}

impl GraphicsCard {
    pub fn is_nvidia(&self) -> bool {
        self.driver.starts_with("nvidia")
    }
}

fn read_trimmed(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).map(|s| s.trim().to_owned()).unwrap_or_default()
}

fn parse_cpuinfo(cpuinfo: &str) -> Processor {
    let mut model = String::new();
    let mut threads = 0;
    let mut cores = BTreeSet::new();
    let mut physical = String::new();
    for line in cpuinfo.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        match key.trim() {
            "processor" => threads += 1,
            "model name" | "Model" if model.is_empty() => model = value.to_owned(),
            "physical id" => physical = value.to_owned(),
            "core id" => {
                cores.insert((physical.clone(), value.to_owned()));
            }
            _ => {}
        }
    }
    Processor {
        model: tidy_model(&model),
        cores: if cores.is_empty() { threads } else { cores.len() },
        threads,
        max_mhz: 0,
    }
}

/// "Intel(R) Xeon(R) CPU E5-2680 v2 @ 2.80GHz" → "Intel Xeon E5-2680 v2", "AMD Ryzen 7 5800X
/// 8-Core Processor" → "AMD Ryzen 7 5800X": the marks, the base clock and the obvious go.
fn tidy_model(model: &str) -> String {
    let model = model.split(" @ ").next().unwrap_or_default();
    let model = model.replace("(R)", "").replace("(TM)", "").replace("(tm)", "");
    model
        .split_whitespace()
        .filter(|word| {
            !matches!(*word, "CPU" | "Processor")
                && !word.ends_with("-Core")
                && !word.ends_with("-core")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn processor() -> Processor {
    let mut processor = parse_cpuinfo(&fs::read_to_string("/proc/cpuinfo").unwrap_or_default());
    processor.max_mhz = read_trimmed("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")
        .parse::<u32>()
        .map_or(0, |khz| khz / 1000);
    processor
}

fn vendor_name(vendor: &str) -> Option<&'static str> {
    match vendor {
        "10de" => Some("NVIDIA"),
        "1002" => Some("AMD"),
        "8086" => Some("Intel"),
        _ => None,
    }
}

/// The vendor's and the device's names in a pci.ids database.
fn lookup_pci_ids(ids: &str, vendor: &str, device: &str) -> (Option<String>, Option<String>) {
    let mut vendor_label = None;
    for line in ids.lines() {
        if vendor_label.is_none() {
            if let Some(rest) = line.strip_prefix(vendor)
                && rest.starts_with("  ")
            {
                vendor_label = Some(rest.trim().to_owned());
            }
            continue;
        }
        if !line.starts_with('\t') && !line.starts_with('#') && !line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix('\t').and_then(|l| l.strip_prefix(device))
            && rest.starts_with("  ")
        {
            return (vendor_label, Some(rest.trim().to_owned()));
        }
    }
    (vendor_label, None)
}

/// "GA107 [GeForce RTX 3050 6GB]" → "GeForce RTX 3050 6GB": the marketing name.
fn marketing_name(device: &str) -> &str {
    match (device.find('['), device.rfind(']')) {
        (Some(open), Some(close)) if close > open => &device[open + 1..close],
        _ => device,
    }
}

fn card_name(device_dir: &Path, vendor: &str, device: &str) -> String {
    // The NVIDIA driver names its cards itself.
    let slot = device_dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let information = fs::read_to_string(format!("/proc/driver/nvidia/gpus/{slot}/information"))
        .unwrap_or_default();
    if let Some(model) = information.lines().find_map(|l| l.strip_prefix("Model:")) {
        return model.trim().to_owned();
    }
    let ids = ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids", "/usr/share/pci.ids"]
        .iter()
        .find_map(|p| fs::read_to_string(p).ok())
        .unwrap_or_default();
    let (vendor_label, device_label) = lookup_pci_ids(&ids, vendor, device);
    let vendor = vendor_name(vendor).map(str::to_owned).or(vendor_label).unwrap_or_default();
    match device_label {
        Some(label) => format!("{vendor} {}", marketing_name(&label)).trim().to_owned(),
        None => format!("{vendor} graphics").trim().to_owned(),
    }
}

fn driver(device_dir: &Path) -> String {
    let name = fs::read_link(device_dir.join("driver"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let version = if name == "nvidia" {
        nvidia_version(&read_trimmed("/proc/driver/nvidia/version"))
    } else {
        None
    };
    match version {
        Some(version) => format!("{name} {version}"),
        None => name,
    }
}

/// "NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  620.70.06  Release Build …" →
/// "620.70.06".
fn nvidia_version(text: &str) -> Option<String> {
    text.lines().next()?.split_whitespace().find_map(|word| {
        let version = word.contains('.') && word.chars().all(|c| c.is_ascii_digit() || c == '.');
        version.then(|| word.to_owned())
    })
}

/// Graphics cards with a DRM device, in card order.
pub fn graphics_cards() -> Vec<GraphicsCard> {
    let mut cards: Vec<(u32, PathBuf)> = fs::read_dir("/sys/class/drm")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let number = name.strip_prefix("card")?.parse::<u32>().ok()?;
            Some((number, fs::canonicalize(entry.path().join("device")).ok()?))
        })
        .collect();
    cards.sort();
    cards.dedup_by(|a, b| a.1 == b.1);
    cards
        .into_iter()
        .map(|(_, device)| {
            let vendor = read_trimmed(device.join("vendor")).trim_start_matches("0x").to_owned();
            let id = read_trimmed(device.join("device")).trim_start_matches("0x").to_owned();
            GraphicsCard { name: card_name(&device, &vendor, &id), driver: driver(&device), device }
        })
        .collect()
}

/// Installed packages and the package manager that counts them, e.g. "1532 (pacman)".
pub fn packages() -> String {
    if let Ok(entries) = fs::read_dir("/var/lib/pacman/local") {
        let count = entries.flatten().filter(|e| e.path().is_dir()).count();
        return format!("{count} (pacman)");
    }
    if let Ok(status) = fs::read_to_string("/var/lib/dpkg/status") {
        let count = status.lines().filter(|l| *l == "Status: install ok installed").count();
        return format!("{count} (dpkg)");
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_processor() {
        let info = parse_cpuinfo(
            "processor\t: 0\nmodel name\t: AMD  Ryzen 5\nphysical id\t: 0\ncore id\t: 0\n\n\
             processor\t: 1\nmodel name\t: AMD  Ryzen 5\nphysical id\t: 0\ncore id\t: 0\n\n\
             processor\t: 2\nmodel name\t: AMD  Ryzen 5\nphysical id\t: 0\ncore id\t: 1\n",
        );
        assert_eq!(info.model, "AMD Ryzen 5");
        assert_eq!(
            tidy_model("Intel(R) Xeon(R) CPU E5-2680 v2 @ 2.80GHz"),
            "Intel Xeon E5-2680 v2"
        );
        assert_eq!(tidy_model("AMD Ryzen 7 5800X 8-Core Processor"), "AMD Ryzen 7 5800X");
        assert_eq!(tidy_model("Intel(R) Core(TM) i7-1165G7 @ 2.80GHz"), "Intel Core i7-1165G7");
        assert_eq!((info.cores, info.threads), (2, 3));
    }

    #[test]
    fn names_cards_from_pci_ids() {
        let ids = "# comment\n10de  NVIDIA Corporation\n\t2584  GA107 [GeForce RTX 3050 6GB]\n\
                   \t2585  Other\n10df  Emulex\n\t2584  Not this one\n";
        assert_eq!(
            lookup_pci_ids(ids, "10de", "2584"),
            (Some("NVIDIA Corporation".into()), Some("GA107 [GeForce RTX 3050 6GB]".into()))
        );
        assert_eq!(lookup_pci_ids(ids, "10de", "9999").1, None);
        assert_eq!(marketing_name("GA107 [GeForce RTX 3050 6GB]"), "GeForce RTX 3050 6GB");
        assert_eq!(marketing_name("Iris Xe"), "Iris Xe");
    }

    #[test]
    fn reads_the_nvidia_driver_version() {
        let open =
            "NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  620.70.06  Release Build";
        let closed = "NVRM version: NVIDIA UNIX x86_64 Kernel Module  550.54.14  Thu Feb 22 01:44:30 UTC 2024";
        assert_eq!(nvidia_version(open).as_deref(), Some("620.70.06"));
        assert_eq!(nvidia_version(closed).as_deref(), Some("550.54.14"));
    }
}
