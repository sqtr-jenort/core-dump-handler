extern crate dotenv;

use crate::events::CoreEvent;

use advisory_lock::{AdvisoryFileLock, FileLockMode};
use log::{debug, error, info, warn};
use serde_json::json;
use serde_json::Value;
use std::env;
use std::fs::File;
use std::io;
use std::io::prelude::*;
use std::process;
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;
use zip::write::FileOptions;
use zip::ZipWriter;

mod config;
mod events;
mod logging;

fn main() -> Result<(), anyhow::Error> {
    let (send, recv) = channel();
    let cc = config::CoreConfig::new()?;
    let recv_time: u64 = cc.timeout as u64;
    thread::spawn(move || {
        let result = handle(cc);
        send.send(result).unwrap();
    });

    let result = recv.recv_timeout(Duration::from_secs(recv_time));

    match result {
        Ok(inner_result) => inner_result,
        Err(_error) => {
            error!("Timeout error during coredump processing.");
            process::exit(32);
        }
    }
}

fn handle(mut cc: config::CoreConfig) -> Result<(), anyhow::Error> {
    cc.set_namespace("default".to_string());
    let l_log_level = cc.log_level.clone();
    let log_path = logging::init_logger(l_log_level)?;
    debug!("Arguments: {:?}", env::args());

    info!(
        "Environment config:\n IGNORE_CRIO={}\nCRIO_IMAGE_CMD={}\nUSE_CRIO_CONF={}",
        cc.ignore_crio, cc.image_command, cc.use_crio_config
    );

    info!("Set logfile to: {:?}", &log_path);
    debug!("Creating dump for {}", cc.get_templated_name());

    let l_crictl_config_path = cc.crictl_config_path.clone();

    let config_path = if cc.use_crio_config {
        Some(
            l_crictl_config_path
                .into_os_string()
                .to_string_lossy()
                .to_string(),
        )
    } else {
        None
    };
    let l_bin_path = cc.bin_path.clone();
    let l_image_command = cc.image_command.clone();

    // Create the base zip file that we are going to put everything into
    let compression_method = if cc.compression {
        zip::CompressionMethod::Deflated
    } else {
        zip::CompressionMethod::Stored
    };
    let options = FileOptions::default()
        .compression_method(compression_method)
        .unix_permissions(0o444)
        .large_file(true);

    let file = match File::create(cc.get_zip_full_path()) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to create file: {}", e);
            process::exit(1);
        }
    };
    file.lock(FileLockMode::Exclusive)?;
    let mut zip = ZipWriter::new(&file);

    debug!(
        "Create a JSON file to store the dump meta data\n{}",
        cc.get_dump_info_filename()
    );

    match zip.start_file(cc.get_dump_info_filename(), options) {
        Ok(v) => v,
        Err(e) => {
            error!("Error starting dump file in zip \n{}", e);
            zip.finish()?;
            file.unlock()?;
            process::exit(1);
        }
    };

    match zip.write_all(cc.get_dump_info().as_bytes()) {
        Ok(v) => v,
        Err(e) => {
            error!("Error writing pod file in zip \n{}", e);
            zip.finish()?;
            file.unlock()?;
            process::exit(1);
        }
    };

    // Pipe the core file to zip
    match zip.start_file(cc.get_core_filename(), options) {
        Ok(v) => v,
        Err(e) => error!("Error starting core file \n{}", e),
    };

    let stdin = io::stdin();
    let mut stdin = stdin.lock();

    match io::copy(&mut stdin, &mut zip) {
        Ok(v) => v,
        Err(e) => {
            error!("Error writing core file \n{}", e);
            process::exit(1);
        }
    };
    zip.flush()?;

    if cc.ignore_crio {
        if cc.core_events {
            let zip_name = format!("{}.zip", cc.get_templated_name());
            let evtdir = format!("{}", cc.event_location.display());
            let evt = CoreEvent::new_no_crio(cc.params, zip_name);
            evt.write_event(&evtdir)?;
        }
        zip.finish()?;
        file.unlock()?;
        process::exit(0);
    }
}
