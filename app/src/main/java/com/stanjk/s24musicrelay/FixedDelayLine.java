package com.stanjk.s24musicrelay;

/**
 * Deterministic PCM presentation delay.
 *
 * This is intentionally separate from PcmSpscRing: the SPSC ring absorbs scheduling jitter,
 * while this delay line owns the user-configured presentation offset.
 *
 * Accessed only by the render thread.
 */
final class FixedDelayLine {
    private final short[] data;
    private final int channels;
    private final int capacityFrames;
    private final int frameMask;
    private final int maxDelayFrames;

    private long writeFrame;
    private int delayFrames;

    FixedDelayLine(int maxDelayFrames, int channels) {
        if (maxDelayFrames < 0) {
            throw new IllegalArgumentException("maxDelayFrames must be >= 0");
        }
        if (channels <= 0) {
            throw new IllegalArgumentException("channels must be > 0");
        }

        this.channels = channels;
        this.maxDelayFrames = maxDelayFrames;

        int capacity = 1;
        while (capacity <= maxDelayFrames) {
            capacity <<= 1;
        }
        this.capacityFrames = capacity;
        this.frameMask = capacity - 1;
        this.data = new short[capacityFrames * channels];
    }

    void setDelayFrames(int frames) {
        delayFrames = Math.max(0, Math.min(maxDelayFrames, frames));
    }

    int getDelayFrames() {
        return delayFrames;
    }

    void process(short[] input, int inputOffsetSamples, short[] output,
                 int outputOffsetSamples, int frames) {
        final int delay = delayFrames;

        for (int frame = 0; frame < frames; frame++) {
            final int writeIndex = (int) (writeFrame & frameMask) * channels;
            final int inputIndex = inputOffsetSamples + frame * channels;

            for (int channel = 0; channel < channels; channel++) {
                data[writeIndex + channel] = input[inputIndex + channel];
            }

            final long delayedFrame = writeFrame - delay;
            final int outputIndex = outputOffsetSamples + frame * channels;

            if (delayedFrame < 0) {
                for (int channel = 0; channel < channels; channel++) {
                    output[outputIndex + channel] = 0;
                }
            } else {
                final int readIndex = (int) (delayedFrame & frameMask) * channels;
                for (int channel = 0; channel < channels; channel++) {
                    output[outputIndex + channel] = data[readIndex + channel];
                }
            }

            writeFrame++;
        }
    }
}
