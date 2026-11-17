//! Minimal CLI test client for Milestone 1 (Networking/Login).

use std::env;
use std::io::{self, BufRead, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use furcadia_client::{
    fetch_account_characters, parse_chat_line, ClientCommand, ClientError, ConnectionConfig,
    FurcadiaClient, PounceEvent, PounceMonitor, ServerMessage, SessionState, DEFAULT_HOST,
    DEFAULT_PORT,
};

struct CliArgs {
    host: String,
    port: u16,
    name: Option<String>,
    account: Option<String>,
    password: Option<String>,
    timeout_secs: Option<u64>,
    watch: Vec<String>,
}

fn parse_args() -> Result<CliArgs, String> {
    let mut args = CliArgs {
        host: DEFAULT_HOST.to_owned(),
        port: DEFAULT_PORT,
        name: None,
        account: None,
        password: None,
        timeout_secs: None,
        watch: Vec::new(),
    };

    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "Usage: furc-cli [--host HOST] [--port PORT] [--name CHAR | --account EMAIL] [--password PASS] [--watch NAME ...] [--timeout-secs N]"
                );
                std::process::exit(0);
            }
            "--host" => {
                args.host = iter.next().ok_or("--host requires a value")?;
            }
            "--port" => {
                let raw = iter.next().ok_or("--port requires a value")?;
                args.port = raw.parse().map_err(|_| format!("invalid port: {raw}"))?;
            }
            "--name" => {
                args.name = Some(iter.next().ok_or("--name requires a value")?);
            }
            "--account" => {
                args.account = Some(iter.next().ok_or("--account requires a value")?);
            }
            "--password" => {
                args.password = Some(iter.next().ok_or("--password requires a value")?);
            }
            "--timeout-secs" => {
                let raw = iter.next().ok_or("--timeout-secs requires a value")?;
                args.timeout_secs =
                    Some(raw.parse().map_err(|_| format!("invalid timeout: {raw}"))?);
            }
            "--watch" => {
                let names = iter.next().ok_or("--watch requires a value")?;
                for name in names.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    args.watch.push(name.to_owned());
                }
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(args)
}

enum LocalAction {
    Send(ClientCommand),
    Watch(String),
    Unwatch(String),
    ListPounce,
}

fn parse_stdin_command(line: &str) -> LocalAction {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if let Some(raw_cmd) = trimmed.strip_prefix('`') {
        LocalAction::Send(ClientCommand::Raw(raw_cmd.to_owned()))
    } else if let Some(text) = trimmed.strip_prefix("/say ") {
        LocalAction::Send(ClientCommand::Say(text.to_owned()))
    } else if let Some(rest) = trimmed.strip_prefix("/wh ") {
        if let Some((target, msg)) = rest.split_once(' ') {
            LocalAction::Send(ClientCommand::Whisper {
                target: target.to_owned(),
                message: msg.to_owned(),
            })
        } else {
            LocalAction::Send(ClientCommand::Raw(trimmed.to_owned()))
        }
    } else if let Some(name) = trimmed.strip_prefix("/onln ") {
        LocalAction::Send(ClientCommand::OnlineCheck(name.trim().to_owned()))
    } else if let Some(name) = trimmed.strip_prefix("/glook ") {
        LocalAction::Send(ClientCommand::Glook(name.trim().to_owned()))
    } else if trimmed == "/who" {
        LocalAction::Send(ClientCommand::Who)
    } else if let Some(name) = trimmed.strip_prefix("/watch ") {
        LocalAction::Watch(name.trim().to_owned())
    } else if let Some(name) = trimmed.strip_prefix("/unwatch ") {
        LocalAction::Unwatch(name.trim().to_owned())
    } else if trimmed == "/pounce" {
        LocalAction::ListPounce
    } else if let Some(raw) = trimmed.strip_prefix("/raw ") {
        LocalAction::Send(ClientCommand::Raw(raw.to_owned()))
    } else if trimmed == "/quit" {
        LocalAction::Send(ClientCommand::Quit)
    } else {
        LocalAction::Send(ClientCommand::Say(trimmed.to_owned()))
    }
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(2);
        }
    };

    let config = ConnectionConfig {
        host: args.host.clone(),
        port: args.port,
        ..Default::default()
    };

    println!("Connecting to {}:{}...", config.host, config.port);
    eprintln!("SECURITY NOTICE: game traffic will use legacy TLS without certificate verification; an active attacker could impersonate the server.");
    let mut client = match FurcadiaClient::connect(&config) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("Connection failed: {err}");
            std::process::exit(1);
        }
    };

    println!("Connected. Waiting for Dragonroar handshake...");
    match client.wait_for_handshake() {
        Ok(pre) => {
            for msg in pre {
                println!("<- {msg:?}");
            }
            println!("Handshake complete: {:?}", client.state());
            match client.enable_tls(std::time::Duration::from_secs(12)) {
                Ok(pre_tls) => {
                    for msg in pre_tls {
                        println!("<- {msg:?}");
                    }
                    println!("TLS encryption established; server certificate was not verified.");
                }
                Err(err) => {
                    eprintln!("Secure connection failed; refusing to send credentials: {err}");
                    std::process::exit(1);
                }
            }
        }
        Err(err) => {
            eprintln!("Handshake failed: {err}");
            std::process::exit(1);
        }
    }

    if let Some(password) = args.password {
        let login_res = match (args.account, args.name) {
            (Some(email), Some(name)) => {
                println!("Logging in account '{email}' as '{name}'...");
                client.login_account(email, name, password)
            }
            (Some(email), None) => {
                println!("Fetching characters for '{email}'...");
                match fetch_account_characters(&email, &password) {
                    Ok(info) if info.characters.is_empty() => {
                        eprintln!("No characters found on account '{email}'.");
                        std::process::exit(1);
                    }
                    Ok(info) if info.characters.len() == 1 => {
                        let ch = &info.characters[0];
                        println!("Auto-selected character '{}' (id {}).", ch.name, ch.id);
                        client.login_account(email, ch.name.clone(), password)
                    }
                    Ok(info) => {
                        eprintln!("Multiple characters on '{email}'; re-run with --name <CHAR>:");
                        for ch in info.characters {
                            eprintln!("  - {} (id {})", ch.name, ch.id);
                        }
                        std::process::exit(2);
                    }
                    Err(err) => {
                        eprintln!("Character discovery failed: {err}");
                        std::process::exit(1);
                    }
                }
            }
            (None, Some(name)) => {
                println!("Logging in character '{name}'...");
                client.login_character(name, password)
            }
            (None, None) => {
                eprintln!("--password provided without --name or --account");
                std::process::exit(2);
            }
        };
        if let Err(err) = login_res {
            eprintln!("Login failed: {err}");
            std::process::exit(1);
        }
    } else {
        println!("No credentials supplied; staying in Handshaked mode. Type /raw <cmd> or /quit.");
    }

    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut pounce = PounceMonitor::default();
    for name in &args.watch {
        pounce.add_watch(name);
    }

    let deadline = args
        .timeout_secs
        .map(|s| Instant::now() + Duration::from_secs(s));

    loop {
        let now = Instant::now();
        if let Some(end) = deadline {
            if now >= end {
                println!("Timeout reached; disconnecting.");
                let _ = client.disconnect();
                return;
            }
        }

        if matches!(client.state(), SessionState::Connected) {
            if let Some(target) = pounce.next_due_check(now) {
                if let Err(err) = client.send_command(&ClientCommand::OnlineCheck(target)) {
                    eprintln!("Pounce query error: {err}");
                }
            }
        }

        match client.poll_message() {
            Ok(Some(msg)) => {
                match &msg {
                    ServerMessage::Text(t) => {
                        let chat = parse_chat_line(t);
                        println!("{}", chat.to_ansi());
                    }
                    ServerMessage::LoginAck => println!("[auth] Login acknowledged"),
                    ServerMessage::CharacterIdentity { uid, name } => {
                        println!("[session] Logged in as {name} (uid {uid})");
                    }
                    ServerMessage::MapName(map) => {
                        println!("[dream] Current map: {map}");
                    }
                    ServerMessage::OnlineStatus { online, name } => {
                        match pounce.handle_online_status(name, *online, Instant::now()) {
                            Some(PounceEvent::CameOnline(n)) => {
                                println!("[pounce] {n} came ONLINE!");
                            }
                            Some(PounceEvent::WentOffline(n)) => {
                                println!("[pounce] {n} went OFFLINE.");
                            }
                            Some(PounceEvent::Checked {
                                name: n,
                                online: st,
                            }) => {
                                let label = if st { "ONLINE" } else { "offline" };
                                println!("[pounce] {n} is {label}");
                            }
                            None => {
                                let label = if *online { "ONLINE" } else { "offline" };
                                println!("[onln] {name} is {label}");
                            }
                        }
                    }
                    other => println!("<- {other:?}"),
                }
                let _ = io::stdout().flush();
            }
            Ok(None) => {}
            Err(ClientError::Disconnected) => {
                println!("Server closed connection.");
                break;
            }
            Err(err) => {
                eprintln!("Read error: {err}");
                break;
            }
        }

        while let Ok(line) = rx.try_recv() {
            match parse_stdin_command(&line) {
                LocalAction::Send(cmd) => {
                    let is_quit = matches!(cmd, ClientCommand::Quit);
                    if let Err(err) = client.send_command(&cmd) {
                        eprintln!("Send error: {err}");
                        break;
                    }
                    if is_quit {
                        let _ = client.disconnect();
                        return;
                    }
                }
                LocalAction::Watch(name) => {
                    if pounce.add_watch(&name) {
                        println!("[pounce] Watching '{name}'");
                    } else {
                        println!("[pounce] Already watching '{name}'");
                    }
                }
                LocalAction::Unwatch(name) => {
                    if pounce.remove_watch(&name) {
                        println!("[pounce] Removed '{name}'");
                    } else {
                        println!("[pounce] '{name}' was not in watchlist");
                    }
                }
                LocalAction::ListPounce => {
                    if pounce.is_empty() {
                        println!("[pounce] Watchlist is empty.");
                    } else {
                        for entry in pounce.entries() {
                            println!("[pounce] {}: {:?}", entry.name, entry.status);
                        }
                    }
                }
            }
        }
    }
}
