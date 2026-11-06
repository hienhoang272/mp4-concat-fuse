<h1 align="center">mp4-concat-fuse</h1>

<p align="center">
  <strong>Watch a folder of camera clips as one video, without writing a merged copy.</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/built_with-Rust-dea584" alt="Built with Rust">
  <img src="https://img.shields.io/badge/mount-read--only-5dd6bf" alt="Read-only mount">
  <img src="https://img.shields.io/badge/media-no_re--encoding-80adff" alt="No re-encoding">
</p>

Your dashcam splits a drive into dozens of files. Your action camera starts a new file halfway through a recording. You want to watch the whole thing, but a conventional merge creates another large video on disk.

`mp4-concat-fuse` gives you a different option. Mount your clips, open `merged.mp4` in a player such as VLC or mpv, and play or seek across them. The file looks like a merged MP4, while its media data still comes from the original clips.

- **No merged media copy during mounting.** The tool builds an MP4 index in memory and reads source bytes on demand.
- **No re-encoding.** Video and audio samples keep their original encoded data.
- **Read-only access.** The mount exposes files for playback without changing the source clips.
- **More than one video per mount.** Group a front camera, a rear camera, or separate recording sessions with a JSON spec.

Use this for compatible recordings from the same camera mode. Keep the source drive connected while the mount is active.

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

## Know the limits

- Use regular, non-fragmented MP4 or MOV clips. Fragmented MP4 is unsupported, and MOV files still need the boxes expected by this parser.
- Keep the codec, resolution, audio format, track order, and timescales consistent. Track-layout checks reject some mismatches, but they do not prove that every codec combination plays correctly.
- Source files must remain available and unchanged until you unmount. Adding clips to a folder does not update an existing mount. Unmount and mount again to rebuild the index.
- Rotation is metadata for the whole output video. It cannot apply a different angle to each clip in one virtual file.
- Video and audio tracks are selected by default. Camera metadata and timecode tracks are omitted from the combined index. Their bytes can still fall within source regions that the virtual file exposes.
- Later clips with edit-list offsets retain their pre-roll instead of trimming it. The CLI prints a warning for that case.
- Each source occurrence keeps a file handle open. The CLI raises its soft limit to the hard limit, but very large specs may still need a higher system limit.
- This is a small project with a custom MP4 parser. Verify your recordings and player before relying on a long unattended session.

## Find your way around the source

| File | Responsibility |
| --- | --- |
| [`src/main.rs`](src/main.rs) | CLI, input ordering, JSON specs, and mount lifecycle |
| [`src/mp4.rs`](src/mp4.rs) | MP4 box reader, sample tables, and box writer |
| [`src/concat.rs`](src/concat.rs) | Combined timing, sample descriptions, offsets, and virtual byte layout |
| [`src/fs.rs`](src/fs.rs) | Byte-range reads and the read-only FUSE filesystem |
