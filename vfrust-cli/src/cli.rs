use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "vfrust", about = "macOS Virtualization.framework VM manager")]
pub struct Cli {
    /// Number of virtual CPUs
    #[arg(long, default_value_t = 1)]
    pub cpus: u32,

    /// Memory in MiB
    #[arg(long, default_value_t = 512)]
    pub memory: u64,

    /// Bootloader specification:
    ///   linux,kernel=<path>[,initrd=<path>][,cmdline=<str>]
    ///   efi,variable-store=<path>[,create]
    ///   macos,machineIdentifierPath=<path>,hardwareModelPath=<path>,auxImagePath=<path>
    #[arg(long)]
    pub bootloader: Option<String>,

    /// [Deprecated] Kernel path (use --bootloader linux,kernel=<path> instead)
    #[arg(long)]
    pub kernel: Option<String>,

    /// [Deprecated] Initrd path (use --bootloader linux,... instead)
    #[arg(long)]
    pub initrd: Option<String>,

    /// [Deprecated] Kernel command line (use --bootloader linux,... instead)
    #[arg(long = "kernel-cmdline")]
    pub kernel_cmdline: Option<String>,

    /// Device specifications (can be repeated).
    /// e.g. --device virtio-blk,path=/tmp/disk.img
    #[arg(long)]
    pub device: Vec<String>,

    /// Log level: debug, info, warn, error
    #[arg(long, default_value = "info")]
    pub log_level: String,

    /// Enable GUI window (auto-adds GPU + input devices if not present)
    #[arg(long)]
    pub gui: bool,

    /// Write PID to file
    #[arg(long)]
    pub pidfile: Option<String>,

    /// Cloud-init files (comma-separated: meta-data,user-data[,network-config])
    #[arg(long)]
    pub cloud_init: Option<String>,

    /// Ignition configuration file path
    #[arg(long)]
    pub ignition: Option<String>,

    /// Enable time synchronization via vsock (port number)
    #[arg(long)]
    pub timesync: Option<u32>,

    /// Enable nested virtualization
    #[arg(long)]
    pub nested: bool,
}
