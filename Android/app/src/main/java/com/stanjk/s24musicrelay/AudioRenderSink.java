package com.stanjk.s24musicrelay;

import android.media.AudioAttributes;
import android.media.AudioDeviceInfo;
import android.media.AudioFormat;
import android.media.AudioTrack;
import android.os.Process;
import android.os.SystemClock;
import android.util.Log;

import java.util.Arrays;

/**
 * One independently-clocked local PCM render sink.
 *
 * Capture fans out into one SPSC ring per sink. Each sink owns its AudioTrack,
 * transport elasticity, presentation delay and blocking render worker so one
 * hardware endpoint can never stall the other.
 */
final class AudioRenderSink implements AutoCloseable {
    private static final String TAG = "S24MusicRelay";

    private final String label;
    private final int sampleRate;
    private final int channels;
    private final int blockFrames;
    private final PcmSpscRing ring;
    private final FixedDelayLine delayLine;
    private final AudioTrack track;

    private volatile boolean running;
    private volatile int configuredDelayFrames;
    private volatile long renderedFrames;
    private volatile long underflowFrames;
    private volatile long droppedFrames;
    private volatile AudioDeviceInfo preferredDevice;

    private Thread renderThread;

    AudioRenderSink(
            String label,
            int sampleRate,
            int channels,
            int blockFrames,
            int ringFrames,
            int maxDelayFrames,
            AudioDeviceInfo preferredDevice) {
        this.label = label;
        this.sampleRate = sampleRate;
        this.channels = channels;
        this.blockFrames = blockFrames;
        this.ring = new PcmSpscRing(ringFrames, channels);
        this.delayLine = new FixedDelayLine(maxDelayFrames, channels);

        AudioAttributes attributes = new AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                .build();

        AudioFormat format = new AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(sampleRate)
                .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                .build();

        int minPlaybackBytes = AudioTrack.getMinBufferSize(
                sampleRate,
                AudioFormat.CHANNEL_OUT_STEREO,
                AudioFormat.ENCODING_PCM_16BIT);
        int playbackBufferBytes = Math.max(
                minPlaybackBytes,
                blockFrames * channels * 2 * 2);

        track = new AudioTrack.Builder()
                .setAudioAttributes(attributes)
                .setAudioFormat(format)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .setBufferSizeInBytes(playbackBufferBytes)
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                .build();

        if (track.getState() != AudioTrack.STATE_INITIALIZED) {
            track.release();
            throw new IllegalStateException(label + " AudioTrack not initialized");
        }

        setPreferredDevice(preferredDevice, "create");
    }

    synchronized void start() {
        if (running) return;

        running = true;
        renderThread = new Thread(this::renderLoop, "PCM-Render-" + label);
        renderThread.start();
    }

    void offer(short[] source, int samples) {
        int alignedSamples = samples - samples % channels;
        if (alignedSamples <= 0) return;

        int frames = alignedSamples / channels;
        int written = ring.write(source, 0, frames);
        if (written < frames) {
            droppedFrames += frames - written;
        }
    }

    void setDelayFrames(int frames) {
        configuredDelayFrames = Math.max(0, frames);
    }

    boolean setPreferredDevice(AudioDeviceInfo device, String reason) {
        if (device == null) {
            Log.w(TAG, "[route/" + label + "] " + reason + " preferred device missing");
            return false;
        }

        preferredDevice = device;
        boolean accepted = track.setPreferredDevice(device);
        Log.i(TAG,
                "[route/" + label + "] " + reason
                        + " accepted=" + accepted
                        + " preferred=" + describeDevice(device));
        return accepted;
    }

    int preferredDeviceId() {
        AudioDeviceInfo device = preferredDevice;
        return device == null ? -1 : device.getId();
    }

    void logRoute(String reason) {
        Log.i(TAG,
                "[route/" + label + "] " + reason
                        + " preferred=" + describeDevice(track.getPreferredDevice())
                        + " actual=" + describeDevice(track.getRoutedDevice()));
    }

    int ringFrames() {
        return ring.availableFrames();
    }

    long ringHighWaterFrames() {
        return ring.highWaterFrames();
    }

    long renderedFrames() {
        return renderedFrames;
    }

    long underflowFrames() {
        return underflowFrames;
    }

    long droppedFrames() {
        return droppedFrames;
    }

    private void renderLoop() {
        try {
            Process.setThreadPriority(Process.THREAD_PRIORITY_AUDIO);
        } catch (Throwable ignored) {
        }

        short[] transportBlock = new short[blockFrames * channels];
        short[] delayedBlock = new short[blockFrames * channels];

        while (running && ring.availableFrames() < blockFrames) {
            SystemClock.sleep(1);
        }
        if (!running) return;

        track.play();
        logRoute("play");

        while (running) {
            int framesRead = ring.read(transportBlock, 0, blockFrames);
            if (framesRead < blockFrames) {
                int missingFrames = blockFrames - framesRead;
                Arrays.fill(
                        transportBlock,
                        framesRead * channels,
                        blockFrames * channels,
                        (short) 0);
                underflowFrames += missingFrames;
            }

            delayLine.setDelayFrames(configuredDelayFrames);
            delayLine.process(
                    transportBlock,
                    0,
                    delayedBlock,
                    0,
                    blockFrames);

            int samplesWritten = 0;
            while (running && samplesWritten < delayedBlock.length) {
                int n = track.write(
                        delayedBlock,
                        samplesWritten,
                        delayedBlock.length - samplesWritten,
                        AudioTrack.WRITE_BLOCKING);

                if (n < 0) {
                    Log.e(TAG, "[render/" + label + "] AudioTrack.write=" + n);
                    running = false;
                    break;
                }
                if (n == 0) {
                    Thread.yield();
                    continue;
                }
                samplesWritten += n;
            }

            if (samplesWritten == delayedBlock.length) {
                renderedFrames += blockFrames;
            }
        }
    }

    @Override
    public synchronized void close() {
        running = false;

        try {
            track.pause();
        } catch (Throwable ignored) {
        }

        Thread thread = renderThread;
        renderThread = null;
        if (thread != null) {
            thread.interrupt();
            try {
                thread.join(500);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }

        try {
            track.flush();
        } catch (Throwable ignored) {
        }
        try {
            track.stop();
        } catch (Throwable ignored) {
        }
        track.release();
    }

    private static String describeDevice(AudioDeviceInfo device) {
        if (device == null) return "null";
        return "type=" + device.getType()
                + ",id=" + device.getId()
                + ",product=" + device.getProductName();
    }
}
