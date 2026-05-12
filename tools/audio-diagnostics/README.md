# Audio Diagnostics

One-off Rust tools for diagnosing audio output from the m3OS smoke
gates. Not part of any binary the OS ships — these run on the host
against recorded WAV files.

## `analyze_wav.rs`

Reads a WAV file and reports:

- Per-second RMS + peak amplitude (locates the audible region)
- Click candidates (sample-by-sample derivative jumps `> 1500` i16)
- Inter-click interval histogram
- Correlation with audio-tic boundaries (1408 source-frame submits
  resampled to the WAV's rate) and BDL-slot boundaries (256 source
  frames)

Build and run:

```
rustc -O tools/audio-diagnostics/analyze_wav.rs -o /tmp/analyze_wav
/tmp/analyze_wav target/audio-smoke/doom-audio.wav
```

Designed for use against the `target/audio-smoke/doom-audio.wav`
file the `cargo xtask doom-audio-smoke` gate produces.

### Interpreting the output

- **Tic-boundary correlation > 50 %** → clicks are caused by the
  audio-server submit boundary (would indicate the pacer is broken
  or the mixer state is changing discontinuously between submits).
- **Inter-click histogram dominated by `[0..5) ms`** → continuous
  high-frequency content (drum noise, sharp musical transients).
  Not isolated artifacts.
- **Large amplitude jumps (> 5000) in short bursts followed by
  quieter ones** → percussion hit (envelope-shaped noise burst).
- **Amplitude jumps clustered at multiples of 30 ms** → note onsets
  at the music's note-pattern grid.

Used to diagnose the music-speedup + clicks symptoms during Phase
63a development; the findings are summarised in
[`docs/appendix/doom-audio-deferred-work.md`](../../docs/appendix/doom-audio-deferred-work.md).
