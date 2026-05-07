mod cli;
mod connect;
mod device;
mod doctor;
mod ls;
mod midi;
mod mtp_session;
mod output;
mod pull;
mod remote;
mod stat;
mod status;
mod tree;
mod usb_owner;

pub use output::AppError;

use clap::Parser;
use cli::{Cli, Command};

pub fn run() -> Result<(), AppError> {
    let cli = Cli::parse();

    init_logging(cli.verbose);

    match &cli.command {
        Command::Devices => {
            let devices = device::list_tp7_devices()?;
            let devices = device::filter_by_serial(devices, cli.device.as_deref())?;
            output::write_devices(&devices, cli.json)
        }
        Command::Doctor => {
            let report = doctor::run_doctor(cli.device.as_deref())?;
            output::write_doctor(&report, cli.json)
        }
        Command::Status => {
            let report = status::run_status(cli.device.as_deref())?;
            output::write_status(&report, cli.json)
        }
        Command::Connect => {
            let report = connect::run_connect(cli.device.as_deref())?;
            output::write_connect(&report, cli.json)
        }
        Command::Ls(args) => {
            let report = ls::run_ls(
                cli.device.as_deref(),
                cli.auto_connect,
                args.remote_path.as_str(),
            )?;
            output::write_ls(&report, cli.json, args.long, args.ids)
        }
        Command::Tree(args) => {
            let report = tree::run_tree(
                cli.device.as_deref(),
                cli.auto_connect,
                args.remote_path.as_str(),
                args.depth,
            )?;
            output::write_tree(&report, cli.json, args.ids)
        }
        Command::Stat(args) => {
            let report = stat::run_stat(
                cli.device.as_deref(),
                cli.auto_connect,
                args.remote_path.as_str(),
            )?;
            output::write_stat(&report, cli.json)
        }
        Command::Pull(args) => {
            let report = pull::run_pull(
                cli.device.as_deref(),
                cli.auto_connect,
                args.remote_path.as_str(),
                args.local_path.as_deref(),
                pull::PullOptions {
                    recursive: args.recursive,
                    overwrite: args.overwrite,
                    skip_existing: args.skip_existing,
                    dry_run: args.dry_run,
                },
            )?;
            output::write_pull(&report, cli.json)
        }
        Command::Push { .. } => Err(AppError::not_implemented("push")),
        Command::Mkdir { .. } => Err(AppError::not_implemented("mkdir")),
        Command::Rm { .. } => Err(AppError::not_implemented("rm")),
        Command::Rename { .. } => Err(AppError::not_implemented("rename")),
        Command::Eject => Err(AppError::not_implemented("eject")),
    }
}

fn init_logging(verbose: u8) {
    let default_filter = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug,nusb=debug",
    };

    let env = env_logger::Env::default().filter_or("TP7_LOG", default_filter);
    let _ = env_logger::Builder::from_env(env).try_init();
}
