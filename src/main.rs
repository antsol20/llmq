//! llmq - ask an OpenAI-compatible LLM from your shell prompt.
//!
//! Draws a TUI on /dev/tty and prints the chosen text on stdout, so a shell
//! widget can capture it with command substitution and drop it into the
//! command line buffer.

mod api;
mod config;
mod ui;

use std::fs::OpenOptions;
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub const ACCEPT_PREFIX: &str = "__llmq_accept__:";
pub const ERR_PREFIX: &str = "__llmq_error__:";

const USAGE: &str = "\
usage: llmq [options]

  --context TEXT     current command line, sent as context
  --config PATH      path to config.toml
  --no-popup         never use the tmux popup
  --print-config     dump effective config and exit
  -h, --help         show this message
";

#[derive(Default)]
struct Args {
    context: String,
    config: String,
    popup_out: String,
    no_popup: bool,
    print_config: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let need = |i: usize, flag: &str| -> Result<String, String> {
            argv.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match argv[i].as_str() {
            "--context" => {
                a.context = need(i, "--context")?;
                i += 1;
            }
            "--config" => {
                a.config = need(i, "--config")?;
                i += 1;
            }
            // internal: the tmux popup writes its result here instead of stdout
            "--popup-out" => {
                a.popup_out = need(i, "--popup-out")?;
                i += 1;
            }
            "--no-popup" => a.no_popup = true,
            "--print-config" => a.print_config = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option: {other}")),
        }
        i += 1;
    }
    Ok(a)
}

// --------------------------------------------------------------------------
// plumbing
// --------------------------------------------------------------------------

fn in_tmux_popup_mode(cfg: &config::Config, args: &Args) -> bool {
    cfg.tmux.enabled
        && std::env::var_os("TMUX").is_some()
        && args.popup_out.is_empty()
        && !args.no_popup
}

/// Race-free temp file (O_EXCL, 0600) for handing the answer back out of the popup.
fn make_temp() -> io::Result<PathBuf> {
    let dir = std::env::temp_dir();
    for attempt in 0..64u32 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = dir.join(format!("llmq-{}-{}-{}", std::process::id(), nanos, attempt));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        "could not create a temp file",
    ))
}

fn run_via_tmux(cfg: &config::Config, args: &Args) -> Result<Option<String>, String> {
    let path = make_temp().map_err(|e| format!("temp file: {e}"))?;
    let me = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;

    let mut inner: Vec<String> = vec![me.to_string_lossy().into_owned()];
    inner.push("--popup-out".into());
    inner.push(path.to_string_lossy().into_owned());
    if !args.context.is_empty() {
        inner.push("--context".into());
        inner.push(args.context.clone());
    }
    if !args.config.is_empty() {
        inner.push("--config".into());
        inner.push(args.config.clone());
    }
    let joined = shlex::try_join(inner.iter().map(String::as_str))
        .map_err(|e| format!("cannot quote command: {e}"))?;

    let status = Command::new("tmux")
        .args([
            "display-popup",
            "-E",
            "-w",
            &cfg.tmux.width,
            "-h",
            &cfg.tmux.height,
            "-T",
            cfg.ui.title.trim(),
            &joined,
        ])
        .status();

    let data = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);

    match status {
        Ok(s) if !s.success() && data.is_empty() => {
            return Err(format!("tmux display-popup exited {}", s.code().unwrap_or(-1)))
        }
        Err(e) => return Err(format!("tmux: {e}")),
        _ => {}
    }

    // `display-popup -E` tears the popup down the instant the inner process
    // exits, so anything it printed there was never readable. It hands failures
    // back through the result file instead; surface them now the popup is gone.
    if let Some(msg) = data.strip_prefix(ERR_PREFIX) {
        return Err(msg.to_string());
    }
    Ok(if data.is_empty() { None } else { Some(data) })
}

/// Point fd 1 (and fd 0) at the terminal so the TUI paints there even when our
/// stdout is a capture pipe, keeping the original stdout for the answer.
fn redirect_to_tty() -> Result<i32, String> {
    let saved = unsafe { libc::dup(1) };
    if saved < 0 {
        return Err("cannot duplicate stdout".into());
    }
    for (fd, flags) in [(1, libc::O_WRONLY), (0, libc::O_RDONLY)] {
        if unsafe { libc::isatty(fd) } == 1 {
            continue;
        }
        let tty = unsafe { libc::open(c"/dev/tty".as_ptr(), flags) };
        if tty < 0 {
            return Err("no controlling terminal (/dev/tty)".into());
        }
        unsafe {
            libc::dup2(tty, fd);
            libc::close(tty);
        }
    }
    Ok(saved)
}

fn write_fd(fd: i32, s: &str) {
    let bytes = s.as_bytes();
    let mut off = 0;
    while off < bytes.len() {
        let n = unsafe {
            libc::write(
                fd,
                bytes[off..].as_ptr() as *const libc::c_void,
                bytes.len() - off,
            )
        };
        if n <= 0 {
            break;
        }
        off += n as usize;
    }
}

/// Report a failure the way the caller can actually see it: through the result
/// file when we are inside a popup, on stderr when we are not.
fn report(args: &Args, msg: &str) {
    if args.popup_out.is_empty() {
        eprintln!("llmq: {msg}");
    } else {
        let _ = std::fs::write(&args.popup_out, format!("{ERR_PREFIX}{msg}"));
    }
}

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("llmq: {e}\n\n{USAGE}");
            return 2;
        }
    };

    // A panic would otherwise leave the terminal in raw mode, and inside a
    // popup it would vanish with the popup. Restore, then route the message.
    {
        let popup_out = args.popup_out.clone();
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = crossterm::terminal::disable_raw_mode();
            let _ = crossterm::execute!(
                io::stdout(),
                crossterm::terminal::LeaveAlternateScreen,
                crossterm::cursor::Show
            );
            let msg = format!("panic: {info}");
            if popup_out.is_empty() {
                eprintln!("llmq: {msg}");
            } else {
                let _ = std::fs::write(&popup_out, format!("{ERR_PREFIX}{msg}"));
            }
            prev(info);
        }));
    }

    let cfg_path = (!args.config.is_empty()).then(|| PathBuf::from(&args.config));
    let cfg = match config::load(cfg_path.as_deref().map(Path::new)) {
        Ok(c) => c,
        Err(e) => {
            if args.print_config {
                // still useful: show what the defaults are
                println!(
                    "{}",
                    serde_json::to_string_pretty(&config::Config::default()).unwrap()
                );
                return 0;
            }
            // stderr, not stdout: stdout is the pipe the shell pastes into.
            eprintln!("llmq: {e}");
            return 2;
        }
    };

    if args.print_config {
        println!("{}", serde_json::to_string_pretty(&cfg).unwrap());
        return 0;
    }

    if in_tmux_popup_mode(&cfg, &args) {
        return match run_via_tmux(&cfg, &args) {
            Ok(Some(result)) => {
                print!("{result}");
                let _ = io::stdout().flush();
                0
            }
            Ok(None) => 0,
            Err(e) => {
                eprintln!("llmq: {e}");
                1
            }
        };
    }

    let saved_out = match redirect_to_tty() {
        Ok(fd) => fd,
        Err(e) => {
            report(&args, &e);
            return 1;
        }
    };

    let mut app = ui::Ui::new(cfg, args.context.clone());
    let result = match app.run() {
        Ok(r) => r,
        Err(e) => {
            report(&args, &format!("{e}"));
            return 1;
        }
    };

    let Some(result) = result else { return 0 };
    if !args.popup_out.is_empty() {
        if let Err(e) = std::fs::write(&args.popup_out, &result) {
            eprintln!("llmq: {e}");
            return 1;
        }
    } else {
        write_fd(saved_out, &result);
    }
    0
}
