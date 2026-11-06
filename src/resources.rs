use crate::model::Record;
use serde::Serialize;
use std::{collections::HashMap, time::Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

pub type UsageMap = HashMap<String, Usage>;

#[derive(Clone, Serialize)]
pub struct Usage {
    pub cpu_percent: Option<f32>,
    pub memory_rss_bytes: Option<u64>,
    pub processes: usize,
    pub sample_interval_ms: Option<u64>,
}

pub struct Sampler {
    system: System,
    previous: HashMap<Pid, u64>,
    sampled_at: Option<Instant>,
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            system: System::new(),
            previous: HashMap::new(),
            sampled_at: None,
        }
    }

    pub fn sample(&mut self, records: &[Record]) -> UsageMap {
        let mut usage = UsageMap::new();
        if !records.iter().any(|r| r.state.active()) {
            return usage;
        }
        self.system.refresh_cpu_usage();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        // sysinfo's macOS insertion path reads memory but leaves CPU counters at
        // zero. Prime newly observed processes before measuring an interval;
        // otherwise a one-shot sample always reports zero even under load.
        #[cfg(target_os = "macos")]
        {
            let fresh: Vec<_> = self
                .system
                .processes()
                .iter()
                .filter(|(pid, p)| self.previous.get(pid) != Some(&p.start_time()))
                .map(|(&pid, _)| pid)
                .collect();
            if !fresh.is_empty() {
                self.system.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&fresh),
                    false,
                    ProcessRefreshKind::nothing().with_cpu().with_memory(),
                );
            }
        }
        let now = Instant::now();
        let elapsed = self.sampled_at.map(|previous| now.duration_since(previous));
        let cpu_ready = elapsed.is_some_and(|d| d >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        for record in records.iter().filter(|r| r.state.active()) {
            let Some(guardian) = record
                .guardian_pid
                .filter(|&pid| pid > 1 && pid <= i32::MAX as u32)
            else {
                continue;
            };
            let Some(process) = self.system.process(Pid::from_u32(guardian)) else {
                continue;
            };
            // The keeper retains this group leader as a child through cleanup.
            // Never attribute an unrelated process selected solely from an old PID.
            if process.parent() != Some(Pid::from_u32(record.keeper_pid))
                || unsafe { libc::getpgid(guardian as i32) } != guardian as i32
            {
                continue;
            }
            let mut memory = 0u64;
            let mut cpu = 0f32;
            let mut count = 0;
            let mut complete_cpu = cpu_ready;
            let mut complete_memory = true;
            for (&pid, process) in self.system.processes() {
                let number = pid.as_u32();
                if number == guardian
                    || number > i32::MAX as u32
                    // Linux snapshots also contain individual threads. Their
                    // counters are already included in the owning process.
                    || process.thread_kind().is_some()
                    || process.status() == sysinfo::ProcessStatus::Zombie
                    || unsafe { libc::getpgid(number as i32) } != guardian as i32
                {
                    continue;
                }
                memory = memory.saturating_add(process.memory());
                complete_memory &= process.memory() > 0;
                cpu += process.cpu_usage();
                count += 1;
                complete_cpu &=
                    process.memory() > 0 && self.previous.get(&pid) == Some(&process.start_time());
            }
            if count > 0 {
                usage.insert(
                    record.id.clone(),
                    Usage {
                        cpu_percent: (complete_cpu && cpu.is_finite()).then_some(cpu),
                        memory_rss_bytes: complete_memory.then_some(memory),
                        processes: count,
                        sample_interval_ms: cpu_ready.then(|| elapsed.unwrap().as_millis() as u64),
                    },
                );
            }
        }
        self.previous = self
            .system
            .processes()
            .iter()
            .map(|(&pid, p)| (pid, p.start_time()))
            .collect();
        self.sampled_at = Some(now);
        usage
    }

    pub fn snapshot(records: &[Record]) -> UsageMap {
        let mut sampler = Self::new();
        let first = sampler.sample(records);
        if first.is_empty() {
            return first;
        }
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        sampler.sample(records)
    }
}
