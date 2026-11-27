//! Graphics load, video memory, temperature, power and clock: NVML for NVIDIA cards, amdgpu's
//! sysfs files for AMD ones. Neither asks a card that runtime power management put to sleep,
//! since asking would wake it.

use std::ffi::{c_char, c_int, c_uint, c_void};
use std::fs;
use std::path::{Path, PathBuf};

use super::hardware::GraphicsCard;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuSample {
    /// Utilisation in `0.0..=1.0`.
    pub load: f32,
    /// Video memory in bytes.
    pub memory_used: u64,
    pub memory_total: u64,
    /// °C; 0 when unknown.
    pub temperature: f32,
    /// Watts; 0 when unknown.
    pub power: f32,
    /// The graphics clock in MHz; 0 when unknown.
    pub clock: f32,
}

impl GpuSample {
    pub fn memory_usage(&self) -> f32 {
        if self.memory_total == 0 {
            0.0
        } else {
            self.memory_used as f32 / self.memory_total as f32
        }
    }
}

fn read_number(path: impl AsRef<Path>) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn asleep(device: &Path) -> bool {
    fs::read_to_string(device.join("power/runtime_status")).is_ok_and(|s| s.trim() == "suspended")
}

/// The first `hwmon*` directory of a device.
fn hwmon(device: &Path) -> Option<PathBuf> {
    fs::read_dir(device.join("hwmon")).ok()?.flatten().map(|e| e.path()).next()
}

fn sample_amd(device: &Path) -> GpuSample {
    let hwmon = hwmon(device);
    let sensor = |name: &str| hwmon.as_ref().and_then(|h| read_number(h.join(name)));
    // pp_dpm_sclk marks the current level with "*": "1: 2100Mhz *".
    let clock = fs::read_to_string(device.join("pp_dpm_sclk"))
        .ok()
        .and_then(|levels| {
            levels.lines().find(|l| l.ends_with('*')).and_then(|l| {
                l.split_whitespace()
                    .nth(1)?
                    .trim_end_matches(|c: char| c.is_alphabetic())
                    .parse()
                    .ok()
            })
        })
        .unwrap_or(0.0);
    GpuSample {
        load: read_number(device.join("gpu_busy_percent")).unwrap_or(0) as f32 / 100.0,
        memory_used: read_number(device.join("mem_info_vram_used")).unwrap_or(0),
        memory_total: read_number(device.join("mem_info_vram_total")).unwrap_or(0),
        temperature: sensor("temp1_input").unwrap_or(0) as f32 / 1000.0,
        power: sensor("power1_average").or_else(|| sensor("power1_input")).unwrap_or(0) as f32
            / 1_000_000.0,
        clock,
    }
}

type Device = *mut c_void;

#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(C)]
#[derive(Default)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

/// The few NVML functions LunaDash needs, from the driver's libnvidia-ml.
struct Nvml {
    device: Device,
    utilization: unsafe extern "C" fn(Device, *mut Utilization) -> c_int,
    memory: unsafe extern "C" fn(Device, *mut Memory) -> c_int,
    temperature: unsafe extern "C" fn(Device, c_int, *mut c_uint) -> c_int,
    power: unsafe extern "C" fn(Device, *mut c_uint) -> c_int,
    clock: unsafe extern "C" fn(Device, c_int, *mut c_uint) -> c_int,
}

// SAFETY: NVML is thread-safe, and the sampler is used by one thread at a time.
unsafe impl Send for Nvml {}

/// Looks a symbol up as a function of type `F`.
///
/// # Safety
/// `F` must be the function's real signature.
unsafe fn symbol<F: Copy>(library: *mut c_void, name: &[u8]) -> Option<F> {
    // SAFETY: `name` is NUL-terminated and `library` came from dlopen.
    let pointer = unsafe { libc::dlsym(library, name.as_ptr() as *const c_char) };
    // SAFETY: the caller guarantees the signature; F is a function pointer of pointer size.
    (!pointer.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, F>(&pointer) })
}

impl Nvml {
    /// Initialises NVML and finds the card at PCI address `bus_id` ("0000:03:00.0").
    fn load(bus_id: &str) -> Option<Self> {
        // SAFETY: a plain dlopen of a system library; it stays loaded for the process.
        let library = unsafe {
            libc::dlopen(c"libnvidia-ml.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL)
        };
        if library.is_null() {
            return None;
        }
        // SAFETY: the signatures are NVML's (nvml.h).
        unsafe {
            let init: unsafe extern "C" fn() -> c_int = symbol(library, b"nvmlInit_v2\0")?;
            let by_bus: unsafe extern "C" fn(*const c_char, *mut Device) -> c_int =
                symbol(library, b"nvmlDeviceGetHandleByPciBusId_v2\0")?;
            if init() != 0 {
                return None;
            }
            let bus = std::ffi::CString::new(bus_id).ok()?;
            let mut device: Device = std::ptr::null_mut();
            if by_bus(bus.as_ptr(), &mut device) != 0 {
                return None;
            }
            Some(Self {
                device,
                utilization: symbol(library, b"nvmlDeviceGetUtilizationRates\0")?,
                memory: symbol(library, b"nvmlDeviceGetMemoryInfo\0")?,
                temperature: symbol(library, b"nvmlDeviceGetTemperature\0")?,
                power: symbol(library, b"nvmlDeviceGetPowerUsage\0")?,
                clock: symbol(library, b"nvmlDeviceGetClockInfo\0")?,
            })
        }
    }

    fn sample(&self) -> GpuSample {
        let mut utilization = Utilization::default();
        let mut memory = Memory::default();
        let (mut temperature, mut power, mut clock) = (0, 0, 0);
        // SAFETY: valid out-pointers for a handle NVML gave us; 0 is NVML_TEMPERATURE_GPU and
        // NVML_CLOCK_GRAPHICS.
        unsafe {
            (self.utilization)(self.device, &mut utilization);
            (self.memory)(self.device, &mut memory);
            (self.temperature)(self.device, 0, &mut temperature);
            (self.power)(self.device, &mut power);
            (self.clock)(self.device, 0, &mut clock);
        }
        GpuSample {
            load: utilization.gpu.min(100) as f32 / 100.0,
            memory_used: memory.used,
            memory_total: memory.total,
            temperature: temperature as f32,
            power: power as f32 / 1000.0,
            clock: clock as f32,
        }
    }
}

enum Backend {
    /// NVML is loaded the first time the card is awake.
    Nvidia(Option<Nvml>),
    Amd,
    /// NVML could not be loaded.
    Unavailable,
}

/// Samples the first card that can report its load. Loading NVML takes a moment, so use it
/// off the UI thread.
pub struct GpuSampler {
    device: PathBuf,
    backend: Backend,
}

impl GpuSampler {
    pub fn new(cards: &[GraphicsCard]) -> Option<Self> {
        cards.iter().find_map(|card| {
            let backend = if card.is_nvidia() {
                Backend::Nvidia(None)
            } else if card.device.join("gpu_busy_percent").exists() {
                Backend::Amd
            } else {
                return None;
            };
            Some(Self { device: card.device.clone(), backend })
        })
    }

    /// False once it turned out that the card cannot be asked (NVML is missing).
    pub fn available(&self) -> bool {
        !matches!(self.backend, Backend::Unavailable)
    }

    /// The card's state, or `None` while it sleeps (or NVML is missing).
    pub fn sample(&mut self) -> Option<GpuSample> {
        if asleep(&self.device) {
            return None;
        }
        if let Backend::Nvidia(None) = self.backend {
            let bus = self.device.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            self.backend =
                Nvml::load(bus).map_or(Backend::Unavailable, |n| Backend::Nvidia(Some(n)));
        }
        match &self.backend {
            Backend::Amd => Some(sample_amd(&self.device)),
            Backend::Nvidia(nvml) => nvml.as_ref().map(Nvml::sample),
            Backend::Unavailable => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_amdgpu_files() {
        let dir = tempfile::tempdir().unwrap();
        let device = dir.path();
        fs::write(device.join("gpu_busy_percent"), "42\n").unwrap();
        fs::write(device.join("mem_info_vram_used"), "1024\n").unwrap();
        fs::write(device.join("mem_info_vram_total"), "4096\n").unwrap();
        fs::write(device.join("pp_dpm_sclk"), "0: 500Mhz\n1: 2100Mhz *\n").unwrap();
        fs::create_dir_all(device.join("hwmon/hwmon3")).unwrap();
        fs::write(device.join("hwmon/hwmon3/temp1_input"), "61000\n").unwrap();
        fs::write(device.join("hwmon/hwmon3/power1_average"), "35000000\n").unwrap();
        let sample = sample_amd(device);
        assert_eq!(sample.load, 0.42);
        assert_eq!(sample.memory_usage(), 0.25);
        assert_eq!((sample.temperature, sample.power, sample.clock), (61.0, 35.0, 2100.0));
    }

    #[test]
    fn sleeping_cards_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("power")).unwrap();
        fs::write(dir.path().join("power/runtime_status"), "suspended\n").unwrap();
        fs::write(dir.path().join("gpu_busy_percent"), "42\n").unwrap();
        let card = GraphicsCard {
            name: "AMD".into(),
            driver: "amdgpu".into(),
            device: dir.path().to_path_buf(),
        };
        assert_eq!(GpuSampler::new(&[card]).unwrap().sample(), None);
    }
}
