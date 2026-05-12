// One-off WAV analyzer. Build: `rustc -O analyze_wav.rs && ./analyze_wav doom-audio.wav`
// Reports per-window energy, derivative spikes (click candidates), and autocorrelation tempo
// for the loudest section.

use std::env;
use std::fs;

fn main() {
    let path = env::args().nth(1).expect("usage: analyze_wav <path>");
    let data = fs::read(&path).expect("read");

    // Find data chunk.
    assert_eq!(&data[0..4], b"RIFF");
    assert_eq!(&data[8..12], b"WAVE");
    let sample_rate = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
    let num_channels = u16::from_le_bytes([data[22], data[23]]);
    println!("sample_rate={sample_rate} channels={num_channels}");

    // Walk chunks to find "data". QEMU's wav backend leaves the data
    // size field at 0 (streaming write doesn't finalise the header
    // on QEMU kill); fall back to file-tail extent when that's the
    // case.
    let mut p = 12;
    let mut data_off = 0;
    let mut data_len = 0;
    while p + 8 <= data.len() {
        let id = &data[p..p + 4];
        let sz = u32::from_le_bytes([data[p + 4], data[p + 5], data[p + 6], data[p + 7]]) as usize;
        if id == b"data" {
            data_off = p + 8;
            data_len = if sz > 0 { sz } else { data.len() - (p + 8) };
            break;
        }
        p += 8 + sz;
    }
    assert!(data_off > 0, "data chunk not found");
    println!("data_off={data_off} data_len={data_len}");

    let bps = 2 * num_channels as usize;
    let frame_count = data_len / bps;
    let duration_s = frame_count as f64 / sample_rate as f64;
    println!("duration: {duration_s:.3}s ({frame_count} frames)");

    // Read samples as interleaved i16. Take only left channel for analysis simplicity.
    let mut samples: Vec<i16> = Vec::with_capacity(frame_count);
    let mut q = data_off;
    for _ in 0..frame_count {
        let left = i16::from_le_bytes([data[q], data[q + 1]]);
        samples.push(left);
        q += bps;
    }

    // Per-second RMS to locate the active region.
    let chunk = sample_rate as usize; // 1 second
    println!("\nper-second RMS (left channel):");
    let mut active_start_sec: Option<usize> = None;
    for (i, w) in samples.chunks(chunk).enumerate() {
        let sum_sq: u64 = w.iter().map(|&s| (s as i64 * s as i64) as u64).sum();
        let rms = ((sum_sq / w.len().max(1) as u64) as f64).sqrt();
        let peak = w.iter().map(|&s| s.unsigned_abs() as u32).max().unwrap_or(0);
        if rms > 100.0 && active_start_sec.is_none() {
            active_start_sec = Some(i);
        }
        if rms > 50.0 || peak > 1000 {
            println!("  sec {i}: rms={rms:7.1}  peak={peak:5}");
        }
    }
    let active_start = active_start_sec.unwrap_or(0);
    println!("\nactive region starts ~ sec {active_start}");

    // Focus on the active region. Window = 100 ms.
    let window_ms = 100;
    let window_frames = (sample_rate as usize * window_ms) / 1000;
    let active_off = active_start * chunk;

    // Click detection: derivative spikes. A click is a sample that's >K standard deviations
    // above the local derivative median.
    let active = &samples[active_off..];
    println!("\nclick detection in active region ({} frames):", active.len());

    let mut diffs: Vec<i32> = Vec::with_capacity(active.len());
    let mut prev = active.first().copied().unwrap_or(0);
    for &s in &active[1..] {
        diffs.push((s as i32 - prev as i32).abs());
        prev = s;
    }

    // Robust statistics: use median + MAD.
    let mut sorted: Vec<i32> = diffs.clone();
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2];
    let mad_values: Vec<i32> = diffs.iter().map(|&d| (d - median).abs()).collect();
    let mut mad_sorted = mad_values.clone();
    mad_sorted.sort_unstable();
    let mad = mad_sorted[mad_sorted.len() / 2].max(1);
    println!("  derivative median={median}  mad={mad}");

    // A click is an amplitude jump > 1500 in i16 space (~ 4.5% of
    // i16::MAX). Below that, the "spike" is just normal
    // high-frequency music content; above it's audible as a sharp
    // transient.
    let click_thresh: i32 = 1500.max(median + 10 * mad);
    let mut clicks: Vec<usize> = Vec::new();
    for (i, &d) in diffs.iter().enumerate() {
        if d > click_thresh {
            clicks.push(i);
        }
    }
    println!("  threshold={click_thresh}  candidates={}", clicks.len());

    // Group adjacent clicks (within 100 frames = ~2 ms) into single events.
    let mut events: Vec<(usize, i32)> = Vec::new();
    let mut group_start: Option<usize> = None;
    let mut group_max: i32 = 0;
    for &i in &clicks {
        match group_start {
            None => {
                group_start = Some(i);
                group_max = diffs[i];
            }
            Some(start) if i - start > 100 => {
                events.push((start, group_max));
                group_start = Some(i);
                group_max = diffs[i];
            }
            _ => {
                group_max = group_max.max(diffs[i]);
            }
        }
    }
    if let Some(start) = group_start {
        events.push((start, group_max));
    }
    println!("  grouped events: {}", events.len());

    // Print first 30 events with timestamps.
    println!("\nfirst 30 click events (relative to active region start):");
    for (i, (off, mag)) in events.iter().take(30).enumerate() {
        let t_ms = (*off as f64 * 1000.0) / sample_rate as f64;
        let abs_t = (active_off + off) as f64 / sample_rate as f64;
        println!(
            "  #{i:2}  t={t_ms:8.2} ms (abs {abs_t:6.3}s)  amplitude jump={mag}"
        );
    }

    // Inter-click interval histogram.
    if events.len() >= 2 {
        let mut intervals_ms: Vec<u32> = events
            .windows(2)
            .map(|w| {
                let dt = w[1].0 - w[0].0;
                ((dt * 1000) / sample_rate as usize) as u32
            })
            .collect();
        intervals_ms.sort_unstable();
        let median_int = intervals_ms[intervals_ms.len() / 2];
        let p25 = intervals_ms[intervals_ms.len() / 4];
        let p75 = intervals_ms[(intervals_ms.len() * 3) / 4];
        println!(
            "\ninter-click intervals: median={median_int} ms  p25={p25}  p75={p75}"
        );
        // Bucketed histogram (5 ms buckets, up to 200 ms).
        let mut hist = vec![0u32; 41];
        for ms in &intervals_ms {
            let b = (*ms / 5).min(40);
            hist[b as usize] += 1;
        }
        println!("histogram (5 ms buckets, 0..200 ms):");
        for (i, count) in hist.iter().enumerate() {
            if *count > 0 {
                println!("  [{:>3}..{:>3}) ms: {count}", i * 5, i * 5 + 5);
            }
        }
    }

    // Detect tic-boundary clicks. Our m3os_sound emits 1408 frames per
    // submit at 48 kHz internal rate. QEMU resamples to the WAV's rate
    // (44100), so the boundary in WAV-frame space is 1408 * sr / 48000.
    let wav_tic_frames = (1408.0 * sample_rate as f64 / 48000.0) as usize;
    println!("\nexpected submit-tic boundary in WAV: every {wav_tic_frames} frames (1408 source frames @ 48 kHz → WAV @ {sample_rate})");
    let mut tic_offsets: Vec<i32> = Vec::new();
    for (off, _mag) in &events {
        let modu = (*off as i64) % wav_tic_frames as i64;
        let signed = if modu > (wav_tic_frames as i64 / 2) {
            modu - wav_tic_frames as i64
        } else {
            modu
        };
        tic_offsets.push(signed as i32);
    }
    if !tic_offsets.is_empty() {
        let near_tic = tic_offsets.iter().filter(|&&v| v.abs() < 64).count();
        println!(
            "tic-boundary correlation: {} of {} clicks within 64 frames of submit boundary ({}%)",
            near_tic,
            tic_offsets.len(),
            (near_tic * 100) / tic_offsets.len()
        );
    }

    // 256-frame BDL slot boundary in WAV space.
    let wav_slot_frames = (256.0 * sample_rate as f64 / 48000.0) as usize;
    let mut slot_offsets: Vec<i32> = Vec::new();
    for (off, _mag) in &events {
        let modu = (*off as i64) % wav_slot_frames as i64;
        let signed = if modu > (wav_slot_frames as i64 / 2) {
            modu - wav_slot_frames as i64
        } else {
            modu
        };
        slot_offsets.push(signed as i32);
    }
    if !slot_offsets.is_empty() {
        let near_slot = slot_offsets.iter().filter(|&&v| v.abs() < 16).count();
        println!(
            "BDL-slot correlation ({wav_slot_frames} frame stride): {} of {} clicks within 16 frames ({}%)",
            near_slot,
            slot_offsets.len(),
            (near_slot * 100) / slot_offsets.len()
        );
    }
}
