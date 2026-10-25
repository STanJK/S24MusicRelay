# S24MusicRelay

Minimal Android playback-capture probe for the Galaxy S24+ experiment.

## v0.0.1 scope

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
- No network relay, local re-render, crossover, delay alignment, or DSP yet.

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
- Android Gradle Plugin 8.13.2
- Gradle 8.13

The Gradle wrapper is intentionally not committed in this first probe commit; Android Studio can sync the project directly, or generate the wrapper with:

```bash
gradle wrapper --gradle-version 8.13
```
