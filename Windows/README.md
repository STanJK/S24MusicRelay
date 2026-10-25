# S24MusicRelay Windows

Rust backend scaffold.

Ownership follows the same principle used by BE200AudioHub:

- `core/` — stable session/domain types; no transport or platform ownership.
- `plane/l2_transport/` — UDP receive, packet contract, sequence/reorder/loss normalization.
- `plane/l1_media/` — continuous PCM timeline, source FIFO, ASRC clock-domain crossing, DSP.
- `platform/windows/` — Windows-specific render adapter (WASAPI).
- `runtime/` — worker lifetime, orchestration, and telemetry.

Important boundary:

```text
UDP datagrams
  -> reorder/loss normalization
  -> continuous source-frame timeline
  -> ASRC
  -> DSP
  -> WASAPI
```

ASRC never owns UDP semantics. Transport never owns render-clock policy.
