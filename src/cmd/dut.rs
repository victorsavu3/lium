// Copyright 2023 The ChromiumOS Authors
//
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file or at
// https://developers.google.com/open-source/licenses/bsd

use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use argh::FromArgs;
use lazy_static::lazy_static;
use lium::cros;
use lium::dut::discover_local_nodes;
use lium::dut::fetch_dut_info_in_parallel;
use lium::dut::DutInfo;
use lium::dut::MonitoredDut;
use lium::dut::SshInfo;
use lium::dut::SSH_CACHE;
use rayon::prelude::*;
use std::collections::HashMap;
use std::env::current_exe;
use std::fs::read_to_string;
use std::io::stdout;
use std::io::Read;
use std::io::Write;
use std::thread;
use std::time;
use termion::screen::IntoAlternateScreen;

#[derive(FromArgs, PartialEq, Debug)]
/// DUT controller
#[argh(subcommand, name = "dut")]
pub struct Args {
    #[argh(subcommand)]
    nested: SubCommand,
}
#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum SubCommand {
    ArcInfo(ArgsArcInfo),
    Discover(ArgsDiscover),
    Do(ArgsDutDo),
    Info(ArgsDutInfo),
    KernelConfig(ArgsDutKernelConfig),
    List(ArgsDutList),
    Shell(ArgsDutShell),
    Monitor(ArgsDutMonitor),
    Pull(ArgsPull),
    Push(ArgsPush),
    Vnc(ArgsVnc),
}
pub fn run(args: &Args) -> Result<()> {
    match &args.nested {
        SubCommand::ArcInfo(args) => run_arc_info(args),
        SubCommand::Discover(args) => run_discover(args),
        SubCommand::Do(args) => run_dut_do(args),
        SubCommand::Info(args) => run_dut_info(args),
        SubCommand::KernelConfig(args) => run_dut_kernel_config(args),
        SubCommand::List(args) => run_dut_list(args),
        SubCommand::Shell(args) => run_dut_shell(args),
        SubCommand::Monitor(args) => run_dut_monitor(args),
        SubCommand::Pull(args) => run_dut_pull(args),
        SubCommand::Push(args) => run_dut_push(args),
        SubCommand::Vnc(args) => run_dut_vnc(args),
    }
}

#[derive(FromArgs, PartialEq, Debug)]
/// Pull files from DUT
#[argh(subcommand, name = "pull")]
struct ArgsPull {
    /// DUT which the files are pulled from
    #[argh(option)]
    dut: String,

    /// pulled file names
    #[argh(positional)]
    files: Vec<String>,

    /// destination directory (current directory by default)
    #[argh(option)]
    dest: Option<String>,
}

fn run_dut_pull(args: &ArgsPull) -> Result<()> {
    cros::ensure_testing_rsa_is_there()?;
    let target = &SshInfo::new(&args.dut)?;

    target.get_files(&args.files, args.dest.as_ref())
}

#[derive(FromArgs, PartialEq, Debug)]
/// Push files from DUT
#[argh(subcommand, name = "push")]
struct ArgsPush {
    /// destination DUT
    #[argh(option)]
    dut: String,

    /// destination directory on a DUT
    #[argh(option)]
    dest: Option<String>,

    /// source files
    #[argh(positional)]
    files: Vec<String>,
}

fn run_dut_push(args: &ArgsPush) -> Result<()> {
    cros::ensure_testing_rsa_is_there()?;
    let target = &SshInfo::new(&args.dut)?;

    target.send_files(&args.files, args.dest.as_ref())
}

#[derive(FromArgs, PartialEq, Debug)]
/// Open Vnc from DUT
#[argh(subcommand, name = "vnc")]
struct ArgsVnc {
    /// DUT which the files are pushed to
    #[argh(option)]
    dut: String,

    /// local port (default: 5900)
    #[argh(option)]
    port: Option<u16>,
}

fn run_dut_vnc(args: &ArgsVnc) -> Result<()> {
    cros::ensure_testing_rsa_is_there()?;
    let target = &SshInfo::new(&args.dut)?;
    let port = if let Some(_port) = args.port {
        _port
    } else {
        5900
    };
    let mut child = target.start_port_forwarding(5900, port, "kmsvnc")?;
    let mut shown = false;

    loop {
        if let Some(status) = child.try_status()? {
            eprintln!("Failed to connect to {}: {}", &args.dut, status);
            return Ok(());
        } else if !shown {
            println!("Connected. Please run `xtightvncviewer -encodings raw localhost:5900`");
            shown = true;
        }
        thread::sleep(time::Duration::from_secs(5));
    }
}
#[derive(FromArgs, PartialEq, Debug)]
/// open a SSH monitor
#[argh(subcommand, name = "monitor")]
struct ArgsDutMonitor {
    /// DUT identifiers to monitor
    #[argh(positional)]
    duts: Vec<String>,
}

fn run_dut_monitor(args: &ArgsDutMonitor) -> Result<()> {
    cros::ensure_testing_rsa_is_there()?;
    let mut targets: Vec<MonitoredDut> = Vec::new();
    let mut port = 4022;

    for dut in &args.duts {
        targets.push(MonitoredDut::new(dut, port)?);
        port += 1;
    }

    let mut screen = stdout().into_alternate_screen().unwrap();
    loop {
        // Draw headers.
        write!(
            screen,
            "{}{}",
            termion::clear::All,
            termion::cursor::Goto(1, 1)
        )?;
        println!("{}", MonitoredDut::get_status_header());

        for target in targets.iter_mut() {
            println!("{}", target.get_status()?);
        }

        thread::sleep(time::Duration::from_secs(5))
    }
}

#[derive(FromArgs, PartialEq, Debug)]
/// open a SSH shell
#[argh(subcommand, name = "shell")]
struct ArgsDutShell {
    /// a DUT identifier (e.g. 127.0.0.1, localhost:2222)
    #[argh(option)]
    dut: String,

    /// if specified, it will invoke autologin before opening a shell
    #[argh(switch)]
    autologin: bool,

    /// if specified, run the command on dut and exit. if not, it will open an interactive shell.
    #[argh(positional)]
    args: Vec<String>,
}
fn run_dut_shell(args: &ArgsDutShell) -> Result<()> {
    cros::ensure_testing_rsa_is_there()?;
    let target = &SshInfo::new(&args.dut)?;
    if args.autologin {
        target.run_autologin()?;
    }
    if args.args.is_empty() {
        target.open_ssh()
    } else {
        target.run_cmd_piped(&args.args)
    }
}

#[derive(FromArgs, PartialEq, Debug)]
/// get the kernel configuration from the DUT
#[argh(subcommand, name = "kernel_config")]
struct ArgsDutKernelConfig {
    /// a DUT identifier (e.g. 127.0.0.1, localhost:2222)
    #[argh(positional)]
    dut: String,
}
fn run_dut_kernel_config(args: &ArgsDutKernelConfig) -> Result<()> {
    cros::ensure_testing_rsa_is_there()?;
    let target = &SshInfo::new(&args.dut)?;
    let config = target.get_host_kernel_config()?;
    println!("{}", config);
    Ok(())
}

type DutAction = Box<fn(&SshInfo) -> Result<()>>;
fn do_reboot(s: &SshInfo) -> Result<()> {
    s.run_cmd_piped(&["reboot; exit"])
}
fn do_login(s: &SshInfo) -> Result<()> {
    s.run_autologin()
}
fn do_tail_messages(s: &SshInfo) -> Result<()> {
    s.run_cmd_piped(&["tail -f /var/log/messages"])
}
lazy_static! {
    static ref DUT_ACTIONS: HashMap<&'static str, DutAction> = {
        let mut m: HashMap<&'static str, DutAction> = HashMap::new();
        m.insert("reboot", Box::new(do_reboot));
        m.insert("login", Box::new(do_login));
        m.insert("tail_messages", Box::new(do_tail_messages));
        m
    };
}
#[derive(FromArgs, PartialEq, Debug)]
/// send actions
#[argh(subcommand, name = "do")]
struct ArgsDutDo {
    /// a DUT identifier (e.g. 127.0.0.1, localhost:2222)
    #[argh(option)]
    dut: Option<String>,
    /// actions to do (--list-actions to see available options)
    #[argh(positional)]
    actions: Vec<String>,
    /// list available actions
    #[argh(switch)]
    list_actions: bool,
}
fn run_dut_do(args: &ArgsDutDo) -> Result<()> {
    cros::ensure_testing_rsa_is_there()?;
    if args.list_actions {
        println!(
            "{}",
            DUT_ACTIONS
                .keys()
                .map(|s| s.to_owned())
                .collect::<Vec<&str>>()
                .join(" ")
        );
        return Ok(());
    }
    let unknown_actions: Vec<&String> = args
        .actions
        .iter()
        .filter(|s| !DUT_ACTIONS.contains_key(s.as_str()))
        .collect();
    if !unknown_actions.is_empty() || args.actions.is_empty() {
        return Err(anyhow!(
            "Unknown action: {unknown_actions:?}. See `lium dut do --list-actions` for available actions."
        ));
    }
    let dut = &SshInfo::new(args.dut.as_ref().context(anyhow!("Please specify --dut"))?)?;
    let actions: Vec<&DutAction> = args
        .actions
        .iter()
        .flat_map(|s| DUT_ACTIONS.get(s.as_str()))
        .collect();
    let actions: Vec<(&String, &&DutAction)> = args.actions.iter().zip(actions.iter()).collect();
    for (name, f) in actions {
        f(dut).context(anyhow!("DUT action: {name}"))?;
    }
    Ok(())
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum DutStatus {
    Online,
    Offline,
    AddressReused,
}
#[derive(FromArgs, PartialEq, Debug)]
/// list all cached DUTs
#[argh(subcommand, name = "list")]
struct ArgsDutList {
    /// clear all DUT caches
    #[argh(switch)]
    clear: bool,

    /// display space-separated DUT IDs on one line (stable)
    #[argh(switch)]
    ids: bool,

    /// display current status of DUTs (may take a few moments)
    #[argh(switch)]
    status: bool,

    /// add a DUT to the list with the connection provided
    #[argh(option)]
    add: Option<String>,

    /// remove a DUT with a specified ID from the list
    #[argh(option)]
    remove: Option<String>,

    /// update the DUT list and show their status
    #[argh(switch)]
    update: bool,
}
fn run_dut_list(args: &ArgsDutList) -> Result<()> {
    if args.clear {
        return SSH_CACHE.clear();
    }
    let duts = SSH_CACHE
        .entries()
        .context(anyhow!("SSH_CACHE is not initialized yet"))?;
    if args.ids {
        let keys: Vec<String> = duts.keys().map(|s| s.to_string()).collect();
        println!("{}", keys.join(" "));
        return Ok(());
    }
    if let Some(dut_to_add) = &args.add {
        eprintln!("Checking DutInfo of {dut_to_add}...");
        let info = DutInfo::new(dut_to_add)?;
        let id = info.id();
        let ssh = info.ssh();
        SSH_CACHE.set(id, ssh.clone())?;
        println!("Added: {:32} {}", id, serde_json::to_string(ssh)?);
        return Ok(());
    }
    if let Some(dut_to_remove) = &args.remove {
        SSH_CACHE.remove(dut_to_remove)?;
        eprintln!("Removed: {dut_to_remove}",);
        return Ok(());
    }
    if args.status || args.update {
        eprintln!(
            "Checking status of {} DUTs. It will take a minute...",
            duts.len()
        );
        let duts: Vec<(String, DutStatus, SshInfo)> = duts
            .par_iter()
            .map(|e| {
                let id = e.0;
                let info = DutInfo::new(id).map(|e| e.info().clone());
                let status = if let Ok(info) = info {
                    if Some(id) == info.get("dut_id") {
                        DutStatus::Online
                    } else {
                        DutStatus::AddressReused
                    }
                } else {
                    DutStatus::Offline
                };
                (id.to_owned(), status, e.1.clone())
            })
            .collect();
        let (duts_to_be_removed, duts) = if args.update {
            (
                duts.iter()
                    .filter(|e| e.1 == DutStatus::AddressReused)
                    .cloned()
                    .collect(),
                duts.iter()
                    .filter(|e| e.1 != DutStatus::AddressReused)
                    .cloned()
                    .collect(),
            )
        } else {
            (Vec::new(), duts)
        };
        for dut in duts {
            println!("{:32} {:13} {:?}", dut.0, &format!("{:?}", dut.1), dut.2);
        }
        if !duts_to_be_removed.is_empty() {
            println!("\nFollowing DUTs are removed: ");
            for dut in duts_to_be_removed {
                println!("{:32} {:13} {:?}", dut.0, &format!("{:?}", dut.1), dut.2);
                SSH_CACHE.remove(&dut.0)?;
            }
        }
        return Ok(());
    }
    // List cached DUTs
    for it in duts.iter() {
        println!("{:32} {}", it.0, serde_json::to_string(it.1)?);
    }
    Ok(())
}

#[derive(FromArgs, PartialEq, Debug)]
/// show DUT info
#[argh(subcommand, name = "info")]
struct ArgsDutInfo {
    /// DUT identifiers (e.g. 127.0.0.1, localhost:2222, droid_NXHKDSJ003138124257611)
    #[argh(option)]
    dut: String,
    /// comma-separated list of attribute names. to show the full list, try `lium dut info --keys ?`
    #[argh(positional)]
    keys: Vec<String>,
}
fn run_dut_info(args: &ArgsDutInfo) -> Result<()> {
    let dut = &args.dut;
    let keys = if args.keys.is_empty() {
        vec![
            "timestamp",
            "dut_id",
            "hwid",
            "release",
            "model",
            "serial",
            "mac",
        ]
    } else {
        args.keys.iter().map(|s| s.as_str()).collect()
    };
    let ssh = SshInfo::new(dut)?;
    let info = DutInfo::fetch_keys(&ssh, &keys)?;
    let result = serde_json::to_string(&info)?;
    println!("{}", result);
    Ok(())
}

#[derive(FromArgs, PartialEq, Debug)]
/// discover DUTs on the same network
#[argh(subcommand, name = "discover")]
pub struct ArgsDiscover {
    /// A network interface to be used for the scan.
    /// if not specified, the first interface in the routing table will be used.
    #[argh(option)]
    interface: Option<String>,
    /// remote machine to do the scan. If not specified, run the discovery locally.
    #[argh(option)]
    remote: Option<String>,
    /// path to a list of DUT_IDs to scan.
    #[argh(option)]
    target_list: Option<String>,
    /// additional attributes to retrieve
    #[argh(positional, greedy)]
    extra_attr: Vec<String>,
}
pub fn run_discover(args: &ArgsDiscover) -> Result<()> {
    if let Some(remote) = &args.remote {
        eprintln!("Using remote machine: {}", remote);
        let lium_path = current_exe()?;
        eprintln!("lium executable path: {:?}", lium_path);
        let remote = SshInfo::new(remote)?;
        remote.send_files(
            &[lium_path.to_string_lossy().to_string()],
            Some(&"~/".to_string()),
        )?;
        let mut cmd = "~/lium dut discover".to_string();
        for ea in &args.extra_attr {
            cmd += " ";
            cmd += ea;
        }
        remote.run_cmd_piped(&[cmd])?;
        return Ok(());
    }
    let addrs = if let Some(target_list) = &args.target_list {
        let addrs: String = if target_list == "-" {
            let mut buffer = Vec::new();
            std::io::stdin().read_to_end(&mut buffer)?;
            Ok(std::str::from_utf8(&buffer)?.to_string())
        } else {
            read_to_string(target_list)
        }?;
        Ok(addrs
            .trim()
            .split('\n')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect())
    } else {
        discover_local_nodes(args.interface.to_owned())
    }?;
    eprintln!("Found {} candidates. Checking...", addrs.len());
    let duts = fetch_dut_info_in_parallel(&addrs, &args.extra_attr)?;
    eprintln!("Discovery completed with {} DUTs", duts.len());
    let duts: Vec<HashMap<String, String>> = duts.iter().map(|e| e.info().to_owned()).collect();
    let dut_list = serde_json::to_string_pretty(&duts)?;
    println!("{}", dut_list);

    Ok(())
}

#[derive(FromArgs, PartialEq, Debug)]
/// get ARC information
#[argh(subcommand, name = "arc_info")]
struct ArgsArcInfo {
    /// a DUT identifier (e.g. 127.0.0.1, localhost:2222)
    #[argh(positional)]
    dut: String,
}
fn run_arc_info(args: &ArgsArcInfo) -> Result<()> {
    cros::ensure_testing_rsa_is_there()?;
    let target = &SshInfo::new(&args.dut)?;
    println!("arch: {}", target.get_arch()?);
    println!("ARC version: {}", target.get_arc_version()?);
    println!("ARC device: {}", target.get_arc_device()?);
    println!("image type: {}", target.get_arc_image_type()?);
    Ok(())
}
