//! Builds the layout of a *virtual* MP4 that is equivalent to
//! `ffmpeg -f concat -i list -map 0:v -map 0:a -c copy -movflags +faststart`.
//!
//! Virtual file = [ftyp][moov (synthesized)][mdat header][src1 region][src2 region]...
//! Each "src region" is a byte range of an original file (the span covering all
//! sample data of the selected tracks), so the media bytes are served straight
//! from the originals and only the small index (`moov`) lives in memory.

use crate::mp4::{Movie, Track, W, be_u32, be_u64};
use anyhow::{Result, bail};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Options {
    /// Handler types of tracks to keep (e.g. "vide", "soun").
    pub handlers: Vec<[u8; 4]>,
    /// Display rotation in degrees, same meaning as ffmpeg `-display_rotation`.
    pub rotate: Option<i32>,
    /// Pad every clip to the same duration on all tracks (what ffmpeg's concat
    /// demuxer does) so A/V stay in sync across joins.
    pub align: bool,
}

/// One contiguous piece of the virtual file.
#[derive(Clone, Debug)]
pub enum Piece {
    /// bytes from `header[start..start+len]`
    Header { start: u64 },
    /// bytes from source file `src` starting at `offset`
    Source { src: usize, offset: u64 },
}

#[derive(Clone, Debug)]
pub struct Extent {
    pub vstart: u64,
    pub len: u64,
    pub piece: Piece,
}

pub struct Layout {
    pub sources: Vec<PathBuf>,
    pub header: Arc<Vec<u8>>,
    pub extents: Vec<Extent>,
    pub size: u64,
    pub duration_secs: f64,
    /// Start time (seconds) of each source clip on the virtual timeline.
    pub clip_starts: Vec<f64>,
}

impl Layout {
    pub fn locate(&self, off: u64) -> Option<usize> {
        if off >= self.size {
            return None;
        }
        let i = self.extents.partition_point(|e| e.vstart <= off);
        Some(i - 1)
    }
}

/// a * b / c rounded to nearest (like av_rescale).
fn rescale(a: u64, b: u64, c: u64) -> u64 {
    if c == 0 {
        return 0;
    }
    ((a as u128 * b as u128 + c as u128 / 2) / c as u128) as u64
}

/// Key used to decide whether two sample entries describe the same stream
/// configuration. Bitrate statistics (`btrt` box, avg/max bitrate and buffer
/// size in `esds`) differ per clip but don't affect decoding; treating them as
/// different would emit a new sample description at every join, which makes
/// players reinitialise their decoder (ffmpeg keeps the first one too).
fn sample_entry_key(entry: &[u8], handler: &[u8; 4]) -> Vec<u8> {
    let fixed = match handler {
        b"vide" => 8 + 78,
        b"soun" if entry.len() >= 18 => {
            8 + 28 + match crate::mp4::be_u16(&entry[16..]) {
                1 => 16,
                2 => 36,
                _ => 0,
            }
        }
        _ => return entry.to_vec(),
    };
    let Some(kids) = entry.get(fixed..) else { return entry.to_vec() };
    let Ok(boxes) = crate::mp4::children(kids) else { return entry.to_vec() };
    let mut key = entry[..fixed].to_vec();
    for b in boxes {
        match &b.typ {
            b"btrt" => {}
            b"esds" => {
                let mut raw = b.raw.to_vec();
                blank_esds_bitrate(&mut raw);
                key.extend_from_slice(&raw);
            }
            _ => key.extend_from_slice(b.raw),
        }
    }
    key
}

/// Zero bufferSizeDB/maxBitrate/avgBitrate of the DecoderConfigDescriptor.
fn blank_esds_bitrate(esds: &mut [u8]) {
    // box header(8) + fullbox(4) then descriptors
    fn desc_len(b: &[u8], mut p: usize) -> Option<(usize, usize)> {
        let mut len = 0usize;
        for _ in 0..4 {
            let x = *b.get(p)?;
            p += 1;
            len = (len << 7) | (x & 0x7f) as usize;
            if x & 0x80 == 0 {
                return Some((len, p));
            }
        }
        None
    }
    let mut p = 12;
    if esds.get(p) != Some(&0x03) {
        return;
    }
    let Some((_, q)) = desc_len(esds, p + 1) else { return };
    p = q;
    let Some(&flags) = esds.get(p + 2) else { return };
    p += 3;
    if flags & 0x80 != 0 {
        p += 2;
    }
    if flags & 0x40 != 0 {
        let Some(&n) = esds.get(p) else { return };
        p += 1 + n as usize;
    }
    if flags & 0x20 != 0 {
        p += 2;
    }
    if esds.get(p) != Some(&0x04) {
        return;
    }
    let Some((_, q)) = desc_len(esds, p + 1) else { return };
    // objectType(1) streamType(1) bufferSize(3) max(4) avg(4)
    if let Some(s) = esds.get_mut(q + 2..q + 13) {
        s.fill(0);
    }
}

struct MergedTrack {
    src: Vec<usize>, // index of this track within each movie
    handler: [u8; 4],
    timescale: u32,
    stsd: Vec<Vec<u8>>,
    stts: Vec<(u32, u32)>,
    ctts: Option<Vec<(u32, i32)>>,
    stss: Option<Vec<u32>>,
    sdtp: Option<Vec<u8>>,
    stsc: Vec<(u32, u32, u32)>,
    sizes: Vec<u32>,
    /// (source index, original chunk offset)
    chunks: Vec<(usize, u64)>,
    duration: u64, // media timescale
}

pub fn build(movies: &[Movie], opt: &Options) -> Result<Layout> {
    if movies.is_empty() {
        bail!("no input files");
    }

    // ---- pick tracks and check compatibility
    let pick = |m: &Movie| -> Vec<usize> {
        m.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| opt.handlers.contains(&t.handler) && !t.sizes.is_empty())
            .map(|(i, _)| i)
            .collect()
    };
    let first_pick = pick(&movies[0]);
    if first_pick.is_empty() {
        bail!("{}: no track with the requested handler types", movies[0].path.display());
    }
    let mut picks = Vec::new();
    for m in movies {
        let p = pick(m);
        let sig = |p: &[usize], m: &Movie| -> Vec<([u8; 4], u32)> {
            p.iter().map(|&i| (m.tracks[i].handler, m.tracks[i].timescale)).collect()
        };
        if sig(&p, m) != sig(&first_pick, &movies[0]) {
            bail!(
                "{}: track layout/timescale differs from {} (not concat-compatible)",
                m.path.display(),
                movies[0].path.display()
            );
        }
        picks.push(p);
    }

    // ---- source byte regions (span of all chunks of the kept tracks)
    let mut regions = Vec::new(); // (start, end) in source
    for (mi, m) in movies.iter().enumerate() {
        let (mut lo, mut hi) = (u64::MAX, 0u64);
        for &ti in &picks[mi] {
            for (off, len) in m.tracks[ti].chunk_extents()? {
                lo = lo.min(off);
                hi = hi.max(off + len);
            }
        }
        if hi > m.file_size {
            bail!("{}: sample data points past end of file", m.path.display());
        }
        regions.push((lo, hi));
    }

    // ---- per-clip durations in microseconds, the way ffmpeg's concat demuxer
    // sees them: the format duration = longest stream, over ALL tracks
    // (including ones we drop, e.g. tmcd / DJI metadata), using edit lists.
    let clip_us: Vec<u64> = movies
        .iter()
        .map(|m| {
            m.tracks
                .iter()
                .filter(|t| !t.sizes.is_empty())
                .map(|t| t.duration_us(m.timescale))
                .max()
                .unwrap_or(0)
        })
        .collect();
    for (m, p) in movies.iter().zip(&picks).skip(1) {
        if p.iter().any(|&ti| m.tracks[ti].edit_media_time != 0) {
            eprintln!(
                "warning: {} starts with an edit-list offset (pre-roll); it will be shown, not trimmed",
                m.path.display()
            );
        }
    }

    // ---- merge tables
    let mut merged = Vec::new();
    for k in 0..first_pick.len() {
        let src: Vec<usize> = picks.iter().map(|p| p[k]).collect();
        let t0 = &movies[0].tracks[src[0]];
        let ts = t0.timescale;
        let any_ctts = movies.iter().zip(&src).any(|(m, &i)| m.tracks[i].ctts.is_some());
        let any_stss = movies.iter().zip(&src).any(|(m, &i)| m.tracks[i].stss.is_some());
        let all_sdtp = movies.iter().zip(&src).all(|(m, &i)| m.tracks[i].sdtp.is_some());
        let mut mt = MergedTrack {
            src: src.clone(),
            handler: t0.handler,
            timescale: ts,
            stsd: Vec::new(),
            stts: Vec::new(),
            ctts: any_ctts.then(Vec::new),
            stss: any_stss.then(Vec::new),
            sdtp: all_sdtp.then(Vec::new),
            stsc: Vec::new(),
            sizes: Vec::new(),
            chunks: Vec::new(),
            duration: 0,
        };
        let mut cum_us: u64 = 0;
        let mut stsd_keys: Vec<Vec<u8>> = Vec::new();
        for (mi, m) in movies.iter().enumerate() {
            let t: &Track = &m.tracks[src[mi]];
            let sample_base = mt.sizes.len() as u32;
            let chunk_base = mt.chunks.len() as u32;

            // sample descriptions: dedupe equivalent entries, remap indices
            let sd_map: Vec<u32> = t
                .stsd_entries
                .iter()
                .map(|e| {
                    let key = sample_entry_key(e, &t.handler);
                    match stsd_keys.iter().position(|x| *x == key) {
                        Some(p) => p as u32 + 1,
                        None => {
                            stsd_keys.push(key);
                            mt.stsd.push(e.clone());
                            mt.stsd.len() as u32
                        }
                    }
                })
                .collect();

            // stts; the last sample of every clip but the final one is
            // stretched so the next clip starts where ffmpeg's concat would put it
            let mut stts = t.stts.clone();
            let own = t.media_duration();
            let mut dur = own;
            cum_us += clip_us[mi];
            if opt.align && mi + 1 < movies.len() {
                // targets are on the presentation timeline; the merged track
                // keeps clip 1's edit-list offset, so add it back for media time
                let target_ticks =
                    rescale(cum_us, ts as u64, 1_000_000) + t0.edit_media_time.max(0) as u64;
                let extra = target_ticks as i64 - (mt.duration + own) as i64;
                if extra != 0 {
                    if let Some(last) = stts.last_mut() {
                        let nd = last.1 as i64 + extra;
                        if nd > 0 && nd <= u32::MAX as i64 {
                            if last.0 > 1 {
                                last.0 -= 1;
                                stts.push((1, nd as u32));
                            } else {
                                last.1 = nd as u32;
                            }
                            dur = (own as i64 + extra) as u64;
                        }
                    }
                }
            }
            for (c, d) in stts {
                match mt.stts.last_mut() {
                    Some(last) if last.1 == d => last.0 += c,
                    _ => mt.stts.push((c, d)),
                }
            }
            mt.duration += dur;

            if let Some(ct) = &mut mt.ctts {
                let entries = t.ctts.clone().unwrap_or_else(|| vec![(t.sample_count() as u32, 0)]);
                for (c, o) in entries {
                    match ct.last_mut() {
                        Some(last) if last.1 == o => last.0 += c,
                        _ => ct.push((c, o)),
                    }
                }
            }
            if let Some(ss) = &mut mt.stss {
                match &t.stss {
                    Some(v) => ss.extend(v.iter().map(|s| s + sample_base)),
                    None => ss.extend((1..=t.sample_count() as u32).map(|s| s + sample_base)),
                }
            }
            if let Some(sd) = &mut mt.sdtp {
                sd.extend_from_slice(t.sdtp.as_ref().unwrap());
            }
            for &(first, spc, sdi) in &t.stsc {
                let sdi = *sd_map.get(sdi as usize - 1).unwrap_or(&1);
                match mt.stsc.last() {
                    Some(&(_, s, d)) if s == spc && d == sdi => {}
                    _ => mt.stsc.push((first + chunk_base, spc, sdi)),
                }
            }
            mt.sizes.extend_from_slice(&t.sizes);
            mt.chunks.extend(t.chunk_offsets.iter().map(|&o| (mi, o)));
        }
        merged.push(mt);
    }

    // ---- assemble: build moov once to learn its size, then for real
    let ftyp = movies[0].ftyp_raw.clone();
    let region_len: Vec<u64> = regions.iter().map(|(a, b)| b - a).collect();
    let mut moov = build_moov(movies, &merged, opt, &regions, &region_len, 0)?;
    let header_len = (ftyp.len() + moov.len() + 16) as u64;
    moov = build_moov(movies, &merged, opt, &regions, &region_len, header_len)?;
    assert_eq!(header_len, (ftyp.len() + moov.len() + 16) as u64);

    let mdat_payload: u64 = region_len.iter().sum();
    let mut header = ftyp;
    header.extend_from_slice(&moov);
    header.extend_from_slice(&1u32.to_be_bytes());
    header.extend_from_slice(b"mdat");
    header.extend_from_slice(&(16 + mdat_payload).to_be_bytes());

    let mut extents = vec![Extent { vstart: 0, len: header.len() as u64, piece: Piece::Header { start: 0 } }];
    let mut v = header.len() as u64;
    for (i, &(start, end)) in regions.iter().enumerate() {
        extents.push(Extent { vstart: v, len: end - start, piece: Piece::Source { src: i, offset: start } });
        v += end - start;
    }

    let mut clip_starts = Vec::new();
    let mut acc = 0u64;
    for c in &clip_us {
        clip_starts.push(acc as f64 / 1e6);
        acc += c;
    }
    let duration_secs = merged
        .iter()
        .map(|t| t.duration as f64 / t.timescale as f64)
        .fold(0.0, f64::max);

    Ok(Layout {
        sources: movies.iter().map(|m| m.path.clone()).collect(),
        header: Arc::new(header),
        extents,
        size: v,
        duration_secs,
        clip_starts,
    })
}

/// Rotation matrix (16.16 / 2.30) in tkhd order, as ffmpeg's av_display_rotation_set.
fn rotation_matrix(deg: i32) -> Result<[u32; 9]> {
    let one = 0x0001_0000u32;
    let neg = (-0x0001_0000i32) as u32;
    let (a, b, c, d) = match deg.rem_euclid(360) {
        0 => (one, 0, 0, one),
        // what ffmpeg's mov muxer writes for `-display_rotation <deg>`
        90 => (0, neg, one, 0),
        180 => (neg, 0, 0, neg),
        270 => (0, one, neg, 0),
        _ => bail!("rotation must be a multiple of 90"),
    };
    Ok([a, b, 0, c, d, 0, 0, 0, 0x4000_0000])
}

fn build_moov(
    movies: &[Movie],
    tracks: &[MergedTrack],
    opt: &Options,
    regions: &[(u64, u64)],
    region_len: &[u64],
    header_len: u64,
) -> Result<Vec<u8>> {
    // virtual start of each source region
    let mut vbase = Vec::with_capacity(regions.len());
    let mut v = header_len;
    for l in region_len {
        vbase.push(v);
        v += l;
    }

    let m0 = &movies[0];
    let mts = m0.timescale as u64;
    let to_movie = |d: u64, ts: u32| -> u64 { (d as u128 * mts as u128 / ts as u128) as u64 };
    let movie_dur = tracks.iter().map(|t| to_movie(t.duration, t.timescale)).max().unwrap_or(0);

    // mvhd fields we copy (rate, volume, matrix...)
    let mvhd = &m0.mvhd;
    let (ctime, mtime, rest) = if mvhd[0] == 1 {
        (be_u64(&mvhd[4..]), be_u64(&mvhd[12..]), &mvhd[32..])
    } else {
        (be_u32(&mvhd[4..]) as u64, be_u32(&mvhd[8..]) as u64, &mvhd[20..])
    };
    // rest = rate(4) volume(2) reserved(10) matrix(36) pre_defined(24) next_track_id(4)
    if rest.len() < 80 {
        bail!("short mvhd");
    }

    let rotation = match opt.rotate {
        Some(r) => Some(rotation_matrix(r)?),
        None => None,
    };

    let mut w = W::new();
    let mut err = None;
    w.boxed(b"moov", |w| {
        w.full(b"mvhd", 1, 0, |w| {
            w.u64(ctime).u64(mtime).u32(m0.timescale).u64(movie_dur);
            w.bytes(&rest[..76]).u32(tracks.len() as u32 + 1);
        });
        for (k, t) in tracks.iter().enumerate() {
            if let Err(e) = write_trak(w, movies, t, k as u32 + 1, to_movie(t.duration, t.timescale), rotation, &vbase, regions) {
                err = Some(e);
            }
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    Ok(w.0)
}

#[allow(clippy::too_many_arguments)]
fn write_trak(
    w: &mut W,
    movies: &[Movie],
    t: &MergedTrack,
    track_id: u32,
    movie_dur: u64,
    rotation: Option<[u32; 9]>,
    vbase: &[u64],
    regions: &[(u64, u64)],
) -> Result<()> {
    let src = &movies[0].tracks[t.src[0]];
    let tk = &src.tkhd;
    let (flags, ctime, mtime, tail) = {
        let flags = be_u32(tk) & 0xff_ffff;
        if tk[0] == 1 {
            (flags, be_u64(&tk[4..]), be_u64(&tk[12..]), &tk[36..])
        } else {
            (flags, be_u32(&tk[4..]) as u64, be_u32(&tk[8..]) as u64, &tk[24..])
        }
    };
    // tail = reserved(8) layer(2) alt_group(2) volume(2) reserved(2) matrix(36) width(4) height(4)
    if tail.len() < 60 {
        bail!("short tkhd");
    }
    let mut tail = tail[..60].to_vec();
    if let (Some(m), b"vide") = (rotation, &t.handler) {
        for (i, v) in m.iter().enumerate() {
            tail[16 + i * 4..20 + i * 4].copy_from_slice(&v.to_be_bytes());
        }
    }

    // chunk offsets in the virtual file
    let mut co = Vec::with_capacity(t.chunks.len());
    for &(mi, off) in &t.chunks {
        let (start, end) = regions[mi];
        if off < start || off > end {
            bail!("chunk offset outside data region");
        }
        co.push(vbase[mi] + (off - start));
    }

    w.boxed(b"trak", |w| {
        w.full(b"tkhd", 1, flags | 1, |w| {
            w.u64(ctime).u64(mtime).u32(track_id).u32(0).u64(movie_dur).bytes(&tail);
        });
        w.boxed(b"edts", |w| {
            w.full(b"elst", 1, 0, |w| {
                w.u32(1).u64(movie_dur).u64(src.edit_media_time as u64).u32(0x0001_0000);
            });
        });
        w.boxed(b"mdia", |w| {
            w.full(b"mdhd", 1, 0, |w| {
                w.u64(ctime).u64(mtime).u32(t.timescale).u64(t.duration).u16(src.language).u16(0);
            });
            w.bytes(&src.hdlr_raw);
            w.boxed(b"minf", |w| {
                w.bytes(&src.media_header_raw);
                w.bytes(&src.dinf_raw);
                w.boxed(b"stbl", |w| {
                    w.full(b"stsd", 0, 0, |w| {
                        w.u32(t.stsd.len() as u32);
                        for e in &t.stsd {
                            w.bytes(e);
                        }
                    });
                    w.full(b"stts", 0, 0, |w| {
                        w.u32(t.stts.len() as u32);
                        for &(c, d) in &t.stts {
                            w.u32(c).u32(d);
                        }
                    });
                    if let Some(ct) = &t.ctts {
                        let ver = if ct.iter().any(|e| e.1 < 0) { 1 } else { 0 };
                        w.full(b"ctts", ver, 0, |w| {
                            w.u32(ct.len() as u32);
                            for &(c, o) in ct {
                                w.u32(c).u32(o as u32);
                            }
                        });
                    }
                    if let Some(ss) = &t.stss {
                        w.full(b"stss", 0, 0, |w| {
                            w.u32(ss.len() as u32);
                            for &s in ss {
                                w.u32(s);
                            }
                        });
                    }
                    if let Some(sd) = &t.sdtp {
                        w.full(b"sdtp", 0, 0, |w| {
                            w.bytes(sd);
                        });
                    }
                    w.full(b"stsc", 0, 0, |w| {
                        w.u32(t.stsc.len() as u32);
                        for &(a, b, c) in &t.stsc {
                            w.u32(a).u32(b).u32(c);
                        }
                    });
                    w.full(b"stsz", 0, 0, |w| {
                        w.u32(0).u32(t.sizes.len() as u32);
                        for &s in &t.sizes {
                            w.u32(s);
                        }
                    });
                    w.full(b"co64", 0, 0, |w| {
                        w.u32(co.len() as u32);
                        for &o in &co {
                            w.u64(o);
                        }
                    });
                });
            });
        });
    });
    Ok(())
}
