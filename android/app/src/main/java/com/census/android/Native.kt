package com.census.android

/** Puente JNI hacia libcensus_android.so (Rust). Todo devuelve JSON. */
object Native {
    init { System.loadLibrary("census_android") }

    external fun nativeOpen(dir: String, pcId: String, device: String): Long
    external fun nativeSetVolumes(h: Long, volumesJson: String): String
    external fun nativeFull(h: Long): String
    external fun nativeSweep(h: Long): String
    external fun nativeDirty(h: Long, dirsJson: String): String
    external fun nativeSearch(h: Long, q: String, limit: Int): String
    external fun nativeStats(h: Long): String
    /** Bloquea hasta que el servidor muera: llamar desde un hilo propio. */
    external fun nativeServe(h: Long, port: Int, lan: Boolean, token: String): String
    external fun nativeDeviceInfo(h: Long): String
    /** "" vuelve al nombre del sistema; si no, sin espacios. */
    external fun nativeSetDeviceName(h: Long, name: String): String
    external fun nativeAiGet(h: Long): String
    external fun nativeAiSet(h: Long, cfgJson: String): String
    external fun nativeAiModels(h: Long, endpointJson: String): String
    /** Bloquea por red: llamar desde un hilo de fondo. */
    external fun nativeAsk(h: Long, question: String, spanish: Boolean): String
    external fun nativeClose(h: Long)
}
