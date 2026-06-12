mod credentials;
mod model;
mod store;
mod templates;

use crate::model::{
    AppType, ClaudeCodeModelConfig, CodexModelConfig, OpenCodeModelConfig, ProviderApps,
    ProviderKind, ProviderModels,
    ProviderProfile,
};
use libc::{self, c_int, c_ulong};
use nix::sys::socket::{
    AddressFamily, ControlMessage, ControlMessageOwned, MsgFlags, SockFlag, SockType, recvmsg,
    sendmsg, socketpair,
};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, Pid, execvp, fork};
use parking_lot::Mutex;
use std::env;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, IoSlice, IoSliceMut, Read, Write};
use std::mem::{offset_of, size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
const SECCOMP_SET_MODE_FILTER: c_ulong = 1;
const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

const IOCTL_READ: u32 = 2;
const IOCTL_WRITE: u32 = 1;
const IOCTL_READ_WRITE: u32 = IOCTL_READ | IOCTL_WRITE;
const SECCOMP_IOC_MAGIC: u32 = b'!' as u32;
const SECCOMP_IOCTL_NOTIF_RECV: c_ulong = ioctl_code_read_write(
    SECCOMP_IOC_MAGIC,
    0,
    size_of::<libc::seccomp_notif>() as u32,
) as c_ulong;
const SECCOMP_IOCTL_NOTIF_SEND: c_ulong = ioctl_code_read_write(
    SECCOMP_IOC_MAGIC,
    1,
    size_of::<libc::seccomp_notif_resp>() as u32,
) as c_ulong;
const SECCOMP_IOCTL_NOTIF_ADDFD: c_ulong = ioctl_code_write(
    SECCOMP_IOC_MAGIC,
    3,
    size_of::<libc::seccomp_notif_addfd>() as u32,
) as c_ulong;

const SSH_AGENT_FAILURE: u8 = 5;
const SSH_AGENT_SUCCESS: u8 = 6;
const SSH_AGENTC_EXTENSION: u8 = 27;
const SSH_AGENT_EXTENSION_FAILURE: u8 = 28;
const SSH_AGENT_EXTENSION_RESPONSE: u8 = 29;

const EXT_QUERY: &str = "query";
const EXT_GET_CREDENTIAL: &str = "get-credential@keybearer.dev";
const EXT_GET_PROVIDER: &str = "get-provider@keybearer.dev";
const EXT_ADD_PROVIDER: &str = "add-provider@keybearer.dev";
const EXT_RELOAD_STORE: &str = "reload-store@keybearer.dev";
const EXTENSIONS: [&str; 4] = [
    EXT_GET_CREDENTIAL,
    EXT_GET_PROVIDER,
    EXT_ADD_PROVIDER,
    EXT_RELOAD_STORE,
];

const EXIT_USAGE: i32 = 64;
const EXIT_UNAVAILABLE: i32 = 69;
const EXIT_DATAERR: i32 = 65;

struct AgentState {
    store: model::KeybearerStore,
}

const fn ioctl_code_read_write(ty: u32, nr: u32, size: u32) -> u32 {
    ioctl_code(IOCTL_READ_WRITE, ty, nr, size)
}

const fn ioctl_code_write(ty: u32, nr: u32, size: u32) -> u32 {
    ioctl_code(IOCTL_WRITE, ty, nr, size)
}

const fn ioctl_code(dir: u32, ty: u32, nr: u32, size: u32) -> u32 {
    (dir << 30) | (size << 16) | (ty << 8) | nr
}

fn main() {
    if let Err(error) = dispatch() {
        eprintln!("keybearer: {error}");
        std::process::exit(1);
    }
}

fn dispatch() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    let Some(command) = args.get(1).map(String::as_str) else {
        eprintln!("Usage: keybearer <agent|add|list|remove|use|check|ssh|run> ...");
        std::process::exit(EXIT_USAGE);
    };

    match command {
        "agent" => agent_command(&args[2..]),
        "add" => add_command(&args[2..]),
        "list" => list_command(&args[2..]),
        "remove" => remove_command(&args[2..]),
        "use" => use_command(&args[2..]),
        "check" => check_command(&args[2..]),
        "ssh" => ssh_command(&args[2..]),
        "run" => run_command(&args[2..]),
        _ => {
            eprintln!("Usage: keybearer <agent|add|list|remove|use|check|ssh|run> ...");
            std::process::exit(EXIT_USAGE);
        }
    }
}

fn agent_command(args: &[String]) -> io::Result<()> {
    let options = parse_agent_options(args);

    if matches!(options.mode, AgentMode::Kill) {
        return kill_agent_command();
    }

    let uid = unsafe { libc::geteuid() };
    let pid = std::process::id();
    let dir = env::temp_dir();
    let agent_sock = options
        .agent_sock
        .unwrap_or_else(|| dir.join(format!("keybearer-agent-{uid}-{pid}.sock")));
    let control_sock = options
        .control_sock
        .unwrap_or_else(|| dir.join(format!("keybearer-control-{uid}-{pid}.sock")));
    let upstream = upstream_agent_sock();

    match options.mode {
        AgentMode::Kill => unreachable!(),
        AgentMode::Foreground => run_agent(agent_sock, control_sock, upstream, None),
        AgentMode::Eval => {
            let (ready_read, ready_write) = nix::unistd::pipe().map_err(nix_err)?;
            match unsafe { fork() }.map_err(nix_err)? {
                ForkResult::Parent { child } => {
                    drop(ready_write);
                    let mut ready = [0u8; 1];
                    let n =
                        nix::unistd::read(ready_read.as_raw_fd(), &mut ready).map_err(nix_err)?;
                    if n != 1 {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "keybearer agent failed to start",
                        ));
                    }
                    print_agent_exports(&agent_sock, &control_sock, upstream.as_deref(), child);
                    Ok(())
                }
                ForkResult::Child => {
                    drop(ready_read);
                    nix::unistd::setsid().map_err(nix_err)?;
                    detach_stdio()?;
                    run_agent(agent_sock, control_sock, upstream, Some(ready_write))?;
                    Ok(())
                }
            }
        }
    }
}

struct AgentOptions {
    mode: AgentMode,
    agent_sock: Option<PathBuf>,
    control_sock: Option<PathBuf>,
}

enum AgentMode {
    Eval,
    Foreground,
    Kill,
}

fn parse_agent_options(args: &[String]) -> AgentOptions {
    let mut options = AgentOptions {
        mode: AgentMode::Eval,
        agent_sock: None,
        control_sock: None,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-D" | "--foreground" => {
                if matches!(options.mode, AgentMode::Kill | AgentMode::Foreground) {
                    agent_usage_exit();
                }
                options.mode = AgentMode::Foreground;
                index += 1;
            }
            "-k" => {
                if !matches!(options.mode, AgentMode::Eval)
                    || options.agent_sock.is_some()
                    || options.control_sock.is_some()
                {
                    agent_usage_exit();
                }
                options.mode = AgentMode::Kill;
                index += 1;
            }
            "-a" | "--agent-sock" => {
                if index + 1 >= args.len()
                    || !matches!(options.mode, AgentMode::Eval | AgentMode::Foreground)
                    || options.agent_sock.is_some()
                {
                    agent_usage_exit();
                }
                options.agent_sock = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            "--control-sock" => {
                if index + 1 >= args.len()
                    || !matches!(options.mode, AgentMode::Eval | AgentMode::Foreground)
                    || options.control_sock.is_some()
                {
                    agent_usage_exit();
                }
                options.control_sock = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            _ => agent_usage_exit(),
        }
    }
    options
}

fn agent_usage_exit() -> ! {
    eprintln!(
        "Usage: keybearer agent [-D] [-k] [-a <path>] [--agent-sock <path>] [--control-sock <path>]"
    );
    std::process::exit(EXIT_USAGE);
}

fn print_agent_exports(
    agent_sock: &Path,
    control_sock: &Path,
    upstream: Option<&Path>,
    child: Pid,
) {
    println!(
        "SSH_AUTH_SOCK={}; export SSH_AUTH_SOCK;",
        shell_quote(agent_sock)
    );
    println!(
        "KEYBEARER_CONTROL_SOCK={}; export KEYBEARER_CONTROL_SOCK;",
        shell_quote(control_sock)
    );
    if let Some(upstream) = upstream {
        println!(
            "KEYBEARER_UPSTREAM_SSH_AUTH_SOCK={}; export KEYBEARER_UPSTREAM_SSH_AUTH_SOCK;",
            shell_quote(upstream)
        );
    }
    println!("KEYBEARER_AGENT_PID={child}; export KEYBEARER_AGENT_PID;");
}

fn kill_agent_command() -> io::Result<()> {
    let pid = env::var("KEYBEARER_AGENT_PID").ok();
    let upstream = env::var("KEYBEARER_UPSTREAM_SSH_AUTH_SOCK").ok();
    let Some(pid) = pid.and_then(|pid| pid.parse::<libc::pid_t>().ok()) else {
        println!("echo 'keybearer: no KEYBEARER_AGENT_PID set' >&2;");
        print_kill_environment_reset(upstream.as_deref());
        return Ok(());
    };

    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        print_kill_environment_reset(upstream.as_deref());
        println!("echo Agent pid {pid} killed;");
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        print_kill_environment_reset(upstream.as_deref());
        println!("echo Agent pid {pid} killed;");
    } else {
        println!("echo 'keybearer: failed to kill agent pid {pid}: {error}' >&2;");
    }
    Ok(())
}

fn print_kill_environment_reset(upstream: Option<&str>) {
    if let Some(upstream) = upstream.filter(|upstream| !upstream.is_empty()) {
        println!(
            "SSH_AUTH_SOCK={}; export SSH_AUTH_SOCK;",
            shell_quote(Path::new(upstream))
        );
    } else {
        println!("unset SSH_AUTH_SOCK;");
    }
    println!("unset KEYBEARER_CONTROL_SOCK;");
    println!("unset KEYBEARER_UPSTREAM_SSH_AUTH_SOCK;");
    println!("unset KEYBEARER_AGENT_PID;");
}

fn upstream_agent_sock() -> Option<PathBuf> {
    let current = env::var_os("SSH_AUTH_SOCK").map(PathBuf::from)?;
    let current_is_keybearer = current
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("keybearer-agent-"));
    if current_is_keybearer {
        env::var_os("KEYBEARER_UPSTREAM_SSH_AUTH_SOCK")
            .map(PathBuf::from)
            .or(Some(current))
    } else {
        Some(current)
    }
}

fn run_agent(
    agent_sock: PathBuf,
    control_sock: PathBuf,
    upstream: Option<PathBuf>,
    ready_write: Option<OwnedFd>,
) -> io::Result<()> {
    let _ = std::fs::remove_file(&agent_sock);
    let _ = std::fs::remove_file(&control_sock);
    let agent_listener = UnixListener::bind(&agent_sock)?;
    let control_listener = UnixListener::bind(&control_sock)?;
    let initial_store = store::load_store()?;
    let state = Arc::new(Mutex::new(AgentState {
        store: initial_store,
    }));
    if let Some(ready_write) = ready_write {
        nix::unistd::write(&ready_write, &[1]).map_err(nix_err)?;
        drop(ready_write);
    }

    {
        let state = Arc::clone(&state);
        thread::spawn(move || {
            serve_listener(control_listener, state, None, true);
        });
    }

    serve_listener(agent_listener, state, upstream, false);
    Ok(())
}

fn serve_listener(
    listener: UnixListener,
    state: Arc<Mutex<AgentState>>,
    upstream: Option<PathBuf>,
    local_control: bool,
) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let state = Arc::clone(&state);
        let upstream = upstream.clone();
        thread::spawn(move || {
            let _ = serve_agent_connection(stream, state, upstream, local_control);
        });
    }
}

fn serve_agent_connection(
    mut stream: UnixStream,
    state: Arc<Mutex<AgentState>>,
    upstream: Option<PathBuf>,
    local_control: bool,
) -> io::Result<()> {
    loop {
        let packet = match read_agent_packet(&mut stream) {
            Ok(packet) => packet,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::ConnectionReset => return Ok(()),
            Err(error) => return Err(error),
        };

        let response = handle_agent_packet(&packet, &state, local_control)?
            .or_else(|| proxy_agent_packet(&packet, upstream.as_deref()).ok())
            .unwrap_or_else(|| vec![SSH_AGENT_FAILURE]);
        write_agent_packet(&mut stream, &response)?;
    }
}

fn handle_agent_packet(
    packet: &[u8],
    state: &Mutex<AgentState>,
    local_control: bool,
) -> io::Result<Option<Vec<u8>>> {
    if packet.first().copied() != Some(SSH_AGENTC_EXTENSION) {
        return Ok(None);
    }

    let mut cursor = PacketCursor::new(&packet[1..]);
    let extension = cursor.read_string_utf8()?;
    match extension.as_str() {
        EXT_QUERY => Ok(Some(extension_response(
            EXT_QUERY,
            &encode_name_list(&EXTENSIONS),
        ))),
        EXT_GET_PROVIDER => {
            let profile_id = cursor.read_string_utf8()?;
            let value = {
                let state = state.lock();
                let Some(profile) = state.store.profiles.get(&profile_id) else {
                    return Ok(Some(vec![SSH_AGENT_EXTENSION_FAILURE]));
                };
                profile.api_key.clone()
            };
            Ok(Some(extension_response(
                EXT_GET_PROVIDER,
                &encode_string(value.as_bytes()),
            )))
        }
        EXT_GET_CREDENTIAL => {
            let app_type = cursor.read_string_utf8()?;
            let profile_id = cursor.read_string_utf8()?;
            let Some(app) = AppType::parse(&app_type) else {
                return Ok(Some(vec![SSH_AGENT_EXTENSION_FAILURE]));
            };
            let profile_id = (!profile_id.is_empty()).then_some(profile_id.as_str());
            let Some(credential) =
                credentials::credential_for_app(&state.lock().store, app, profile_id)
            else {
                return Ok(Some(vec![SSH_AGENT_EXTENSION_FAILURE]));
            };
            let json = serde_json::to_vec(&credential).map_err(invalid_data)?;
            Ok(Some(extension_response(
                EXT_GET_CREDENTIAL,
                &encode_string(&json),
            )))
        }
        EXT_ADD_PROVIDER if local_control => {
            let (profile_id, profile) = profile_from_agent_payload(&mut cursor)?;
            let mut next_store = state.lock().store.clone();
            store::upsert_provider(&mut next_store, profile_id, profile);
            store::save_store(&next_store)?;
            state.lock().store = next_store;
            Ok(Some(vec![SSH_AGENT_SUCCESS]))
        }
        EXT_ADD_PROVIDER => Ok(Some(vec![SSH_AGENT_EXTENSION_FAILURE])),
        EXT_RELOAD_STORE if local_control => match store::load_store() {
            Ok(next_store) => {
                state.lock().store = next_store;
                Ok(Some(vec![SSH_AGENT_SUCCESS]))
            }
            Err(_) => Ok(Some(vec![SSH_AGENT_FAILURE])),
        },
        EXT_RELOAD_STORE => Ok(Some(vec![SSH_AGENT_EXTENSION_FAILURE])),
        _ => Ok(None),
    }
}

fn proxy_agent_packet(packet: &[u8], upstream: Option<&Path>) -> io::Result<Vec<u8>> {
    let Some(upstream) = upstream else {
        return Ok(vec![SSH_AGENT_FAILURE]);
    };
    let mut stream = UnixStream::connect(upstream)?;
    write_agent_packet(&mut stream, packet)?;
    read_agent_packet(&mut stream)
}

fn add_command(args: &[String]) -> io::Result<()> {
    if args.len() < 3 {
        eprintln!(
            "Usage: keybearer add <provider-kind> <profile-id> <api-key> [--name <display-name>] [--base-url <url>] [--app codex] [--app opencode] [--model <model>]"
        );
        std::process::exit(EXIT_USAGE);
    }

    let (profile_id, profile) = parse_add_profile(args)?;
    let mut next_store = store::load_store()?;
    store::upsert_provider(&mut next_store, profile_id.clone(), profile.clone());
    store::save_store(&next_store)?;
    let reload_ok = notify_agent_reload();
    if let Some(socket) = env::var_os("KEYBEARER_CONTROL_SOCK") {
        if is_unix_socket(Path::new(&socket)) {
            let _ = send_agent_profile(Path::new(&socket), &profile_id, &profile);
        }
    }
    if reload_ok || env::var_os("KEYBEARER_CONTROL_SOCK").is_none() {
        println!("keybearer: saved profile {profile_id}");
    } else {
        println!(
            "keybearer: saved profile {profile_id}; warning: running agent did not reload store"
        );
    }
    Ok(())
}

fn list_command(args: &[String]) -> io::Result<()> {
    if !args.is_empty() {
        eprintln!("Usage: keybearer list");
        std::process::exit(EXIT_USAGE);
    }
    let current_store = store::load_store()?;
    if current_store.profiles.is_empty() {
        println!("No provider profiles configured.");
        return Ok(());
    }
    println!("ID KIND APPS DEFAULT_FOR NAME");
    for (profile_id, profile) in &current_store.profiles {
        let defaults: Vec<&str> = current_store
            .defaults
            .iter()
            .filter_map(|(app, id)| (id == profile_id).then_some(app.as_str()))
            .collect();
        println!(
            "{} {} {} {} {}",
            profile_id,
            profile.provider_kind.as_str(),
            profile.apps.csv(),
            defaults.join(","),
            profile.name
        );
    }
    Ok(())
}

fn remove_command(args: &[String]) -> io::Result<()> {
    if args.len() != 1 {
        eprintln!("Usage: keybearer remove <profile-id>");
        std::process::exit(EXIT_USAGE);
    }
    let mut current_store = store::load_store()?;
    if !store::remove_provider(&mut current_store, &args[0]) {
        eprintln!("keybearer: provider profile not found: {}", args[0]);
        std::process::exit(EXIT_UNAVAILABLE);
    }
    store::save_store(&current_store)?;
    let _ = notify_agent_reload();
    Ok(())
}

fn use_command(args: &[String]) -> io::Result<()> {
    if args.len() != 2 {
        eprintln!("Usage: keybearer use <app> <profile-id>");
        std::process::exit(EXIT_USAGE);
    }
    let Some(app) = AppType::parse(&args[0]) else {
        eprintln!("keybearer: unsupported app: {}", args[0]);
        std::process::exit(EXIT_USAGE);
    };
    let mut current_store = store::load_store()?;
    let Some(profile) = current_store.profiles.get(&args[1]) else {
        eprintln!("keybearer: provider profile not found: {}", args[1]);
        std::process::exit(EXIT_UNAVAILABLE);
    };
    if !profile.apps.enables(&app) {
        eprintln!(
            "keybearer: profile {} is not enabled for {}",
            args[1],
            app.as_str()
        );
        std::process::exit(EXIT_USAGE);
    }
    store::set_default_profile(&mut current_store, app, args[1].clone());
    store::save_store(&current_store)?;
    let _ = notify_agent_reload();
    Ok(())
}

fn check_command(args: &[String]) -> io::Result<()> {
    if !args.is_empty() {
        eprintln!("Usage: keybearer check");
        std::process::exit(EXIT_USAGE);
    }
    let path = store::store_path();
    if !path.exists() {
        eprintln!("keybearer: config not found: {}", path.display());
        std::process::exit(EXIT_UNAVAILABLE);
    }
    match store::load_store() {
        Ok(_) => {
            println!("keybearer: config ok: {}", path.display());
            Ok(())
        }
        Err(error) => {
            eprintln!("keybearer: config invalid: {error}");
            std::process::exit(EXIT_DATAERR);
        }
    }
}

fn parse_add_profile(args: &[String]) -> io::Result<(String, ProviderProfile)> {
    let provider_kind = ProviderKind::parse(&args[0]).unwrap_or_else(|| {
        eprintln!("keybearer: unsupported provider kind: {}", args[0]);
        std::process::exit(EXIT_USAGE);
    });
    let id = args[1].clone();
    if !valid_profile_id(&id) {
        eprintln!("keybearer: invalid profile id");
        std::process::exit(EXIT_USAGE);
    }
    let api_key = args[2].clone();
    let mut name = id.clone();
    let mut base_url = None;
    let mut apps = ProviderApps::default();
    let mut saw_app = false;
    let mut model_list: Vec<String> = Vec::new();
    let mut index = 3;
    while index < args.len() {
        match args[index].as_str() {
            "--name" if index + 1 < args.len() => {
                name = args[index + 1].clone();
                index += 2;
            }
            "--base-url" if index + 1 < args.len() => {
                base_url = Some(args[index + 1].clone());
                index += 2;
            }
            "--app" if index + 1 < args.len() => {
                saw_app = true;
                match AppType::parse(&args[index + 1]) {
                    Some(AppType::Codex) => apps.codex = true,
                    Some(AppType::OpenCode) => apps.open_code = true,
                    Some(AppType::ClaudeCode) => apps.claude_code = true,
                    None => {
                        eprintln!("keybearer: unsupported app: {}", args[index + 1]);
                        std::process::exit(EXIT_USAGE);
                    }
                }
                index += 2;
            }
            "--model" if index + 1 < args.len() => {
                for m in args[index + 1]
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                {
                    if !model_list.contains(&m) {
                        model_list.push(m);
                    }
                }
                index += 2;
            }
            _ => {
                eprintln!(
                    "Usage: keybearer add <provider-kind> <profile-id> <api-key> [--name <display-name>] [--base-url <url>] [--app codex] [--app opencode] [--app claudeCode] [--model <model1,model2>]"
                );
                std::process::exit(EXIT_USAGE);
            }
        }
    }
    if !saw_app
        && matches!(
            provider_kind,
            ProviderKind::OpenAI | ProviderKind::OpenAICompatible
        )
    {
        apps.codex = true;
    }
    let mut models = ProviderModels::default();
    if !model_list.is_empty() {
        if apps.codex {
            models.codex = Some(CodexModelConfig {
                models: model_list.clone(),
                reasoning_effort: None,
                disable_response_storage: None,
            });
        }
        if apps.open_code {
            models.open_code = Some(OpenCodeModelConfig {
                models: model_list.clone(),
            });
        }
        if apps.claude_code {
            models.claude_code = Some(ClaudeCodeModelConfig {
                models: model_list.clone(),
            });
        }
        if !apps.codex && !apps.open_code && !apps.claude_code {
            eprintln!("keybearer: warning: --model ignored because no app is enabled");
        }
    }
    Ok((
        id,
        ProviderProfile {
            name,
            provider_kind,
            apps,
            base_url,
            api_key,
            models,
            meta: Default::default(),
        },
    ))
}

fn valid_profile_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn profile_from_agent_payload(
    cursor: &mut PacketCursor<'_>,
) -> io::Result<(String, ProviderProfile)> {
    let id = cursor.read_string_utf8()?;
    let provider_kind = ProviderKind::parse(&cursor.read_string_utf8()?)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid provider kind"))?;
    let api_key = cursor.read_string_utf8()?;
    let name = cursor.read_string_utf8()?;
    let base_url = match cursor.read_string_utf8()? {
        value if value.is_empty() => None,
        value => Some(value),
    };
    let apps_csv = cursor.read_string_utf8()?;
    let model_csv = cursor.read_string_utf8()?;
    let model_list: Vec<String> = model_csv
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut apps = ProviderApps::default();
    for app in apps_csv.split(',').filter(|part| !part.is_empty()) {
        match AppType::parse(app) {
            Some(AppType::Codex) => apps.codex = true,
            Some(AppType::OpenCode) => apps.open_code = true,
            Some(AppType::ClaudeCode) => apps.claude_code = true,
            None => return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid app")),
        }
    }
    let mut models = ProviderModels::default();
    if !model_list.is_empty() {
        if apps.codex {
            models.codex = Some(CodexModelConfig {
                models: model_list.clone(),
                reasoning_effort: None,
                disable_response_storage: None,
            });
        }
        if apps.open_code {
            models.open_code = Some(OpenCodeModelConfig {
                models: model_list.clone(),
            });
        }
        if apps.claude_code {
            models.claude_code = Some(ClaudeCodeModelConfig {
                models: model_list.clone(),
            });
        }
    }
    Ok((
        id,
        ProviderProfile {
            name,
            provider_kind,
            apps,
            base_url,
            api_key,
            models,
            meta: Default::default(),
        },
    ))
}

fn send_agent_profile(
    socket: &Path,
    profile_id: &str,
    profile: &ProviderProfile,
) -> io::Result<()> {
    let mut body = Vec::new();
    body.push(SSH_AGENTC_EXTENSION);
    body.extend(encode_string(EXT_ADD_PROVIDER.as_bytes()));
    body.extend(encode_string(profile_id.as_bytes()));
    body.extend(encode_string(profile.provider_kind.as_str().as_bytes()));
    body.extend(encode_string(profile.api_key.as_bytes()));
    body.extend(encode_string(profile.name.as_bytes()));
    body.extend(encode_string(
        profile.base_url.as_deref().unwrap_or("").as_bytes(),
    ));
    body.extend(encode_string(profile.apps.csv().as_bytes()));
    let model_csv = profile
        .models
        .codex
        .as_ref()
        .filter(|c| !c.models.is_empty())
        .map(|c| c.models.join(","))
        .or_else(|| {
            profile
                .models
                .open_code
                .as_ref()
                .filter(|c| !c.models.is_empty())
                .map(|c| c.models.join(","))
        })
        .or_else(|| {
            profile
                .models
                .claude_code
                .as_ref()
                .filter(|c| !c.models.is_empty())
                .map(|c| c.models.join(","))
        })
        .unwrap_or_default();
    body.extend(encode_string(model_csv.as_bytes()));
    let response = agent_request(socket, &body)?;
    if response.first().copied() == Some(SSH_AGENT_SUCCESS) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "agent rejected provider profile",
        ))
    }
}

fn notify_agent_reload() -> bool {
    let Some(socket) = env::var_os("KEYBEARER_CONTROL_SOCK") else {
        return true;
    };
    if !is_unix_socket(Path::new(&socket)) {
        return false;
    }
    let mut body = Vec::new();
    body.push(SSH_AGENTC_EXTENSION);
    body.extend(encode_string(EXT_RELOAD_STORE.as_bytes()));
    agent_request(Path::new(&socket), &body)
        .map(|response| response.first().copied() == Some(SSH_AGENT_SUCCESS))
        .unwrap_or(false)
}

fn ssh_command(args: &[String]) -> io::Result<()> {
    if args.is_empty() {
        eprintln!("Usage: keybearer ssh [ssh-args...] <host>");
        std::process::exit(EXIT_USAGE);
    }

    if args[0] == "--fallback-socket" {
        let socket = env::var_os("KEYBEARER_SOCK")
            .unwrap_or_else(|| exit_unavailable("KEYBEARER_SOCK is not set or not a socket"));
        ensure_unix_socket_or_exit(&PathBuf::from(&socket), "KEYBEARER_SOCK");
        let uid = unsafe { libc::geteuid() };
        let mut ssh_args = vec!["ssh".to_string(), "-R".to_string()];
        ssh_args.push(format!(
            "/tmp/keybearer-{uid}.sock:{}",
            socket.to_string_lossy()
        ));
        ssh_args.extend(args[1..].iter().cloned());
        return exec_child(&ssh_args);
    }

    let socket = env::var_os("SSH_AUTH_SOCK")
        .unwrap_or_else(|| exit_unavailable("SSH_AUTH_SOCK is not set or not a socket"));
    ensure_unix_socket_or_exit(&PathBuf::from(&socket), "SSH_AUTH_SOCK");
    let mut ssh_args = vec!["ssh".to_string(), "-A".to_string()];
    ssh_args.extend(args.iter().cloned());
    exec_child(&ssh_args)
}

fn run_command(args: &[String]) -> io::Result<()> {
    let args = if args.first().map(String::as_str) == Some("--") {
        &args[1..]
    } else {
        args
    };
    if args.is_empty() {
        eprintln!("Usage: keybearer run [--] <command> [args...]");
        std::process::exit(EXIT_USAGE);
    }

    let has_primary = env::var_os("SSH_AUTH_SOCK")
        .map(|path| is_unix_socket(Path::new(&path)))
        .unwrap_or(false);
    let has_fallback = env::var_os("KEYBEARER_SOCK")
        .map(|path| is_unix_socket(Path::new(&path)))
        .unwrap_or(false);
    if !has_primary && !has_fallback {
        eprintln!("keybearer: SSH_AUTH_SOCK is not set or not a socket");
        std::process::exit(EXIT_UNAVAILABLE);
    }

    let (parent_sock, child_sock) = socketpair(
        AddressFamily::Unix,
        SockType::Datagram,
        None,
        SockFlag::empty(),
    )
    .map_err(nix_err)?;

    match unsafe { fork() }.map_err(nix_err)? {
        ForkResult::Parent { child } => {
            drop(child_sock);
            let notify_fd = recv_fd(parent_sock.as_raw_fd())?;
            thread::spawn(move || {
                let _ = supervise(notify_fd.as_raw_fd());
            });
            let wait_status = waitpid(child, None).map_err(nix_err)?;
            exit_like_child(wait_status);
        }
        ForkResult::Child => {
            drop(parent_sock);
            let notify_fd = install_seccomp_filter(env::var_os("KEYBEARER_DEBUG").is_some())?;
            send_fd(child_sock.as_raw_fd(), notify_fd.as_raw_fd())?;
            drop(child_sock);
            drop(notify_fd);
            exec_child(args)?;
        }
    }

    Ok(())
}
fn exec_child(args: &[String]) -> io::Result<()> {
    let Some(command) = args.first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing command",
        ));
    };
    let cmd = CString::new(command.as_str()).map_err(invalid_input)?;
    let argv: Vec<CString> = args
        .iter()
        .map(|arg| CString::new(arg.as_str()).map_err(invalid_input))
        .collect::<io::Result<_>>()?;
    execvp(&cmd, &argv).map_err(nix_err)?;
    unreachable!()
}

fn install_seccomp_filter(debug_reads: bool) -> io::Result<OwnedFd> {
    syscall_check(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) })?;

    let mut filter = seccomp_filter(debug_reads);
    let mut program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };

    let fd = syscall_check(unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            libc::SECCOMP_FILTER_FLAG_NEW_LISTENER,
            &mut program as *mut libc::sock_fprog,
        ) as c_int
    })?;

    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn seccomp_filter(debug_reads: bool) -> Vec<libc::sock_filter> {
    let arch_offset = offset_of!(libc::seccomp_data, arch) as u32;
    let nr_offset = offset_of!(libc::seccomp_data, nr) as u32;
    let mut trapped = vec![libc::SYS_open, libc::SYS_openat, libc::SYS_openat2];

    if debug_reads {
        trapped.extend([
            libc::SYS_read,
            libc::SYS_pread64,
            libc::SYS_readv,
            libc::SYS_preadv,
            libc::SYS_preadv2,
        ]);
    }

    let mut filter = vec![
        stmt(BPF_LD | BPF_W | BPF_ABS, arch_offset),
        jump(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        stmt(BPF_RET | BPF_K, libc::SECCOMP_RET_KILL_PROCESS),
        stmt(BPF_LD | BPF_W | BPF_ABS, nr_offset),
    ];

    for (index, syscall) in trapped.iter().enumerate() {
        filter.push(jump(
            BPF_JMP | BPF_JEQ | BPF_K,
            *syscall as u32,
            (trapped.len() - index) as u8,
            0,
        ));
    }
    filter.push(stmt(BPF_RET | BPF_K, libc::SECCOMP_RET_ALLOW));
    filter.push(stmt(BPF_RET | BPF_K, libc::SECCOMP_RET_USER_NOTIF));
    filter
}
fn supervise(notify_fd: RawFd) -> io::Result<()> {
    loop {
        let mut req: libc::seccomp_notif = unsafe { zeroed() };
        let recv = unsafe { libc::ioctl(notify_fd, SECCOMP_IOCTL_NOTIF_RECV, &mut req) };
        if recv < 0 {
            let error = io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::EINTR)) {
                continue;
            }
            if matches!(error.raw_os_error(), Some(libc::ENOENT)) {
                return Ok(());
            }
            return Err(error);
        }

        if let Err(error) = handle_notification(notify_fd, &req) {
            let mut resp: libc::seccomp_notif_resp = unsafe { zeroed() };
            resp.id = req.id;
            resp.error = -error.raw_os_error().unwrap_or(libc::EIO);
            send_response(notify_fd, &mut resp)?;
        }
    }
}

fn handle_notification(notify_fd: RawFd, req: &libc::seccomp_notif) -> io::Result<()> {
    if is_read_family_syscall(req.data.nr) {
        debug_log(&format_read_debug(req));
        return continue_syscall(notify_fd, req.id);
    }

    let Some(path_ptr) = pathname_arg(req) else {
        return continue_syscall(notify_fd, req.id);
    };

    let path = read_child_c_string(req.pid, path_ptr)?;
    debug_log(&format!(
        "[keybearer:debug] {}: {path}",
        syscall_name(req.data.nr)
    ));

    let Some(config) = templates::app_config_for_path(&path) else {
        return continue_syscall(notify_fd, req.id);
    };
    let remote_base = match config.mode {
        templates::AppConfigMode::Replace => None,
        templates::AppConfigMode::Merge => read_remote_base(&path)?,
    };

    match request_credential(config.app)? {
        CredentialReply::Found(credential) => {
            let Some(contents) =
                templates::render_app_config(&config, &credential, remote_base.as_deref())
            else {
                return continue_syscall(notify_fd, req.id);
            };
            eprintln!("[keybearer] intercepted {path}");
            let file = memfd_with_contents("keybearer-config", &contents)?;
            inject_fd(notify_fd, req.id, file.as_raw_fd())?;
            Ok(())
        }
        CredentialReply::NotFound => continue_syscall(notify_fd, req.id),
        CredentialReply::Denied => {
            eprintln!("keybearer: denied virtual credential for {path}");
            Err(io::Error::from_raw_os_error(libc::EACCES))
        }
    }
}

fn pathname_arg(req: &libc::seccomp_notif) -> Option<u64> {
    match req.data.nr as i64 {
        x if x == libc::SYS_open as i64 => Some(req.data.args[0]),
        x if x == libc::SYS_openat as i64 || x == libc::SYS_openat2 as i64 => {
            Some(req.data.args[1])
        }
        _ => None,
    }
}

fn is_read_family_syscall(number: c_int) -> bool {
    matches!(
        number as i64,
        x if x == libc::SYS_read as i64
            || x == libc::SYS_pread64 as i64
            || x == libc::SYS_readv as i64
            || x == libc::SYS_preadv as i64
            || x == libc::SYS_preadv2 as i64
    )
}

fn format_read_debug(req: &libc::seccomp_notif) -> String {
    let name = syscall_name(req.data.nr);
    let fd = req.data.args[0];
    match req.data.nr as i64 {
        x if x == libc::SYS_read as i64 || x == libc::SYS_pread64 as i64 => {
            format!(
                "[keybearer:debug] {name}(fd={fd}, count={})",
                req.data.args[2]
            )
        }
        x if x == libc::SYS_readv as i64
            || x == libc::SYS_preadv as i64
            || x == libc::SYS_preadv2 as i64 =>
        {
            format!(
                "[keybearer:debug] {name}(fd={fd}, iovcnt={})",
                req.data.args[2]
            )
        }
        _ => format!("[keybearer:debug] {name}(fd={fd})"),
    }
}

fn syscall_name(number: c_int) -> &'static str {
    match number as i64 {
        x if x == libc::SYS_open as i64 => "open",
        x if x == libc::SYS_openat as i64 => "openat",
        x if x == libc::SYS_openat2 as i64 => "openat2",
        x if x == libc::SYS_read as i64 => "read",
        x if x == libc::SYS_pread64 as i64 => "pread64",
        x if x == libc::SYS_readv as i64 => "readv",
        x if x == libc::SYS_preadv as i64 => "preadv",
        x if x == libc::SYS_preadv2 as i64 => "preadv2",
        _ => "syscall",
    }
}

const MAX_REMOTE_BASE_BYTES: u64 = 1024 * 1024;

enum CredentialReply {
    Found(credentials::CredentialResponse),
    NotFound,
    Denied,
}

fn read_remote_base(path: &str) -> io::Result<Option<Vec<u8>>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Ok(None),
    };
    if !metadata.is_file() || metadata.len() > MAX_REMOTE_BASE_BYTES {
        return Ok(None);
    }
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(_) => Ok(None),
    }
}

fn request_credential(app: AppType) -> io::Result<CredentialReply> {
    if let Some(socket) = env::var_os("SSH_AUTH_SOCK") {
        if is_unix_socket(Path::new(&socket)) {
            match request_credential_from_agent(Path::new(&socket), app)? {
                CredentialReply::NotFound => {}
                reply => return Ok(reply),
            }
        }
    }

    Ok(CredentialReply::NotFound)
}

fn request_credential_from_agent(socket: &Path, app: AppType) -> io::Result<CredentialReply> {
    let profile_id = profile_id_from_env();
    let mut body = Vec::new();
    body.push(SSH_AGENTC_EXTENSION);
    body.extend(encode_string(EXT_GET_CREDENTIAL.as_bytes()));
    body.extend(encode_string(app.as_str().as_bytes()));
    body.extend(encode_string(profile_id.as_bytes()));
    let response = agent_request(socket, &body)?;
    match response.first().copied() {
        Some(SSH_AGENT_EXTENSION_RESPONSE) => {
            let mut cursor = PacketCursor::new(&response[1..]);
            let extension = cursor.read_string_utf8()?;
            if extension != EXT_GET_CREDENTIAL {
                return Ok(CredentialReply::Denied);
            }
            let bytes = cursor.read_string()?;
            let Ok(credential) = serde_json::from_slice::<credentials::CredentialResponse>(bytes)
            else {
                return Ok(CredentialReply::Denied);
            };
            if credential.schema_version != credentials::CREDENTIAL_SCHEMA_VERSION {
                return Ok(CredentialReply::Denied);
            }
            Ok(CredentialReply::Found(credential))
        }
        Some(SSH_AGENT_EXTENSION_FAILURE) | Some(SSH_AGENT_FAILURE) => {
            Ok(CredentialReply::NotFound)
        }
        _ => Ok(CredentialReply::Denied),
    }
}

fn profile_id_from_env() -> String {
    env::var("KEYBEARER_PROFILE_ID").unwrap_or_default()
}

fn agent_request(socket: &Path, body: &[u8]) -> io::Result<Vec<u8>> {
    let mut stream = UnixStream::connect(socket)?;
    write_agent_packet(&mut stream, body)?;
    read_agent_packet(&mut stream)
}

fn read_agent_packet(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid ssh-agent packet length",
        ));
    }
    let mut packet = vec![0u8; len];
    stream.read_exact(&mut packet)?;
    Ok(packet)
}

fn write_agent_packet(stream: &mut UnixStream, packet: &[u8]) -> io::Result<()> {
    if packet.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ssh-agent packet too large",
        ));
    }
    stream.write_all(&(packet.len() as u32).to_be_bytes())?;
    stream.write_all(packet)
}

struct PacketCursor<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> PacketCursor<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    fn read_string(&mut self) -> io::Result<&'a [u8]> {
        let len = self.read_u32()? as usize;
        if self.offset + len > self.input.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated ssh-agent string",
            ));
        }
        let out = &self.input[self.offset..self.offset + len];
        self.offset += len;
        Ok(out)
    }

    fn read_string_utf8(&mut self) -> io::Result<String> {
        String::from_utf8(self.read_string()?.to_vec()).map_err(invalid_data)
    }

    fn read_u32(&mut self) -> io::Result<u32> {
        if self.offset + 4 > self.input.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated ssh-agent uint32",
            ));
        }
        let bytes: [u8; 4] = self.input[self.offset..self.offset + 4]
            .try_into()
            .map_err(invalid_data)?;
        self.offset += 4;
        Ok(u32::from_be_bytes(bytes))
    }
}

fn extension_response(extension: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 4 + extension.len() + payload.len());
    out.push(SSH_AGENT_EXTENSION_RESPONSE);
    out.extend(encode_string(extension.as_bytes()));
    out.extend(payload);
    out
}

fn encode_name_list(names: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend((names.len() as u32).to_be_bytes());
    for name in names {
        out.extend(encode_string(name.as_bytes()));
    }
    out
}

fn encode_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend((bytes.len() as u32).to_be_bytes());
    out.extend(bytes);
    out
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn detach_stdio() -> io::Result<()> {
    let devnull = CString::new("/dev/null").map_err(invalid_input)?;
    let fd =
        syscall_check(unsafe { libc::open(devnull.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) })?;
    for target in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        syscall_check(unsafe { libc::dup2(fd, target) })?;
    }
    syscall_check(unsafe { libc::close(fd) })?;
    Ok(())
}

fn ensure_unix_socket_or_exit(path: &Path, name: &str) {
    if !is_unix_socket(path) {
        exit_unavailable(&format!("{name} is not set or not a socket"));
    }
}

fn is_unix_socket(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.file_type().is_socket())
        .unwrap_or(false)
}

fn exit_unavailable(message: &str) -> ! {
    eprintln!("keybearer: {message}");
    std::process::exit(EXIT_UNAVAILABLE);
}

fn read_child_c_string(pid: u32, address: u64) -> io::Result<String> {
    let mem_path = format!("/proc/{pid}/mem");
    let mem = File::open(mem_path)?;
    let mut out = Vec::with_capacity(128);
    let mut cursor = address;
    let mut chunk = [0u8; 256];

    loop {
        let n = std::os::unix::fs::FileExt::read_at(&mem, &mut chunk, cursor)?;
        if n == 0 {
            break;
        }
        if let Some(end) = chunk[..n].iter().position(|&byte| byte == 0) {
            out.extend_from_slice(&chunk[..end]);
            break;
        }
        out.extend_from_slice(&chunk[..n]);
        if out.len() > 4096 {
            break;
        }
        cursor += n as u64;
    }

    String::from_utf8(out).map_err(invalid_data)
}

fn memfd_with_contents(name: &str, contents: &[u8]) -> io::Result<File> {
    let name = CString::new(name).map_err(invalid_input)?;
    let fd = syscall_check(unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) })?;
    let file = unsafe { File::from_raw_fd(fd) };
    std::os::unix::fs::FileExt::write_all_at(&file, contents, 0)?;
    Ok(file)
}

fn inject_fd(notify_fd: RawFd, id: u64, source_fd: RawFd) -> io::Result<()> {
    let mut addfd = libc::seccomp_notif_addfd {
        id,
        flags: libc::SECCOMP_ADDFD_FLAG_SEND as u32,
        srcfd: source_fd as u32,
        newfd: 0,
        newfd_flags: libc::O_CLOEXEC as u32,
    };
    syscall_check(unsafe { libc::ioctl(notify_fd, SECCOMP_IOCTL_NOTIF_ADDFD, &mut addfd) })?;
    Ok(())
}

fn continue_syscall(notify_fd: RawFd, id: u64) -> io::Result<()> {
    let mut resp: libc::seccomp_notif_resp = unsafe { zeroed() };
    resp.id = id;
    resp.flags = libc::SECCOMP_USER_NOTIF_FLAG_CONTINUE as u32;
    send_response(notify_fd, &mut resp)
}

fn send_response(notify_fd: RawFd, resp: &mut libc::seccomp_notif_resp) -> io::Result<()> {
    syscall_check(unsafe { libc::ioctl(notify_fd, SECCOMP_IOCTL_NOTIF_SEND, resp) })?;
    Ok(())
}

fn send_fd(socket: RawFd, fd: RawFd) -> io::Result<()> {
    let bytes = [0u8];
    let iov = [IoSlice::new(&bytes)];
    let fds = [fd];
    let cmsg = [ControlMessage::ScmRights(&fds)];
    sendmsg::<()>(socket, &iov, &cmsg, MsgFlags::empty(), None).map_err(nix_err)?;
    Ok(())
}

fn recv_fd(socket: RawFd) -> io::Result<OwnedFd> {
    let mut bytes = [0u8];
    let mut iov = [IoSliceMut::new(&mut bytes)];
    let mut cmsg_space = nix::cmsg_space!([RawFd; 1]);
    let msg = recvmsg::<()>(socket, &mut iov, Some(&mut cmsg_space), MsgFlags::empty())
        .map_err(nix_err)?;

    for cmsg in msg.cmsgs() {
        if let ControlMessageOwned::ScmRights(fds) = cmsg {
            if let Some(fd) = fds.first() {
                return Ok(unsafe { OwnedFd::from_raw_fd(*fd) });
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "child did not send seccomp notification fd",
    ))
}

fn exit_like_child(status: WaitStatus) -> ! {
    match status {
        WaitStatus::Exited(_, code) => std::process::exit(code),
        WaitStatus::Signaled(_, signal, _) => std::process::exit(128 + signal as i32),
        _ => std::process::exit(1),
    }
}

fn debug_log(line: &str) {
    let Some(path) = env::var_os("KEYBEARER_DEBUG") else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

fn syscall_check(value: c_int) -> io::Result<c_int> {
    if value < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}

fn nix_err(error: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
}

fn invalid_input(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error)
}

fn invalid_data(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
