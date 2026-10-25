# S24MusicRelay

Experimental split audio relay for Samsung Galaxy S24+ and Windows.

## Repository layout

- `Android/` — Android playback capture, local re-render, Samsung per-app volume control through Shizuku, and future UDP sender.
- `Windows/` — Rust backend for UDP receive/reorder, canonical PCM timeline, ASRC/clock recovery, DSP, and Windows render.

## Data path

```text
Android AudioPlaybackCapture
    -> local render
    -> UDP sender
    -> Windows L2 transport
    -> canonical source timeline
    -> PCM / ASRC
    -> DSP
    -> WASAPI
```

The Windows tree is intentionally scaffold-only at this stage. Module boundaries are established before implementation.
