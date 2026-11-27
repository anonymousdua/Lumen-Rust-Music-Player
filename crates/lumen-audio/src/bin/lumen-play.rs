//! `lumen-play`: command-line test player for the Lumen audio engine.
//!
//! ```text
//! lumen-play [options] <file|folder>...
//! ```
//! Run `lumen-play --help` for options and the interactive commands.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use lumen_audio::{
    Command, Engine, EngineConfig, Event, OutputConfig, PlaybackState, QueueItem, SignalPath,
    list_devices,
};
use lumen_core::{RepeatMode, ReplayGainMode, is_audio_path};

const HELP: &str = "\
lumen-play: test player for the Lumen audio engine

USAGE:
    lumen-play [OPTIONS] <FILE|FOLDER>...

Folders are searched recursively and played in path order.

OPTIONS:
    --list-devices          List output devices and exit
    --device <ID|NAME>      Output device (id, or part of its name); default: system default
    --shared                Use WASAPI shared mode (default: exclusive, bit-perfect)
    --no-gapless            Leave a gap between tracks
    --rg <off|track|album>  ReplayGain (default: off)
    --volume <0-100>        Digital volume (default: 100 = bit-perfect)
    --start <SECONDS>       Start position in the first track
    --shuffle               Shuffle the queue
    --repeat <off|all|one>  Repeat mode (default: off)
    --at <SECONDS>:<CMD>    Run an interactive command at a time (repeatable), e.g. --at 3:s60
    --quiet                 Only print track changes and errors
    -h, --help              Show this help

INTERACTIVE COMMANDS (type, then Enter):
    p            play / pause          n   next          b   previous (restart if > 3 s)
    s <SECONDS>  seek                  f / r   forward / back 10 s
    v <0-100>    volume                sh  toggle shuffle
    rp <off|all|one>  repeat           i   show signal path
    d            list devices          q   quit
";

struct Options {
    paths: Vec<PathBuf>,
    list_devices: bool,
    device: Option<String>,
    shared: bool,
    gapless: bool,
    replaygain: ReplayGainMode,
    volume: f32,
    start: Duration,
    shuffle: bool,
    repeat: RepeatMode,
    script: Vec<(Duration, String)>,
    quiet: bool,
}

fn parse_args() -> Result<Options, String> {
    let mut o = Options {
        paths: Vec::new(),
        list_devices: false,
        device: None,
        shared: false,
        gapless: true,
        replaygain: ReplayGainMode::Off,
        volume: 1.0,
        start: Duration::ZERO,
        shuffle: false,
        repeat: RepeatMode::Off,
        script: Vec::new(),
        quiet: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "--list-devices" => o.list_devices = true,
            "--device" => o.device = Some(value("--device")?),
            "--shared" => o.shared = true,
            "--no-gapless" => o.gapless = false,
            "--quiet" => o.quiet = true,
            "--shuffle" => o.shuffle = true,
            "--rg" => {
                o.replaygain = match value("--rg")?.as_str() {
                    "off" => ReplayGainMode::Off,
                    "track" => ReplayGainMode::Track,
                    "album" => ReplayGainMode::Album,
                    other => return Err(format!("unknown ReplayGain mode {other}")),
                }
            }
            "--repeat" => o.repeat = parse_repeat(&value("--repeat")?)?,
            "--volume" => {
                let v: f32 = value("--volume")?
                    .parse()
                    .map_err(|_| "--volume takes 0-100")?;
                o.volume = (v / 100.0).clamp(0.0, 1.0);
            }
            "--start" => o.start = parse_secs(&value("--start")?)?,
            "--at" => {
                let spec = value("--at")?;
                let (t, cmd) = spec.split_once(':').ok_or("--at takes SECONDS:COMMAND")?;
                o.script.push((parse_secs(t)?, cmd.to_string()));
            }
            s if s.starts_with('-') => return Err(format!("unknown option {s} (try --help)")),
            _ => o.paths.push(PathBuf::from(a)),
        }
    }
    o.script.sort_by_key(|(t, _)| *t);
    Ok(o)
}

fn parse_secs(s: &str) -> Result<Duration, String> {
    let v: f64 = s
        .trim()
        .parse()
        .map_err(|_| format!("not a number of seconds: {s}"))?;
    Ok(Duration::from_secs_f64(v.max(0.0)))
}

fn parse_repeat(s: &str) -> Result<RepeatMode, String> {
    match s.trim() {
        "off" => Ok(RepeatMode::Off),
        "all" => Ok(RepeatMode::All),
        "one" => Ok(RepeatMode::One),
        other => Err(format!("unknown repeat mode {other}")),
    }
}

fn collect_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut found: Vec<PathBuf> = walkdir::WalkDir::new(p)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file() && is_audio_path(e.path()))
                .map(|e| e.into_path())
                .collect();
            found.sort_by_key(|p| p.to_string_lossy().to_lowercase());
            out.extend(found);
        } else {
            out.push(p.clone());
        }
    }
    out
}

fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

fn print_signal_path(sp: &SignalPath) {
    println!(
        "  source : {}, {} ch{}",
        sp.source.format_badge(),
        sp.source.channels,
        sp.source
            .duration
            .map(|d| format!(", {}", fmt_time(d)))
            .unwrap_or_default()
    );
    println!(
        "  output : {} -> {} ({})",
        sp.mode_badge(),
        sp.device_name,
        sp.output
    );
    if let Some(reason) = &sp.exclusive_fallback {
        println!("  note   : exclusive mode not used: {reason}");
    }
    if let Some((from, to)) = sp.resampled {
        println!("  resample: {from} Hz -> {to} Hz (rubato)");
    }
    if let Some(db) = sp.replaygain_db {
        println!("  replaygain: {db:+.2} dB");
    }
    if sp.volume_attenuates {
        println!("  volume : below 100% (digital, changes samples)");
    }
    println!(
        "  bit-perfect: {}",
        if sp.bit_perfect { "YES" } else { "no" }
    );
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("LUMEN_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("lumen-play: {e}");
            std::process::exit(2);
        }
    };

    if opts.list_devices {
        match list_devices() {
            Ok(devices) => {
                for d in devices {
                    let mark = if d.is_default { " (default)" } else { "" };
                    println!("{}{mark}\n    id: {}", d.name, d.id);
                }
            }
            Err(e) => eprintln!("lumen-play: {e}"),
        }
        return;
    }

    let files = collect_files(&opts.paths);
    if files.is_empty() {
        eprint!("{HELP}");
        eprintln!("\nlumen-play: no playable files given");
        std::process::exit(2);
    }

    let device_id = match &opts.device {
        None => None,
        Some(wanted) => {
            let devices = list_devices().unwrap_or_default();
            let needle = wanted.to_lowercase();
            match devices
                .iter()
                .find(|d| d.id == *wanted || d.name.to_lowercase().contains(&needle))
            {
                Some(d) => Some(d.id.clone()),
                None => {
                    eprintln!("lumen-play: no device matches \"{wanted}\" (see --list-devices)");
                    std::process::exit(2);
                }
            }
        }
    };

    let engine = match Engine::start(EngineConfig {
        output: OutputConfig {
            rate: None,
            device_id,
            exclusive: !opts.shared,
        },
        replaygain: opts.replaygain,
        gapless: opts.gapless,
        volume: opts.volume,
        ..Default::default()
    }) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("lumen-play: {e}");
            std::process::exit(1);
        }
    };

    let items: Vec<QueueItem> = files.iter().map(QueueItem::from_path).collect();
    let total = items.len();
    println!(
        "Queue: {total} file(s). Type a command and Enter (p n b s f r v i q); --help lists them."
    );
    if opts.shuffle {
        engine.send(Command::SetShuffle(true));
    }
    engine.send(Command::SetRepeat(opts.repeat));
    engine.send(Command::SetQueue {
        items,
        start: 0,
        position: opts.start,
        play: true,
    });

    let (line_tx, line_rx) = crossbeam_channel::unbounded();
    spawn_stdin_reader(line_tx);
    run(&engine, &opts, &files, line_rx);
}

fn spawn_stdin_reader(tx: Sender<String>) {
    let _ = std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            // EOF (e.g. stdin redirected): keep playing, just no more commands.
        });
}

struct Ui {
    position: Duration,
    duration: Option<Duration>,
    volume: f32,
    shuffle: bool,
    repeat: RepeatMode,
    state: PlaybackState,
    last_signal: Option<SignalPath>,
    status_shown: bool,
    tty: bool,
    quiet: bool,
}

fn run(engine: &Engine, opts: &Options, files: &[PathBuf], lines: Receiver<String>) {
    let started = Instant::now();
    let mut script = opts.script.iter().peekable();
    let mut ui = Ui {
        position: opts.start,
        duration: None,
        volume: opts.volume,
        shuffle: opts.shuffle,
        repeat: opts.repeat,
        state: PlaybackState::Stopped,
        last_signal: None,
        status_shown: false,
        tty: std::io::stdout().is_terminal(),
        quiet: opts.quiet,
    };
    let mut last_status = Instant::now();
    let mut underruns = 0u64;

    loop {
        // Scripted commands.
        while let Some((t, cmd)) = script.peek() {
            if started.elapsed() < *t {
                break;
            }
            ui.clear_status();
            println!("[{:.1}s] > {cmd}", started.elapsed().as_secs_f64());
            if !handle_line(engine, cmd, &mut ui) {
                return;
            }
            script.next();
        }
        // Typed commands.
        while let Ok(line) = lines.try_recv() {
            ui.clear_status();
            if !handle_line(engine, &line, &mut ui) {
                return;
            }
        }

        let event = match engine.events().recv_timeout(Duration::from_millis(50)) {
            Ok(e) => e,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(_) => return,
        };
        match event {
            Event::TrackChanged { index, item, .. } => {
                ui.clear_status();
                let name = item
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                println!("\n> [{}/{}] {name}", index + 1, files.len());
            }
            Event::SignalPath(sp) => {
                ui.clear_status();
                if !ui.quiet {
                    print_signal_path(&sp);
                }
                ui.last_signal = Some(sp);
            }
            Event::Position {
                position, duration, ..
            } => {
                ui.position = position;
                ui.duration = duration;
                if !ui.quiet && last_status.elapsed() >= Duration::from_millis(250) {
                    last_status = Instant::now();
                    ui.show_status(underruns);
                }
            }
            Event::StateChanged(s) => {
                ui.state = s;
                if !ui.quiet {
                    ui.clear_status();
                    println!("  [{s:?}]");
                }
            }
            Event::QueueChanged {
                shuffle, repeat, ..
            } => {
                ui.shuffle = shuffle;
                ui.repeat = repeat;
            }
            Event::TrackSkipped { path, reason, .. } => {
                ui.clear_status();
                println!("  skipped {}: {reason}", path.display());
            }
            Event::Underruns(n) => {
                underruns = n;
                ui.clear_status();
                println!("  ! underrun (device starved), total {n}");
            }
            Event::Devices(devices) => {
                ui.clear_status();
                for d in devices {
                    let mark = if d.is_default { " (default)" } else { "" };
                    println!("  {}{mark}", d.name);
                }
            }
            Event::Error(e) => {
                ui.clear_status();
                eprintln!("  error: {e}");
            }
            Event::EndOfQueue => {
                ui.clear_status();
                println!(
                    "\nEnd of queue. Underruns: {underruns}. Played for {:.1} s.",
                    started.elapsed().as_secs_f64()
                );
                return;
            }
        }
    }
}

impl Ui {
    fn show_status(&mut self, underruns: u64) {
        let line = format!(
            "  {} / {}  vol {:>3}%  {}{}{}",
            fmt_time(self.position),
            self.duration.map(fmt_time).unwrap_or_else(|| "?".into()),
            (self.volume * 100.0).round(),
            if self.shuffle { "shuffle " } else { "" },
            match self.repeat {
                RepeatMode::Off => "",
                RepeatMode::All => "repeat-all ",
                RepeatMode::One => "repeat-one ",
            },
            if underruns > 0 {
                format!("underruns {underruns}")
            } else {
                String::new()
            }
        );
        if self.tty {
            print!("\r{line:<70}");
            let _ = std::io::stdout().flush();
            self.status_shown = true;
        } else if self.position.subsec_millis() < 260 {
            // Piped output: about one line per second.
            println!("{line}");
        }
    }

    fn clear_status(&mut self) {
        if self.status_shown {
            print!("\r{:<70}\r", "");
            let _ = std::io::stdout().flush();
            self.status_shown = false;
        }
    }
}

/// Returns false to quit.
fn handle_line(engine: &Engine, line: &str, ui: &mut Ui) -> bool {
    let line = line.trim();
    let (cmd, arg) = match line.split_once(char::is_whitespace) {
        Some((c, a)) => (c, a.trim()),
        None => {
            // Allow "s60" as well as "s 60".
            let split = line
                .find(|c: char| c.is_ascii_digit())
                .unwrap_or(line.len());
            (&line[..split], line[split..].trim())
        }
    };
    match cmd {
        "" | "p" => engine.send(Command::TogglePause),
        "q" | "quit" => {
            engine.send(Command::Stop);
            return false;
        }
        "n" => engine.send(Command::Next),
        "b" => engine.send(Command::Prev),
        "s" => match parse_secs(arg) {
            Ok(t) => engine.send(Command::Seek(t)),
            Err(e) => println!("  {e}"),
        },
        "f" => engine.send(Command::Seek(ui.position + Duration::from_secs(10))),
        "r" => engine.send(Command::Seek(
            ui.position.saturating_sub(Duration::from_secs(10)),
        )),
        "v" => match arg.parse::<f32>() {
            Ok(v) => {
                ui.volume = (v / 100.0).clamp(0.0, 1.0);
                engine.send(Command::SetVolume(ui.volume));
            }
            Err(_) => println!("  v takes 0-100"),
        },
        "sh" => engine.send(Command::SetShuffle(!ui.shuffle)),
        "rp" => match parse_repeat(arg) {
            Ok(r) => engine.send(Command::SetRepeat(r)),
            Err(e) => println!("  {e}"),
        },
        "i" => match &ui.last_signal {
            Some(sp) => print_signal_path(sp),
            None => println!("  nothing playing"),
        },
        "d" => engine.send(Command::ListDevices),
        "h" | "help" | "?" => print!("{HELP}"),
        other => println!("  unknown command \"{other}\" (h for help)"),
    }
    true
}
