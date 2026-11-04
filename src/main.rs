mod concat;
mod fs;
mod mp4;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Expose several MP4 clips as ONE virtual MP4 (like `ffmpeg -f concat -c copy`)
/// without writing the merged file: only a synthesized index is kept in RAM,
/// media bytes are read on demand from the original clips.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Mount a read-only FUSE filesystem containing the virtual merged file.
    Mount {
        /// Empty directory to mount on.
        mountpoint: PathBuf,
        #[command(flatten)]
        input: Input,
        /// Name of the virtual file inside the mountpoint.
        #[arg(long, default_value = "merged.mp4")]
        name: String,
        /// Let other users (e.g. a media server) read the mount (needs user_allow_other in /etc/fuse.conf).
        #[arg(long)]
        allow_other: bool,
        /// FUSE worker threads.
        #[arg(long, default_value_t = 4)]
        threads: usize,
    },
    /// Mount several virtual files at once, described by a JSON spec:
    /// {"files":[{"name":"a.mp4","inputs":["/x/1.MP4","/x/2.MP4"],"rotate":180}],
    ///  "tracks":["vide","soun"]}
    /// Prints "READY <mountpoint>" on stdout once the mount is live.
    MountSpec {
        mountpoint: PathBuf,
        spec: PathBuf,
        #[arg(long)]
        allow_other: bool,
        #[arg(long, default_value_t = 4)]
        threads: usize,
    },
    /// Write the virtual file to disk / stdout ("-"), mainly for verification.
    Export {
        output: PathBuf,
        #[command(flatten)]
        input: Input,
    },
    /// Print the virtual layout (clips, start times, size) without mounting.
    Info {
        #[command(flatten)]
        input: Input,
    },
}

#[derive(Args, Clone)]
struct Input {
    /// Input clips and/or directories (directories are scanned for --ext, natural sort).
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// Extension to pick up when scanning directories (case-insensitive).
    #[arg(long, default_value = "mp4")]
    ext: String,
    /// Display rotation in degrees (0/90/180/270), same as ffmpeg -display_rotation.
    #[arg(long, allow_hyphen_values = true)]
    rotate: Option<i32>,
    /// Track handler types to keep (vide, soun, meta, ...).
    #[arg(long, value_delimiter = ',', default_value = "vide,soun")]
    tracks: Vec<String>,
    /// Don't pad clips to equal per-track duration (ffmpeg concat does pad).
    #[arg(long)]
    no_align: bool,
}

#[derive(serde::Deserialize)]
struct Spec {
    files: Vec<SpecFile>,
    #[serde(default)]
    tracks: Vec<String>,
}

#[derive(serde::Deserialize)]
struct SpecFile {
    name: String,
    inputs: Vec<PathBuf>,
    #[serde(default)]
    rotate: Option<i32>,
}

fn parse_handlers(tracks: &[String]) -> Result<Vec<[u8; 4]>> {
    let mut handlers = Vec::new();
    for h in tracks {
        let b = h.as_bytes();
        if b.len() != 4 {
            bail!("track handler must be 4 chars: {h}");
        }
        handlers.push([b[0], b[1], b[2], b[3]]);
    }
    Ok(handlers)
}

fn build_layout(paths: &[PathBuf], opt: &concat::Options) -> Result<concat::Layout> {
    let t = Instant::now();
    eprintln!("Indexing {} clips...", paths.len());
    let movies = paths.iter().map(|p| mp4::open_movie(p)).collect::<Result<Vec<_>>>()?;
    let layout = concat::build(&movies, opt)?;
    eprintln!(
        "Built virtual index in {:.2?}: {} clips, {} (header/moov {}), duration {}",
        t.elapsed(),
        paths.len(),
        human(layout.size),
        human(layout.header.len() as u64),
        hms(layout.duration_secs)
    );
    Ok(layout)
}

fn natural_key(p: &Path) -> Vec<(u64, String)> {
    let name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut digits = false;
    for ch in name.chars().chain(std::iter::once('\0')) {
        let d = ch.is_ascii_digit();
        if !cur.is_empty() && (d != digits || ch == '\0') {
            out.push(if digits { (cur.parse().unwrap_or(u64::MAX), String::new()) } else { (0, cur.clone()) });
            cur.clear();
        }
        digits = d;
        cur.push(ch);
    }
    out
}

fn collect(input: &Input) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in &input.inputs {
        if p.is_dir() {
            let mut v: Vec<PathBuf> = std::fs::read_dir(p)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|f| {
                    f.is_file()
                        && f.extension().is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(&input.ext))
                })
                .collect();
            v.sort_by_key(|f| natural_key(f));
            out.extend(v);
        } else if p.is_file() {
            out.push(p.clone());
        } else {
            bail!("{} does not exist", p.display());
        }
    }
    if out.is_empty() {
        bail!("no input clips found");
    }
    Ok(out)
}

fn load(input: &Input) -> Result<concat::Layout> {
    let paths = collect(input)?;
    let opt = concat::Options {
        handlers: parse_handlers(&input.tracks)?,
        rotate: input.rotate,
        align: !input.no_align,
    };
    build_layout(&paths, &opt)
}

fn human(n: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.2} {}", units[i])
}

fn hms(s: f64) -> String {
    let ms = (s * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02}.{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// Each source clip stays open while mounted (one fd per clip), so long
/// dashcam days (hundreds of 1-minute clips per camera) exceed the default
/// soft limit of 1024. Raise the soft limit to the hard limit.
fn raise_fd_limit() {
    unsafe {
        let mut lim: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) == 0 && lim.rlim_cur < lim.rlim_max {
            lim.rlim_cur = lim.rlim_max;
            libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
        }
    }
}

fn main() -> Result<()> {
    raise_fd_limit();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Info { input } => {
            let l = load(&input)?;
            for (i, (p, s)) in l.sources.iter().zip(&l.clip_starts).enumerate() {
                let e = &l.extents[i + 1];
                println!(
                    "{:3}  {}  bytes {:>14}..{:<14}  {}",
                    i + 1,
                    hms(*s),
                    e.vstart,
                    e.vstart + e.len,
                    p.display()
                );
            }
            println!("total: {} bytes, {}", l.size, hms(l.duration_secs));
        }
        Cmd::Export { output, input } => {
            let l = load(&input)?;
            let vf = fs::VirtualFile::new("x".into(), l)?;
            let mut out: Box<dyn Write> = if output.as_os_str() == "-" {
                Box::new(std::io::stdout().lock())
            } else {
                Box::new(std::fs::File::create(&output).with_context(|| format!("create {}", output.display()))?)
            };
            let mut buf = vec![0u8; 8 << 20];
            let mut off = 0;
            loop {
                let n = vf.read_at(&mut buf, off)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                off += n as u64;
            }
            out.flush()?;
        }
        Cmd::Mount { mountpoint, input, name, allow_other, threads } => {
            let l = load(&input)?;
            let vf = fs::VirtualFile::new(name.clone(), l)?;
            mount(&mountpoint, vec![vf], allow_other, threads)?;
        }
        Cmd::MountSpec { mountpoint, spec, allow_other, threads } => {
            let text = std::fs::read_to_string(&spec).with_context(|| format!("read {}", spec.display()))?;
            let spec: Spec = serde_json::from_str(&text).context("parse spec")?;
            if spec.files.is_empty() {
                bail!("spec has no files");
            }
            let tracks = if spec.tracks.is_empty() {
                vec!["vide".to_string(), "soun".to_string()]
            } else {
                spec.tracks
            };
            let handlers = parse_handlers(&tracks)?;
            let mut vfs = Vec::new();
            for f in spec.files {
                if f.name.is_empty() || f.name.contains('/') || f.name == "." || f.name == ".." {
                    bail!("invalid file name in spec: {:?}", f.name);
                }
                if vfs.iter().any(|v: &fs::VirtualFile| v.name == f.name) {
                    bail!("duplicate file name in spec: {}", f.name);
                }
                let opt = concat::Options { handlers: handlers.clone(), rotate: f.rotate, align: true };
                let layout = build_layout(&f.inputs, &opt).with_context(|| format!("{}", f.name))?;
                vfs.push(fs::VirtualFile::new(f.name, layout)?);
            }
            mount(&mountpoint, vfs, allow_other, threads)?;
        }
    }
    Ok(())
}

/// Device id of our FUSE mount, recorded right after mounting.
static MOUNT_DEV: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

fn mount(mountpoint: &Path, vfs: Vec<fs::VirtualFile>, allow_other: bool, threads: usize) -> Result<()> {
    use fuser::{Config, MountOption, Session, SessionACL};

    // Block SIGINT/SIGTERM/SIGHUP in all threads; a dedicated thread waits for
    // them and unmounts cleanly, which makes the session loop below return.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGHUP);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        let mp = mountpoint.to_path_buf();
        std::thread::spawn(move || {
            let mut sig = 0;
            libc::sigwait(&set, &mut sig);
            // Only unmount if the path is still OUR mount: after a lazy unmount
            // another instance may have mounted at the same path.
            let ours = MOUNT_DEV.get().is_some_and(|&dev| {
                use std::os::unix::fs::MetadataExt;
                std::fs::metadata(&mp).map(|m| m.dev() == dev).unwrap_or(false)
            });
            if ours {
                eprintln!("Unmounting {}...", mp.display());
                let _ = std::process::Command::new("fusermount3").arg("-u").arg("-z").arg(&mp).status();
            }
            std::process::exit(0);
        });
    }

    let mut cfg = Config::default();
    cfg.mount_options = vec![
        MountOption::RO,
        MountOption::FSName("mp4-concat-fuse".into()),
        MountOption::Subtype("mp4concat".into()),
        MountOption::CUSTOM("max_read=1048576".into()),
    ];
    if allow_other {
        cfg.acl = SessionACL::All;
    }
    cfg.n_threads = Some(threads.max(1));

    let names: Vec<String> = vfs.iter().map(|v| v.name.clone()).collect();
    let session = Session::new(fs::ConcatFs::new(vfs), mountpoint, &cfg).context("FUSE mount failed")?;
    {
        // stat() of the mount root is answered by the session loop, so do it
        // from another thread once session.run() below is serving requests.
        use std::os::unix::fs::MetadataExt;
        let mp = mountpoint.to_path_buf();
        std::thread::spawn(move || {
            if let Ok(m) = std::fs::metadata(&mp) {
                let _ = MOUNT_DEV.set(m.dev());
            }
        });
    }
    for n in &names {
        eprintln!("Mounted: {}", mountpoint.join(n).display());
    }
    eprintln!("(Ctrl-C to unmount)");
    println!("READY {}", mountpoint.display());
    let _ = std::io::stdout().flush();
    session.run().context("FUSE session")?;
    Ok(())
}
