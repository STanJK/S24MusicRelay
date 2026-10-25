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
import android.widget.EditText;
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
    private TextView delayLabel;
    private TextView shizukuStatus;
    private EditText udpHost;
    private EditText udpPort;
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
        projectionManager = (MediaProjectionManager) getSystemService(Context.MEDIA_PROJECTION_SERVICE);

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
        setZero.setText("Set 0");
        setZero.setOnClickListener(v -> setTestAppVolume(0));

        Button setHundred = new Button(this);
        setHundred.setText("Set 100");
        setHundred.setOnClickListener(v -> setTestAppVolume(100));

        udpHost = new EditText(this);
        udpHost.setHint("PC IPv4 for UDP (blank = disabled)");
        udpHost.setSingleLine(true);

        udpPort = new EditText(this);
        udpPort.setHint("UDP port");
        udpPort.setText("32124");
        udpPort.setSingleLine(true);
        udpPort.setInputType(android.text.InputType.TYPE_CLASS_NUMBER);

        delayLabel = new TextView(this);
        delayLabel.setPadding(0, 24, 0, 8);
        delayLabel.setTextSize(16);

        SeekBar delay = new SeekBar(this);
        delay.setMax(500);
        delay.setProgress(CaptureService.getConfiguredDelayMs());
        updateDelayLabel(delay.getProgress());
        delay.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
            @Override
            public void onProgressChanged(SeekBar seekBar, int progress, boolean fromUser) {
                CaptureService.setConfiguredDelayMs(progress);
                updateDelayLabel(progress);
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
        start.setText("Start capture + local rerender");
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
        root.addView(udpHost);
        root.addView(udpPort);
        root.addView(delayLabel);
        root.addView(delay);
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
                ShizukuAudioBridge.setAppVolume(TEST_UID, ratio, getPackageName());
                int actual = ShizukuAudioBridge.getAppVolume(TEST_UID);
                runOnUiThread(() ->
                        status.setText("UID " + TEST_UID + " app volume = " + actual));
            } catch (Throwable t) {
                runOnUiThread(() ->
                        status.setText("Shizuku audio error: "
                                + t.getClass().getSimpleName()
                                + ": "
                                + t.getMessage()));
            }
        }, "Shizuku-Audio").start();
    }

    private void updateDelayLabel(int delayMs) {
        delayLabel.setText("Local presentation delay: " + delayMs + " ms");
    }

    private void beginCapture() {
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[]{Manifest.permission.RECORD_AUDIO}, REQ_AUDIO);
            return;
        }
        startActivityForResult(projectionManager.createScreenCaptureIntent(), REQ_PROJECTION);
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode == REQ_AUDIO && grantResults.length > 0 &&
                grantResults[0] == PackageManager.PERMISSION_GRANTED) {
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

        String host = udpHost.getText().toString().trim();
        if (!host.isEmpty()) {
            svc.putExtra(CaptureService.EXTRA_UDP_HOST, host);

            int port = UdpPcmSender.DEFAULT_PORT;
            try {
                port = Integer.parseInt(udpPort.getText().toString().trim());
            } catch (Throwable ignored) {
            }
            svc.putExtra(CaptureService.EXTRA_UDP_PORT, port);
        }

        startForegroundService(svc);
        status.setText("Starting capture + local rerender...");
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
        runOnUiThread(() -> {
            meter.setText(String.format(
                    "Capture\n" +
                            "  frames: %,d\n" +
                            "  peak: %.6f\n" +
                            "  RMS:  %.6f\n" +
                            "  PCM[0..7]: %s\n\n" +
                            "Transport ring\n" +
                            "  occupancy: %d frames / %.2f ms\n" +
                            "  high-water: %d frames\n" +
                            "  dropped: %,d frames\n\n" +
                            "Presentation\n" +
                            "  configured delay: %d ms\n" +
                            "  rendered: %,d frames\n" +
                            "  underflow: %,d frames\n\n" +
                            "UDP\n" +
                            "  packets: %,d\n" +
                            "  last seq/frame: %d / %d\n" +
                            "  queue dropped: %,d frames",
                    snapshot.capturedFrames,
                    snapshot.peak,
                    snapshot.rms,
                    snapshot.samples,
                    snapshot.ringFrames,
                    snapshot.ringMs,
                    snapshot.ringHighWaterFrames,
                    snapshot.droppedFrames,
                    snapshot.configuredDelayMs,
                    snapshot.renderedFrames,
                    snapshot.underflowFrames,
                    snapshot.udpPacketsSent,
                    snapshot.udpLastSequence,
                    snapshot.udpLastFirstFrame,
                    snapshot.udpQueueDroppedFrames));
        });
    }

    @Override
    public void onCaptureError(String message) {
        runOnUiThread(() -> status.setText("Capture error: " + message));
    }
}
