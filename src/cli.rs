use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "tp7",
    version,
    about = "Teenage Engineering TP-7 file access CLI"
)]
#[command(arg_required_else_help = true)]
pub struct Cli {
    #[arg(
        short = 'd',
        long,
        global = true,
        value_name = "SERIAL",
        help = "Select a TP-7 by serial number"
    )]
    pub device: Option<String>,

    #[arg(
        short = 'j',
        long,
        global = true,
        help = "Write machine-readable JSON output"
    )]
    pub json: bool,

    #[arg(
        short = 'a',
        long,
        global = true,
        help = "Allow commands to switch the device into MTP mode automatically"
    )]
    pub auto_connect: bool,

    #[arg(long, global = true, help = "Disable progress output")]
    pub no_progress: bool,

    #[arg(
        short,
        long,
        global = true,
        action = clap::ArgAction::Count,
        help = "Increase diagnostic logging"
    )]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "List connected TP-7 devices")]
    Devices,

    #[command(about = "Run macOS USB/MTP diagnostics")]
    Doctor,

    #[command(about = "Show TP-7 device and storage status")]
    Status,

    #[command(about = "Switch or validate TP-7 MTP mode")]
    Connect,

    #[command(about = "List files on the TP-7", disable_help_flag = true)]
    Ls(LsArgs),

    #[command(about = "Show a recursive file tree")]
    Tree(TreeArgs),

    #[command(about = "Show metadata for one remote object")]
    Stat(RemotePathArgs),

    #[command(about = "Download a file or directory from the TP-7")]
    Pull(PullArgs),

    #[command(about = "Upload a file or directory to the TP-7")]
    Push(PushArgs),

    #[command(about = "Create a remote folder")]
    Mkdir(MkdirArgs),

    #[command(about = "Delete a remote file or folder")]
    Rm(RmArgs),

    #[command(about = "Rename a remote object without moving it")]
    Rename(RenameArgs),

    #[command(about = "Open and close an MTP session cleanly")]
    Eject,
}

#[derive(Debug, Args)]
pub struct LsArgs {
    #[arg(default_value = "/")]
    pub remote_path: String,

    #[arg(short = 'l', long, help = "Use a detailed listing")]
    pub long: bool,

    #[arg(short = 'i', long, help = "Show MTP object IDs")]
    pub ids: bool,

    #[arg(short = 's', long, help = "Show object sizes in the compact listing")]
    pub size: bool,

    #[arg(short = 'S', long = "sort-size", help = "Sort by size, largest first")]
    pub sort_size: bool,

    #[arg(
        short = 't',
        long = "sort-time",
        help = "Sort by modification time, newest first"
    )]
    pub sort_time: bool,

    #[arg(short = 'r', long, help = "Reverse the listing order")]
    pub reverse: bool,

    #[arg(
        short = 'h',
        long,
        help = "Format displayed sizes in human-readable units"
    )]
    pub human_readable: bool,

    #[arg(long = "help", action = clap::ArgAction::HelpLong, help = "Print help")]
    pub help: Option<bool>,
}

#[derive(Debug, Args)]
pub struct TreeArgs {
    #[arg(default_value = "/")]
    pub remote_path: String,

    #[arg(long)]
    pub depth: Option<usize>,

    #[arg(long)]
    pub ids: bool,
}

#[derive(Debug, Args)]
pub struct RemotePathArgs {
    pub remote_path: String,
}

#[derive(Debug, Args)]
pub struct PullArgs {
    pub remote_path: String,
    pub local_path: Option<String>,

    #[arg(long)]
    pub recursive: bool,

    #[arg(long)]
    pub overwrite: bool,

    #[arg(long)]
    pub skip_existing: bool,

    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct PushArgs {
    pub local_path: String,
    pub remote_path: String,

    #[arg(long)]
    pub recursive: bool,

    #[arg(long)]
    pub overwrite: bool,

    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct MkdirArgs {
    pub remote_path: String,

    #[arg(long)]
    pub parents: bool,
}

#[derive(Debug, Args)]
pub struct RmArgs {
    pub remote_path: String,

    #[arg(long)]
    pub recursive: bool,

    #[arg(long)]
    pub force: bool,

    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct RenameArgs {
    pub remote_path: String,
    pub new_name: String,
}
