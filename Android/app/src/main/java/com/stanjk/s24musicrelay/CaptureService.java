package com.stanjk.s24musicrelay;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.media.AudioAttributes;
import android.media.AudioDeviceCallback;
import android.media.AudioDeviceInfo;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioPlaybackCaptureConfiguration;
import android.media.AudioRecord;
import android.media.AudioTrack;
import android.media.projection.MediaProjection;
import android.media.projection.MediaProjectionManager;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.Process;
import android.os.SystemClock;
import android.util.Log;

import java.util.Arrays;

public final class CaptureService extends Service {
    private static final String TAG = "S24MusicRelay";

    public static final String ACTION_START = "com.stanjk.s24musicrelay.START";
    public static final String EXTRA_RESULT_CODE = "resultCode";
    public static final String EXTRA_RESULT_DATA = "resultData";

    private static final String UDP_HOST = "192.168.137.1";
    private static final int UDP_PORT = 32124;

    private static final String CHANNEL_ID = "capture";
    private static final int NOTIFICATION_ID = 24;

    private static final int SAMPLE_RATE = 48_000;
    private static final int CHANNELS = 2;
    private static final int RENDER_BLOCK_FRAMES = 480;      // 10 ms
    private static final int TRANSPORT_RING_FRAMES = 4096;   // ~85 ms capacity, not presentation delay
    private static final int MAX_DELAY_MS = 500;

    private static volatile int configuredDelayMs;

    public interface MeterListener {
        void onMeter(MeterSnapshot snapshot);
        void onCaptureError(String message);
    }

    public static final class MeterSnapshot {
        public final long capturedFrames;
        public final long renderedFrames;
        public final double peak;
        public final double rms;
        public final String samples;
        public final int ringFrames;
        public final double ringMs;
        public final long ringHighWaterFrames;
        public final long underflowFrames;
        public final long droppedFrames;
        public final int configuredDelayMs;
        public final long udpPacketsSent;
        public final long udpLastSequence;
        public final long udpLastFirstFrame;
        public final long udpQueueDroppedFrames;

        MeterSnapshot(
                long capturedFrames,
                long renderedFrames,
                double peak,
                double rms,
                String samples,
                int ringFrames,
                double ringMs,
                long ringHighWaterFrames,
                long underflowFrames,
                long droppedFrames,
                int configuredDelayMs,
                long udpPacketsSent,
                long udpLastSequence,
                long udpLastFirstFrame,
                long udpQueueDroppedFrames) {
            this.capturedFrames = capturedFrames;
            this.renderedFrames = renderedFrames;
            this.peak = peak;
            this.rms = rms;
            this.samples = samples;
            this.ringFrames = ringFrames;
            this.ringMs = ringMs;
            this.ringHighWaterFrames = ringHighWaterFrames;
            this.underflowFrames = underflowFrames;
            this.droppedFrames = droppedFrames;
            this.configuredDelayMs = configuredDelayMs;
            this.udpPacketsSent = udpPacketsSent;
            this.udpLastSequence = udpLastSequence;
            this.udpLastFirstFrame = udpLastFirstFrame;
            this.udpQueueDroppedFrames = udpQueueDroppedFrames;
        }
    }

    private static volatile MeterListener meterListener;

    public static void setMeterListener(MeterListener listener) {
        meterListener = listener;
    }

    public static void setConfiguredDelayMs(int delayMs) {
        configuredDelayMs = Math.max(0, Math.min(MAX_DELAY_MS, delayMs));
    }

    public static int getConfiguredDelayMs() {
        return configuredDelayMs;
    }

    private volatile boolean running;
    private volatile long renderedFrames;
    private volatile long renderUnderflowFrames;
    private volatile long captureDroppedFrames;

    private Thread captureThread;
    private Thread renderThread;
    private AudioRecord audioRecord;
    private AudioTrack audioTrack;
    private MediaProjection mediaProjection;
    private PcmSpscRing transportRing;
    private FixedDelayLine delayLine;
    private UdpPcmSender udpSender;
    private AudioManager audioManager;
    private Handler mainHandler;
    private AudioDeviceCallback audioDeviceCallback;

    @Override
    public void onCreate() {
        super.onCreate();

        mainHandler = new Handler(Looper.getMainLooper());
        audioManager = getSystemService(AudioManager.class);
        audioDeviceCallback = new AudioDeviceCallback() {
            @Override
            public void onAudioDevicesAdded(AudioDeviceInfo[] addedDevices) {
                routeLocalTrackToBuiltinSpeaker("device-added");
            }

            @Override
            public void onAudioDevicesRemoved(AudioDeviceInfo[] removedDevices) {
                routeLocalTrackToBuiltinSpeaker("device-removed");
            }
        };
        if (audioManager != null) {
            audioManager.registerAudioDeviceCallback(audioDeviceCallback, mainHandler);
        }

        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel(
                CHANNEL_ID, "Audio capture", NotificationManager.IMPORTANCE_LOW));
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent == null || !ACTION_START.equals(intent.getAction())) {
            return START_NOT_STICKY;
        }

        Notification notification = new Notification.Builder(this, CHANNEL_ID)
                .setSmallIcon(android.R.drawable.ic_btn_speak_now)
                .setContentTitle("S24 Music Relay")
                .setContentText("Capturing and locally re-rendering PCM")
                .setOngoing(true)
                .build();

        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(
                    NOTIFICATION_ID,
                    notification,
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION);
        } else {
            startForeground(NOTIFICATION_ID, notification);
        }

        int resultCode = intent.getIntExtra(EXTRA_RESULT_CODE, 0);
        Intent resultData;
        if (Build.VERSION.SDK_INT >= 33) {
            resultData = intent.getParcelableExtra(EXTRA_RESULT_DATA, Intent.class);
        } else {
            //noinspection deprecation
            resultData = intent.getParcelableExtra(EXTRA_RESULT_DATA);
        }

        if (resultData == null) {
            fail("Missing MediaProjection consent data");
            stopSelf();
            return START_NOT_STICKY;
        }

        stopCapture();
        try {
            startCapture(resultCode, resultData);
        } catch (Throwable t) {
            fail(t.getClass().getSimpleName() + ": " + t.getMessage());
            stopSelf();
        }
        return START_NOT_STICKY;
    }

    private void startCapture(int resultCode, Intent resultData) {
        MediaProjectionManager pm = getSystemService(MediaProjectionManager.class);
        mediaProjection = pm.getMediaProjection(resultCode, resultData);
        if (mediaProjection == null) {
            throw new IllegalStateException("getMediaProjection returned null");
        }

        mediaProjection.registerCallback(new MediaProjection.Callback() {
            @Override
            public void onStop() {
                stopCapture();
                stopSelf();
            }
        }, new Handler(Looper.getMainLooper()));

        AudioPlaybackCaptureConfiguration config =
                new AudioPlaybackCaptureConfiguration.Builder(mediaProjection)
                        .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
                        .addMatchingUsage(AudioAttributes.USAGE_GAME)
                        .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
                        .excludeUid(Process.myUid())
                        .build();

        AudioFormat captureFormat = new AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(SAMPLE_RATE)
                .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
                .build();

        int minCaptureBytes = AudioRecord.getMinBufferSize(
                SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_STEREO,
                AudioFormat.ENCODING_PCM_16BIT);
        int captureBufferBytes = Math.max(
                minCaptureBytes * 2,
                RENDER_BLOCK_FRAMES * CHANNELS * 2 * 2);

        audioRecord = new AudioRecord.Builder()
                .setAudioFormat(captureFormat)
                .setBufferSizeInBytes(captureBufferBytes)
                .setAudioPlaybackCaptureConfig(config)
                .build();

        if (audioRecord.getState() != AudioRecord.STATE_INITIALIZED) {
            throw new IllegalStateException("AudioRecord not initialized");
        }

        AudioAttributes playbackAttributes = new AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                .build();

        AudioFormat playbackFormat = new AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(SAMPLE_RATE)
                .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                .build();

        int minPlaybackBytes = AudioTrack.getMinBufferSize(
                SAMPLE_RATE,
                AudioFormat.CHANNEL_OUT_STEREO,
                AudioFormat.ENCODING_PCM_16BIT);
        int playbackBufferBytes = Math.max(
                minPlaybackBytes,
                RENDER_BLOCK_FRAMES * CHANNELS * 2 * 2);

        audioTrack = new AudioTrack.Builder()
                .setAudioAttributes(playbackAttributes)
                .setAudioFormat(playbackFormat)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .setBufferSizeInBytes(playbackBufferBytes)
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                .build();

        if (audioTrack.getState() != AudioTrack.STATE_INITIALIZED) {
            throw new IllegalStateException("AudioTrack not initialized");
        }

        routeLocalTrackToBuiltinSpeaker("track-created");

        transportRing = new PcmSpscRing(TRANSPORT_RING_FRAMES, CHANNELS);
        delayLine = new FixedDelayLine(msToFrames(MAX_DELAY_MS), CHANNELS);

        try {
            udpSender = new UdpPcmSender(
                    UDP_HOST,
                    UDP_PORT,
                    SAMPLE_RATE,
                    CHANNELS);
            udpSender.start();
        } catch (Throwable t) {
            fail("UDP disabled: "
                    + t.getClass().getSimpleName()
                    + ": "
                    + t.getMessage());
            udpSender = null;
        }

        renderedFrames = 0;
        renderUnderflowFrames = 0;
        captureDroppedFrames = 0;
        running = true;

        audioRecord.startRecording();

        captureThread = new Thread(this::captureLoop, "PCM-Capture");
        renderThread = new Thread(this::renderLoop, "PCM-Render");
        captureThread.start();
        renderThread.start();
    }

    private void captureLoop() {
        final AudioRecord record = audioRecord;
        final PcmSpscRing ring = transportRing;
        if (record == null || ring == null) return;

        try {
            Process.setThreadPriority(Process.THREAD_PRIORITY_AUDIO);
        } catch (Throwable ignored) {
        }

        short[] pcm = new short[RENDER_BLOCK_FRAMES * CHANNELS];
        long capturedFrames = 0;
        long lastPublishNs = 0;

        while (running) {
            int samplesRead = record.read(pcm, 0, pcm.length, AudioRecord.READ_BLOCKING);
            if (samplesRead < 0) {
                if (running) fail("AudioRecord.read = " + samplesRead);
                break;
            }
            if (samplesRead == 0) continue;

            samplesRead -= samplesRead % CHANNELS;
            if (samplesRead == 0) continue;

            int framesRead = samplesRead / CHANNELS;
            long firstFrameId = capturedFrames;
            capturedFrames += framesRead;

            UdpPcmSender sender = udpSender;
            if (sender != null) {
                sender.offer(pcm, samplesRead, firstFrameId);
            }

            int framesWritten = ring.write(pcm, 0, framesRead);
            if (framesWritten < framesRead) {
                captureDroppedFrames += framesRead - framesWritten;
            }

            long now = System.nanoTime();
            if (now - lastPublishNs < 100_000_000L) continue;
            lastPublishNs = now;

            int peakAbs = 0;
            double sumSq = 0.0;
            for (int i = 0; i < samplesRead; i++) {
                int v = pcm[i];
                int a = Math.abs(v == Short.MIN_VALUE ? 32768 : v);
                if (a > peakAbs) peakAbs = a;
                double f = v / 32768.0;
                sumSq += f * f;
            }

            StringBuilder sb = new StringBuilder();
            int show = Math.min(8, samplesRead);
            for (int i = 0; i < show; i++) {
                if (i != 0) sb.append(", ");
                sb.append(pcm[i]);
            }

            int ringFrames = ring.availableFrames();
            MeterSnapshot snapshot = new MeterSnapshot(
                    capturedFrames,
                    renderedFrames,
                    peakAbs / 32768.0,
                    Math.sqrt(sumSq / samplesRead),
                    sb.toString(),
                    ringFrames,
                    framesToMs(ringFrames),
                    ring.highWaterFrames(),
                    renderUnderflowFrames,
                    captureDroppedFrames,
                    configuredDelayMs,
                    udpSender == null ? 0 : udpSender.packetsSent(),
                    udpSender == null ? -1 : udpSender.lastSequence(),
                    udpSender == null ? -1 : udpSender.lastFirstFrameId(),
                    udpSender == null ? 0 : udpSender.queueDroppedFrames());

            MeterListener listener = meterListener;
            if (listener != null) listener.onMeter(snapshot);
        }
    }

    private void renderLoop() {
        final AudioTrack track = audioTrack;
        final PcmSpscRing ring = transportRing;
        final FixedDelayLine delay = delayLine;
        if (track == null || ring == null || delay == null) return;

        try {
            Process.setThreadPriority(Process.THREAD_PRIORITY_AUDIO);
        } catch (Throwable ignored) {
        }

        short[] transportBlock = new short[RENDER_BLOCK_FRAMES * CHANNELS];
        short[] delayedBlock = new short[RENDER_BLOCK_FRAMES * CHANNELS];

        // Startup priming is transport elasticity only. Presentation delay is handled below.
        while (running && ring.availableFrames() < RENDER_BLOCK_FRAMES) {
            SystemClock.sleep(1);
        }
        if (!running) return;

        track.play();
        Handler handler = mainHandler;
        if (handler != null) {
            handler.postDelayed(() -> logLocalTrackRoute("after-play"), 250);
        }

        while (running) {
            int framesRead = ring.read(transportBlock, 0, RENDER_BLOCK_FRAMES);
            if (framesRead < RENDER_BLOCK_FRAMES) {
                int missingFrames = RENDER_BLOCK_FRAMES - framesRead;
                Arrays.fill(
                        transportBlock,
                        framesRead * CHANNELS,
                        RENDER_BLOCK_FRAMES * CHANNELS,
                        (short) 0);
                renderUnderflowFrames += missingFrames;
            }

            delay.setDelayFrames(msToFrames(configuredDelayMs));
            delay.process(
                    transportBlock,
                    0,
                    delayedBlock,
                    0,
                    RENDER_BLOCK_FRAMES);

            int samplesWritten = 0;
            final int samplesToWrite = delayedBlock.length;
            while (running && samplesWritten < samplesToWrite) {
                int n = track.write(
                        delayedBlock,
                        samplesWritten,
                        samplesToWrite - samplesWritten,
                        AudioTrack.WRITE_BLOCKING);

                if (n < 0) {
                    if (running) fail("AudioTrack.write = " + n);
                    running = false;
                    break;
                }
                if (n == 0) {
                    Thread.yield();
                    continue;
                }
                samplesWritten += n;
            }

            if (samplesWritten == samplesToWrite) {
                renderedFrames += RENDER_BLOCK_FRAMES;
            }
        }
    }

    private void routeLocalTrackToBuiltinSpeaker(String reason) {
        AudioTrack track = audioTrack;
        AudioManager manager = audioManager;
        if (track == null || manager == null) return;

        AudioDeviceInfo speaker = null;
        for (AudioDeviceInfo device :
                manager.getDevices(AudioManager.GET_DEVICES_OUTPUTS)) {
            if (device.getType() == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER) {
                speaker = device;
                break;
            }
        }

        if (speaker == null) {
            Log.w(TAG, "[route] " + reason + " builtin speaker not found");
            return;
        }

        boolean accepted = track.setPreferredDevice(speaker);
        Log.i(TAG,
                "[route] " + reason
                        + " prefer builtin speaker accepted=" + accepted
                        + " id=" + speaker.getId()
                        + " product=" + speaker.getProductName());

        Handler handler = mainHandler;
        if (handler != null) {
            handler.postDelayed(() -> logLocalTrackRoute(reason + "/settled"), 250);
        }
    }

    private void logLocalTrackRoute(String reason) {
        AudioTrack track = audioTrack;
        if (track == null) return;

        AudioDeviceInfo preferred = track.getPreferredDevice();
        AudioDeviceInfo routed = track.getRoutedDevice();

        Log.i(TAG,
                "[route] " + reason
                        + " preferred=" + describeAudioDevice(preferred)
                        + " actual=" + describeAudioDevice(routed));
    }

    private static String describeAudioDevice(AudioDeviceInfo device) {
        if (device == null) return "null";
        return "type=" + device.getType()
                + ",id=" + device.getId()
                + ",product=" + device.getProductName();
    }

    private static int msToFrames(int ms) {
        return (int) Math.round(ms * (SAMPLE_RATE / 1000.0));
    }

    private static double framesToMs(long frames) {
        return frames * 1000.0 / SAMPLE_RATE;
    }

    private void fail(String message) {
        MeterListener listener = meterListener;
        if (listener != null) listener.onCaptureError(message);
    }

    private synchronized void stopCapture() {
        running = false;

        AudioRecord record = audioRecord;
        audioRecord = null;
        if (record != null) {
            try { record.stop(); } catch (Throwable ignored) {}
            record.release();
        }

        AudioTrack track = audioTrack;
        audioTrack = null;
        if (track != null) {
            try { track.pause(); } catch (Throwable ignored) {}
            try { track.flush(); } catch (Throwable ignored) {}
            try { track.stop(); } catch (Throwable ignored) {}
            track.release();
        }

        captureThread = null;
        renderThread = null;
        transportRing = null;
        delayLine = null;

        UdpPcmSender sender = udpSender;
        udpSender = null;
        if (sender != null) {
            sender.close();
        }

        MediaProjection projection = mediaProjection;
        mediaProjection = null;
        if (projection != null) {
            try { projection.stop(); } catch (Throwable ignored) {}
        }
    }

    @Override
    public void onDestroy() {
        stopCapture();

        AudioManager manager = audioManager;
        AudioDeviceCallback callback = audioDeviceCallback;
        if (manager != null && callback != null) {
            try {
                manager.unregisterAudioDeviceCallback(callback);
            } catch (Throwable ignored) {
            }
        }
        audioDeviceCallback = null;
        audioManager = null;
        mainHandler = null;

        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
