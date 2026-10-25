package com.stanjk.s24musicrelay;

import com.sun.jna.Function;
import com.sun.jna.Memory;
import com.sun.jna.NativeLibrary;
import com.sun.jna.Pointer;

/**
 * Thin 48 kHz stereo wrapper around Google liblc3's exported C API.
 *
 * The Mentra AAR supplies liblc3.so. Its Java helper is intentionally not used
 * because that helper fixes the codec rate at 16 kHz. We create two independent
 * mono decoder states here, matching Bluetooth LC3 stereo-split framing.
 */
final class Lc3StereoDecoder implements AutoCloseable {
    static final int SAMPLE_RATE = 48_000;
    static final int FRAME_US = 10_000;
    static final int SAMPLES_PER_FRAME = 480;
    static final int OCTETS_PER_CHANNEL = 100;

    private static final int PCM_FORMAT_S16 = 0;

    private final NativeLibrary library;
    private final Function decoderSizeFn;
    private final Function setupDecoderFn;
    private final Function decodeFn;

    private Memory leftState;
    private Memory rightState;
    private Pointer leftDecoder;
    private Pointer rightDecoder;
    private final Memory leftPcm = new Memory(SAMPLES_PER_FRAME * 2L);
    private final Memory rightPcm = new Memory(SAMPLES_PER_FRAME * 2L);
    private final Memory encoded = new Memory(OCTETS_PER_CHANNEL);

    Lc3StereoDecoder() {
        library = NativeLibrary.getInstance("lc3");
        decoderSizeFn = library.getFunction("lc3_decoder_size");
        setupDecoderFn = library.getFunction("lc3_setup_decoder");
        decodeFn = library.getFunction("lc3_decode");
        reset();
    }

    synchronized void reset() {
        int stateBytes = decoderSizeFn.invokeInt(
                new Object[]{FRAME_US, SAMPLE_RATE});
        if (stateBytes <= 0) {
            throw new IllegalStateException(
                    "lc3_decoder_size returned " + stateBytes);
        }

        Memory newLeft = new Memory(stateBytes);
        Memory newRight = new Memory(stateBytes);
        newLeft.clear();
        newRight.clear();

        Pointer newLeftDecoder = setupDecoderFn.invokePointer(
                new Object[]{FRAME_US, SAMPLE_RATE, 0, newLeft});
        Pointer newRightDecoder = setupDecoderFn.invokePointer(
                new Object[]{FRAME_US, SAMPLE_RATE, 0, newRight});

        if (newLeftDecoder == null || Pointer.nativeValue(newLeftDecoder) == 0
                || newRightDecoder == null || Pointer.nativeValue(newRightDecoder) == 0) {
            throw new IllegalStateException("lc3_setup_decoder failed");
        }

        leftState = newLeft;
        rightState = newRight;
        leftDecoder = newLeftDecoder;
        rightDecoder = newRightDecoder;
    }

    synchronized short[] decode(byte[] left, byte[] right) {
        if (left == null || right == null
                || left.length != OCTETS_PER_CHANNEL
                || right.length != OCTETS_PER_CHANNEL) {
            throw new IllegalArgumentException("LC3 stereo frame must be 100 B/channel");
        }

        decodeChannel(leftDecoder, left, leftPcm);
        decodeChannel(rightDecoder, right, rightPcm);
        return interleave();
    }

    /**
     * Run one LC3 packet-loss-concealment step on both channels.
     */
    synchronized short[] conceal() {
        decodePlc(leftDecoder, leftPcm);
        decodePlc(rightDecoder, rightPcm);
        return interleave();
    }

    private void decodeChannel(Pointer decoder, byte[] frame, Memory pcm) {
        encoded.write(0, frame, 0, frame.length);
        int result = decodeFn.invokeInt(new Object[]{
                decoder,
                encoded,
                OCTETS_PER_CHANNEL,
                PCM_FORMAT_S16,
                pcm,
                1
        });

        if (result < 0) {
            throw new IllegalStateException("lc3_decode failed: " + result);
        }
    }

    private void decodePlc(Pointer decoder, Memory pcm) {
        int result = decodeFn.invokeInt(new Object[]{
                decoder,
                Pointer.NULL,
                OCTETS_PER_CHANNEL,
                PCM_FORMAT_S16,
                pcm,
                1
        });

        // liblc3 returns 1 when PLC was operated.
        if (result < 0) {
            throw new IllegalStateException("lc3_decode PLC failed: " + result);
        }
    }

    private short[] interleave() {
        short[] stereo = new short[SAMPLES_PER_FRAME * 2];
        for (int i = 0; i < SAMPLES_PER_FRAME; i++) {
            stereo[i * 2] = leftPcm.getShort(i * 2L);
            stereo[i * 2 + 1] = rightPcm.getShort(i * 2L);
        }
        return stereo;
    }

    @Override
    public synchronized void close() {
        leftDecoder = null;
        rightDecoder = null;
        leftState = null;
        rightState = null;
        library.close();
    }
}
