package com.stanjk.s24musicrelay;

import android.os.IBinder;
import android.os.Parcel;

import rikka.shizuku.ShizukuBinderWrapper;
import rikka.shizuku.SystemServiceHelper;

final class ShizukuAudioBridge {
    private static final String DESCRIPTOR = "android.media.IAudioService";
    private static final int TX_SET_APP_VOLUME = 293;
    private static final int TX_GET_APP_VOLUME = 294;

    private ShizukuAudioBridge() {}

    static int getAppVolume(int uid) throws Exception {
        Parcel data = Parcel.obtain();
        Parcel reply = Parcel.obtain();
        try {
            data.writeInterfaceToken(DESCRIPTOR);
            data.writeInt(uid);
            binder().transact(TX_GET_APP_VOLUME, data, reply, 0);
            reply.readException();
            return reply.readInt();
        } finally {
            reply.recycle();
            data.recycle();
        }
    }

    static void setAppVolume(int uid, int ratio, String packageName) throws Exception {
        Parcel data = Parcel.obtain();
        Parcel reply = Parcel.obtain();
        try {
            data.writeInterfaceToken(DESCRIPTOR);
            data.writeInt(uid);
            data.writeInt(ratio);
            data.writeString(packageName);
            binder().transact(TX_SET_APP_VOLUME, data, reply, 0);
            reply.readException();
        } finally {
            reply.recycle();
            data.recycle();
        }
    }

    private static IBinder binder() {
        IBinder raw = SystemServiceHelper.getSystemService("audio");
        if (raw == null) throw new IllegalStateException("audio service unavailable");
        return new ShizukuBinderWrapper(raw);
    }
}
