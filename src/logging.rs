//! CSV logging, byte-compatible with the bash `cryocon.sh` logs so that
//! `match_temps.py` and `plot_ramp.py` keep working unchanged.
//!
//! Header (identical to cryocon.sh):
//!   timestamp,event,setpoint_loop1_K,setpoint_loop2_K,input_A_K,input_B_K
//! Filenames: cryocon_YYYYMMDD_HHMMSS.csv in a user-chosen folder.

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use crate::device::Snapshot;

pub struct CsvLog {
    file: File,
    pub path: PathBuf,
}

fn fmt(v: Option<f64>) -> String {
    match v {
        // six decimals mirrors the instrument's own answer format
        Some(x) => format!("{x:.6}"),
        None => "?".to_string(), // same placeholder as the bash log
    }
}

impl CsvLog {
    pub fn create(dir: &PathBuf) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let path = dir.join(format!("cryocon_{stamp}.csv"));
        let mut file = File::create(&path)?;
        writeln!(
            file,
            "timestamp,event,setpoint_loop1_K,setpoint_loop2_K,input_A_K,input_B_K"
        )?;
        Ok(Self { file, path })
    }

    /// Append one row. `event` uses the same strings as cryocon.sh
    /// ("poll loop1 T=... sp=... P=...", "set loop1 -> 299", ...).
    pub fn row(&mut self, event: &str, s: &Snapshot) {
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(
            self.file,
            "{ts},{event},{},{},{},{}",
            fmt(s.setpoint_1),
            "?", // loop 2 has no meaning on this instrument; keep the column
            fmt(s.t_a),
            fmt(s.t_b),
        );
        let _ = self.file.flush();
    }
}
