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

public final class MainActivity extends Activity implements CaptureService.MeterListener {
    private static final int REQ_AUDIO = 100;
    private static final int REQ_PROJECTION = 101;

    private TextView status;
    private TextView meter;
    private TextView delayLabel;
    private MediaProjectionManager projectionManager;

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
        root.addView(delayLabel);
        root.addView(delay);
        root.addView(meter);
        root.addView(start);
        root.addView(stop);
        setContentView(root);
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
        startForegroundService(svc);
        status.setText("Starting capture + local rerender…");
    }

    @Override
    protected void onStart() {
        super.onStart();
        CaptureService.setMeterListener(this);
    }

    @Override
    protected void onStop() {
        CaptureService.setMeterListener(null);
        super.onStop();
    }

    @Override
    public void onMeter(CaptureService.MeterSnapshot snapshot) {
        runOnUiThread(() -> {
            status.setText("48 kHz / PCM16 / stereo — self UID excluded");
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
                            "  underflow: %,d frames",
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
                    snapshot.underflowFrames));
        });
    }

    @Override
    public void onCaptureError(String message) {
        runOnUiThread(() -> status.setText("Capture error: " + message));
    }
}
