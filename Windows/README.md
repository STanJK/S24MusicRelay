# S24MusicRelay Windows

Rust backend.

## Current output-first milestone

The first HIL path deliberately bypasses UDP:

```text
Synthetic 48 kHz Float32 source
    -> independent producer clock (+/- configurable ppm)
    -> SourcePcmRing (peek + retire)
    -> ClockServo
    -> 63-tap / 2048-phase Kaiser windowed-sinc ASRC
    -> WASAPI shared/event-driven render
    -> selected Windows endpoint
```

The WASAPI render callback is the output clock authority. The synthetic source
runs on a separate wall-clock schedule so `--source-ppm` can intentionally
stress the ASRC control loop.

Run:

```powershell
cargo run --release -- render-test --device Realtek --seconds 30
```

Stress the clock servo:

```powershell
cargo run --release -- render-test --device Realtek --seconds 60 --source-ppm 500
cargo run --release -- render-test --device Realtek --seconds 60 --source-ppm -500
```

The normal source format remains 48 kHz nominal on both sides. ASRC corrects
clock-domain mismatch; it is not a nominal 44.1 -> 48 kHz converter.

## Ownership

- `core/` — stable domain contracts.
- `plane/l2_transport/` — UDP receive/reorder/loss normalization.
- `plane/l1_media/pcm/` — continuous source PCM FIFO.
- `plane/l1_media/asrc/` — fractional FIR + clock servo + consumer runtime.
- `plane/l1_media/dsp/` — post-ASRC DSP.
- `platform/windows/` — WASAPI adapter.
- `runtime/` — orchestration/HIL and telemetry.
