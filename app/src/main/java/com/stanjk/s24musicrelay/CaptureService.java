package com.stanjk.s24musicrelay;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioPlaybackCaptureConfiguration;
import android.media.AudioRecord;
import android.media.projection.MediaProjection;
import android.media.projection.MediaProjectionManager;
import android.os.Build;
import android.os.IBinder;
import android.os.Process;

import java.util.Locale;

public final class CaptureService extends Service {
    public static final String ACTION_START = "com.stanjk.s24musicrelay.START";
    public static final String EXTRA_RESULT_CODE = "resultCode";
    public static final String EXTRA_RESULT_DATA = "resultData";

    private static final String CHANNEL_ID = "capture";
    private static final int NOTIFICATION_ID = 24;
    private static final int SAMPLE_RATE = 48_000;

    public interface MeterListener {
        void onMeter(MeterSnapshot snapshot);
        void onCaptureError(String message);
    }

    public static final class MeterSnapshot {
        public final long frames;
        public final double peak;
        public final double rms;
        public final String samples;

        MeterSnapshot(long frames, double peak, double rms, String samples) {
            this.frames = frames;
            this.peak = peak;
            this.rms = rms;
            this.samples = samples;
        }
    }

    private static volatile MeterListener meterListener;

    public static void setMeterListener(MeterListener listener) {
        meterListener = listener;
    }

    private volatile boolean running;
    private Thread captureThread;
    private AudioRecord audioRecord;
    private MediaProjection mediaProjection;

    @Override
    public void onCreate() {
        super.onCreate();
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
                .setContentText("Capturing playback PCM")
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

        AudioFormat format = new AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(SAMPLE_RATE)
                .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
                .build();

        int minBytes = AudioRecord.getMinBufferSize(
                SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_STEREO,
                AudioFormat.ENCODING_PCM_16BIT);
        int bufferBytes = Math.max(minBytes * 2, 8192);

        audioRecord = new AudioRecord.Builder()
                .setAudioFormat(format)
                .setBufferSizeInBytes(bufferBytes)
                .setAudioPlaybackCaptureConfig(config)
                .build();

        if (audioRecord.getState() != AudioRecord.STATE_INITIALIZED) {
            throw new IllegalStateException("AudioRecord not initialized");
        }

        running = true;
        audioRecord.startRecording();

        captureThread = new Thread(this::captureLoop, "PCM-Capture");
        captureThread.start();
    }

    private void captureLoop() {
        short[] pcm = new short[1920]; // 20 ms stereo @ 48 kHz
        long frames = 0;
        long lastPublishNs = 0;

        while (running) {
            int n = record.read(pcm, 0, pcm.length, AudioRecord.READ_BLOCKING);
            if (n < 0) {
                fail("AudioRecord.read = " + n);
                break;
            }
            if (n == 0) continue;

            frames += n / 2L;

            long now = System.nanoTime();
            if (now - lastPublishNs < 100_000_000L) continue;
            lastPublishNs = now;

            int peakAbs = 0;
            double sumSq = 0.0;
            for (int i = 0; i < n; i++) {
                int v = pcm[i];
                int a = Math.abs(v == Short.MIN_VALUE ? 32768 : v);
                if (a > peakAbs) peakAbs = a;
                double f = v / 32768.0;
                sumSq += f * f;
            }

            StringBuilder sb = new StringBuilder();
            int show = Math.min(8, n);
            for (int i = 0; i < show; i++) {
                if (i != 0) sb.append(", ");
                sb.append(pcm[i]);
            }

            MeterSnapshot snapshot = new MeterSnapshot(
                    frames,
                    peakAbs / 32768.0,
                    Math.sqrt(sumSq / n),
                    sb.toString());

            MeterListener listener = meterListener;
            if (listener != null) listener.onMeter(snapshot);
        }
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

        MediaProjection projection = mediaProjection;
        mediaProjection = null;
        if (projection != null) {
            try { projection.stop(); } catch (Throwable ignored) {}
        }
    }

    @Override
    public void onDestroy() {
        stopCapture();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
