package com.stanjk.s24musicrelay;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.media.projection.MediaProjectionManager;
import android.os.Bundle;
import android.view.Gravity;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.SeekBar;
import android.widget.TextView;

import rikka.shizuku.Shizuku;

public final class MainActivity extends Activity implements CaptureService.MeterListener {
    private static final int REQ_AUDIO = 100;
    private static final int REQ_PROJECTION = 101;
    private static final int REQ_SHIZUKU = 102;
    private static final int TEST_UID = 10341;

    private TextView status;
    private TextView meter;
    private TextView speakerDelayLabel;
    private TextView usbDelayLabel;
    private TextView shizukuStatus;
    private MediaProjectionManager projectionManager;

    private final Shizuku.OnBinderReceivedListener binderReceivedListener =
            this::refreshShizukuStatus;

    private final Shizuku.OnBinderDeadListener binderDeadListener =
            this::refreshShizukuStatus;

    private final Shizuku.OnRequestPermissionResultListener permissionResultListener =
            (requestCode, grantResult) -> {
                if (requestCode == REQ_SHIZUKU) {
                    refreshShizukuStatus();
                }
            };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        projectionManager =
                (MediaProjectionManager) getSystemService(Context.MEDIA_PROJECTION_SERVICE);

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(48, 64, 48, 48);
        root.setGravity(Gravity.CENTER_HORIZONTAL);

        status = new TextView(this);
        status.setText("Idle");
        status.setTextSize(20);

        shizukuStatus = new TextView(this);
        shizukuStatus.setText("Shizuku: waiting");
        shizukuStatus.setTextSize(16);
        shizukuStatus.setPadding(0, 20, 0, 8);

        Button setZero = new Button(this);
        setZero.setText("Set source app 0");
        setZero.setOnClickListener(v -> setTestAppVolume(0));

        Button setHundred = new Button(this);
        setHundred.setText("Set source app 100");
        setHundred.setOnClickListener(v -> setTestAppVolume(100));

        speakerDelayLabel = new TextView(this);
        speakerDelayLabel.setPadding(0, 24, 0, 8);
        speakerDelayLabel.setTextSize(16);

        SeekBar speakerDelay = new SeekBar(this);
        speakerDelay.setMax(500);
        speakerDelay.setProgress(CaptureService.getConfiguredSpeakerDelayMs());
        updateSpeakerDelayLabel(speakerDelay.getProgress());
        speakerDelay.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
            @Override
            public void onProgressChanged(SeekBar seekBar, int progress, boolean fromUser) {
                CaptureService.setConfiguredSpeakerDelayMs(progress);
                updateSpeakerDelayLabel(progress);
            }

            @Override
            public void onStartTrackingTouch(SeekBar seekBar) {
            }

            @Override
            public void onStopTrackingTouch(SeekBar seekBar) {
            }
        });

        usbDelayLabel = new TextView(this);
        usbDelayLabel.setPadding(0, 24, 0, 8);
        usbDelayLabel.setTextSize(16);

        SeekBar usbDelay = new SeekBar(this);
        usbDelay.setMax(500);
        usbDelay.setProgress(CaptureService.getConfiguredUsbDelayMs());
        updateUsbDelayLabel(usbDelay.getProgress());
        usbDelay.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
            @Override
            public void onProgressChanged(SeekBar seekBar, int progress, boolean fromUser) {
                CaptureService.setConfiguredUsbDelayMs(progress);
                updateUsbDelayLabel(progress);
            }

            @Override
            public void onStartTrackingTouch(SeekBar seekBar) {
            }

            @Override
            public void onStopTrackingTouch(SeekBar seekBar) {
            }
        });

        meter = new TextView(this);
        meter.setText("PCM: --");
        meter.setTextSize(16);
        meter.setPadding(0, 32, 0, 32);
        meter.setTextIsSelectable(true);

        Button start = new Button(this);
        start.setText("Start capture + speaker + USB");
        start.setOnClickListener(v -> beginCapture());

        Button stop = new Button(this);
        stop.setText("Stop");
        stop.setOnClickListener(v -> {
            stopService(new Intent(this, CaptureService.class));
            status.setText("Stopped");
        });

        root.addView(status);
        root.addView(shizukuStatus);
        root.addView(setZero);
        root.addView(setHundred);
        root.addView(speakerDelayLabel);
        root.addView(speakerDelay);
        root.addView(usbDelayLabel);
        root.addView(usbDelay);
        root.addView(meter);
        root.addView(start);
        root.addView(stop);
        setContentView(root);

        Shizuku.addBinderReceivedListenerSticky(binderReceivedListener);
        Shizuku.addBinderDeadListener(binderDeadListener);
        Shizuku.addRequestPermissionResultListener(permissionResultListener);

        ensureShizukuPermission();
    }

    @Override
    protected void onDestroy() {
        Shizuku.removeBinderReceivedListener(binderReceivedListener);
        Shizuku.removeBinderDeadListener(binderDeadListener);
        Shizuku.removeRequestPermissionResultListener(permissionResultListener);
        super.onDestroy();
    }

    private void ensureShizukuPermission() {
        try {
            if (!Shizuku.pingBinder()) {
                refreshShizukuStatus();
                return;
            }

            if (Shizuku.checkSelfPermission() == PackageManager.PERMISSION_GRANTED) {
                refreshShizukuStatus();
                return;
            }

            if (Shizuku.shouldShowRequestPermissionRationale()) {
                shizukuStatus.setText("Shizuku: denied; allow in Shizuku app");
                return;
            }

            Shizuku.requestPermission(REQ_SHIZUKU);
        } catch (Throwable t) {
            shizukuStatus.setText("Shizuku: " + t.getClass().getSimpleName());
        }
    }

    private void refreshShizukuStatus() {
        runOnUiThread(() -> {
            try {
                if (!Shizuku.pingBinder()) {
                    shizukuStatus.setText("Shizuku: unavailable");
                    return;
                }

                boolean granted =
                        Shizuku.checkSelfPermission() == PackageManager.PERMISSION_GRANTED;
                shizukuStatus.setText(
                        "Shizuku: UID " + Shizuku.getUid()
                                + (granted ? " authorized" : " authorization required"));
            } catch (Throwable t) {
                shizukuStatus.setText("Shizuku: " + t.getClass().getSimpleName());
            }
        });
    }

    private void setTestAppVolume(int ratio) {
        if (!Shizuku.pingBinder()
                || Shizuku.checkSelfPermission() != PackageManager.PERMISSION_GRANTED) {
            ensureShizukuPermission();
            return;
        }

        status.setText("Setting UID " + TEST_UID + " volume to " + ratio + "...");
        new Thread(() -> {
            try {
                int route = ShizukuAudioBridge.getAppDevice(TEST_UID);
                if (ratio == 0) {
                    ShizukuAudioBridge.setAppDevice(
                            TEST_UID,
                            ShizukuAudioBridge.DEVICE_OUT_SPEAKER,
                            false);
                    route = ShizukuAudioBridge.getAppDevice(TEST_UID);
                    if ((route & ShizukuAudioBridge.DEVICE_OUT_SPEAKER) == 0) {
                        final int rejectedRoute = route;
                        runOnUiThread(() ->
                                status.setText(
                                        "Refused Set 0: source route is 0x"
                                                + Integer.toHexString(rejectedRoute)
                                                + ", not speaker"));
                        return;
                    }
                }

                ShizukuAudioBridge.setAppVolume(TEST_UID, ratio, getPackageName());
                int actual = ShizukuAudioBridge.getAppVolume(TEST_UID);
                final int actualRoute = route;
                runOnUiThread(() ->
                        status.setText(
                                "UID " + TEST_UID
                                        + " route=0x" + Integer.toHexString(actualRoute)
                                        + " volume=" + actual));
            } catch (Throwable t) {
                runOnUiThread(() ->
                        status.setText("Shizuku audio error: "
                                + t.getClass().getSimpleName()
                                + ": "
                                + t.getMessage()));
            }
        }, "Shizuku-Audio").start();
    }

    private void updateSpeakerDelayLabel(int delayMs) {
        speakerDelayLabel.setText("Speaker presentation delay: " + delayMs + " ms");
    }

    private void updateUsbDelayLabel(int delayMs) {
        usbDelayLabel.setText("USB presentation delay: " + delayMs + " ms");
    }

    private void beginCapture() {
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO)
                != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[]{Manifest.permission.RECORD_AUDIO}, REQ_AUDIO);
            return;
        }
        startActivityForResult(
                projectionManager.createScreenCaptureIntent(),
                REQ_PROJECTION);
    }

    @Override
    public void onRequestPermissionsResult(
            int requestCode,
            String[] permissions,
            int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode == REQ_AUDIO
                && grantResults.length > 0
                && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
            beginCapture();
        } else if (requestCode == REQ_AUDIO) {
            status.setText("RECORD_AUDIO denied");
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != REQ_PROJECTION) return;

        if (resultCode != RESULT_OK || data == null) {
            status.setText("Projection consent cancelled");
            return;
        }

        Intent svc = new Intent(this, CaptureService.class);
        svc.setAction(CaptureService.ACTION_START);
        svc.putExtra(CaptureService.EXTRA_RESULT_CODE, resultCode);
        svc.putExtra(CaptureService.EXTRA_RESULT_DATA, data);
        startForegroundService(svc);
        status.setText("Starting capture + speaker + USB...");
    }

    @Override
    protected void onStart() {
        super.onStart();
        CaptureService.setMeterListener(this);
        refreshShizukuStatus();
    }

    @Override
    protected void onStop() {
        CaptureService.setMeterListener(null);
        super.onStop();
    }

    @Override
    public void onMeter(CaptureService.MeterSnapshot snapshot) {
        runOnUiThread(() -> meter.setText(String.format(
                "Capture\n"
                        + "  frames: %,d\n"
                        + "  peak: %.6f\n"
                        + "  RMS:  %.6f\n"
                        + "  PCM[0..7]: %s\n\n"
                        + "Speaker sink\n"
                        + "  delay: %d ms\n"
                        + "  ring: %d frames / %.2f ms\n"
                        + "  high-water: %d\n"
                        + "  rendered: %,d\n"
                        + "  underflow: %,d\n"
                        + "  dropped: %,d\n\n"
                        + "USB sink\n"
                        + "  active: %s\n"
                        + "  delay: %d ms\n"
                        + "  ring: %d frames / %.2f ms\n"
                        + "  high-water: %d\n"
                        + "  rendered: %,d\n"
                        + "  underflow: %,d\n"
                        + "  dropped: %,d\n\n"
                        + "UDP\n"
                        + "  packets: %,d\n"
                        + "  last seq/frame: %d / %d\n"
                        + "  queue dropped: %,d frames",
                snapshot.capturedFrames,
                snapshot.peak,
                snapshot.rms,
                snapshot.samples,

                snapshot.speakerDelayMs,
                snapshot.speakerRingFrames,
                snapshot.speakerRingMs,
                snapshot.speakerRingHighWaterFrames,
                snapshot.speakerRenderedFrames,
                snapshot.speakerUnderflowFrames,
                snapshot.speakerDroppedFrames,

                snapshot.usbActive,
                snapshot.usbDelayMs,
                snapshot.usbRingFrames,
                snapshot.usbRingMs,
                snapshot.usbRingHighWaterFrames,
                snapshot.usbRenderedFrames,
                snapshot.usbUnderflowFrames,
                snapshot.usbDroppedFrames,

                snapshot.udpPacketsSent,
                snapshot.udpLastSequence,
                snapshot.udpLastFirstFrame,
                snapshot.udpQueueDroppedFrames)));
    }

    @Override
    public void onCaptureError(String message) {
        runOnUiThread(() -> status.setText("Capture error: " + message));
    }
}
