use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn keybearer_bin() -> &'static str {
    env!("CARGO_BIN_EXE_keybearer")
}

struct TestAgent {
    ssh_auth_sock: String,
    control_sock: String,
    config_dir: PathBuf,
    pid: i32,
}

impl TestAgent {
    fn start() -> Self {
        Self::start_with_config(temp_config_dir())
    }

    fn start_with_config(config_dir: PathBuf) -> Self {
        let output = Command::new(keybearer_bin())
            .arg("agent")
            .env_remove("SSH_AUTH_SOCK")
            .env("KEYBEARER_CONFIG_DIR", &config_dir)
            .output()
            .expect("failed to start keybearer agent");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "agent should start, stderr: {stderr}"
        );

        let ssh_auth_sock = export_value(&stdout, "SSH_AUTH_SOCK");
        let control_sock = export_value(&stdout, "KEYBEARER_CONTROL_SOCK");
        let pid: i32 = export_value(&stdout, "KEYBEARER_AGENT_PID")
            .parse()
            .expect("agent pid should parse");

        for _ in 0..100 {
            if std::os::unix::fs::FileTypeExt::is_socket(
                &std::fs::metadata(&ssh_auth_sock)
                    .expect("agent socket metadata")
                    .file_type(),
            ) && std::os::unix::fs::FileTypeExt::is_socket(
                &std::fs::metadata(&control_sock)
                    .expect("control socket metadata")
                    .file_type(),
            ) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        Self {
            ssh_auth_sock,
            control_sock,
            config_dir,
            pid,
        }
    }

    fn add_provider(&self, provider: &str, key: &str) {
        self.add_profile(provider, provider, key, &["--app", "codex"]);
        self.use_profile("codex", provider);
    }

    fn add_profile(&self, kind: &str, id: &str, key: &str, extra: &[&str]) {
        let mut command = Command::new(keybearer_bin());
        command.arg("add").arg(kind).arg(id).arg(key);
        for arg in extra {
            command.arg(arg);
        }
        let output = command
            .env("SSH_AUTH_SOCK", &self.ssh_auth_sock)
            .env("KEYBEARER_CONTROL_SOCK", &self.control_sock)
            .env("KEYBEARER_CONFIG_DIR", &self.config_dir)
            .output()
            .expect("failed to add provider key");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "add should succeed, stderr: {stderr}"
        );
    }

    fn use_profile(&self, app: &str, id: &str) {
        let output = Command::new(keybearer_bin())
            .arg("use")
            .arg(app)
            .arg(id)
            .env("SSH_AUTH_SOCK", &self.ssh_auth_sock)
            .env("KEYBEARER_CONTROL_SOCK", &self.control_sock)
            .env("KEYBEARER_CONFIG_DIR", &self.config_dir)
            .output()
            .expect("failed to set default profile");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "use should succeed, stderr: {stderr}"
        );
    }
}

impl Drop for TestAgent {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.pid, libc::SIGTERM);
        }
        let _ = std::fs::remove_file(&self.ssh_auth_sock);
        let _ = std::fs::remove_file(&self.control_sock);
        let _ = std::fs::remove_dir_all(&self.config_dir);
    }
}

fn temp_config_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "keybearer-config-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ))
}

fn export_value(output: &str, name: &str) -> String {
    let prefix = format!("{name}=");
    let line = output
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("missing {name} export in {output}"));
    let rest = line.strip_prefix(&prefix).unwrap();
    let Some((quoted, _)) = rest.split_once(';') else {
        panic!("missing semicolon in {name} export: {line}");
    };
    unquote_shell(quoted.trim())
}

fn unquote_shell(value: &str) -> String {
    let value = value
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .unwrap_or(value);
    value.replace("'\\''", "'")
}

fn codex_auth_path() -> PathBuf {
    let codex_dir = dirs::home_dir().unwrap().join(".codex");
    std::fs::create_dir_all(&codex_dir).ok();
    codex_dir.join("auth.json")
}

fn temp_app_path(suffix: &str) -> PathBuf {
    let path = temp_config_dir().join(suffix);
    std::fs::create_dir_all(path.parent().unwrap()).ok();
    path
}

fn send_get_credential(agent: &TestAgent, app: &str, profile_id: &str) -> Vec<u8> {
    let mut stream = UnixStream::connect(&agent.ssh_auth_sock).unwrap();
    let mut body = vec![27];
    body.extend(encode_string(b"get-credential@keybearer.dev"));
    body.extend(encode_string(app.as_bytes()));
    body.extend(encode_string(profile_id.as_bytes()));
    write_agent_packet(&mut stream, &body);
    read_agent_packet(&mut stream)
}

fn credential_json_from_response(response: &[u8]) -> serde_json::Value {
    assert_eq!(response[0], 29);
    let mut offset = 1;
    assert_eq!(
        read_ssh_string(response, &mut offset),
        b"get-credential@keybearer.dev"
    );
    serde_json::from_slice(&read_ssh_string(response, &mut offset)).unwrap()
}

#[test]
fn top_level_usage_exits_64() {
    let output = Command::new(keybearer_bin())
        .output()
        .expect("failed to run keybearer");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(64));
    assert!(
        stderr.contains("Usage: keybearer <agent|add|list|remove|use|check|ssh|run|dry-run> ..."),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn run_usage_exits_64() {
    let output = Command::new(keybearer_bin())
        .arg("run")
        .output()
        .expect("failed to run keybearer run");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(64));
    assert!(
        stderr.contains("Usage: keybearer run [--] <command> [args...]"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn agent_preserves_upstream_ssh_agent() {
    let config_dir = temp_config_dir();
    let upstream_path = std::env::temp_dir().join(format!(
        "keybearer-upstream-{}-{}.sock",
        std::process::id(),
        unique_suffix()
    ));
    let _ = std::fs::remove_file(&upstream_path);
    let upstream = UnixListener::bind(&upstream_path).expect("bind fake upstream agent");
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().expect("accept upstream request");
        let packet = read_agent_packet(&mut stream);
        assert_eq!(packet, vec![11], "proxy should forward request-identities");
        write_agent_packet(&mut stream, &[5]);
    });

    let output = Command::new(keybearer_bin())
        .arg("agent")
        .env("SSH_AUTH_SOCK", &upstream_path)
        .env("KEYBEARER_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to start keybearer agent");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "agent should start, stderr: {stderr}"
    );

    let ssh_auth_sock = export_value(&stdout, "SSH_AUTH_SOCK");
    let exported_upstream = export_value(&stdout, "KEYBEARER_UPSTREAM_SSH_AUTH_SOCK");
    let pid: i32 = export_value(&stdout, "KEYBEARER_AGENT_PID")
        .parse()
        .expect("agent pid should parse");
    assert_eq!(exported_upstream, upstream_path.to_string_lossy());

    let mut proxy = UnixStream::connect(&ssh_auth_sock).expect("connect proxy agent");
    write_agent_packet(&mut proxy, &[11]);
    assert_eq!(read_agent_packet(&mut proxy), vec![5]);

    upstream_thread
        .join()
        .expect("upstream thread should finish");
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let _ = std::fs::remove_file(&ssh_auth_sock);
    let _ = std::fs::remove_file(&upstream_path);
}

#[test]
fn agent_reeval_preserves_original_upstream_ssh_agent() {
    let config_dir = temp_config_dir();
    let upstream_path = std::env::temp_dir().join(format!(
        "keybearer-upstream-reeval-{}-{}.sock",
        std::process::id(),
        unique_suffix()
    ));
    let upstream = UnixListener::bind(&upstream_path).unwrap();
    let upstream_thread = std::thread::spawn(move || {
        let (mut stream, _) = upstream.accept().unwrap();
        assert_eq!(read_agent_packet(&mut stream), vec![11]);
        write_agent_packet(&mut stream, &[5]);
    });

    let first = Command::new(keybearer_bin())
        .arg("agent")
        .env("SSH_AUTH_SOCK", &upstream_path)
        .env("KEYBEARER_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to start first keybearer agent");
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    let first_sock = export_value(&first_stdout, "SSH_AUTH_SOCK");
    let first_pid: i32 = export_value(&first_stdout, "KEYBEARER_AGENT_PID")
        .parse()
        .unwrap();
    let exported_upstream = export_value(&first_stdout, "KEYBEARER_UPSTREAM_SSH_AUTH_SOCK");

    let second = Command::new(keybearer_bin())
        .arg("agent")
        .env("SSH_AUTH_SOCK", &first_sock)
        .env("KEYBEARER_UPSTREAM_SSH_AUTH_SOCK", &exported_upstream)
        .env("KEYBEARER_CONFIG_DIR", &config_dir)
        .output()
        .expect("failed to start second keybearer agent");
    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    let second_sock = export_value(&second_stdout, "SSH_AUTH_SOCK");
    let second_pid: i32 = export_value(&second_stdout, "KEYBEARER_AGENT_PID")
        .parse()
        .unwrap();
    assert_eq!(
        export_value(&second_stdout, "KEYBEARER_UPSTREAM_SSH_AUTH_SOCK"),
        upstream_path.to_string_lossy()
    );

    let mut proxy = UnixStream::connect(&second_sock).expect("connect second proxy agent");
    write_agent_packet(&mut proxy, &[11]);
    assert_eq!(read_agent_packet(&mut proxy), vec![5]);

    upstream_thread
        .join()
        .expect("upstream thread should finish");
    unsafe {
        libc::kill(first_pid, libc::SIGTERM);
        libc::kill(second_pid, libc::SIGTERM);
    }
    let _ = std::fs::remove_file(&first_sock);
    let _ = std::fs::remove_file(&second_sock);
    let _ = std::fs::remove_file(&upstream_path);
}

#[test]
fn agent_d_foreground_uses_explicit_sockets_without_exports() {
    let temp = temp_config_dir();
    let agent_sock = temp.join("agent.sock");
    let control_sock = temp.join("control.sock");
    std::fs::create_dir_all(&temp).expect("create temp config dir");

    let mut child = Command::new(keybearer_bin())
        .arg("agent")
        .arg("-D")
        .arg("-a")
        .arg(&agent_sock)
        .arg("--control-sock")
        .arg(&control_sock)
        .env("KEYBEARER_CONFIG_DIR", &temp)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn foreground agent");

    for _ in 0..100 {
        if is_unix_socket(&agent_sock) && is_unix_socket(&control_sock) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(is_unix_socket(&agent_sock), "agent socket should exist");
    assert!(is_unix_socket(&control_sock), "control socket should exist");

    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let output = child.wait_with_output().expect("wait foreground agent");
    assert!(
        output.stdout.is_empty(),
        "foreground agent should not print exports"
    );

    let _ = std::fs::remove_file(&agent_sock);
    let _ = std::fs::remove_file(&control_sock);
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn agent_eval_accepts_dash_a_and_prints_exports() {
    let temp = temp_config_dir();
    let agent_sock = temp.join("agent.sock");
    std::fs::create_dir_all(&temp).expect("create temp config dir");

    let output = Command::new(keybearer_bin())
        .arg("agent")
        .arg("-a")
        .arg(&agent_sock)
        .env("KEYBEARER_CONFIG_DIR", &temp)
        .output()
        .expect("start eval agent");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "agent should start, stderr: {stderr}"
    );

    assert_eq!(
        export_value(&stdout, "SSH_AUTH_SOCK"),
        agent_sock.to_string_lossy()
    );
    assert!(!export_value(&stdout, "KEYBEARER_CONTROL_SOCK").is_empty());
    let pid: i32 = export_value(&stdout, "KEYBEARER_AGENT_PID")
        .parse()
        .expect("agent pid should parse");
    assert!(
        is_unix_socket(&agent_sock),
        "explicit agent socket should exist"
    );

    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let _ = std::fs::remove_file(&agent_sock);
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn agent_k_restores_upstream_and_kills_current_agent() {
    let temp = temp_config_dir();
    let upstream_path = temp.join("upstream.sock");
    std::fs::create_dir_all(&temp).expect("create temp config dir");
    let upstream = UnixListener::bind(&upstream_path).expect("bind fake upstream agent");

    let output = Command::new(keybearer_bin())
        .arg("agent")
        .env("SSH_AUTH_SOCK", &upstream_path)
        .env("KEYBEARER_CONFIG_DIR", &temp)
        .output()
        .expect("start keybearer agent");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "agent should start, stderr: {stderr}"
    );

    let pid: i32 = export_value(&stdout, "KEYBEARER_AGENT_PID")
        .parse()
        .expect("agent pid should parse");
    let ssh_auth_sock = export_value(&stdout, "SSH_AUTH_SOCK");
    let control_sock = export_value(&stdout, "KEYBEARER_CONTROL_SOCK");
    let upstream_export = export_value(&stdout, "KEYBEARER_UPSTREAM_SSH_AUTH_SOCK");
    assert_eq!(upstream_export, upstream_path.to_string_lossy());

    let kill = Command::new(keybearer_bin())
        .arg("agent")
        .arg("-k")
        .env("KEYBEARER_AGENT_PID", pid.to_string())
        .env("SSH_AUTH_SOCK", &ssh_auth_sock)
        .env("KEYBEARER_CONTROL_SOCK", &control_sock)
        .env("KEYBEARER_UPSTREAM_SSH_AUTH_SOCK", &upstream_export)
        .output()
        .expect("run keybearer agent -k");
    let kill_stdout = String::from_utf8_lossy(&kill.stdout);
    let kill_stderr = String::from_utf8_lossy(&kill.stderr);
    assert!(
        kill.status.success(),
        "agent -k should succeed, stderr: {kill_stderr}"
    );
    assert!(
        kill_stdout.lines().any(|line| {
            line.starts_with("SSH_AUTH_SOCK=")
                && line.contains(upstream_path.to_str().unwrap())
                && line.contains("; export SSH_AUTH_SOCK;")
        }),
        "kill output should restore upstream: {kill_stdout}"
    );
    assert!(kill_stdout.contains("unset KEYBEARER_CONTROL_SOCK;"));
    assert!(kill_stdout.contains("unset KEYBEARER_UPSTREAM_SSH_AUTH_SOCK;"));
    assert!(kill_stdout.contains("unset KEYBEARER_AGENT_PID;"));
    assert!(kill_stdout.contains(&format!("echo Agent pid {pid} killed;")));
    assert!(!kill_stdout.contains("unset SSH_AUTH_SOCK;"));
    assert_process_exits(pid);

    drop(upstream);
    let _ = std::fs::remove_file(&ssh_auth_sock);
    let _ = std::fs::remove_file(&control_sock);
    let _ = std::fs::remove_file(&upstream_path);
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn agent_k_unsets_ssh_auth_sock_when_no_upstream_exists() {
    let temp = temp_config_dir();
    std::fs::create_dir_all(&temp).expect("create temp config dir");

    let output = Command::new(keybearer_bin())
        .arg("agent")
        .env_remove("SSH_AUTH_SOCK")
        .env("KEYBEARER_CONFIG_DIR", &temp)
        .output()
        .expect("start keybearer agent");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "agent should start, stderr: {stderr}"
    );

    let pid: i32 = export_value(&stdout, "KEYBEARER_AGENT_PID")
        .parse()
        .expect("agent pid should parse");
    let ssh_auth_sock = export_value(&stdout, "SSH_AUTH_SOCK");
    let control_sock = export_value(&stdout, "KEYBEARER_CONTROL_SOCK");

    let kill = Command::new(keybearer_bin())
        .arg("agent")
        .arg("-k")
        .env("KEYBEARER_AGENT_PID", pid.to_string())
        .env("SSH_AUTH_SOCK", &ssh_auth_sock)
        .env("KEYBEARER_CONTROL_SOCK", &control_sock)
        .env_remove("KEYBEARER_UPSTREAM_SSH_AUTH_SOCK")
        .output()
        .expect("run keybearer agent -k");
    let kill_stdout = String::from_utf8_lossy(&kill.stdout);
    let kill_stderr = String::from_utf8_lossy(&kill.stderr);
    assert!(
        kill.status.success(),
        "agent -k should succeed, stderr: {kill_stderr}"
    );
    assert!(kill_stdout.contains("unset SSH_AUTH_SOCK;"));
    assert!(kill_stdout.contains("unset KEYBEARER_CONTROL_SOCK;"));
    assert!(kill_stdout.contains("unset KEYBEARER_UPSTREAM_SSH_AUTH_SOCK;"));
    assert!(kill_stdout.contains("unset KEYBEARER_AGENT_PID;"));
    assert!(kill_stdout.contains(&format!("echo Agent pid {pid} killed;")));
    assert_process_exits(pid);

    let _ = std::fs::remove_file(&ssh_auth_sock);
    let _ = std::fs::remove_file(&control_sock);
    let _ = std::fs::remove_dir_all(&temp);
}

fn is_unix_socket(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| std::os::unix::fs::FileTypeExt::is_socket(&metadata.file_type()))
        .unwrap_or(false)
}

fn assert_process_exits(pid: i32) {
    for _ in 0..100 {
        if unsafe { libc::kill(pid, 0) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EPERM) {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("process {pid} should exit after SIGTERM");
}

fn write_agent_packet(stream: &mut UnixStream, packet: &[u8]) {
    stream
        .write_all(&(packet.len() as u32).to_be_bytes())
        .expect("write packet length");
    stream.write_all(packet).expect("write packet body");
}

fn encode_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend((bytes.len() as u32).to_be_bytes());
    out.extend(bytes);
    out
}

fn read_ssh_string(packet: &[u8], offset: &mut usize) -> Vec<u8> {
    let len = u32::from_be_bytes(packet[*offset..*offset + 4].try_into().unwrap()) as usize;
    *offset += 4;
    let out = packet[*offset..*offset + len].to_vec();
    *offset += len;
    out
}

#[test]
fn store_roundtrips_provider_without_printing_key() {
    let config_dir = temp_config_dir();
    let output = Command::new(keybearer_bin())
        .arg("add")
        .arg("openai")
        .arg("work")
        .arg("sk-test")
        .arg("--app")
        .arg("codex")
        .arg("--model")
        .arg("gpt-4o")
        .env("KEYBEARER_CONFIG_DIR", &config_dir)
        .output()
        .expect("run add");
    assert!(
        output.status.success(),
        "add stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store_path = config_dir.join("config.yaml");
    let yaml: serde_yaml::Value =
        serde_yaml::from_slice(&std::fs::read(&store_path).unwrap()).unwrap();
    assert_eq!(yaml["profiles"]["work"]["apiKey"], "sk-test");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&store_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let list = Command::new(keybearer_bin())
        .arg("list")
        .env("KEYBEARER_CONFIG_DIR", &config_dir)
        .output()
        .expect("run list");
    let stdout = String::from_utf8_lossy(&list.stdout);
    let stderr = String::from_utf8_lossy(&list.stderr);
    assert!(stdout.contains("work"));
    assert!(stdout.contains("openai"));
    assert!(stdout.contains("codex"));
    assert!(!stdout.contains("sk-test"));
    assert!(!stderr.contains("sk-test"));
    let _ = std::fs::remove_dir_all(config_dir);
}

#[test]
fn anthropic_profile_does_not_render_codex() {
    let agent = TestAgent::start();
    agent.add_profile("anthropic", "anthropic", "sk-ant-test", &["--app", "claudeCode"]);
    agent.use_profile("claudeCode", "anthropic");
    let output = Command::new(keybearer_bin())
        .arg("run")
        .arg("cat")
        .arg(codex_auth_path())
        .env("SSH_AUTH_SOCK", &agent.ssh_auth_sock)
        .output()
        .expect("failed to run keybearer supervisor");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("[keybearer] intercepted"));
}

#[test]
fn agent_loads_store_and_renders_codex_auth() {
    let config_dir = temp_config_dir();
    let add = Command::new(keybearer_bin())
        .arg("add")
        .arg("openai")
        .arg("work")
        .arg("sk-test")
        .arg("--app")
        .arg("codex")
        .env("KEYBEARER_CONFIG_DIR", &config_dir)
        .output()
        .unwrap();
    assert!(add.status.success());
    assert!(
        Command::new(keybearer_bin())
            .arg("use")
            .arg("codex")
            .arg("work")
            .env("KEYBEARER_CONFIG_DIR", &config_dir)
            .output()
            .unwrap()
            .status
            .success()
    );
    let agent = TestAgent::start_with_config(config_dir);
    let output = Command::new(keybearer_bin())
        .arg("run")
        .arg("cat")
        .arg(codex_auth_path())
        .env("SSH_AUTH_SOCK", &agent.ssh_auth_sock)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        r#"{"OPENAI_API_KEY":"sk-test"}"#
    );
}
#[test]
fn codex_config_toml_is_merged() {
    let agent = TestAgent::start();
    agent.add_profile(
        "openai",
        "work",
        "sk-test",
        &["--app", "codex", "--model", "gpt-4o"],
    );
    agent.use_profile("codex", "work");
    let config_path = temp_app_path(".codex/config.toml");
    std::fs::write(
        &config_path,
        br#"approval_policy = "on-request"
[projects."/work/repo"]
trust_level = "trusted"
[model_providers.existing]
name = "Existing"
base_url = "https://existing.example/v1"
wire_api = "responses"
"#,
    )
    .unwrap();

    let output = Command::new(keybearer_bin())
        .arg("run")
        .arg("cat")
        .arg(config_path)
        .env("SSH_AUTH_SOCK", &agent.ssh_auth_sock)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("approval_policy = \"on-request\""));
    assert!(stdout.contains("[projects.\"/work/repo\"]"));
    assert!(stdout.contains("trust_level = \"trusted\""));
    assert!(stdout.contains("[model_providers.existing]"));
    assert!(stdout.contains("https://existing.example/v1"));
    assert!(stdout.contains("model_provider = \"keybearer-work\""));
    assert!(stdout.contains("model_providers"));
    assert!(stdout.contains("keybearer-work"));
    assert!(stdout.contains("base_url = \"https://api.openai.com/v1\""));
    assert!(!stdout.contains("sk-test"));
}

#[test]
fn malformed_codex_config_continues_original_file() {
    let agent = TestAgent::start();
    agent.add_provider("openai", "sk-test");
    let config_path = temp_app_path(".codex/config.toml");
    let invalid = b"[broken\n";
    std::fs::write(&config_path, invalid).unwrap();

    let output = Command::new(keybearer_bin())
        .arg("run")
        .arg("cat")
        .arg(config_path)
        .env("SSH_AUTH_SOCK", &agent.ssh_auth_sock)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("[keybearer] intercepted"));
    assert_eq!(output.stdout, invalid);
}

#[test]
fn get_credential_uses_explicit_profile_id() {
    let agent = TestAgent::start();
    agent.add_profile("openai", "personal", "sk-personal", &["--app", "codex"]);
    agent.add_profile("openai", "work", "sk-work", &["--app", "codex"]);
    agent.use_profile("codex", "personal");

    let json = credential_json_from_response(&send_get_credential(&agent, "codex", "work"));
    assert_eq!(json["profileId"], "work");
    assert_eq!(json["apiKey"], "sk-work");
    assert_eq!(send_get_credential(&agent, "codex", "unknown"), vec![28]);
}

#[test]
fn get_credential_rejects_profile_not_enabled_for_app() {
    let agent = TestAgent::start();
    agent.add_profile(
        "openai-compatible",
        "oc",
        "sk-oc",
        &[
            "--app",
            "opencode",
            "--base-url",
            "https://api.example.com/v1",
        ],
    );
    assert_eq!(send_get_credential(&agent, "codex", "oc"), vec![28]);
}

#[test]
fn opencode_config_is_merged() {
    let agent = TestAgent::start();
    agent.add_profile(
        "openai-compatible",
        "oc",
        "sk-test",
        &[
            "--app",
            "opencode",
            "--base-url",
            "https://api.example.com/v1",
            "--model",
            "gpt-4o",
        ],
    );
    agent.use_profile("opencode", "oc");
    let path = temp_app_path(".config/opencode/opencode.json");
    std::fs::write(
        &path,
        br#"{
  "$schema": "https://opencode.ai/config.json",
  "mcp": { "existing": { "type": "local" } },
  "provider": {
    "existing": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Existing",
      "options": { "baseURL": "https://existing.example/v1", "apiKey": "remote-placeholder" },
      "models": { "existing-model": { "name": "Existing Model" } }
    }
  }
}
"#,
    )
    .unwrap();

    let output = Command::new(keybearer_bin())
        .arg("run")
        .arg("cat")
        .arg(path)
        .env("SSH_AUTH_SOCK", &agent.ssh_auth_sock)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["mcp"]["existing"]["type"], "local");
    assert_eq!(
        json["provider"]["existing"]["options"]["baseURL"],
        "https://existing.example/v1"
    );
    assert_eq!(
        json["provider"]["keybearer-oc"]["npm"],
        "@ai-sdk/openai-compatible"
    );
    assert_eq!(
        json["provider"]["keybearer-oc"]["options"]["baseURL"],
        "https://api.example.com/v1"
    );
    assert_eq!(
        json["provider"]["keybearer-oc"]["options"]["apiKey"],
        "sk-test"
    );
}

#[test]
fn malformed_opencode_config_continues_original_file() {
    let agent = TestAgent::start();
    agent.add_profile(
        "openai-compatible",
        "oc",
        "sk-test",
        &[
            "--app",
            "opencode",
            "--base-url",
            "https://api.example.com/v1",
        ],
    );
    agent.use_profile("opencode", "oc");
    let path = temp_app_path(".config/opencode/opencode.json");
    let invalid = b"{ not json";
    std::fs::write(&path, invalid).unwrap();

    let output = Command::new(keybearer_bin())
        .arg("run")
        .arg("cat")
        .arg(path)
        .env("SSH_AUTH_SOCK", &agent.ssh_auth_sock)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("[keybearer] intercepted"));
    assert_eq!(output.stdout, invalid);
}

#[test]
fn get_credential_returns_profile_json_for_app() {
    let agent = TestAgent::start();
    agent.add_profile("openai", "work", "sk-test-openai", &["--app", "codex"]);
    agent.use_profile("codex", "work");

    let json = credential_json_from_response(&send_get_credential(&agent, "codex", ""));
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["app"], "codex");
    assert_eq!(json["profileId"], "work");
    assert_eq!(json["providerKind"], "openai");
    assert_eq!(json["apiKey"], "sk-test-openai");
    assert_eq!(json["model"], "gpt-5.5");
}

#[test]
fn get_credential_rejects_unknown_app() {
    let agent = TestAgent::start();
    assert_eq!(send_get_credential(&agent, "unknown", ""), vec![28]);
}

#[test]
fn query_extension_includes_reload_store() {
    let agent = TestAgent::start();
    let mut stream = UnixStream::connect(&agent.ssh_auth_sock).unwrap();
    let mut body = vec![27];
    body.extend(encode_string(b"query"));
    write_agent_packet(&mut stream, &body);
    let response = read_agent_packet(&mut stream);
    assert_eq!(response[0], 29);
    let mut offset = 1;
    assert_eq!(read_ssh_string(&response, &mut offset), b"query");
    let count = u32::from_be_bytes(response[offset..offset + 4].try_into().unwrap());
    offset += 4;
    let mut names = Vec::new();
    for _ in 0..count {
        names.push(String::from_utf8(read_ssh_string(&response, &mut offset)).unwrap());
    }
    assert!(
        names
            .iter()
            .any(|name| name == "reload-store@keybearer.dev")
    );
    assert!(
        names
            .iter()
            .any(|name| name == "get-credential@keybearer.dev")
    );
    assert!(
        !names
            .iter()
            .any(|name| name == &format!("get-{}@keybearer.dev", "file"))
    );
}

fn read_agent_packet(stream: &mut UnixStream) -> Vec<u8> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).expect("read packet length");
    let mut packet = vec![0u8; u32::from_be_bytes(len) as usize];
    stream.read_exact(&mut packet).expect("read packet body");
    packet
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

#[test]
fn supervisor_intercepts_codex_auth() {
    let agent = TestAgent::start();
    agent.add_provider("openai", "sk-test-openai");
    let auth_path = codex_auth_path();

    let output = Command::new(keybearer_bin())
        .arg("run")
        .arg("cat")
        .arg(&auth_path)
        .env("SSH_AUTH_SOCK", &agent.ssh_auth_sock)
        .output()
        .expect("failed to run keybearer supervisor");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "supervised command should succeed, stderr: {stderr}"
    );
    assert!(
        stderr.contains("[keybearer] intercepted"),
        "supervisor should log interception, got stderr: {stderr}"
    );
    assert_eq!(
        stdout.trim(),
        r#"{"OPENAI_API_KEY":"sk-test-openai"}"#,
        "should read virtual auth content"
    );
}

#[test]
fn debug_mode_logs_to_file() {
    let agent = TestAgent::start();
    agent.add_provider("openai", "sk-debug-test");
    let auth_path = codex_auth_path();
    let log_path = std::env::temp_dir().join(format!("keybearer-test-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&log_path);

    let output = Command::new(keybearer_bin())
        .arg("run")
        .arg("cat")
        .arg(&auth_path)
        .env("SSH_AUTH_SOCK", &agent.ssh_auth_sock)
        .env("KEYBEARER_DEBUG", &log_path)
        .output()
        .expect("failed to run keybearer supervisor");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let log_content = std::fs::read_to_string(&log_path).expect("debug log file should exist");

    assert!(
        output.status.success(),
        "supervised command should succeed, stderr: {stderr}"
    );
    assert!(
        log_content.contains("openat:"),
        "debug log should contain open traces, got: {log_content}"
    );
    assert!(
        log_content.contains("read(fd=") || log_content.contains("readv(fd="),
        "debug log should contain read-family traces, got: {log_content}"
    );
    assert!(
        stderr.contains("[keybearer] intercepted"),
        "interception should still go to stderr"
    );

    let _ = std::fs::remove_file(&log_path);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn intercepts_direct_syscall_openat() {
    let agent = TestAgent::start();
    agent.add_provider("openai", "sk-test-openai");
    let helper = build_raw_openat_helper();
    let auth_path = codex_auth_path();

    let output = Command::new(keybearer_bin())
        .arg("run")
        .arg(helper)
        .arg(&auth_path)
        .env("SSH_AUTH_SOCK", &agent.ssh_auth_sock)
        .output()
        .expect("failed to run direct syscall helper");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "direct syscall helper should succeed, stderr: {stderr}"
    );
    assert!(
        stderr.contains("[keybearer] intercepted"),
        "raw openat syscall should be intercepted, got stderr: {stderr}"
    );
    assert_eq!(stdout.trim(), r#"{"OPENAI_API_KEY":"sk-test-openai"}"#);
}

#[cfg(target_arch = "x86_64")]
fn build_raw_openat_helper() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("keybearer-raw-openat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create helper dir");
    let source = dir.join("raw_openat.rs");
    let binary = dir.join("raw_openat");

    std::fs::write(
        &source,
        r#"
use std::arch::asm;
use std::env;
use std::ffi::CString;

fn main() {
    let path = CString::new(env::args().nth(1).expect("path argument")).unwrap();
    let fd = unsafe { openat(path.as_ptr()) };
    if fd < 0 {
        std::process::exit(1);
    }

    let mut buf = [0u8; 4096];
    let n = unsafe { read_fd(fd, buf.as_mut_ptr(), buf.len()) };
    if n < 0 {
        std::process::exit(2);
    }
    unsafe { write_stdout(buf.as_ptr(), n as usize) };
}

unsafe fn openat(path: *const i8) -> isize {
    let ret: isize;
    asm!(
        "syscall",
        inlateout("rax") 257isize => ret,
        in("rdi") -100isize,
        in("rsi") path,
        in("rdx") 0isize,
        in("r10") 0isize,
        lateout("rcx") _,
        lateout("r11") _,
    );
    ret
}

unsafe fn read_fd(fd: isize, buf: *mut u8, len: usize) -> isize {
    let ret: isize;
    asm!(
        "syscall",
        inlateout("rax") 0isize => ret,
        in("rdi") fd,
        in("rsi") buf,
        in("rdx") len,
        lateout("rcx") _,
        lateout("r11") _,
    );
    ret
}

unsafe fn write_stdout(buf: *const u8, len: usize) {
    let _: isize;
    asm!(
        "syscall",
        inlateout("rax") 1isize => _,
        in("rdi") 1isize,
        in("rsi") buf,
        in("rdx") len,
        lateout("rcx") _,
        lateout("r11") _,
    );
}
"#,
    )
    .expect("write helper source");

    let status = Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .status()
        .expect("run rustc for raw openat helper");
    assert!(status.success(), "raw openat helper should compile");

    binary
}
