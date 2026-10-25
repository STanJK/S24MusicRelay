package com.stanjk.s24musicrelay;

import android.os.Process;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.SocketTimeoutException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.TreeMap;
import java.util.concurrent.atomic.AtomicLong;

/**
 * S24MusicRelay UDP v1 receiver.
 *
 * Network semantics terminate here: bounded packet reordering and short-gap
 * silence concealment are converted into one continuous PCM stream before the
 * AudioTrack consumer sees it.
 */
final class UdpPcmReceiver implements AutoCloseable {
    static final int PORT = 32124;

    private static final int HEADER_BYTES = 40;
    private static final int SAMPLE_RATE = 48_000;
    private static final int CHANNELS = 2;
    private static final int FORMAT_PCM16_LE = 1;
    private static final int MAX_PACKET_FRAMES = 240; // 5 ms
    private static final int MAX_PENDING_PACKETS = 4; // ~20 ms reorder window
    private static final int MAX_INLINE_CONCEAL_FRAMES = 960; // 20 ms
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
        final int frames;
        final short[] samples;

        Packet(long sessionId, long sequence, long firstFrameId, int frames, short[] samples) {
            this.sessionId = sessionId;
            this.sequence = sequence;
            this.firstFrameId = firstFrameId;
            this.frames = frames;
            this.samples = samples;
        }
    }

    private final PcmSpscRing ring;
    private final TreeMap<Long, Packet> pending = new TreeMap<>();

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
        if (running) return socket == null ? 0 : socket.getReceiveBufferSize();

        DatagramSocket s = new DatagramSocket(PORT);
        s.setReceiveBufferSize(4 * 1024 * 1024);
        s.setSoTimeout(100);
        socket = s;

        running = true;
        thread = new Thread(this::receiveLoop, "UDP-PCM-Receiver");
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

        byte[] storage = new byte[2048];

        while (running) {
            DatagramSocket s = socket;
            if (s == null) break;

            DatagramPacket datagram = new DatagramPacket(storage, storage.length);
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
            ingest(packet);
        }
    }

    private void ingest(Packet packet) {
        if (!haveSession || packet.sessionId != sessionId) {
            haveSession = true;
            sessionId = packet.sessionId;
            nextSequence = packet.sequence;
            nextFrameId = packet.firstFrameId;
            pending.clear();
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
                    lostPackets.addAndGet(firstPending - nextSequence);
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

    private void emit(Packet packet) {
        int trimFrames = 0;

        if (packet.firstFrameId > nextFrameId) {
            long gapLong = packet.firstFrameId - nextFrameId;
            if (gapLong <= MAX_INLINE_CONCEAL_FRAMES) {
                int gap = (int) gapLong;
                short[] zeros = new short[gap * CHANNELS];
                int written = ring.write(zeros, 0, gap);
                if (written < gap) {
                    ringDroppedFrames.addAndGet(gap - written);
                }
                concealedGapFrames.addAndGet(gap);
            } else {
                skippedGapFrames.addAndGet(gapLong);
            }
            nextFrameId = packet.firstFrameId;
        } else if (packet.firstFrameId < nextFrameId) {
            long overlap = nextFrameId - packet.firstFrameId;
            if (overlap >= packet.frames) {
                duplicateOrStale.incrementAndGet();
                return;
            }
            trimFrames = (int) overlap;
        }

        int frames = packet.frames - trimFrames;
        int written = ring.write(packet.samples, trimFrames * CHANNELS, frames);
        if (written < frames) {
            ringDroppedFrames.addAndGet(frames - written);
        }

        nextFrameId = packet.firstFrameId + packet.frames;
    }

    private Packet decode(byte[] bytes, int length) {
        if (length < HEADER_BYTES) return null;

        for (int i = 0; i < MAGIC.length; i++) {
            if (bytes[i] != MAGIC[i]) return null;
        }

        ByteBuffer b = ByteBuffer.wrap(bytes, 0, length).order(ByteOrder.LITTLE_ENDIAN);
        b.position(4);

        int version = b.get() & 0xff;
        b.get(); // flags
        int headerBytes = b.getShort() & 0xffff;
        long session = b.getLong();
        long sequence = b.getLong();
        long firstFrame = b.getLong();
        int frameCount = b.getShort() & 0xffff;
        int channels = b.get() & 0xff;
        int format = b.get() & 0xff;
        int sampleRate = b.getInt();

        if (version != 1
                || headerBytes != HEADER_BYTES
                || channels != CHANNELS
                || format != FORMAT_PCM16_LE
                || sampleRate != SAMPLE_RATE
                || frameCount <= 0
                || frameCount > MAX_PACKET_FRAMES) {
            return null;
        }

        int expected = HEADER_BYTES + frameCount * CHANNELS * 2;
        if (length != expected) return null;

        short[] samples = new short[frameCount * CHANNELS];
        for (int i = 0; i < samples.length; i++) {
            samples[i] = b.getShort();
        }

        return new Packet(session, sequence, firstFrame, frameCount, samples);
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
    }
}
