// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Block-body encoding benchmark for ADR-046 chunk format v2.
//!
//! Compares candidate block bodies on the same corpus for compressed size
//! and per-query decode cost:
//!
//! * `columnar` — the original v2 draft (`ts i64[]`, `level u8[]`,
//!   `stream u8[]`, msg offsets + bytes, fields offsets + bytes)
//! * `columnar-d` — what ships: timestamps delta-varint encoded and offsets
//!   stored as varint lengths (2× smaller on real logs)
//! * `ndjson` — one JSON object per line (`zstd -dc | jq` friendly)
//! * `text` — `<rfc3339> <stream> <LEVEL> <msg>[ <fields-json>]`
//!   (`zstd -dc | grep` friendly)
//!
//! For each body × zstd level × block size it reports: compressed bytes,
//! ratio vs raw text, encode time, and three reader workloads over the
//! whole chunk: full decode (materialise every line), `ERROR`-only filter,
//! and a substring needle search.
//!
//! Run: `cargo run --release -p temps-log-aggregator --example format_bench -- <logfile> [max_lines]`

use std::io::{BufRead, BufReader};
use std::time::Instant;

use chrono::{DateTime, TimeZone, Utc};
use memchr::memmem;

#[derive(Clone)]
struct Line {
    ts_nanos: i64,
    level: u8,  // 0..=4
    stream: u8, // 0 stdout / 1 stderr
    msg: String,
    fields: Option<String>, // pre-serialised JSON object
}

const LEVEL_NAMES: [&str; 5] = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"];
const STREAM_NAMES: [&str; 2] = ["stdout", "stderr"];

fn load(path: &str, max: usize) -> Vec<Line> {
    let f = std::fs::File::open(path).expect("open corpus");
    let ansi = regex::Regex::new("\x1b\\[[0-9;]*m").unwrap();
    let mut out = Vec::new();
    let mut ts = Utc
        .with_ymd_and_hms(2026, 9, 19, 10, 0, 0)
        .unwrap()
        .timestamp_nanos_opt()
        .unwrap();
    let mut n = 0u64;
    for l in BufReader::new(f).lines().map_while(Result::ok) {
        let l = ansi.replace_all(&l, "").into_owned();
        if l.trim().is_empty() {
            continue;
        }
        // Real tracing lines start with an RFC3339 timestamp; use it when present so
        // time deltas are realistic, otherwise synthesise a steady 1–7 ms cadence.
        let (ts_nanos, rest) = match l.get(0..27).and_then(|s| s.parse::<DateTime<Utc>>().ok()) {
            Some(t) => (t.timestamp_nanos_opt().unwrap_or(ts), l[28..].to_string()),
            None => {
                ts += 1_000_000 + (n % 7) as i64 * 1_000_000;
                (ts, l)
            }
        };
        let level = LEVEL_NAMES
            .iter()
            .position(|name| rest.contains(name))
            .unwrap_or(2) as u8;
        let stream = if level >= 3 { 1 } else { 0 };
        // Every 5th line carries a small structured fields object, roughly the
        // share of JSON-structured lines we see in real app logs.
        let fields = if n.is_multiple_of(5) {
            Some(format!(
                r#"{{"request_id":"req-{n}","duration_ms":{},"status":{}}}"#,
                n % 350,
                if level == 4 { 500 } else { 200 }
            ))
        } else {
            None
        };
        out.push(Line {
            ts_nanos,
            level,
            stream,
            msg: rest,
            fields,
        });
        n += 1;
        if out.len() >= max {
            break;
        }
    }
    out
}

// ─── body encoders ────────────────────────────────────────────────────────

trait Body {
    fn name(&self) -> &'static str;
    fn encode(&self, lines: &[Line]) -> Vec<u8>;
    /// Materialise every line (message string included).
    fn decode_all(&self, body: &[u8]) -> usize;
    /// Count + materialise only lines with level == ERROR.
    fn decode_level(&self, body: &[u8], level: u8) -> usize;
    /// Count lines whose message contains `needle`.
    fn needle(&self, body: &[u8], needle: &memmem::Finder) -> usize;
}

fn ts_str(nanos: i64) -> String {
    let secs = nanos.div_euclid(1_000_000_000);
    let sub = nanos.rem_euclid(1_000_000_000) as u32;
    Utc.timestamp_opt(secs, sub)
        .single()
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

struct Columnar;
impl Body for Columnar {
    fn name(&self) -> &'static str {
        "columnar"
    }
    fn encode(&self, lines: &[Line]) -> Vec<u8> {
        let n = lines.len() as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(&n.to_le_bytes());
        for l in lines {
            buf.extend_from_slice(&l.ts_nanos.to_le_bytes());
        }
        for l in lines {
            buf.push(l.level);
        }
        for l in lines {
            buf.push(l.stream);
        }
        let mut off = 0u32;
        buf.extend_from_slice(&off.to_le_bytes());
        for l in lines {
            off += l.msg.len() as u32;
            buf.extend_from_slice(&off.to_le_bytes());
        }
        for l in lines {
            buf.extend_from_slice(l.msg.as_bytes());
        }
        let mut off = 0u32;
        buf.extend_from_slice(&off.to_le_bytes());
        for l in lines {
            off += l.fields.as_ref().map_or(0, |f| f.len()) as u32;
            buf.extend_from_slice(&off.to_le_bytes());
        }
        for l in lines {
            if let Some(f) = &l.fields {
                buf.extend_from_slice(f.as_bytes());
            }
        }
        buf
    }
    fn decode_all(&self, body: &[u8]) -> usize {
        let (n, ts, lv, _st, moff, mbytes) = columnar_view(body);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let (a, b) = (moff[i] as usize, moff[i + 1] as usize);
            out.push((
                ts[i],
                lv[i],
                String::from_utf8_lossy(&mbytes[a..b]).into_owned(),
            ));
        }
        out.len()
    }
    fn decode_level(&self, body: &[u8], level: u8) -> usize {
        let (n, ts, lv, _st, moff, mbytes) = columnar_view(body);
        let mut out = Vec::new();
        for i in 0..n {
            if lv[i] == level {
                let (a, b) = (moff[i] as usize, moff[i + 1] as usize);
                out.push((ts[i], String::from_utf8_lossy(&mbytes[a..b]).into_owned()));
            }
        }
        out.len()
    }
    fn needle(&self, body: &[u8], needle: &memmem::Finder) -> usize {
        let (_n, _ts, _lv, _st, moff, mbytes) = columnar_view(body);
        // One pass over the contiguous message column; map hits back to lines.
        let mut hits = 0;
        let mut pos = 0;
        while let Some(p) = needle.find(&mbytes[pos..]) {
            let abs = pos + p;
            let line = moff.partition_point(|&o| (o as usize) <= abs) - 1;
            hits += 1;
            pos = moff[line + 1] as usize;
            if pos >= mbytes.len() {
                break;
            }
        }
        hits
    }
}

#[allow(clippy::type_complexity)]
fn columnar_view(body: &[u8]) -> (usize, Vec<i64>, &[u8], &[u8], Vec<u32>, &[u8]) {
    let n = u32::from_le_bytes(body[0..4].try_into().unwrap()) as usize;
    let mut c = 4;
    let ts: Vec<i64> = (0..n)
        .map(|i| i64::from_le_bytes(body[c + i * 8..c + i * 8 + 8].try_into().unwrap()))
        .collect();
    c += n * 8;
    let lv = &body[c..c + n];
    c += n;
    let st = &body[c..c + n];
    c += n;
    let moff: Vec<u32> = (0..=n)
        .map(|i| u32::from_le_bytes(body[c + i * 4..c + i * 4 + 4].try_into().unwrap()))
        .collect();
    c += (n + 1) * 4;
    let mlen = *moff.last().unwrap() as usize;
    let mbytes = &body[c..c + mlen];
    (n, ts, lv, st, moff, mbytes)
}

fn put_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}
fn get_varint(buf: &[u8], c: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = buf[*c];
        *c += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return v;
        }
        shift += 7;
    }
}
fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}
fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

struct ColumnarDelta;
impl Body for ColumnarDelta {
    fn name(&self) -> &'static str {
        "columnar-d"
    }
    fn encode(&self, lines: &[Line]) -> Vec<u8> {
        let mut buf = Vec::new();
        put_varint(&mut buf, lines.len() as u64);
        let mut prev = 0i64;
        for l in lines {
            put_varint(&mut buf, zigzag(l.ts_nanos - prev));
            prev = l.ts_nanos;
        }
        for l in lines {
            buf.push(l.level);
        }
        for l in lines {
            buf.push(l.stream);
        }
        for l in lines {
            put_varint(&mut buf, l.msg.len() as u64);
        }
        for l in lines {
            buf.extend_from_slice(l.msg.as_bytes());
        }
        for l in lines {
            put_varint(&mut buf, l.fields.as_ref().map_or(0, |f| f.len()) as u64);
        }
        for l in lines {
            if let Some(f) = &l.fields {
                buf.extend_from_slice(f.as_bytes());
            }
        }
        buf
    }
    fn decode_all(&self, body: &[u8]) -> usize {
        let (ts, lv, moff, mbytes) = delta_view(body);
        let mut out = Vec::with_capacity(ts.len());
        for i in 0..ts.len() {
            out.push((
                ts[i],
                lv[i],
                String::from_utf8_lossy(&mbytes[moff[i]..moff[i + 1]]).into_owned(),
            ));
        }
        out.len()
    }
    fn decode_level(&self, body: &[u8], level: u8) -> usize {
        let (ts, lv, moff, mbytes) = delta_view(body);
        let mut out = Vec::new();
        for i in 0..ts.len() {
            if lv[i] == level {
                out.push((
                    ts[i],
                    String::from_utf8_lossy(&mbytes[moff[i]..moff[i + 1]]).into_owned(),
                ));
            }
        }
        out.len()
    }
    fn needle(&self, body: &[u8], needle: &memmem::Finder) -> usize {
        let (_ts, _lv, moff, mbytes) = delta_view(body);
        let mut hits = 0;
        let mut pos = 0;
        while let Some(p) = needle.find(&mbytes[pos..]) {
            let abs = pos + p;
            let line = moff.partition_point(|&o| o <= abs) - 1;
            hits += 1;
            pos = moff[line + 1];
            if pos >= mbytes.len() {
                break;
            }
        }
        hits
    }
}

fn delta_view(body: &[u8]) -> (Vec<i64>, &[u8], Vec<usize>, &[u8]) {
    let mut c = 0;
    let n = get_varint(body, &mut c) as usize;
    let mut ts = Vec::with_capacity(n);
    let mut prev = 0i64;
    for _ in 0..n {
        prev += unzigzag(get_varint(body, &mut c));
        ts.push(prev);
    }
    let lv = &body[c..c + n];
    c += 2 * n; // level + stream
    let mut moff = Vec::with_capacity(n + 1);
    let mut acc = 0usize;
    moff.push(0);
    for _ in 0..n {
        acc += get_varint(body, &mut c) as usize;
        moff.push(acc);
    }
    let mbytes = &body[c..c + acc];
    (ts, lv, moff, mbytes)
}

struct Ndjson;
impl Body for Ndjson {
    fn name(&self) -> &'static str {
        "ndjson"
    }
    fn encode(&self, lines: &[Line]) -> Vec<u8> {
        let mut buf = Vec::new();
        for l in lines {
            let mut obj = serde_json::Map::new();
            obj.insert("ts".into(), ts_str(l.ts_nanos).into());
            obj.insert("level".into(), LEVEL_NAMES[l.level as usize].into());
            obj.insert("stream".into(), STREAM_NAMES[l.stream as usize].into());
            obj.insert("msg".into(), l.msg.clone().into());
            if let Some(f) = &l.fields {
                obj.insert("fields".into(), serde_json::from_str(f).unwrap());
            }
            serde_json::to_writer(&mut buf, &obj).unwrap();
            buf.push(b'\n');
        }
        buf
    }
    fn decode_all(&self, body: &[u8]) -> usize {
        let mut out = Vec::new();
        for l in body.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let v: serde_json::Value = serde_json::from_slice(l).unwrap();
            out.push((
                v["ts"].as_str().unwrap().parse::<DateTime<Utc>>().unwrap(),
                v["level"].as_str().unwrap().to_string(),
                v["msg"].as_str().unwrap().to_string(),
            ));
        }
        out.len()
    }
    fn decode_level(&self, body: &[u8], level: u8) -> usize {
        // Cheap pre-check on the raw bytes, full parse only for candidates.
        let probe = format!("\"level\":\"{}\"", LEVEL_NAMES[level as usize]);
        let finder = memmem::Finder::new(probe.as_bytes());
        let mut out = Vec::new();
        for l in body.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            if finder.find(l).is_none() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_slice(l).unwrap();
            if v["level"].as_str() == Some(LEVEL_NAMES[level as usize]) {
                out.push((
                    v["ts"].as_str().unwrap().parse::<DateTime<Utc>>().unwrap(),
                    v["msg"].as_str().unwrap().to_string(),
                ));
            }
        }
        out.len()
    }
    fn needle(&self, body: &[u8], needle: &memmem::Finder) -> usize {
        // Raw scan over the whole body; a hit is confirmed by parsing the line
        // (the needle could otherwise sit in a key or in `fields`).
        let mut hits = 0;
        let mut pos = 0;
        while let Some(p) = needle.find(&body[pos..]) {
            let abs = pos + p;
            let start = body[..abs]
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |i| i + 1);
            let end = body[abs..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(body.len(), |i| abs + i);
            let v: serde_json::Value = serde_json::from_slice(&body[start..end]).unwrap();
            if needle
                .find(v["msg"].as_str().unwrap_or("").as_bytes())
                .is_some()
            {
                hits += 1;
            }
            pos = end;
            if pos >= body.len() {
                break;
            }
        }
        hits
    }
}

struct Text;
impl Body for Text {
    fn name(&self) -> &'static str {
        "text"
    }
    fn encode(&self, lines: &[Line]) -> Vec<u8> {
        let mut buf = Vec::new();
        for l in lines {
            buf.extend_from_slice(ts_str(l.ts_nanos).as_bytes());
            buf.push(b' ');
            buf.extend_from_slice(STREAM_NAMES[l.stream as usize].as_bytes());
            buf.push(b' ');
            buf.extend_from_slice(LEVEL_NAMES[l.level as usize].as_bytes());
            buf.push(b' ');
            // Messages may contain newlines: escape as in NDJSON would. We assume
            // the writer normalises them (as the collector does today).
            buf.extend_from_slice(l.msg.replace('\n', "\\n").as_bytes());
            if let Some(f) = &l.fields {
                buf.push(b'\t');
                buf.extend_from_slice(f.as_bytes());
            }
            buf.push(b'\n');
        }
        buf
    }
    fn decode_all(&self, body: &[u8]) -> usize {
        let mut out = Vec::new();
        for l in body.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let s = std::str::from_utf8(l).unwrap();
            let mut it = s.splitn(4, ' ');
            let ts = it.next().unwrap().parse::<DateTime<Utc>>().unwrap();
            let _stream = it.next().unwrap();
            let level = it.next().unwrap();
            let rest = it.next().unwrap_or("");
            let msg = rest.split('\t').next().unwrap_or("");
            out.push((ts, level.to_string(), msg.to_string()));
        }
        out.len()
    }
    fn decode_level(&self, body: &[u8], level: u8) -> usize {
        let name = LEVEL_NAMES[level as usize].as_bytes();
        let mut out = Vec::new();
        for l in body.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            // ts(30) ' ' stream(6) ' ' LEVEL — check the fixed level column first.
            let lvl_start = 30 + 1 + 6 + 1;
            if l.len() <= lvl_start + name.len() || &l[lvl_start..lvl_start + name.len()] != name {
                continue;
            }
            let s = std::str::from_utf8(l).unwrap();
            let mut it = s.splitn(4, ' ');
            let ts = it.next().unwrap().parse::<DateTime<Utc>>().unwrap();
            let rest = it.nth(2).unwrap_or("");
            out.push((ts, rest.split('\t').next().unwrap_or("").to_string()));
        }
        out.len()
    }
    fn needle(&self, body: &[u8], needle: &memmem::Finder) -> usize {
        let mut hits = 0;
        let mut pos = 0;
        while let Some(p) = needle.find(&body[pos..]) {
            let abs = pos + p;
            let end = body[abs..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(body.len(), |i| abs + i);
            hits += 1;
            pos = end;
            if pos >= body.len() {
                break;
            }
        }
        hits
    }
}

// ─── harness ──────────────────────────────────────────────────────────────

struct Result_ {
    body: &'static str,
    level: i32,
    block_kb: usize,
    raw: usize,
    compressed: usize,
    encode_ms: f64,
    full_ms: f64,
    error_ms: f64,
    needle_ms: f64,
}

fn split_blocks(lines: &[Line], target: usize) -> Vec<&[Line]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut acc = 0;
    for (i, l) in lines.iter().enumerate() {
        acc += l.msg.len() + l.fields.as_ref().map_or(0, |f| f.len()) + 48;
        if acc >= target {
            out.push(&lines[start..=i]);
            start = i + 1;
            acc = 0;
        }
    }
    if start < lines.len() {
        out.push(&lines[start..]);
    }
    out
}

fn bench(body: &dyn Body, lines: &[Line], level: i32, block_bytes: usize, needle: &str) -> Result_ {
    let blocks = split_blocks(lines, block_bytes);
    let t = Instant::now();
    let encoded: Vec<Vec<u8>> = blocks
        .iter()
        .map(|b| zstd::encode_all(body.encode(b).as_slice(), level).unwrap())
        .collect();
    let encode_ms = t.elapsed().as_secs_f64() * 1e3;
    let raw: usize = blocks.iter().map(|b| body.encode(b).len()).sum();
    let compressed: usize = encoded.iter().map(Vec::len).sum();

    let reps = 3;
    let mut full = 0f64;
    let mut err = 0f64;
    let mut ndl = 0f64;
    let finder = memmem::Finder::new(needle.as_bytes());
    let mut check = (0, 0, 0);
    for _ in 0..reps {
        let t = Instant::now();
        check.0 = encoded
            .iter()
            .map(|e| body.decode_all(&zstd::decode_all(e.as_slice()).unwrap()))
            .sum();
        full += t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        check.1 = encoded
            .iter()
            .map(|e| body.decode_level(&zstd::decode_all(e.as_slice()).unwrap(), 4))
            .sum();
        err += t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        check.2 = encoded
            .iter()
            .map(|e| body.needle(&zstd::decode_all(e.as_slice()).unwrap(), &finder))
            .sum();
        ndl += t.elapsed().as_secs_f64() * 1e3;
    }
    eprintln!(
        "  {:<11} L{level:<2} {:>4}K  lines={} errors={} needle_hits={}",
        body.name(),
        block_bytes / 1024,
        check.0,
        check.1,
        check.2
    );
    Result_ {
        body: body.name(),
        level,
        block_kb: block_bytes / 1024,
        raw,
        compressed,
        encode_ms,
        full_ms: full / reps as f64,
        error_ms: err / reps as f64,
        needle_ms: ndl / reps as f64,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .expect("usage: format_bench <logfile> [max_lines] [needle]");
    let max: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(200_000);
    let needle = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "connection refused".into());
    let lines = load(path, max);
    let text_bytes: usize = lines.iter().map(|l| l.msg.len() + 40).sum();
    println!(
        "corpus: {} lines, ~{:.1} MB as plain text, needle={needle:?}",
        lines.len(),
        text_bytes as f64 / 1e6
    );

    let bodies: Vec<Box<dyn Body>> = vec![
        Box::new(Columnar),
        Box::new(ColumnarDelta),
        Box::new(Ndjson),
        Box::new(Text),
    ];
    let mut results = Vec::new();
    for body in &bodies {
        for &level in &[1, 3, 6, 9, 19] {
            results.push(bench(body.as_ref(), &lines, level, 256 * 1024, &needle));
        }
    }
    // Block-size sweep at the default level for the two front-runners.
    for body in &bodies {
        for &kb in &[64usize, 1024, 4096] {
            results.push(bench(body.as_ref(), &lines, 3, kb * 1024, &needle));
        }
    }

    println!();
    println!(
        "{:<11} {:>3} {:>6} {:>9} {:>9} {:>7} {:>9} {:>9} {:>9} {:>9}",
        "body",
        "lvl",
        "block",
        "raw KB",
        "zstd KB",
        "ratio",
        "enc ms",
        "full ms",
        "ERROR ms",
        "needle ms"
    );
    for r in &results {
        println!(
            "{:<11} {:>3} {:>5}K {:>9} {:>9} {:>6.1}x {:>9.1} {:>9.1} {:>9.1} {:>9.1}",
            r.body,
            r.level,
            r.block_kb,
            r.raw / 1024,
            r.compressed / 1024,
            text_bytes as f64 / r.compressed as f64,
            r.encode_ms,
            r.full_ms,
            r.error_ms,
            r.needle_ms
        );
    }
}
