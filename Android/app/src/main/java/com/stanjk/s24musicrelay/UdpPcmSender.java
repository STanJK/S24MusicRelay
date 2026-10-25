package com.stanjk.s24musicrelay;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.Arrays;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicLong;

final class UdpPcmSender implements AutoCloseable {
    static final int DEFAULT_PORT = 32124;
    static final int MAX_PACKET_FRAMES = 240; // 5 ms @ 48 kHz
    static final int HEADER_BYTES = 40;

    private static final byte[] MAGIC = new byte[]{'S', '2', '4', 'R'};
    private static final int VERSION = 1;
    private static final int FORMAT_PCM16_LE = 1;
    private static final int QUEUE_CAPACITY_CHUNKS = 64;

    private final InetAddress address;
    private final int port;
    private final int sampleRate;
    private final int channels;
    private final long sessionId;
    private final ArrayBlockingQueue<Chunk> queue =
            new ArrayBlockingQueue<>(QUEUE_CAPACITY_CHUNKS);

    private final AtomicLong packetsSent = new AtomicLong();
    private final AtomicLong queueDroppedFrames = new AtomicLong();
    private final AtomicLong lastSequence = new AtomicLong(-1);
    private final AtomicLong lastFirstFrameId = new AtomicLong(-1);

    private volatile boolean running;
    private DatagramSocket socket;
    private Thread thread;

    UdpPcmSender(String host, int port, int sampleRate, int channels) throws Exception {
        if (port <= 0 || port > 65535) {
            throw new IllegalArgumentException("UDP port must be 1..65535");
        }
        if (sampleRate != 48_000 || channels != 2) {
            throw new IllegalArgumentException("v1 protocol requires 48 kHz stereo");
        }

        this.address = InetAddress.getByName(host);
        this.port = port;
        this.sampleRate = sampleRate;
        this.channels = channels;
        this.sessionId =
                System.nanoTime() ^ (((long) android.os.Process.myPid()) << 32);
    }

    void start() throws Exception {
        if (running) return;
        socket = new DatagramSocket();
        running = true;
        thread = new Thread(this::sendLoop, "UDP-PCM-Sender");
        thread.start();
    }

    void offer(short[] source, int samples, long firstFrameId) {
        if (!running || samples <= 0) return;

        samples -= samples % channels;
        if (samples <= 0) return;

        short[] copy = Arrays.copyOf(source, samples);
        if (!queue.offer(new Chunk(copy, firstFrameId))) {
            queueDroppedFrames.addAndGet(samples / channels);
        }
    }

    long packetsSent() {
        return packetsSent.get();
    }

    long queueDroppedFrames() {
        return queueDroppedFrames.get();
    }

    long lastSequence() {
        return lastSequence.get();
    }

    long lastFirstFrameId() {
        return lastFirstFrameId.get();
    }

    private void sendLoop() {
        long sequence = 0;

        try {
            while (running || !queue.isEmpty()) {
                Chunk chunk = queue.poll(100, TimeUnit.MILLISECONDS);
                if (chunk == null) continue;

                int totalFrames = chunk.samples.length / channels;
                int frameOffset = 0;

                while (frameOffset < totalFrames) {
                    int frames = Math.min(
                            MAX_PACKET_FRAMES,
                            totalFrames - frameOffset);
                    long firstFrameId = chunk.firstFrameId + frameOffset;
                    long packetSequence = sequence++;

                    byte[] bytes = encode(
                            chunk.samples,
                            frameOffset,
                            frames,
                            packetSequence,
                            firstFrameId);

                    DatagramPacket packet =
                            new DatagramPacket(bytes, bytes.length, address, port);
                    socket.send(packet);

                    lastSequence.set(packetSequence);
                    lastFirstFrameId.set(firstFrameId);
                    packetsSent.incrementAndGet();

                    frameOffset += frames;
                }
            }
        } catch (InterruptedException ignored) {
            Thread.currentThread().interrupt();
        } catch (Throwable ignored) {
            // Keep local capture/rerender isolated from network failures.
        }
    }

    private byte[] encode(
            short[] samples,
            int frameOffset,
            int frames,
            long sequence,
            long firstFrameId) {
        int payloadBytes = frames * channels * 2;
        ByteBuffer buffer = ByteBuffer
                .allocate(HEADER_BYTES + payloadBytes)
                .order(ByteOrder.LITTLE_ENDIAN);

        buffer.put(MAGIC);
        buffer.put((byte) VERSION);
        buffer.put((byte) 0);
        buffer.putShort((short) HEADER_BYTES);
        buffer.putLong(sessionId);
        buffer.putLong(sequence);
        buffer.putLong(firstFrameId);
        buffer.putShort((short) frames);
        buffer.put((byte) channels);
        buffer.put((byte) FORMAT_PCM16_LE);
        buffer.putInt(sampleRate);

        int sampleOffset = frameOffset * channels;
        int sampleCount = frames * channels;
        for (int i = 0; i < sampleCount; i++) {
            buffer.putShort(samples[sampleOffset + i]);
        }

        return buffer.array();
    }

    @Override
    public void close() {
        running = false;

        DatagramSocket localSocket = socket;
        socket = null;
        if (localSocket != null) {
            localSocket.close();
        }

        Thread localThread = thread;
        thread = null;
        if (localThread != null) {
            localThread.interrupt();
            try {
                localThread.join(500);
            } catch (InterruptedException ignored) {
                Thread.currentThread().interrupt();
            }
        }

        queue.clear();
    }

    private static final class Chunk {
        final short[] samples;
        final long firstFrameId;

        Chunk(short[] samples, long firstFrameId) {
            this.samples = samples;
            this.firstFrameId = firstFrameId;
        }
    }
}
