use clap::{ArgAction, Parser};
use rand::Rng;
use regex::Regex;
use std::env;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Retry wrapper for the official Claude CLI/EXE.
///
/// Usage examples:
///   rusty-claude                          # interactive (TTY, no args)
///   echo '{"...": "..."}' | rusty-claude -- --json
///   rusty-claude -- --help               # pass args after `--` to the child CLI
#[derive(Parser, Debug)]
#[command(name="rusty-claude", about="A retry wrapper for the official Claude CLI/EXE", version=env!("CARGO_PKG_VERSION"))]
struct Cli {
    /// Command to run (default: `claude` on Linux, `claude.exe` on Windows)
    #[arg(long)]
    cmd: Option<String>,

    /// Arguments to pass to the real CLI (everything after `--` goes here)
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,

    /// Max retries on overload/429/5xx/network errors (non-interactive only)
    #[arg(long, default_value_t = 6)]
    max_retries: u32,

    /// Base backoff (ms)
    #[arg(long, default_value_t = 500)]
    base_delay_ms: u64,

    /// Max backoff cap (ms)
    #[arg(long, default_value_t = 20_000)]
    max_delay_ms: u64,

    /// Retry even when no overload pattern matches (any non-zero exit)
    #[arg(long, action = ArgAction::SetTrue)]
    retry_on_any_error: bool,

    /// Force tee piping even if TTY interactive (debug capture); default off
    #[arg(long, action = ArgAction::SetTrue)]
    force_tee: bool,

    /// Extra retry regex patterns (pipe-separated). ENV override: CLAUDE_SUPERVISOR_PATTERNS
    #[arg(long)]
    patterns: Option<String>,
}

fn default_cmd() -> String {
    #[cfg(windows)]
    {
        "claude.exe".to_string()
    }
    #[cfg(not(windows))]
    {
        "claude".to_string()
    }
}

fn backoff_ms(attempt: u32, base: u64, cap: u64) -> u64 {
    let exp = base.saturating_mul(2u64.saturating_pow(attempt));
    let upper = cap.min(exp).max(base);
    let mut rng = rand::rng();
    base + rng.random_range(0..=upper.saturating_sub(base)) + rng.random_range(0..=250)
    // small extra jitter
}

fn compile_patterns(extra: Option<String>) -> Vec<Regex> {
    // Own the strings to avoid lifetime issues, then compile
    let mut pats: Vec<String> = vec![
        "(?i)overloaded".into(),
        r"(?i)HTTP\s*500".into(),
        r"(?i)\b5\d\d\s*(Server\s*Error|Error)\b".into(),
        r"(?i)status\s*code\s*=\s*5\d\d".into(),
        r"(?i)Too\s*Many\s*Requests".into(),
        r"(?i)\b429\b".into(),
        r"(?i)ECONNRESET".into(),
        r"(?i)ETIMEDOUT".into(),
        r"(?i)Gateway\s*Timeout".into(),
        r"(?i)upstream\s*timeout".into(),
        r"(?i)temporary\s*failure".into(),
        r"(?i)(fetch|network)\s*error".into(),
        r"(?i)socket\s*hang\s*up".into(),
    ];

    if let Ok(env_pats) = std::env::var("CLAUDE_SUPERVISOR_PATTERNS") {
        for s in env_pats
            .split('|')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            pats.push(s.to_owned());
        }
    }
    if let Some(cli_pats) = extra {
        for s in cli_pats
            .split('|')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            pats.push(s.to_owned());
        }
    }
    pats.into_iter()
        .filter_map(|p| Regex::new(&p).ok())
        .collect()
}

fn find_retry_after_ms(text: &str) -> Option<u64> {
    // Look for "Retry-After: N" (seconds). The Node CLI may print this if it surfaces headers.
    if let Ok(re) = Regex::new(r"(?i)Retry-After:\s*(\d+)\b") {
        if let Some(c) = re.captures(text) {
            if let Some(s) = c.get(1) {
                if let Ok(n) = s.as_str().parse::<u64>() {
                    return Some(n * 1000);
                }
            }
        }
    }
    None
}

fn should_retry(
    output: &str,
    exit_code: Option<i32>,
    retry_on_any: bool,
    regexes: &[Regex],
) -> (bool, Option<u64>) {
    for re in regexes {
        if re.is_match(output) {
            return (true, find_retry_after_ms(output));
        }
    }
    if retry_on_any {
        if let Some(code) = exit_code {
            if code != 0 {
                return (true, None);
            }
        } else {
            return (true, None);
        }
    }
    (false, None)
}

/// Read a pipe, write through to dst (stdout/stderr), and buffer for later inspection.
fn tee_reader(
    mut src: impl Read + Send + 'static,
    mut dst: impl Write + Send + 'static,
) -> thread::JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 8192];
        loop {
            match src.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    dst.write_all(&tmp[..n])?;
                    dst.flush()?;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(buf)
    })
}

fn main() -> io::Result<()> {
    let mut cli = Cli::parse();

    // Env overrides for convenience
    if let Ok(v) = env::var("CLAUDE_SUPERVISOR_MAX_RETRIES") {
        if let Ok(n) = v.parse::<u32>() {
            cli.max_retries = n;
        }
    }
    if let Ok(v) = env::var("CLAUDE_SUPERVISOR_BASE_MS") {
        if let Ok(n) = v.parse::<u64>() {
            cli.base_delay_ms = n;
        }
    }
    if let Ok(v) = env::var("CLAUDE_SUPERVISOR_CAP_MS") {
        if let Ok(n) = v.parse::<u64>() {
            cli.max_delay_ms = n;
        }
    }

    let real_cmd = cli.cmd.clone().unwrap_or_else(default_cmd);
    let retry_regexes = compile_patterns(cli.patterns.clone());

    // If stdin is piped, capture it once to replay on retries
    let mut stdin_buf = Vec::new();
    let stdin_is_tty = atty::is(atty::Stream::Stdin);
    if !stdin_is_tty {
        io::stdin().read_to_end(&mut stdin_buf)?;
    }

    // Decide mode:
    // - Interactive if: stdin is TTY, no child args, and not forcing tee
    // - Otherwise non-interactive (piped/child args present/forced tee)
    let interactive = stdin_buf.is_empty() && stdin_is_tty && cli.args.is_empty() && !cli.force_tee;

    if stdin_buf.is_empty() && cli.args.is_empty() && !stdin_is_tty {
        eprintln!(
            "[rusty-claude] No stdin and no child args. \
            To run interactive mode, invoke from a TTY (no pipe). \
            To run non-interactive mode, provide child args after `--` (e.g., -- --json)."
        );
    }

    for attempt in 0..=cli.max_retries {
        let mut cmd = Command::new(&real_cmd);
        cmd.args(&cli.args).envs(env::vars());

        if interactive {
            cmd.stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        } else {
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
            if stdin_buf.is_empty() {
                cmd.stdin(Stdio::inherit());
            } else {
                cmd.stdin(Stdio::piped());
            }
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[rusty-claude] failed to spawn `{}`: {e}", real_cmd);
                std::process::exit(127);
            }
        };

        // If we captured stdin, replay it
        if !stdin_buf.is_empty() {
            if let Some(mut child_stdin) = child.stdin.take() {
                child_stdin.write_all(&stdin_buf)?;
                drop(child_stdin); // EOF
            }
        }

        if interactive {
            // In interactive mode, just wait and return child's exit code
            let status = child.wait()?;
            if status.success() {
                return Ok(());
            }

            if attempt == cli.max_retries {
                std::process::exit(status.code().unwrap_or(1));
            }
            let wait = backoff_ms(attempt, cli.base_delay_ms, cli.max_delay_ms);
            eprintln!(
                "[rusty-claude] attempt={} interactive process failed (code={:?}); retrying in {}ms",
                attempt + 1, status.code(), wait
            );
            thread::sleep(Duration::from_millis(wait));
            continue;
        }

        // Non-interactive: tee outputs and decide to retry based on content/exit code.
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let stdout_handle = tee_reader(stdout, io::stdout());
        let stderr_handle = tee_reader(stderr, io::stderr());

        let status = child.wait()?;

        // Join readers & collect buffers for pattern matching
        let out_buf = stdout_handle.join().unwrap_or_else(|_| Ok(Vec::new()))?;
        let err_buf = stderr_handle.join().unwrap_or_else(|_| Ok(Vec::new()))?;
        let combined_text = {
            let mut s = String::from_utf8_lossy(&out_buf).to_string();
            s.push('\n');
            s.push_str(&String::from_utf8_lossy(&err_buf));
            s
        };

        if status.success() {
            // Success: return same exit code (0)
            return Ok(());
        }

        let code = status.code();
        let (retry, retry_after_ms) =
            should_retry(&combined_text, code, cli.retry_on_any_error, &retry_regexes);
        if !retry || attempt == cli.max_retries {
            // Final failure: exit with the child's code
            std::process::exit(code.unwrap_or(1));
        }

        let wait = retry_after_ms
            .unwrap_or_else(|| backoff_ms(attempt, cli.base_delay_ms, cli.max_delay_ms));
        eprintln!(
            "[rusty-claude] attempt={} failed (code={:?}); retrying in {}ms",
            attempt + 1,
            code,
            wait
        );
        thread::sleep(Duration::from_millis(wait));
    }

    Ok(())
}
