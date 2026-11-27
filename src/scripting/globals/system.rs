//! The `System` and `Battery` globals, polled every two seconds; the graphics card is asked on
//! a thread of its own, since its driver can take a while to answer.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use slint::{Timer, TimerMode};
use slint_interpreter::Value;
use slintlu::system::{self, BatteryState, CpuSampler, GpuSample, GpuSampler, NetworkSampler};
use slintlu::util::format;

use crate::scripting::bridge::{list_value, string, struct_value};
use crate::scripting::{ShellHost, host};

const INTERVAL: Duration = Duration::from_secs(2);
/// One minute of samples.
const HISTORY: usize = 30;

/// The latest samples, oldest first.
struct History(VecDeque<f64>);

impl Default for History {
    fn default() -> Self {
        Self(std::iter::repeat_n(0.0, HISTORY).collect())
    }
}

impl History {
    fn push(&mut self, value: f64) {
        self.0.pop_front();
        self.0.push_back(value);
    }

    fn peak(&self) -> f64 {
        self.0.iter().copied().fold(0.0, f64::max)
    }

    /// The samples as fractions of `scale`.
    fn value(&self, scale: f64) -> Value {
        list_value(self.0.iter().map(|v| number(if scale > 0.0 { v / scale } else { 0.0 })))
    }
}

#[derive(Default)]
struct Samplers {
    cpu: CpuSampler,
    network: NetworkSampler,
    cpu_history: History,
    memory_history: History,
    download_history: History,
    upload_history: History,
}

fn number(value: impl Into<f64>) -> Value {
    Value::Number(value.into())
}

fn poll(host: &ShellHost, samplers: &mut Samplers) {
    let (cpu, cores) = samplers.cpu.sample();
    host.set_global("System", "cpu", number(cpu));
    host.set_global("System", "cpu-cores", list_value(cores.into_iter().map(number)));
    samplers.cpu_history.push(cpu.into());
    host.set_global("System", "cpu-history", samplers.cpu_history.value(1.0));

    let memory = system::memory();
    host.set_global("System", "memory", number(memory.usage()));
    samplers.memory_history.push(memory.usage().into());
    host.set_global("System", "memory-history", samplers.memory_history.value(1.0));
    host.set_global("System", "swap", number(memory.swap_usage()));
    host.set_global(
        "System",
        "memory-text",
        string(format!("{} / {}", format::bytes(memory.used()), format::bytes(memory.total))),
    );

    if let Some(disk) = system::disk_usage(Path::new("/")) {
        host.set_global("System", "disk", number(disk.usage()));
        host.set_global(
            "System",
            "disk-text",
            string(format!("{} free", format::bytes(disk.available))),
        );
    }

    let temperatures = system::temperatures();
    let hottest = system::headline_temperature(&temperatures);
    host.set_global("System", "temperature", number(hottest));
    host.set_global(
        "System",
        "temperatures",
        list_value(
            temperatures.iter().map(|t| {
                struct_value([("label", string(&t.label)), ("celsius", number(t.celsius))])
            }),
        ),
    );

    let traffic = samplers.network.sample();
    host.set_global("System", "download", number(traffic.download));
    host.set_global("System", "upload", number(traffic.upload));
    host.set_global("System", "download-text", string(format::rate(traffic.download)));
    host.set_global("System", "upload-text", string(format::rate(traffic.upload)));
    samplers.download_history.push(traffic.download);
    samplers.upload_history.push(traffic.upload);
    let peak = samplers.download_history.peak().max(samplers.upload_history.peak());
    host.set_global("System", "download-history", samplers.download_history.value(peak));
    host.set_global("System", "upload-history", samplers.upload_history.value(peak));
    host.set_global("System", "network-peak", number(peak));
    host.set_global("System", "network-peak-text", string(format::rate(peak)));
    host.set_global("System", "uptime", string(format::duration(system::uptime())));

    let battery = system::batteries().into_iter().next();
    host.set_global("Battery", "available", Value::Bool(battery.is_some()));
    if let Some(battery) = battery {
        host.set_global("Battery", "level", number(battery.level));
        host.set_global(
            "Battery",
            "charging",
            Value::Bool(battery.state == BatteryState::Charging),
        );
        host.set_global("Battery", "state", string(battery.state.as_str()));
        host.set_global(
            "Battery",
            "time-remaining",
            string(
                battery
                    .time_remaining
                    .map(|s| format::duration(Duration::from_secs(s)))
                    .unwrap_or_default(),
            ),
        );
    }
}

fn publish_gpu(host: &ShellHost, sample: Option<GpuSample>, history: &History) {
    let sample = sample.as_ref();
    host.set_global("System", "gpu-available", Value::Bool(sample.is_some()));
    let sample = sample.copied().unwrap_or_default();
    host.set_global("System", "gpu", number(sample.load));
    host.set_global("System", "gpu-memory", number(sample.memory_usage()));
    host.set_global(
        "System",
        "gpu-memory-text",
        string(if sample.memory_total == 0 {
            String::new()
        } else {
            format!(
                "{} / {}",
                format::bytes(sample.memory_used),
                format::bytes(sample.memory_total)
            )
        }),
    );
    host.set_global("System", "gpu-temperature", number(sample.temperature));
    host.set_global("System", "gpu-power", number(sample.power));
    host.set_global("System", "gpu-clock", number(sample.clock));
    host.set_global("System", "gpu-history", history.value(1.0));
}

/// Asks the first card that can tell its load every two seconds, until it turns out it cannot.
fn spawn_gpu_sampler(cards: &[system::GraphicsCard]) {
    let Some(mut sampler) = GpuSampler::new(cards) else { return };
    let spawned = std::thread::Builder::new().name("slintlu-gpu".into()).spawn(move || {
        let mut history = History::default();
        loop {
            let sample = sampler.sample();
            history.push(sample.map_or(0.0, |s| s.load.into()));
            let published = History(history.0.clone());
            host::post(move |host| publish_gpu(host, sample, &published));
            if !sampler.available() {
                break;
            }
            std::thread::sleep(INTERVAL);
        }
    });
    if let Err(e) = spawned {
        log::warn!("cannot start the graphics card thread: {e}");
    }
}

fn publish_hardware(host: &ShellHost) {
    let processor = system::processor();
    host.set_global("System", "processor", string(&processor.model));
    host.set_global("System", "processor-cores", number(processor.cores as f64));
    host.set_global("System", "processor-threads", number(processor.threads as f64));
    host.set_global(
        "System",
        "processor-speed",
        string(if processor.max_mhz == 0 {
            String::new()
        } else {
            format!("{:.1} GHz", f64::from(processor.max_mhz) / 1000.0)
        }),
    );
    host.set_global("System", "architecture", string(std::env::consts::ARCH));
    host.set_global("System", "memory-total", string(format::bytes(system::memory().total)));
    let cards = system::graphics_cards();
    host.set_global("System", "gpus", list_value(cards.iter().map(|c| string(&c.name))));
    host.set_global("System", "gpu-drivers", list_value(cards.iter().map(|c| string(&c.driver))));
    spawn_gpu_sampler(&cards);
    // Counting packages reads a directory per package: not on the UI thread.
    std::thread::spawn(|| {
        let packages = system::packages();
        host::post(move |host| host.set_global("System", "packages", string(packages)));
    });
}

pub fn install(host: &Rc<ShellHost>) {
    let info = system::host_info();
    host.set_global("System", "hostname", string(&info.hostname));
    host.set_global("System", "kernel", string(&info.kernel));
    host.set_global("System", "os", string(&info.os));
    publish_hardware(host);

    let samplers = RefCell::new(Samplers::default());
    poll(host, &mut samplers.borrow_mut());
    let weak = Rc::downgrade(host);
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, INTERVAL, move || {
        if let Some(host) = weak.upgrade() {
            poll(&host, &mut samplers.borrow_mut());
        }
    });
    // Polling lasts as long as the process.
    std::mem::forget(timer);
}
