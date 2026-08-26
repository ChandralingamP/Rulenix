#[cfg(not(unix))]
fn main() {
    eprintln!("rulenix-egress-helper is supported only on Linux");
    std::process::exit(1);
}

#[cfg(unix)]
mod linux {
    use serde::{Deserialize, Serialize};
    use std::{
        collections::BTreeSet,
        fs, io,
        net::{Ipv4Addr, ToSocketAddrs},
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        process::Command,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::{UnixListener, UnixStream},
        sync::Mutex,
    };

    #[derive(Clone)]
    struct Settings {
        socket: PathBuf,
        interface: String,
        netplan_file: PathBuf,
        state_file: PathBuf,
        socket_uid: u32,
        verify_url: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        operation: String,
        ip_address: String,
    }

    #[derive(Serialize)]
    struct Response {
        ok: bool,
        configured: bool,
        verified: bool,
        observed_ip: Option<String>,
        message: String,
    }

    fn public_ipv4(value: &str) -> Result<Ipv4Addr, String> {
        let ip: Ipv4Addr = value
            .trim()
            .parse()
            .map_err(|_| "invalid IPv4 address".to_owned())?;
        let o = ip.octets();
        let invalid = ip.is_unspecified()
            || ip.is_loopback()
            || ip.is_private()
            || ip.is_link_local()
            || ip.is_multicast()
            || ip == Ipv4Addr::BROADCAST
            || o[0] == 0
            || o[0] >= 240
            || (o[0] == 100 && (64..=127).contains(&o[1]))
            || (o[0] == 192 && o[1] == 0 && matches!(o[2], 0 | 2))
            || (o[0] == 198 && matches!(o[1], 18 | 19))
            || (o[0] == 198 && o[1] == 51 && o[2] == 100)
            || (o[0] == 203 && o[1] == 0 && o[2] == 113);
        if invalid {
            Err("address is not a globally routable public IPv4".into())
        } else {
            Ok(ip)
        }
    }

    fn run_command(program: &str, args: &[&str]) -> Result<String, String> {
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|error| format!("could not execute {program}: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            let stderr: String = String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(512)
                .collect();
            Err(format!("{program} failed: {}", stderr.trim()))
        }
    }

    fn address_present(interface: &str, ip: Ipv4Addr) -> bool {
        run_command("/usr/sbin/ip", &["-4", "address", "show", "dev", interface]).is_ok_and(
            |output| {
                output
                    .lines()
                    .any(|line| line.contains(&format!("inet {ip}/")))
            },
        )
    }

    fn load_managed(path: &Path) -> Result<BTreeSet<Ipv4Addr>, String> {
        let mut values = BTreeSet::new();
        let raw = match fs::read_to_string(path) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(values),
            Err(error) => return Err(format!("could not read managed address state: {error}")),
        };
        for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
            values.insert(public_ipv4(line)?);
        }
        Ok(values)
    }

    fn atomic_write(path: &Path, contents: &str, mode: u32) -> Result<(), String> {
        let parent = path
            .parent()
            .ok_or_else(|| "managed path has no parent".to_owned())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create state directory: {error}"))?;
        let temporary = parent.join(format!(
            ".{}.tmp-{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("egress"),
            std::process::id()
        ));
        fs::write(&temporary, contents)
            .map_err(|error| format!("could not write temporary configuration: {error}"))?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))
            .map_err(|error| format!("could not protect temporary configuration: {error}"))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("could not activate configuration: {error}"))
    }

    fn persist(settings: &Settings, addresses: &BTreeSet<Ipv4Addr>) -> Result<(), String> {
        let state = addresses
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let yaml_addresses = addresses
            .iter()
            .map(|ip| format!("        - \"{ip}/32\""))
            .collect::<Vec<_>>()
            .join("\n");
        let yaml = format!(
            "# Managed only by rulenix-egress-helper. Addresses are never automatically removed.\nnetwork:\n  version: 2\n  ethernets:\n    {}:\n      addresses:\n{}\n",
            settings.interface, yaml_addresses
        );
        let previous = fs::read(&settings.netplan_file).ok();
        if settings.netplan_file.exists() {
            let backup_dir = settings
                .state_file
                .parent()
                .unwrap_or(Path::new("/var/lib/rulenix-egress"))
                .join("backups");
            fs::create_dir_all(&backup_dir)
                .map_err(|error| format!("could not create backup directory: {error}"))?;
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            fs::copy(
                &settings.netplan_file,
                backup_dir.join(format!("netplan-{stamp}.yaml")),
            )
            .map_err(|error| format!("could not back up managed netplan: {error}"))?;
        }
        atomic_write(&settings.netplan_file, &yaml, 0o600)?;
        if let Err(error) = run_command("/usr/sbin/netplan", &["generate"]) {
            if let Some(previous) = previous {
                fs::write(&settings.netplan_file, previous).map_err(|restore| {
                    format!("persistent netplan validation failed ({error}); backup restore also failed: {restore}")
                })?;
            } else {
                fs::remove_file(&settings.netplan_file).map_err(|restore| {
                    format!("persistent netplan validation failed ({error}); invalid file cleanup also failed: {restore}")
                })?;
            }
            let _ = run_command("/usr/sbin/netplan", &["generate"]);
            return Err(format!("persistent netplan validation failed: {error}"));
        }
        atomic_write(&settings.state_file, &state, 0o600)
    }

    fn backend_pid() -> Result<String, String> {
        let name = run_command(
            "/usr/bin/docker",
            &[
                "ps",
                "--filter",
                "label=com.docker.compose.project=rulenix",
                "--filter",
                "label=com.docker.compose.service=backend",
                "--format",
                "{{.Names}}",
            ],
        )?
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
        if name.is_empty() {
            return Err("Rulenix backend container is not running".into());
        }
        let pid = run_command(
            "/usr/bin/docker",
            &["inspect", "--format", "{{.State.Pid}}", &name],
        )?;
        if pid.parse::<u32>().ok().filter(|value| *value > 1).is_none() {
            return Err("Rulenix backend container has no valid network namespace".into());
        }
        Ok(pid)
    }

    fn configure_container(pid: &str, ip: Ipv4Addr) -> Result<(), String> {
        let address = format!("{ip}/32");
        let shown = run_command(
            "/usr/bin/nsenter",
            &[
                "--target",
                pid,
                "--net",
                "--",
                "/usr/sbin/ip",
                "-4",
                "address",
                "show",
                "dev",
                "lo",
            ],
        )?;
        if !shown
            .lines()
            .any(|line| line.contains(&format!("inet {ip}/")))
        {
            run_command(
                "/usr/bin/nsenter",
                &[
                    "--target",
                    pid,
                    "--net",
                    "--",
                    "/usr/sbin/ip",
                    "address",
                    "add",
                    &address,
                    "dev",
                    "lo",
                ],
            )?;
        }
        Ok(())
    }

    fn verify(pid: &str, ip: Ipv4Addr, verify_url: &str) -> Result<String, String> {
        let parsed = url::Url::parse(verify_url)
            .map_err(|_| "invalid helper verification URL".to_owned())?;
        if parsed.scheme() != "https" {
            return Err("helper verification URL must use HTTPS".into());
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| "verification URL has no host".to_owned())?;
        let resolved = (host, 443)
            .to_socket_addrs()
            .map_err(|error| format!("verification DNS failed: {error}"))?
            .find(|address| address.is_ipv4())
            .ok_or_else(|| "verification host has no IPv4 address".to_owned())?;
        let resolve = format!("{host}:443:{}", resolved.ip());
        let output = run_command(
            "/usr/bin/nsenter",
            &[
                "--target",
                pid,
                "--net",
                "--",
                "/usr/bin/curl",
                "-4",
                "--fail",
                "--silent",
                "--show-error",
                "--max-time",
                "15",
                "--interface",
                &ip.to_string(),
                "--resolve",
                &resolve,
                verify_url,
            ],
        )?;
        if output.trim() != ip.to_string() {
            return Err(format!(
                "external verifier observed an unexpected source address: {}",
                output.trim()
            ));
        }
        Ok(output.trim().to_owned())
    }

    fn process(settings: &Settings, request: Request) -> Response {
        if request.operation != "configure_and_verify" {
            return Response {
                ok: false,
                configured: false,
                verified: false,
                observed_ip: None,
                message: "unsupported operation".into(),
            };
        }
        let ip = match public_ipv4(&request.ip_address) {
            Ok(value) => value,
            Err(message) => {
                return Response {
                    ok: false,
                    configured: false,
                    verified: false,
                    observed_ip: None,
                    message,
                };
            }
        };
        let already_on_host = address_present(&settings.interface, ip);
        if !already_on_host {
            let mut managed = match load_managed(&settings.state_file) {
                Ok(value) => value,
                Err(message) => {
                    return Response {
                        ok: false,
                        configured: false,
                        verified: false,
                        observed_ip: None,
                        message,
                    };
                }
            };
            managed.insert(ip);
            if let Err(message) = run_command(
                "/usr/sbin/ip",
                &[
                    "address",
                    "add",
                    &format!("{ip}/32"),
                    "dev",
                    &settings.interface,
                ],
            ) {
                return Response {
                    ok: false,
                    configured: false,
                    verified: false,
                    observed_ip: None,
                    message,
                };
            }
            if let Err(message) = persist(settings, &managed) {
                return Response {
                    ok: false,
                    configured: true,
                    verified: false,
                    observed_ip: None,
                    message,
                };
            }
        }
        let pid = match backend_pid() {
            Ok(value) => value,
            Err(message) => {
                return Response {
                    ok: false,
                    configured: true,
                    verified: false,
                    observed_ip: None,
                    message,
                };
            }
        };
        if let Err(message) = configure_container(&pid, ip) {
            return Response {
                ok: false,
                configured: true,
                verified: false,
                observed_ip: None,
                message,
            };
        }
        match verify(&pid, ip, &settings.verify_url) {
            Ok(observed) => Response {
                ok: true,
                configured: true,
                verified: true,
                observed_ip: Some(observed),
                message: "configured and externally verified".into(),
            },
            Err(message) => Response {
                ok: false,
                configured: true,
                verified: false,
                observed_ip: None,
                message,
            },
        }
    }

    async fn handle(stream: UnixStream, settings: Settings, lock: Arc<Mutex<()>>) {
        let (reader, mut writer) = stream.into_split();
        let mut line = String::new();
        let response = match BufReader::new(reader).read_line(&mut line).await {
            Ok(size) if size > 0 && size <= 8_192 => match serde_json::from_str::<Request>(&line) {
                Ok(request) => {
                    let _guard = lock.lock().await;
                    process(&settings, request)
                }
                Err(_) => Response {
                    ok: false,
                    configured: false,
                    verified: false,
                    observed_ip: None,
                    message: "invalid typed helper request".into(),
                },
            },
            _ => Response {
                ok: false,
                configured: false,
                verified: false,
                observed_ip: None,
                message: "invalid helper request size".into(),
            },
        };
        if let Ok(payload) = serde_json::to_vec(&response) {
            let _ = writer.write_all(&payload).await;
            let _ = writer.write_all(b"\n").await;
        }
    }

    fn settings() -> anyhow::Result<Settings> {
        Ok(Settings {
            socket: std::env::var("RULENIX_EGRESS_SOCKET")
                .unwrap_or_else(|_| "/run/rulenix-egress/helper.sock".into())
                .into(),
            interface: std::env::var("RULENIX_EGRESS_INTERFACE").unwrap_or_else(|_| "ens3".into()),
            netplan_file: std::env::var("RULENIX_EGRESS_NETPLAN_FILE")
                .unwrap_or_else(|_| "/etc/netplan/60-rulenix-egress.yaml".into())
                .into(),
            state_file: std::env::var("RULENIX_EGRESS_STATE_FILE")
                .unwrap_or_else(|_| "/var/lib/rulenix-egress/managed-ipv4".into())
                .into(),
            socket_uid: std::env::var("RULENIX_EGRESS_SOCKET_UID")
                .unwrap_or_else(|_| "10001".into())
                .parse()?,
            verify_url: std::env::var("RULENIX_EGRESS_VERIFY_URL")
                .unwrap_or_else(|_| "https://api.ipify.org".into()),
        })
    }

    pub async fn run() -> anyhow::Result<()> {
        if unsafe { libc::geteuid() } != 0 {
            anyhow::bail!("rulenix-egress-helper must run as root");
        }
        let settings = settings()?;
        let args: Vec<String> = std::env::args().collect();
        if args.len() > 1 {
            if args.len() != 3 || args[1] != "configure" {
                anyhow::bail!("usage: rulenix-egress-helper [configure PUBLIC_IPV4]");
            }
            let response = process(
                &settings,
                Request {
                    operation: "configure_and_verify".into(),
                    ip_address: args[2].clone(),
                },
            );
            println!("{}", serde_json::to_string(&response)?);
            if response.ok {
                return Ok(());
            }
            anyhow::bail!(response.message);
        }
        let parent = settings
            .socket
            .parent()
            .ok_or_else(|| anyhow::anyhow!("socket has no parent"))?;
        fs::create_dir_all(parent)?;
        if settings.socket.exists() {
            fs::remove_file(&settings.socket)?;
        }
        let listener = UnixListener::bind(&settings.socket)?;
        fs::set_permissions(&settings.socket, fs::Permissions::from_mode(0o600))?;
        let owner = format!("{}:0", settings.socket_uid);
        run_command(
            "/usr/bin/chown",
            &[
                &owner,
                settings
                    .socket
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("non-UTF8 socket path"))?,
            ],
        )
        .map_err(anyhow::Error::msg)?;
        let lock = Arc::new(Mutex::new(()));
        loop {
            let (stream, _) = listener.accept().await?;
            tokio::spawn(handle(stream, settings.clone(), lock.clone()));
        }
    }
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    linux::run().await
}
