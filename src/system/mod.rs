//! Machine statistics read from `/proc` and `/sys`: CPU, memory, disks, temperatures,
//! batteries, network traffic, graphics cards and host information.

mod backlight;
mod battery;
mod cpu;
mod disk;
mod gpu;
mod hardware;
mod host;
mod memory;
mod network;
mod thermal;

pub use backlight::{Backlight, backlight};
pub use battery::{Battery, BatteryState, batteries};
pub use cpu::CpuSampler;
pub use disk::{DiskUsage, disk_usage};
pub use gpu::{GpuSample, GpuSampler};
pub use hardware::{GraphicsCard, Processor, graphics_cards, packages, processor};
pub use host::{HostInfo, host_info, uptime};
pub use memory::{MemoryInfo, memory};
pub use network::{NetworkSampler, NetworkTraffic};
pub use thermal::{Temperature, headline as headline_temperature, temperatures};
