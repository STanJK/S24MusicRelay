package com.stanjk.s24musicrelay;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.media.AudioAttributes;
import android.media.AudioDeviceInfo;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioTrack;
import android.os.Build;
import android.os.IBinder;
import android.os.Process;
import android.os.SystemClock;
import android.util.Log;

import java.util.Arrays;

public final class NetworkSpeakerService extends Service {
    private static final String TAG = "S24MusicRelay";
    public static final String ACTION_START =
            "com.stanjk.s24musicrelay.START_NETWORK_SPEAKER";

    private static final String CHANNEL_ID = "network-speaker";
    private static final int NOTIFICATION_ID = 25;

    private static final int SAMPLE_RATE = 48_000;
    private static final int CHANNELS = 2;
    private static final int BLOCK_FRAMES = 480; // 10 ms
    private static final int RING_FRAMES = 16_384; // ~341 ms physical capacity
    private static final int STARTUP_PRIME_FRAMES = 1_920; // 40 ms

    public interface Listener {
        void onNetworkSpeakerMeter(Snapshot snapshot);
        void onNetworkSpeakerError(String message);
    }

    public static final class Snapshot {
        public final long packets;
        public final long lost;
        public final long reordered;
        public final long decodeErrors;
        public final long concealedFrames;
        public final long skippedFrames;
        public final long ringDroppedFrames;
        public final long lastSequence;
        public final long lastFirstFrameId;

        public final int ringFrames;
        public final double ringMs;
        public final long ringHighWaterFrames;
        public final long renderedFrames;
        public final long underflowFrames;
        public final String actualRoute;

        Snapshot(
                long packets,
                long lost,
                long reordered,
                long decodeErrors,
                long concealedFrames,
                long skippedFrames,
                long ringDroppedFrames,
                long lastSequence,
                long lastFirstFrameId,
                int ringFrames,
                double ringMs,
                long ringHighWaterFrames,
                long renderedFrames,
                long underflowFrames,
                String actualRoute) {
            this.packets = packets;
            this.lost = lost;
            this.reordered = reordered;
            this.decodeErrors = decodeErrors;
            this.concealedFrames = concealedFrames;
            this.skippedFrames = skippedFrames;
            this.ringDroppedFrames = ringDroppedFrames;
            this.lastSequence = lastSequence;
            this.lastFirstFrameId = lastFirstFrameId;
            this.ringFrames = ringFrames;
            this.ringMs = ringMs;
            this.ringHighWaterFrames = ringHighWaterFrames;
            this.renderedFrames = renderedFrames;
            this.underflowFrames = underflowFrames;
            this.actualRoute = actualRoute;
        }
    }

    private static volatile Listener listener;

    public static void setListener(Listener value) {
        listener = value;
    }

    private volatile boolean running;
    private volatile long renderedFrames;
    private volatile long underflowFrames;

    private PcmSpscRing ring;
    private UdpPcmReceiver receiver;
    private AudioTrack track;
    private Thread renderThread;
    private int receiveBufferBytes;

    @Override
    public void onCreate() {
        super.onCreate();
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel(
                CHANNEL_ID,
                "Network speaker",
                NotificationManager.IMPORTANCE_LOW));
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent == null || !ACTION_START.equals(intent.getAction())) {
            return START_NOT_STICKY;
        }

        Notification notification = new Notification.Builder(this, CHANNEL_ID)
                .setSmallIcon(android.R.drawable.ic_btn_speak_now)
                .setContentTitle("S24 Music Relay")
                .setContentText("LC3 network speaker on UDP 32124")
                .setOngoing(true)
                .build();

        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(
                    NOTIFICATION_ID,
                    notification,
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
        } else {
            startForeground(NOTIFICATION_ID, notification);
        }

        stopNetworkSpeaker();

        try {
            startNetworkSpeaker();
        } catch (Throwable t) {
            fail(t.getClass().getSimpleName() + ": " + t.getMessage());
            stopSelf();
        }

        return START_NOT_STICKY;
    }

    private void startNetworkSpeaker() throws Exception {
        AudioManager manager = getSystemService(AudioManager.class);
        AudioDeviceInfo speaker = null;
        if (manager != null) {
            for (AudioDeviceInfo device :
                    manager.getDevices(AudioManager.GET_DEVICES_OUTPUTS)) {
                if (device.getType() == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER) {
                    speaker = device;
                    break;
                }
            }
        }

        if (speaker == null) {
            throw new IllegalStateException("built-in speaker not found");
        }

        AudioAttributes attributes = new AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                .build();

        AudioFormat format = new AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(SAMPLE_RATE)
                .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                .build();

        int minBytes = AudioTrack.getMinBufferSize(
                SAMPLE_RATE,
                AudioFormat.CHANNEL_OUT_STEREO,
                AudioFormat.ENCODING_PCM_16BIT);
        int requestedBytes = Math.max(
                minBytes,
                BLOCK_FRAMES * CHANNELS * 2 * 2);

        AudioTrack localTrack = new AudioTrack.Builder()
                .setAudioAttributes(attributes)
                .setAudioFormat(format)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .setBufferSizeInBytes(requestedBytes)
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                .build();

        if (localTrack.getState() != AudioTrack.STATE_INITIALIZED) {
            localTrack.release();
            throw new IllegalStateException("AudioTrack not initialized");
        }

        boolean accepted = localTrack.setPreferredDevice(speaker);
        Log.i(TAG,
                "[net-speaker] prefer builtin accepted=" + accepted
                        + " id=" + speaker.getId()
                        + " product=" + speaker.getProductName());

        PcmSpscRing localRing = new PcmSpscRing(RING_FRAMES, CHANNELS);
        UdpPcmReceiver localReceiver = new UdpPcmReceiver(localRing);

        receiveBufferBytes = localReceiver.start();
        ring = localRing;
        receiver = localReceiver;
        track = localTrack;

        renderedFrames = 0;
        underflowFrames = 0;
        running = true;

        renderThread = new Thread(this::renderLoop, "Network-Speaker-Render");
        renderThread.start();

        Log.i(TAG,
                "[net-speaker] codec=LC3 48k/10ms/100Bch udp=0.0.0.0:" + UdpPcmReceiver.PORT
                        + " rcvbuf=" + receiveBufferBytes
                        + " prime=" + STARTUP_PRIME_FRAMES);
    }

    private void renderLoop() {
        PcmSpscRing localRing = ring;
        AudioTrack localTrack = track;
        UdpPcmReceiver localReceiver = receiver;
        if (localRing == null || localTrack == null || localReceiver == null) return;

        try {
            Process.setThreadPriority(Process.THREAD_PRIORITY_AUDIO);
        } catch (Throwable ignored) {
        }

        while (running && localRing.availableFrames() < STARTUP_PRIME_FRAMES) {
            publishSnapshot(localRing, localTrack, localReceiver);
            SystemClock.sleep(20);
        }
        if (!running) return;

        localTrack.play();
        Log.i(TAG,
                "[net-speaker] play actual=" + describeDevice(localTrack.getRoutedDevice()));

        short[] block = new short[BLOCK_FRAMES * CHANNELS];
        long lastPublishNs = 0;

        while (running) {
            int got = localRing.read(block, 0, BLOCK_FRAMES);
            if (got < BLOCK_FRAMES) {
                int missing = BLOCK_FRAMES - got;
                Arrays.fill(
                        block,
                        got * CHANNELS,
                        block.length,
                        (short) 0);
                underflowFrames += missing;
            }

            int samplesWritten = 0;
            while (running && samplesWritten < block.length) {
                int n = localTrack.write(
                        block,
                        samplesWritten,
                        block.length - samplesWritten,
                        AudioTrack.WRITE_BLOCKING);

                if (n < 0) {
                    fail("AudioTrack.write=" + n);
                    running = false;
                    break;
                }
                if (n == 0) {
                    Thread.yield();
                    continue;
                }
                samplesWritten += n;
            }

            if (samplesWritten == block.length) {
                renderedFrames += BLOCK_FRAMES;
            }

            long now = System.nanoTime();
            if (now - lastPublishNs >= 200_000_000L) {
                lastPublishNs = now;
                publishSnapshot(localRing, localTrack, localReceiver);
            }
        }
    }

    private void publishSnapshot(
            PcmSpscRing localRing,
            AudioTrack localTrack,
            UdpPcmReceiver localReceiver) {
        Listener current = listener;
        if (current == null) return;

        UdpPcmReceiver.Stats net = localReceiver.stats();
        int available = localRing.availableFrames();

        current.onNetworkSpeakerMeter(new Snapshot(
                net.packetsReceived,
                net.lostPackets,
                net.reorderedArrivals,
                net.decodeErrors,
                net.concealedGapFrames,
                net.skippedGapFrames,
                net.ringDroppedFrames,
                net.lastSequence,
                net.lastFirstFrameId,
                available,
                available * 1000.0 / SAMPLE_RATE,
                localRing.highWaterFrames(),
                renderedFrames,
                underflowFrames,
                describeDevice(localTrack.getRoutedDevice())));
    }

    private static String describeDevice(AudioDeviceInfo device) {
        if (device == null) return "null";
        return "type=" + device.getType()
                + ",id=" + device.getId()
                + ",product=" + device.getProductName();
    }

    private void fail(String message) {
        Log.e(TAG, "[net-speaker] " + message);
        Listener current = listener;
        if (current != null) current.onNetworkSpeakerError(message);
    }

    private synchronized void stopNetworkSpeaker() {
        running = false;

        UdpPcmReceiver localReceiver = receiver;
        receiver = null;
        if (localReceiver != null) {
            localReceiver.close();
        }

        AudioTrack localTrack = track;
        track = null;
        if (localTrack != null) {
            // Pause first to unblock a blocking writer, but keep the native
            // AudioTrack alive until the render thread has actually exited.
            try { localTrack.pause(); } catch (Throwable ignored) {}
        }

        Thread localThread = renderThread;
        renderThread = null;
        if (localThread != null) {
            localThread.interrupt();
            try {
                localThread.join(500);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }

        if (localTrack != null) {
            try { localTrack.flush(); } catch (Throwable ignored) {}
            try { localTrack.stop(); } catch (Throwable ignored) {}
            localTrack.release();
        }

        ring = null;
    }

    @Override
    public void onDestroy() {
        stopNetworkSpeaker();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
