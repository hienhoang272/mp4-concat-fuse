<h1 align="center">mp4-concat-fuse</h1>

<p align="center">
  <strong>Watch a folder of camera clips as one video, without writing a merged copy.</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/built_with-Rust-dea584" alt="Built with Rust">
  <img src="https://img.shields.io/badge/mount-read--only-5dd6bf" alt="Read-only mount">
  <img src="https://img.shields.io/badge/media-no_re--encoding-80adff" alt="No re-encoding">
</p>

<p align="center">
  <a href="https://github.com/hienhoang272/mp4-concat-fuse">GitHub</a> ·
  <a href="#try-it">Try it</a> ·
  <a href="#install-on-your-os">Installation</a> ·
  <a href="#how-the-virtual-file-works">How it works</a> ·
  <a href="#use-your-own-clips">Usage</a> ·
  <a href="#make-shortcuts">Make shortcuts</a>
</p>

<table align="center">
<tr>
<td>

https://github.com/user-attachments/assets/54909212-35f1-4b73-8d98-94815a5b8706

</td>
</tr>
</table>

Your dashcam splits a drive into dozens of files. Your action camera starts a new file halfway through a recording. You want to watch the whole thing, but a conventional merge creates another large video on disk.

`mp4-concat-fuse` gives you a different option. Mount your clips, open `merged.mp4` in a player such as VLC or mpv, and play or seek across them. The file looks like a merged MP4, while its media data still comes from the original clips.

- **No merged media copy during mounting.** The tool builds an MP4 index in memory and reads source bytes on demand.
- **No re-encoding.** Video and audio samples keep their original encoded data.
- **Read-only access.** The mount exposes files for playback without changing the source clips.
- **More than one video per mount.** Group a front camera, a rear camera, or separate recording sessions with a JSON spec.

Use this for compatible recordings from the same camera mode. Keep the source drive connected while the mount is active.

## Try it

Get the source from [GitHub](https://github.com/hienhoang272/mp4-concat-fuse):

```bash
git clone https://github.com/hienhoang272/mp4-concat-fuse.git
cd mp4-concat-fuse
```

[Install the prerequisites for your OS](#install-on-your-os), then run these commands from the repository directory:

```bash
make build
make info INPUTS=/media/card/DCIM
make mount INPUTS=/media/card/DCIM MOUNTPOINT=/tmp/trip
```

Keep that terminal open. In another terminal, play the virtual file:

```bash
mpv /tmp/trip/merged.mp4
```

Press `Ctrl+C` in the mount terminal to unmount. You can also run:

```bash
make unmount MOUNTPOINT=/tmp/trip
```

## Install on your OS

| Environment | Status | Filesystem backend |
| --- | --- | --- |
| Linux | Built and verified with a live mount | Linux FUSE |
| macOS, Apple Silicon or Intel | Source checks pass for both architectures; live mounting is not yet verified on a Mac | macFUSE with its libfuse2 interface |
| Windows through WSL2 | Expected to work as a Linux application; not yet verified on a Windows host | Linux FUSE inside WSL2 |
| Windows native | Not supported | No WinFsp backend in this project |

The macOS checks use `fuser/macos-no-mount` for cross-compilation. They check Rust code and platform types, but do **not** link macFUSE or test its driver. WSL2 instructions below use the Linux build, rather than a Windows executable.

### Linux

1. On Ubuntu or Debian, install the build tools and FUSE utility:

   ```bash
   sudo apt update
   sudo apt install build-essential pkg-config fuse3 curl git
   ```

2. Install the current stable Rust toolchain through [rustup](https://rustup.rs/) if Cargo is not installed:

   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   . "$HOME/.cargo/env"
   ```

3. Open the repository directory and build:

   ```bash
   cargo build --release --locked
   ./target/release/mp4-concat-fuse --help
   ```

   To put the command on your PATH, run `cargo install --path . --locked`. The Makefile runs the local release binary, so installation is optional.

4. Confirm that FUSE is available, then mount your clips:

   ```bash
   ls -l /dev/fuse
   mkdir -p "$HOME/Videos/trip"
   ./target/release/mp4-concat-fuse mount "$HOME/Videos/trip" /media/card/DCIM
   ```

   Wait for `READY`, then open `~/Videos/trip/merged.mp4`. Keep the process running while you play the video. To unmount from another terminal:

   ```bash
   fusermount3 -u "$HOME/Videos/trip"
   ```

If `/dev/fuse` is missing, try `sudo modprobe fuse` and check again. A container also needs access to the host FUSE device and permission to mount filesystems.

### macOS

This is an experimental setup guide. The MP4 reader uses Unix APIs available on macOS, and `fuser` provides a macFUSE backend. The CLI uses one worker thread by default on macOS and `/sbin/umount` for signal-triggered cleanup.

1. Install Apple's command line tools:

   ```bash
   xcode-select --install
   ```

   If the tools are already installed, continue. With [Homebrew](https://brew.sh/) installed, add Rust and pkg-config:

   ```bash
   brew install rust pkgconf git
   ```

2. Download and install [macFUSE](https://macfuse.github.io/). Complete the approval and restart steps required by its installer. Follow the [official setup guide](https://github.com/macfuse/macfuse/wiki/Getting-Started) for your Mac and macOS version.

   This project's pinned `fuser` version links the `fuse` pkg-config library through its macFUSE compatibility backend. This guide targets that traditional backend. Compatibility with the newer FSKit backend and with FUSE-T has not been established for this project.

3. Confirm that the build can find macFUSE:

   ```bash
   pkg-config --modversion fuse
   ```

   If `fuse.pc` is installed but pkg-config cannot find it, add the usual package locations:

   ```bash
   export PKG_CONFIG_PATH="/usr/local/lib/pkgconfig:/opt/homebrew/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
   pkg-config --modversion fuse
   ```

   If that still fails, check the macFUSE installation before building. Do not use `macos-no-mount` for a runnable mount binary.

4. From the repository directory, build and inspect your clips:

   ```bash
   cargo build --release --locked
   ./target/release/mp4-concat-fuse info "/Volumes/CAMERA/DCIM"
   ```

5. Create a mount directory and start the filesystem:

   ```bash
   mkdir -p "$HOME/Movies/trip"
   ./target/release/mp4-concat-fuse mount "$HOME/Movies/trip" "/Volumes/CAMERA/DCIM" --threads 1
   ```

   After `READY`, open the file in VLC from a second terminal:

   ```bash
   open -a VLC "$HOME/Movies/trip/merged.mp4"
   ```

6. Close the player, then press `Ctrl+C` in the mount terminal. Alternatively:

   ```bash
   /sbin/umount "$HOME/Movies/trip"
   ```

   If the volume is busy, close players and Finder windows that use it, and move terminal working directories outside the mount before retrying.

`make build`, `make mount`, and `make unmount` also work with this setup. Keep `--threads 1` if you set the thread count explicitly. `fuser` 0.18 only supports multiple worker threads on Linux. The [pinned dependency's build script](https://docs.rs/crate/fuser/0.18.0/source/build.rs) documents how it locates macFUSE.

### Windows with WSL2

The current code depends on Unix file access, POSIX signals, and `fuser`. Installing WinFsp alone does not make it a native Windows application. Use Ubuntu inside WSL2.

1. In an administrator PowerShell window, install WSL and Ubuntu:

   ```powershell
   wsl --install -d Ubuntu
   ```

   Restart if prompted, open Ubuntu, and finish creating your Linux user. For an existing installation, update WSL and check the distribution version:

   ```powershell
   wsl --update
   wsl --list --verbose
   ```

   Ubuntu must show version `2`. If it shows `1`, convert it:

   ```powershell
   wsl --set-version Ubuntu 2
   ```

   See Microsoft's [WSL installation guide](https://learn.microsoft.com/en-us/windows/wsl/install) for host requirements.

2. Inside Ubuntu, follow the [Linux installation steps](#linux) above. Copy or clone this repository into your Linux home directory, then build it there. Run all Cargo, Make, and mount commands in Ubuntu.

3. Check FUSE before proceeding:

   ```bash
   ls -l /dev/fuse
   fusermount3 --version
   ```

   If the device is missing, try `sudo modprobe fuse`. If the kernel lacks FUSE support, update WSL, run `wsl --shutdown` from PowerShell, reopen Ubuntu, and check again. A host or kernel that still cannot provide FUSE can use `export` instead of mounting.

4. Windows drives are normally available under `/mnt`. For example, `D:\Dashcam` becomes `/mnt/d/Dashcam`. Mount into a directory in the Linux filesystem:

   ```bash
   mkdir -p "$HOME/Videos/trip"
   ./target/release/mp4-concat-fuse info "/mnt/d/Dashcam"
   ./target/release/mp4-concat-fuse mount "$HOME/Videos/trip" "/mnt/d/Dashcam"
   ```

   If a removable drive is absent, follow Microsoft's [drive mounting guidance](https://learn.microsoft.com/en-us/windows/wsl/wsl2-mount-disk). Do not assume that every attached card appears automatically.

5. For playback, use a Linux player inside WSL. On a host with [WSLg support](https://learn.microsoft.com/en-us/windows/wsl/tutorials/gui-apps), open another Ubuntu terminal:

   ```bash
   sudo apt install mpv
   mpv "$HOME/Videos/trip/merged.mp4"
   ```

   Access to this FUSE mount from Windows players through `\\wsl.localhost\...` is not verified. Use a WSL player for virtual playback. If you need a normal file for a Windows player, export to a Windows directory instead:

   ```bash
   ./target/release/mp4-concat-fuse export "/mnt/d/trip.mp4" "/mnt/d/Dashcam"
   ```

   Export writes the full merged file and needs enough free disk space.

6. Close the player and press `Ctrl+C` in the mount terminal, or run inside Ubuntu:

   ```bash
   fusermount3 -u "$HOME/Videos/trip"
   ```

## How the virtual file works

An MP4 has an index, called `moov`, that describes sample timing and byte positions. Its `mdat` area holds the encoded video and audio.

The tool reads each clip's index and builds a combined one. It places that index before the media data, so a player can read the layout first. The resulting virtual file looks like this:

```text
What the player sees
┌───────────┬────────────────┬─────────────┬────────────┬────────────┐
│ MP4 header│ Combined index │ Clip 1 data │ Clip 2 data│ Clip 3 data│
└───────────┴────────────────┴─────────────┴────────────┴────────────┘
      Generated in memory           Read from the original files
```

When the player requests a byte range, the filesystem finds the corresponding source clip and reads that range. Seeking uses the same mapping. Memory use grows with the sample tables, rather than requiring a full copy of the footage in RAM.

The filesystem reports the virtual file's full logical size. That size does not mean a merged copy occupies your disk. Copying the virtual file elsewhere, however, reads and writes all its media data.

The default timing alignment follows the longest track in each clip, including tracks omitted from the output. It adjusts the last sample's duration at joins to align the next clip. It does not insert silence or re-encode frames. Complex edit lists can differ from an FFmpeg remux, so check playback with your camera's files.

## Use your own clips

The examples below assume `cargo install --path . --locked`. Otherwise, replace `mp4-concat-fuse` with `./target/release/mp4-concat-fuse`.

### Pick folders, files, or both

```bash
mp4-concat-fuse info /media/card/DCIM
mp4-concat-fuse mount /tmp/trip clip1.mp4 clip2.mp4 --name trip.mp4
mp4-concat-fuse mount /tmp/trip /media/card/DCIM --ext mov
mp4-concat-fuse mount /tmp/trip /media/card/DCIM --rotate 180
```

Create an empty mount directory before running a direct `mount` command. Folder scans are not recursive. They select `.mp4` by default, ignore extension case, and sort naturally, so `clip2` comes before `clip10`. Explicit files keep the order you supply. Multiple input folders are scanned in the order you supply them.

### Group several virtual videos

Create a JSON file with an ordered list of **file paths** for each virtual video:

```json
{
  "files": [
    {
      "name": "front.mp4",
      "inputs": ["/media/card/front/001.MP4", "/media/card/front/002.MP4"]
    },
    {
      "name": "rear.mp4",
      "inputs": ["/media/card/rear/001.MP4", "/media/card/rear/002.MP4"],
      "rotate": 180
    }
  ],
  "tracks": ["vide", "soun"]
}
```

```bash
mkdir -p /tmp/trip
mp4-concat-fuse mount-spec /tmp/trip spec.json
```

Unlike `mount`, `mount-spec` does not expand folders or sort the inputs. Relative paths resolve from the process's working directory, not from the JSON file's directory. Each output name must be unique and contain no `/`.

The command prints `READY <mountpoint>` when the session is created. It stays in the foreground while the mount serves reads.

### Export a standalone file

```bash
mp4-concat-fuse export trip.mp4 /media/card/DCIM
mp4-concat-fuse export - /media/card/DCIM | sha256sum
```

Export uses the same virtual layout as the mount, but writes every byte to a file or standard output. Choose an output path outside the input clips. The command overwrites an existing output file.

### Choose playback options

| Option | Applies to | Behavior |
| --- | --- | --- |
| `--name trip.mp4` | `mount` | Set the virtual filename. Default: `merged.mp4`. |
| `--ext mov` | `mount`, `info`, `export` | Select the extension when scanning folders. Default: `mp4`. |
| `--rotate 90` | `mount`, `info`, `export` | Set display rotation metadata. Multiples of 90 degrees only. |
| `--tracks vide` | `mount`, `info`, `export` | Keep video only. Default: `vide,soun` for video and audio. |
| `--no-align` | `mount`, `info`, `export` | Append track timing without the default join alignment. |
| `--threads 4` | `mount`, `mount-spec` | Set worker count. Default: 4 on Linux, 1 on macOS. Use 1 on macOS. |
| `--allow-other` | `mount`, `mount-spec` | Allow other users to read. On Linux, requires `user_allow_other` in `/etc/fuse.conf`. |

For JSON specs, `rotate` is per file and `tracks` applies to the whole mount. Spec mounts always use the default timing alignment.

## Know the limits

- Use regular, non-fragmented MP4 or MOV clips. Fragmented MP4 is unsupported, and MOV files still need the boxes expected by this parser.
- Keep the codec, resolution, audio format, track order, and timescales consistent. Track-layout checks reject some mismatches, but they do not prove that every codec combination plays correctly.
- Source files must remain available and unchanged until you unmount. Adding clips to a folder does not update an existing mount. Unmount and mount again to rebuild the index.
- Rotation is metadata for the whole output video. It cannot apply a different angle to each clip in one virtual file.
- Video and audio tracks are selected by default. Camera metadata and timecode tracks are omitted from the combined index. Their bytes can still fall within source regions that the virtual file exposes.
- Later clips with edit-list offsets retain their pre-roll instead of trimming it. The CLI prints a warning for that case.
- Each source occurrence keeps a file handle open. The CLI raises its soft limit to the hard limit, but very large specs may still need a higher system limit.
- This is a small project with a custom MP4 parser. Verify your recordings and player before relying on a long unattended session.

## Make shortcuts

Run `make` to see commands grouped by purpose. `make lint` and `make fmt` need Clippy and rustfmt. With rustup, install them with `rustup component add clippy rustfmt`.

| Group | Commands |
| --- | --- |
| Build and develop | `make build`, `make check`, `make test`, `make lint`, `make fmt`, `make install`, `make clean` |
| Inspect and use | `make info`, `make mount`, `make mount-spec`, `make unmount`, `make export` |

```bash
make mount INPUTS=/media/card/DCIM MOUNTPOINT=/tmp/trip NAME=trip.mp4
make info INPUTS="clip1.mp4 clip2.mp4"
make mount-spec SPEC=spec.json MOUNTPOINT=/tmp/trip
make export INPUTS=/media/card/DCIM OUTPUT=/tmp/trip.mp4
make mount INPUTS=/media/card/DCIM ARGS="--rotate 180 --tracks vide"
```

Defaults are `INPUTS=clips`, `MOUNTPOINT=mnt`, `NAME=merged.mp4`, `SPEC=spec.json`, and `OUTPUT=merged.mp4`. `INPUTS` is a shell-separated list. For individual paths containing spaces, use the CLI directly and quote each path. `ARGS` adds CLI flags.

## Find your way around the source

| File | Responsibility |
| --- | --- |
| [`src/main.rs`](src/main.rs) | CLI, input ordering, JSON specs, and mount lifecycle |
| [`src/mp4.rs`](src/mp4.rs) | MP4 box reader, sample tables, and box writer |
| [`src/concat.rs`](src/concat.rs) | Combined timing, sample descriptions, offsets, and virtual byte layout |
| [`src/fs.rs`](src/fs.rs) | Byte-range reads and the read-only FUSE filesystem |
