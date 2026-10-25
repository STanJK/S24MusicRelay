package com.stanjk.s24musicrelay;

import android.os.Process;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.SocketTimeoutException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.Arrays;
import java.util.TreeMap;
import java.util.concurrent.atomic.AtomicLong;

/**
 * S24MusicRelay UDP v2 LC3 receiver.
 *
 * One datagram carries exactly one 10 ms stereo LC3 frame:
 *   left[100 B] + right[100 B]
 *
 * Short packet loss is represented by LC3 PLC. Long missing runs preserve the
 * source timeline with equal-duration silence, so recovery cannot permanently
 * shift acoustic alignment.
 */
final class UdpPcmReceiver implements AutoCloseable {
    static final int PORT = 32124;

    private static final int HEADER_BYTES = 40;
    private static final int VERSION = 2;
    private static final int FORMAT_LC3 = 2;
    private static final int SAMPLE_RATE = 48_000;
    private static final int CHANNELS = 2;
    private static final int FRAME_SAMPLES = 480; // 10 ms
    private static final int LC3_BYTES_PER_CHANNEL = 100;
    private static final int PACKET_BYTES =
            HEADER_BYTES + LC3_BYTES_PER_CHANNEL * CHANNELS;

    private static final int MAX_PENDING_PACKETS = 4; // 40 ms reorder window
    private static final int MAX_PLC_PACKETS = 4;     // 40 ms conceal ceiling
    private static final byte[] MAGIC = new byte[]{'S', '2', '4', 'R'};

    static final class Stats {
        final long packetsReceived;
        final long decodeErrors;
        final long lostPackets;
        final long reorderedArrivals;
        final long duplicateOrStale;
        final long concealedGapFrames;
        final long skippedGapFrames;
        final long ringDroppedFrames;
        final long lastSequence;
        final long lastFirstFrameId;

        Stats(
                long packetsReceived,
                long decodeErrors,
                long lostPackets,
                long reorderedArrivals,
                long duplicateOrStale,
                long concealedGapFrames,
                long skippedGapFrames,
                long ringDroppedFrames,
                long lastSequence,
                long lastFirstFrameId) {
            this.packetsReceived = packetsReceived;
            this.decodeErrors = decodeErrors;
            this.lostPackets = lostPackets;
            this.reorderedArrivals = reorderedArrivals;
            this.duplicateOrStale = duplicateOrStale;
            this.concealedGapFrames = concealedGapFrames;
            this.skippedGapFrames = skippedGapFrames;
            this.ringDroppedFrames = ringDroppedFrames;
            this.lastSequence = lastSequence;
            this.lastFirstFrameId = lastFirstFrameId;
        }
    }

    private static final class Packet {
        final long sessionId;
        final long sequence;
        final long firstFrameId;
        final byte[] left;
        final byte[] right;

        Packet(
                long sessionId,
                long sequence,
                long firstFrameId,
                byte[] left,
                byte[] right) {
            this.sessionId = sessionId;
            this.sequence = sequence;
            this.firstFrameId = firstFrameId;
            this.left = left;
            this.right = right;
        }
    }

    private final PcmSpscRing ring;
    private final TreeMap<Long, Packet> pending = new TreeMap<>();
    private final Lc3StereoDecoder decoder = new Lc3StereoDecoder();

    private final AtomicLong packetsReceived = new AtomicLong();
    private final AtomicLong decodeErrors = new AtomicLong();
    private final AtomicLong lostPackets = new AtomicLong();
    private final AtomicLong reorderedArrivals = new AtomicLong();
    private final AtomicLong duplicateOrStale = new AtomicLong();
    private final AtomicLong concealedGapFrames = new AtomicLong();
    private final AtomicLong skippedGapFrames = new AtomicLong();
    private final AtomicLong ringDroppedFrames = new AtomicLong();
    private final AtomicLong lastSequence = new AtomicLong(-1);
    private final AtomicLong lastFirstFrameId = new AtomicLong(-1);

    private volatile boolean running;
    private DatagramSocket socket;
    private Thread thread;

    private boolean haveSession;
    private long sessionId;
    private long nextSequence;
    private long nextFrameId;

    UdpPcmReceiver(PcmSpscRing ring) {
        this.ring = ring;
    }

    int start() throws Exception {
        if (running) {
            return socket == null ? 0 : socket.getReceiveBufferSize();
        }

        DatagramSocket s = new DatagramSocket(PORT);
        s.setReceiveBufferSize(4 * 1024 * 1024);
        s.setSoTimeout(100);
        socket = s;

        running = true;
        thread = new Thread(this::receiveLoop, "UDP-LC3-Receiver");
        thread.start();
        return s.getReceiveBufferSize();
    }

    Stats stats() {
        return new Stats(
                packetsReceived.get(),
                decodeErrors.get(),
                lostPackets.get(),
                reorderedArrivals.get(),
                duplicateOrStale.get(),
                concealedGapFrames.get(),
                skippedGapFrames.get(),
                ringDroppedFrames.get(),
                lastSequence.get(),
                lastFirstFrameId.get());
    }

    private void receiveLoop() {
        try {
            Process.setThreadPriority(Process.THREAD_PRIORITY_AUDIO);
        } catch (Throwable ignored) {
        }

        byte[] storage = new byte[512];

        while (running) {
            DatagramSocket s = socket;
            if (s == null) break;

            DatagramPacket datagram =
                    new DatagramPacket(storage, storage.length);
            try {
                s.receive(datagram);
            } catch (SocketTimeoutException ignored) {
                continue;
            } catch (Throwable t) {
                if (running) decodeErrors.incrementAndGet();
                break;
            }

            Packet packet = decode(storage, datagram.getLength());
            if (packet == null) {
                decodeErrors.incrementAndGet();
                continue;
            }

            packetsReceived.incrementAndGet();
            lastSequence.set(packet.sequence);
            lastFirstFrameId.set(packet.firstFrameId);

            try {
                ingest(packet);
            } catch (Throwable t) {
                decodeErrors.incrementAndGet();
                // Codec state may be poisoned by a corrupt frame. Re-enter from
                // a clean state while preserving the live source timeline.
                try {
                    decoder.reset();
                } catch (Throwable ignored) {
                }
            }
        }
    }

    private void ingest(Packet packet) {
        if (!haveSession || packet.sessionId != sessionId) {
            haveSession = true;
            sessionId = packet.sessionId;
            nextSequence = packet.sequence;
            nextFrameId = packet.firstFrameId;
            pending.clear();
            ring.clear();
            decoder.reset();
        }

        if (packet.sequence < nextSequence) {
            duplicateOrStale.incrementAndGet();
            return;
        }

        if (packet.sequence > nextSequence) {
            reorderedArrivals.incrementAndGet();
            pending.putIfAbsent(packet.sequence, packet);

            if (pending.size() >= MAX_PENDING_PACKETS) {
                long firstPending = pending.firstKey();
                if (firstPending > nextSequence) {
                    long missing = firstPending - nextSequence;
                    lostPackets.addAndGet(missing);
                    representMissingPackets(missing);
                    nextSequence = firstPending;
                }
                drainPending();
            }
            return;
        }

        emit(packet);
        nextSequence++;
        drainPending();
    }

    private void drainPending() {
        while (true) {
            Packet packet = pending.remove(nextSequence);
            if (packet == null) return;

            emit(packet);
            nextSequence++;
        }
    }

    private void representMissingPackets(long count) {
        if (count <= 0) return;

        if (count <= MAX_PLC_PACKETS) {
            for (long i = 0; i < count; i++) {
                writePcm(decoder.conceal());
                concealedGapFrames.addAndGet(FRAME_SAMPLES);
                nextFrameId += FRAME_SAMPLES;
            }
            return;
        }

        long skipped = count * FRAME_SAMPLES;

        // Music-robust policy: a long missing run may lose content, but it must
        // never shorten the source timeline. Insert equal-duration silence so
        // post-outage audio keeps the same absolute presentation position.
        writeSilenceFrames(skipped);
        skippedGapFrames.addAndGet(skipped);
        nextFrameId += skipped;
        decoder.reset();
    }

    private void emit(Packet packet) {
        if (packet.firstFrameId > nextFrameId) {
            long gapFrames = packet.firstFrameId - nextFrameId;

            if (gapFrames % FRAME_SAMPLES == 0) {
                long missingPackets = gapFrames / FRAME_SAMPLES;
                representMissingPackets(missingPackets);
            } else {
                writeSilenceFrames(gapFrames);
                skippedGapFrames.addAndGet(gapFrames);
                decoder.reset();
            }

            nextFrameId = packet.firstFrameId;
        } else if (packet.firstFrameId < nextFrameId) {
            // LC3 frames are indivisible. Unlike PCM v1 we cannot trim a
            // fractional stale overlap, so discard the complete frame.
            duplicateOrStale.incrementAndGet();
            return;
        }

        writePcm(decoder.decode(packet.left, packet.right));
        nextFrameId = packet.firstFrameId + FRAME_SAMPLES;
    }

    private void writeSilenceFrames(long frames) {
        short[] zero = new short[FRAME_SAMPLES * CHANNELS];
        long remaining = frames;

        while (remaining > 0) {
            int chunk = (int) Math.min(FRAME_SAMPLES, remaining);
            int written = ring.write(zero, 0, chunk);
            if (written < chunk) {
                ringDroppedFrames.addAndGet(chunk - written);
            }
            remaining -= chunk;
        }
    }

    private void writePcm(short[] stereo) {
        int frames = stereo.length / CHANNELS;
        int written = ring.write(stereo, 0, frames);
        if (written < frames) {
            ringDroppedFrames.addAndGet(frames - written);
        }
    }

    private Packet decode(byte[] bytes, int length) {
        if (length != PACKET_BYTES) return null;

        for (int i = 0; i < MAGIC.length; i++) {
            if (bytes[i] != MAGIC[i]) return null;
        }

        ByteBuffer b =
                ByteBuffer.wrap(bytes, 0, length).order(ByteOrder.LITTLE_ENDIAN);
        b.position(4);

        int version = b.get() & 0xff;
        b.get(); // flags
        int headerBytes = b.getShort() & 0xffff;
        long session = b.getLong();
        long sequence = b.getLong();
        long firstFrame = b.getLong();
        int frameSamples = b.getShort() & 0xffff;
        int channels = b.get() & 0xff;
        int format = b.get() & 0xff;
        int sampleRate = b.getInt();

        if (version != VERSION
                || headerBytes != HEADER_BYTES
                || frameSamples != FRAME_SAMPLES
                || channels != CHANNELS
                || format != FORMAT_LC3
                || sampleRate != SAMPLE_RATE) {
            return null;
        }

        byte[] left = Arrays.copyOfRange(
                bytes,
                HEADER_BYTES,
                HEADER_BYTES + LC3_BYTES_PER_CHANNEL);
        byte[] right = Arrays.copyOfRange(
                bytes,
                HEADER_BYTES + LC3_BYTES_PER_CHANNEL,
                PACKET_BYTES);

        return new Packet(session, sequence, firstFrame, left, right);
    }

    @Override
    public void close() {
        running = false;

        DatagramSocket s = socket;
        socket = null;
        if (s != null) s.close();

        Thread t = thread;
        thread = null;
        if (t != null) {
            t.interrupt();
            try {
                t.join(500);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }

        pending.clear();
        decoder.close();
    }
}
