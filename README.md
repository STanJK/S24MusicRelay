# S24MusicRelay

Minimal Android playback-capture probe for the Galaxy S24+ experiment.

## Current scope

- Uses `MediaProjection` + `AudioPlaybackCapture`.
- Captures global capturable playback for:
  - `USAGE_MEDIA`
  - `USAGE_GAME`
  - `USAGE_UNKNOWN`
- Explicitly excludes this app's UID with `excludeUid(Process.myUid())`.
- Capture format: 48 kHz, stereo, PCM16.
- Displays:
  - total captured frames
  - normalized peak
  - normalized RMS
  - first 8 raw PCM16 sample values
- Local re-render path is now enabled:
  - capture thread -> SPSC transport ring
  - independent fixed presentation-delay line
  - AudioTrack streaming renderer
- Transport ring and presentation delay are intentionally separate concepts.
- UI exposes 0-500 ms local presentation delay and ring/render telemetry.
- No network relay, crossover, PC receiver, or clock recovery yet.

## Test

1. Build/install the app.
2. Tap **Start global capture** and grant RECORD_AUDIO + screen/audio capture consent.
3. Play music in a capturable app.
4. Confirm peak/RMS and raw PCM values move.
5. In Samsung Sound Assistant, mute or reduce only that source app.
6. Observe whether this capture remains full-scale or follows the per-app gain.

That last test tells us whether Samsung's per-app gain/mute is downstream or upstream of the public playback-capture tap.

## Build requirements

- JDK 17
- Android SDK 36
- Android Gradle Plugin 9.4.1
- Gradle 9.6

The Gradle wrapper is intentionally not committed in this first probe commit; Android Studio can sync the project directly, or generate the wrapper with:

```bash
gradle wrapper --gradle-version 9.6.0
```


## Local re-render test

1. In Samsung Sound Assistant, mute only the source music app.
2. Start capture in S24MusicRelay.
3. The source app should remain muted while S24MusicRelay re-renders the captured PCM through its own AudioTrack.
4. Move **Local presentation delay** from 0 ms upward and verify the re-rendered audio follows the configured delay.
5. Watch **Transport ring** telemetry separately. Ring occupancy/high-water are transport-health metrics, not the configured presentation delay.

Current audio path:

```
AudioPlaybackCapture
        |
        v
PcmSpscRing          (elasticity / thread decoupling only)
        |
        v
FixedDelayLine       (explicit presentation offset)
        |
        v
AudioTrack
        |
        v
S24+ speakers
```
