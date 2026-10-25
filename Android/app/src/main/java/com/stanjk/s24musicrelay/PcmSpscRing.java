package com.stanjk.s24musicrelay;

/**
 * Single-producer / single-consumer PCM ring.
 *
 * Indices are monotonic frame counters. The backing storage wraps, the timeline does not.
 * This ring is transport elasticity only; it does not implement presentation delay.
 */
final class PcmSpscRing {
    private final short[] data;
    private final int channels;
    private final int capacityFrames;
    private final int frameMask;

    private volatile long readFrame;
    private volatile long writeFrame;
    private volatile long highWaterFrames;

    PcmSpscRing(int capacityFrames, int channels) {
        if (Integer.bitCount(capacityFrames) != 1) {
            throw new IllegalArgumentException("capacityFrames must be a power of two");
        }
        if (channels <= 0) {
            throw new IllegalArgumentException("channels must be > 0");
        }
        this.capacityFrames = capacityFrames;
        this.channels = channels;
        this.frameMask = capacityFrames - 1;
        this.data = new short[capacityFrames * channels];
    }

    int write(short[] source, int sourceOffsetSamples, int requestedFrames) {
        if (requestedFrames <= 0) return 0;

        final long localWrite = writeFrame;
        final long localRead = readFrame;
        final long used = localWrite - localRead;
        final int freeFrames = (int) Math.max(0L, capacityFrames - used);
        final int framesToWrite = Math.min(requestedFrames, freeFrames);
        if (framesToWrite <= 0) return 0;

        final int writeIndex = (int) (localWrite & frameMask);
        final int firstFrames = Math.min(framesToWrite, capacityFrames - writeIndex);
        final int secondFrames = framesToWrite - firstFrames;

        System.arraycopy(
                source,
                sourceOffsetSamples,
                data,
                writeIndex * channels,
                firstFrames * channels);

        if (secondFrames > 0) {
            System.arraycopy(
                    source,
                    sourceOffsetSamples + firstFrames * channels,
                    data,
                    0,
                    secondFrames * channels);
        }

        // Publish after all PCM writes are visible.
        final long newWrite = localWrite + framesToWrite;
        writeFrame = newWrite;

        final long occupancy = newWrite - readFrame;
        if (occupancy > highWaterFrames) {
            highWaterFrames = occupancy;
        }

        return framesToWrite;
    }

    int read(short[] destination, int destinationOffsetSamples, int requestedFrames) {
        if (requestedFrames <= 0) return 0;

        final long localRead = readFrame;
        final long localWrite = writeFrame;
        final long available = localWrite - localRead;
        final int framesToRead = (int) Math.min(requestedFrames, Math.max(0L, available));
        if (framesToRead <= 0) return 0;

        final int readIndex = (int) (localRead & frameMask);
        final int firstFrames = Math.min(framesToRead, capacityFrames - readIndex);
        final int secondFrames = framesToRead - firstFrames;

        System.arraycopy(
                data,
                readIndex * channels,
                destination,
                destinationOffsetSamples,
                firstFrames * channels);

        if (secondFrames > 0) {
            System.arraycopy(
                    data,
                    0,
                    destination,
                    destinationOffsetSamples + firstFrames * channels,
                    secondFrames * channels);
        }

        // Release storage after all PCM reads are complete.
        readFrame = localRead + framesToRead;
        return framesToRead;
    }

    int availableFrames() {
        final long available = writeFrame - readFrame;
        if (available <= 0) return 0;
        return (int) Math.min(available, capacityFrames);
    }

    long highWaterFrames() {
        return highWaterFrames;
    }

    int capacityFrames() {
        return capacityFrames;
    }
}
