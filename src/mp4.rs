//! Minimal ISO-BMFF (MP4/MOV) reader: just enough to pull out the sample
//! tables of each track so they can be merged into one synthetic `moov`.

use anyhow::{Context, Result, anyhow, bail};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- box walking

pub struct BoxRef<'a> {
    pub typ: [u8; 4],
    /// Payload (after the size/type header).
    pub data: &'a [u8],
    /// Whole box, header included.
    pub raw: &'a [u8],
}

pub fn children(buf: &[u8]) -> Result<Vec<BoxRef<'_>>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= buf.len() {
        let size32 = be_u32(&buf[pos..]) as u64;
        let typ: [u8; 4] = buf[pos + 4..pos + 8].try_into().unwrap();
        let (size, hdr) = match size32 {
            0 => ((buf.len() - pos) as u64, 8),
            1 => {
                if pos + 16 > buf.len() {
                    bail!("truncated large box header");
                }
                (be_u64(&buf[pos + 8..]), 16)
            }
            s => (s, 8),
        };
        if size < hdr as u64 || pos as u64 + size > buf.len() as u64 {
            bail!("box {} has invalid size {size}", fourcc(&typ));
        }
        let end = pos + size as usize;
        out.push(BoxRef { typ, data: &buf[pos + hdr..end], raw: &buf[pos..end] });
        pos = end;
    }
    Ok(out)
}

pub fn find<'a>(buf: &'a [u8], typ: &[u8; 4]) -> Result<Option<BoxRef<'a>>> {
    Ok(children(buf)?.into_iter().find(|b| &b.typ == typ))
}

pub fn need<'a>(buf: &'a [u8], typ: &[u8; 4]) -> Result<BoxRef<'a>> {
    find(buf, typ)?.ok_or_else(|| anyhow!("missing '{}' box", fourcc(typ)))
}

pub fn fourcc(t: &[u8; 4]) -> String {
    String::from_utf8_lossy(t).into_owned()
}

pub fn be_u16(b: &[u8]) -> u16 {
    u16::from_be_bytes(b[..2].try_into().unwrap())
}
pub fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}
pub fn be_u64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b[..8].try_into().unwrap())
}

/// Bounds-checked cursor over a full-box payload.
struct Rd<'a> {
    b: &'a [u8],
    p: usize,
    what: &'static str,
}
impl<'a> Rd<'a> {
    fn new(b: &'a [u8], what: &'static str) -> Self {
        Rd { b, p: 0, what }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.p + n > self.b.len() {
            bail!("'{}' box truncated", self.what);
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(be_u32(self.take(4)?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(be_u64(self.take(8)?))
    }
    /// version byte + 24-bit flags
    fn full(&mut self) -> Result<(u8, u32)> {
        let v = self.u32()?;
        Ok(((v >> 24) as u8, v & 0xff_ffff))
    }
}

// ---------------------------------------------------------------- model

pub struct Track {
    pub handler: [u8; 4],
    pub tkhd: Vec<u8>, // payload
    pub timescale: u32,
    pub language: u16,
    pub hdlr_raw: Vec<u8>,
    pub media_header_raw: Vec<u8>, // vmhd / smhd / gmhd / nmhd ...
    pub dinf_raw: Vec<u8>,
    /// raw sample-entry boxes from stsd
    pub stsd_entries: Vec<Vec<u8>>,
    pub stts: Vec<(u32, u32)>,
    pub ctts: Option<Vec<(u32, i32)>>,
    pub stss: Option<Vec<u32>>,
    pub sdtp: Option<Vec<u8>>,
    pub stsc: Vec<(u32, u32, u32)>,
    pub sizes: Vec<u32>,
    pub chunk_offsets: Vec<u64>,
    /// media_time of the first non-empty edit (0 if none)
    pub edit_media_time: i64,
    /// total edit-list duration in movie timescale (None if no edit list)
    pub edit_duration: Option<u64>,
}

impl Track {
    pub fn sample_count(&self) -> usize {
        self.sizes.len()
    }
    /// Stream duration in microseconds as ffmpeg's mov demuxer reports it:
    /// the media span after the edit-list start offset, clipped to the edit
    /// length (edit lengths are in the coarser movie timescale, so the real
    /// sample end wins when it is shorter).
    pub fn duration_us(&self, movie_timescale: u32) -> u64 {
        let ts = self.timescale as u128;
        if ts == 0 {
            return 0;
        }
        let mut d = self.media_duration().saturating_sub(self.edit_media_time.max(0) as u64) as u128;
        if let Some(e) = self.edit_duration.filter(|&e| e > 0 && movie_timescale > 0) {
            d = d.min((e as u128 * ts + movie_timescale as u128 / 2) / movie_timescale as u128);
        }
        ((d * 1_000_000 + ts / 2) / ts) as u64
    }
    pub fn media_duration(&self) -> u64 {
        self.stts.iter().map(|&(c, d)| c as u64 * d as u64).sum()
    }
    /// (offset, byte length) of every chunk.
    pub fn chunk_extents(&self) -> Result<Vec<(u64, u64)>> {
        let n = self.chunk_offsets.len();
        let mut out = Vec::with_capacity(n);
        let mut sample = 0usize;
        for (i, &(first, spc, _)) in self.stsc.iter().enumerate() {
            let next = self.stsc.get(i + 1).map(|e| e.0 as usize).unwrap_or(n + 1);
            for chunk in first as usize..next.min(n + 1) {
                let end = sample + spc as usize;
                if end > self.sizes.len() {
                    bail!("stsc references more samples than stsz has");
                }
                let len: u64 = self.sizes[sample..end].iter().map(|&s| s as u64).sum();
                out.push((self.chunk_offsets[chunk - 1], len));
                sample = end;
            }
        }
        if out.len() != n || sample != self.sizes.len() {
            bail!("inconsistent stsc/stco/stsz tables");
        }
        Ok(out)
    }
}

pub struct Movie {
    pub path: PathBuf,
    pub file_size: u64,
    pub ftyp_raw: Vec<u8>,
    pub mvhd: Vec<u8>, // payload
    pub timescale: u32,
    pub tracks: Vec<Track>,
}

// ---------------------------------------------------------------- parsing

pub fn open_movie(path: &Path) -> Result<Movie> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let file_size = f.metadata()?.len();

    let mut ftyp_raw = None;
    let mut moov = None;
    let mut pos = 0u64;
    let mut hdr = [0u8; 16];
    while pos + 8 <= file_size {
        f.read_exact_at(&mut hdr[..8], pos)?;
        let typ: [u8; 4] = hdr[4..8].try_into().unwrap();
        let (size, _hl) = match be_u32(&hdr) {
            0 => (file_size - pos, 8),
            1 => {
                f.read_exact_at(&mut hdr[8..16], pos + 8)?;
                (be_u64(&hdr[8..]), 16)
            }
            s => (s as u64, 8),
        };
        if size < 8 || pos + size > file_size {
            bail!("{}: corrupt top-level box '{}' at {pos}", path.display(), fourcc(&typ));
        }
        match &typ {
            b"ftyp" | b"moov" => {
                let mut buf = vec![0u8; size as usize];
                f.read_exact_at(&mut buf, pos)?;
                if &typ == b"ftyp" {
                    ftyp_raw = Some(buf);
                } else {
                    moov = Some(buf);
                }
            }
            _ => {}
        }
        pos += size;
    }
    let moov = moov.ok_or_else(|| anyhow!("{}: no moov box", path.display()))?;
    let ftyp_raw = ftyp_raw.ok_or_else(|| anyhow!("{}: no ftyp box", path.display()))?;
    let moov_payload = children(&moov)?[0].data;

    let mvhd = need(moov_payload, b"mvhd")?.data.to_vec();
    let timescale = {
        let mut r = Rd::new(&mvhd, "mvhd");
        let (v, _) = r.full()?;
        r.take(if v == 1 { 16 } else { 8 })?;
        r.u32()?
    };

    let mut tracks = Vec::new();
    for b in children(moov_payload)? {
        if &b.typ == b"trak" {
            tracks.push(parse_trak(b.data).with_context(|| format!("{}: trak", path.display()))?);
        }
    }
    if find(moov_payload, b"mvex")?.is_some() {
        bail!("{}: fragmented MP4 is not supported", path.display());
    }

    Ok(Movie { path: path.to_path_buf(), file_size, ftyp_raw, mvhd, timescale, tracks })
}

fn parse_trak(trak: &[u8]) -> Result<Track> {
    let tkhd = need(trak, b"tkhd")?.data.to_vec();
    let mdia = need(trak, b"mdia")?.data;
    let mdhd = need(mdia, b"mdhd")?.data;
    let (timescale, language) = {
        let mut r = Rd::new(mdhd, "mdhd");
        let (v, _) = r.full()?;
        r.take(if v == 1 { 16 } else { 8 })?;
        let ts = r.u32()?;
        r.take(if v == 1 { 8 } else { 4 })?;
        let lang = be_u16(r.take(2)?);
        (ts, lang)
    };
    let hdlr = need(mdia, b"hdlr")?;
    let handler: [u8; 4] = hdlr.data.get(8..12).ok_or_else(|| anyhow!("short hdlr"))?.try_into()?;
    let minf = need(mdia, b"minf")?.data;
    let mut media_header_raw = Vec::new();
    for b in children(minf)? {
        if matches!(&b.typ, b"vmhd" | b"smhd" | b"gmhd" | b"nmhd" | b"hmhd" | b"sthd") {
            media_header_raw = b.raw.to_vec();
        }
    }
    let dinf_raw = need(minf, b"dinf")?.raw.to_vec();
    let stbl = need(minf, b"stbl")?.data;

    // stsd
    let stsd = need(stbl, b"stsd")?.data;
    let stsd_entries = children(stsd.get(8..).ok_or_else(|| anyhow!("short stsd"))?)?
        .into_iter()
        .map(|b| b.raw.to_vec())
        .collect();

    // stts
    let mut stts = Vec::new();
    {
        let mut r = Rd::new(need(stbl, b"stts")?.data, "stts");
        r.full()?;
        for _ in 0..r.u32()? {
            stts.push((r.u32()?, r.u32()?));
        }
    }
    // ctts
    let ctts = match find(stbl, b"ctts")? {
        None => None,
        Some(b) => {
            let mut r = Rd::new(b.data, "ctts");
            r.full()?;
            let n = r.u32()?;
            let mut v = Vec::with_capacity(n as usize);
            for _ in 0..n {
                v.push((r.u32()?, r.u32()? as i32));
            }
            Some(v)
        }
    };
    // stss
    let stss = match find(stbl, b"stss")? {
        None => None,
        Some(b) => {
            let mut r = Rd::new(b.data, "stss");
            r.full()?;
            let n = r.u32()?;
            Some((0..n).map(|_| r.u32()).collect::<Result<Vec<_>>>()?)
        }
    };
    // stsz / stz2
    let sizes = if let Some(b) = find(stbl, b"stsz")? {
        let mut r = Rd::new(b.data, "stsz");
        r.full()?;
        let fixed = r.u32()?;
        let n = r.u32()?;
        if fixed != 0 {
            vec![fixed; n as usize]
        } else {
            (0..n).map(|_| r.u32()).collect::<Result<Vec<_>>>()?
        }
    } else if let Some(b) = find(stbl, b"stz2")? {
        let mut r = Rd::new(b.data, "stz2");
        r.full()?;
        let field = r.u32()? & 0xff;
        let n = r.u32()? as usize;
        let mut v = Vec::with_capacity(n);
        match field {
            4 => {
                let bytes = r.take(n.div_ceil(2))?;
                for i in 0..n {
                    let byte = bytes[i / 2];
                    v.push(if i % 2 == 0 { byte >> 4 } else { byte & 0xf } as u32);
                }
            }
            8 => v.extend(r.take(n)?.iter().map(|&x| x as u32)),
            16 => {
                for _ in 0..n {
                    v.push(be_u16(r.take(2)?) as u32);
                }
            }
            _ => bail!("bad stz2 field size"),
        }
        v
    } else {
        bail!("missing stsz");
    };
    // sdtp (one byte per sample)
    let sdtp = match find(stbl, b"sdtp")? {
        Some(b) if b.data.len() >= 4 + sizes.len() => Some(b.data[4..4 + sizes.len()].to_vec()),
        _ => None,
    };
    // stsc
    let mut stsc = Vec::new();
    {
        let mut r = Rd::new(need(stbl, b"stsc")?.data, "stsc");
        r.full()?;
        for _ in 0..r.u32()? {
            stsc.push((r.u32()?, r.u32()?, r.u32()?));
        }
    }
    // stco / co64
    let chunk_offsets = if let Some(b) = find(stbl, b"co64")? {
        let mut r = Rd::new(b.data, "co64");
        r.full()?;
        let n = r.u32()?;
        (0..n).map(|_| r.u64()).collect::<Result<Vec<_>>>()?
    } else {
        let mut r = Rd::new(need(stbl, b"stco")?.data, "stco");
        r.full()?;
        let n = r.u32()?;
        (0..n).map(|_| r.u32().map(|x| x as u64)).collect::<Result<Vec<_>>>()?
    };

    // edit list: first non-empty edit
    let mut edit_media_time = None;
    let mut edit_duration = None;
    if let Some(edts) = find(trak, b"edts")? {
        if let Some(elst) = find(edts.data, b"elst")? {
            let mut r = Rd::new(elst.data, "elst");
            let (v, _) = r.full()?;
            let mut total = 0u64;
            for _ in 0..r.u32()? {
                let (dur, mt) = if v == 1 {
                    (r.u64()?, r.u64()? as i64)
                } else {
                    (r.u32()? as u64, r.u32()? as i32 as i64)
                };
                r.u32()?; // rate
                total += dur;
                if mt >= 0 && edit_media_time.is_none() {
                    edit_media_time = Some(mt);
                }
            }
            edit_duration = Some(total);
        }
    }
    let edit_media_time = edit_media_time.unwrap_or(0);

    Ok(Track {
        handler,
        tkhd,
        timescale,
        language,
        hdlr_raw: hdlr.raw.to_vec(),
        media_header_raw,
        dinf_raw,
        stsd_entries,
        stts,
        ctts,
        stss,
        sdtp,
        stsc,
        sizes,
        chunk_offsets,
        edit_media_time,
        edit_duration,
    })
}

// ---------------------------------------------------------------- writing

pub struct W(pub Vec<u8>);
impl W {
    pub fn new() -> Self {
        W(Vec::new())
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
    /// Write a box whose body is produced by `f`.
    pub fn boxed(&mut self, typ: &[u8; 4], f: impl FnOnce(&mut W)) -> &mut Self {
        let start = self.0.len();
        self.u32(0).bytes(typ);
        f(self);
        let size = self.0.len() - start;
        assert!(size <= u32::MAX as usize, "box too large");
        self.0[start..start + 4].copy_from_slice(&(size as u32).to_be_bytes());
        self
    }
    pub fn full(&mut self, typ: &[u8; 4], version: u8, flags: u32, f: impl FnOnce(&mut W)) -> &mut Self {
        self.boxed(typ, |w| {
            w.u32(((version as u32) << 24) | (flags & 0xff_ffff));
            f(w)
        })
    }
}
