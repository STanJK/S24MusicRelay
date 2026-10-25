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
import android.media.projection.MediaProjection;
import android.media.projection.MediaProjectionManager;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.Process;
import android.util.Log;

public final class CaptureService extends Service {
    private static final String TAG = "S24MusicRelay";

    public static final String ACTION_START = "com.stanjk.s24musicrelay.START";
    public static final String EXTRA_RESULT_CODE = "resultCode";
    public static final String EXTRA_RESULT_DATA = "resultData";

    private static final String UDP_HOST = "192.168.137.1";
    private static final int UDP_PORT = 32124;

    private static final String CHANNEL_ID = "capture";
    private static final int NOTIFICATION_ID = 24;

    private static final int SOURCE_UID = 10341;

    private static final int SAMPLE_RATE = 48_000;
    private static final int CHANNELS = 2;
    private static final int RENDER_BLOCK_FRAMES = 480;      // 10 ms
    private static final int TRANSPORT_RING_FRAMES = 4096;   // ~85 ms per sink
    private static final int MAX_DELAY_MS = 500;

    private static volatile int configuredSpeakerDelayMs;
    private static volatile int configuredUsbDelayMs;

    public interface MeterListener {
        void onMeter(MeterSnapshot snapshot);
        void onCaptureError(String message);
    }

    public static final class MeterSnapshot {
        public final long capturedFrames;
        public final double peak;
        public final double rms;
        public final String samples;

        public final long speakerRenderedFrames;
        public final int speakerRingFrames;
        public final double speakerRingMs;
        public final long speakerRingHighWaterFrames;
        public final long speakerUnderflowFrames;
        public final long speakerDroppedFrames;
        public final int speakerDelayMs;

        public final boolean usbActive;
        public final long usbRenderedFrames;
        public final int usbRingFrames;
        public final double usbRingMs;
        public final long usbRingHighWaterFrames;
        public final long usbUnderflowFrames;
        public final long usbDroppedFrames;
        public final int usbDelayMs;

        public final long udpPacketsSent;
        public final long udpLastSequence;
        public final long udpLastFirstFrame;
        public final long udpQueueDroppedFrames;

        MeterSnapshot(
                long capturedFrames,
                double peak,
                double rms,
                String samples,
                long speakerRenderedFrames,
                int speakerRingFrames,
                double speakerRingMs,
                long speakerRingHighWaterFrames,
                long speakerUnderflowFrames,
                long speakerDroppedFrames,
                int speakerDelayMs,
                boolean usbActive,
                long usbRenderedFrames,
                int usbRingFrames,
                double usbRingMs,
                long usbRingHighWaterFrames,
                long usbUnderflowFrames,
                long usbDroppedFrames,
                int usbDelayMs,
                long udpPacketsSent,
                long udpLastSequence,
                long udpLastFirstFrame,
                long udpQueueDroppedFrames) {
            this.capturedFrames = capturedFrames;
            this.peak = peak;
            this.rms = rms;
            this.samples = samples;

            this.speakerRenderedFrames = speakerRenderedFrames;
            this.speakerRingFrames = speakerRingFrames;
            this.speakerRingMs = speakerRingMs;
            this.speakerRingHighWaterFrames = speakerRingHighWaterFrames;
            this.speakerUnderflowFrames = speakerUnderflowFrames;
            this.speakerDroppedFrames = speakerDroppedFrames;
            this.speakerDelayMs = speakerDelayMs;

            this.usbActive = usbActive;
            this.usbRenderedFrames = usbRenderedFrames;
            this.usbRingFrames = usbRingFrames;
            this.usbRingMs = usbRingMs;
            this.usbRingHighWaterFrames = usbRingHighWaterFrames;
            this.usbUnderflowFrames = usbUnderflowFrames;
            this.usbDroppedFrames = usbDroppedFrames;
            this.usbDelayMs = usbDelayMs;

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

    public static void setConfiguredSpeakerDelayMs(int delayMs) {
        configuredSpeakerDelayMs = clampDelayMs(delayMs);
    }

    public static int getConfiguredSpeakerDelayMs() {
        return configuredSpeakerDelayMs;
    }

    public static void setConfiguredUsbDelayMs(int delayMs) {
        configuredUsbDelayMs = clampDelayMs(delayMs);
    }

    public static int getConfiguredUsbDelayMs() {
        return configuredUsbDelayMs;
    }

    private volatile boolean running;

    private Thread captureThread;
    private AudioRecord audioRecord;
    private MediaProjection mediaProjection;
    private UdpPcmSender udpSender;

    private volatile AudioRenderSink speakerSink;
    private volatile AudioRenderSink usbSink;

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
                if (pinSourceAppToSpeaker("device-added")) {
                    refreshRenderDevices("device-added");
                } else {
                    Log.e(TAG, "[route/usb] device-added blocked: source UID is not pinned to speaker");
                }
            }

            @Override
            public void onAudioDevicesRemoved(AudioDeviceInfo[] removedDevices) {
                pinSourceAppToSpeaker("device-removed");
                refreshRenderDevices("device-removed");
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
                .setContentText("Capturing PCM to speaker + USB DAC")
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
        }, mainHandler);

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

        // Own the source-app route before any USB sink is created. On Samsung,
        // allowing the source UID to migrate to USB makes setAppVolume(0)
        // silence the playback-capture tap as well.
        if (!pinSourceAppToSpeaker("capture-start")) {
            throw new IllegalStateException("source app could not be pinned to built-in speaker");
        }

        AudioDeviceInfo speaker = findBuiltinSpeaker();
        if (speaker == null) {
            throw new IllegalStateException("Built-in speaker output not found");
        }

        speakerSink = createSink("speaker", speaker, configuredSpeakerDelayMs);

        AudioDeviceInfo usb = findUsbOutput();
        if (usb != null) {
            usbSink = createSink("usb", usb, configuredUsbDelayMs);
        } else {
            usbSink = null;
            Log.i(TAG, "[route/usb] no USB output at capture start");
        }

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

        running = true;
        audioRecord.startRecording();

        speakerSink.start();
        AudioRenderSink currentUsb = usbSink;
        if (currentUsb != null) {
            currentUsb.start();
        }

        captureThread = new Thread(this::captureLoop, "PCM-Capture");
        captureThread.start();

        Handler handler = mainHandler;
        if (handler != null) {
            handler.postDelayed(() -> {
                AudioRenderSink currentSpeaker = speakerSink;
                if (currentSpeaker != null) currentSpeaker.logRoute("settled");
                AudioRenderSink settledUsb = usbSink;
                if (settledUsb != null) settledUsb.logRoute("settled");
            }, 250);
        }
    }

    private AudioRenderSink createSink(
            String label,
            AudioDeviceInfo preferredDevice,
            int delayMs) {
        AudioRenderSink sink = new AudioRenderSink(
                label,
                SAMPLE_RATE,
                CHANNELS,
                RENDER_BLOCK_FRAMES,
                TRANSPORT_RING_FRAMES,
                msToFrames(MAX_DELAY_MS),
                preferredDevice);
        sink.setDelayFrames(msToFrames(delayMs));
        return sink;
    }

    private void captureLoop() {
        final AudioRecord record = audioRecord;
        if (record == null) return;

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

            AudioRenderSink currentSpeaker = speakerSink;
            if (currentSpeaker != null) {
                currentSpeaker.setDelayFrames(msToFrames(configuredSpeakerDelayMs));
                currentSpeaker.offer(pcm, samplesRead);
            }

            AudioRenderSink currentUsb = usbSink;
            if (currentUsb != null) {
                currentUsb.setDelayFrames(msToFrames(configuredUsbDelayMs));
                currentUsb.offer(pcm, samplesRead);
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

            currentSpeaker = speakerSink;
            currentUsb = usbSink;

            MeterSnapshot snapshot = new MeterSnapshot(
                    capturedFrames,
                    peakAbs / 32768.0,
                    Math.sqrt(sumSq / samplesRead),
                    sb.toString(),

                    currentSpeaker == null ? 0 : currentSpeaker.renderedFrames(),
                    currentSpeaker == null ? 0 : currentSpeaker.ringFrames(),
                    currentSpeaker == null ? 0.0 : framesToMs(currentSpeaker.ringFrames()),
                    currentSpeaker == null ? 0 : currentSpeaker.ringHighWaterFrames(),
                    currentSpeaker == null ? 0 : currentSpeaker.underflowFrames(),
                    currentSpeaker == null ? 0 : currentSpeaker.droppedFrames(),
                    configuredSpeakerDelayMs,

                    currentUsb != null,
                    currentUsb == null ? 0 : currentUsb.renderedFrames(),
                    currentUsb == null ? 0 : currentUsb.ringFrames(),
                    currentUsb == null ? 0.0 : framesToMs(currentUsb.ringFrames()),
                    currentUsb == null ? 0 : currentUsb.ringHighWaterFrames(),
                    currentUsb == null ? 0 : currentUsb.underflowFrames(),
                    currentUsb == null ? 0 : currentUsb.droppedFrames(),
                    configuredUsbDelayMs,

                    sender == null ? 0 : sender.packetsSent(),
                    sender == null ? -1 : sender.lastSequence(),
                    sender == null ? -1 : sender.lastFirstFrameId(),
                    sender == null ? 0 : sender.queueDroppedFrames());

            MeterListener listener = meterListener;
            if (listener != null) listener.onMeter(snapshot);
        }
    }

    private boolean pinSourceAppToSpeaker(String reason) {
        try {
            ShizukuAudioBridge.setAppDevice(
                    SOURCE_UID,
                    ShizukuAudioBridge.DEVICE_OUT_SPEAKER,
                    false);
            int actual = ShizukuAudioBridge.getAppDevice(SOURCE_UID);
            boolean pinned =
                    (actual & ShizukuAudioBridge.DEVICE_OUT_SPEAKER) != 0;
            Log.i(TAG,
                    "[source-route] " + reason
                            + " uid=" + SOURCE_UID
                            + " requested=0x"
                            + Integer.toHexString(ShizukuAudioBridge.DEVICE_OUT_SPEAKER)
                            + " actual=0x"
                            + Integer.toHexString(actual)
                            + " pinned=" + pinned);
            return pinned;
        } catch (Throwable t) {
            Log.e(TAG,
                    "[source-route] " + reason
                            + " failed: "
                            + t.getClass().getSimpleName()
                            + ": "
                            + t.getMessage());
            return false;
        }
    }

    private synchronized void refreshRenderDevices(String reason) {
        if (!running) return;

        AudioDeviceInfo speaker = findBuiltinSpeaker();
        AudioRenderSink currentSpeaker = speakerSink;
        if (speaker != null && currentSpeaker != null) {
            currentSpeaker.setPreferredDevice(speaker, reason);
        }

        AudioDeviceInfo usb = findUsbOutput();
        AudioRenderSink currentUsb = usbSink;

        if (usb == null) {
            if (currentUsb != null) {
                usbSink = null;
                currentUsb.close();
                Log.i(TAG, "[route/usb] " + reason + " USB output removed");
            }
            return;
        }

        if (currentUsb != null && currentUsb.preferredDeviceId() == usb.getId()) {
            currentUsb.setPreferredDevice(usb, reason);
            return;
        }

        AudioRenderSink replacement;
        try {
            replacement = createSink("usb", usb, configuredUsbDelayMs);
            replacement.start();
        } catch (Throwable t) {
            fail("USB render init failed: "
                    + t.getClass().getSimpleName()
                    + ": "
                    + t.getMessage());
            return;
        }

        usbSink = replacement;
        if (currentUsb != null) {
            currentUsb.close();
        }

        Handler handler = mainHandler;
        if (handler != null) {
            handler.postDelayed(() -> {
                AudioRenderSink settledUsb = usbSink;
                if (settledUsb == replacement) {
                    settledUsb.logRoute(reason + "/settled");
                }
            }, 250);
        }
    }

    private AudioDeviceInfo findBuiltinSpeaker() {
        AudioManager manager = audioManager;
        if (manager == null) return null;

        for (AudioDeviceInfo device :
                manager.getDevices(AudioManager.GET_DEVICES_OUTPUTS)) {
            if (device.getType() == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER) {
                return device;
            }
        }
        return null;
    }

    private AudioDeviceInfo findUsbOutput() {
        AudioManager manager = audioManager;
        if (manager == null) return null;

        for (AudioDeviceInfo device :
                manager.getDevices(AudioManager.GET_DEVICES_OUTPUTS)) {
            int type = device.getType();
            if (type == AudioDeviceInfo.TYPE_USB_DEVICE
                    || type == AudioDeviceInfo.TYPE_USB_HEADSET
                    || type == AudioDeviceInfo.TYPE_USB_ACCESSORY) {
                return device;
            }
        }
        return null;
    }

    private static int clampDelayMs(int ms) {
        return Math.max(0, Math.min(MAX_DELAY_MS, ms));
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
        Log.e(TAG, message);
    }

    private synchronized void stopCapture() {
        running = false;

        AudioRecord record = audioRecord;
        audioRecord = null;
        if (record != null) {
            try { record.stop(); } catch (Throwable ignored) {}
            record.release();
        }

        captureThread = null;

        AudioRenderSink speaker = speakerSink;
        speakerSink = null;
        if (speaker != null) {
            speaker.close();
        }

        AudioRenderSink usb = usbSink;
        usbSink = null;
        if (usb != null) {
            usb.close();
        }

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
