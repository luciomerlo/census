package com.census.android

import android.content.Context
import android.os.Build
import androidx.work.Constraints
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import org.json.JSONObject
import java.util.UUID
import java.util.concurrent.TimeUnit

/**
 * Orquesta el indice: un unico motor nativo por proceso y una unica operacion
 * de reescritura a la vez (el nucleo ya serializa, esto evita apilar hilos).
 *
 * Tres fuentes de cambios, un solo destino (`nativeDirty` / `nativeSweep`):
 *  1. MediaStore (generacion + JobScheduler con TriggerContentUri): inmediato.
 *  2. Barrido podado por mtime de carpeta: cubre borrados y lo que MediaStore
 *     no ve; cada 6 h con bateria no baja, o ~2 min despues de un borrado.
 *  3. Indexado completo: solo la primera vez o a pedido.
 */
object Indexer {
    @Volatile private var handle = 0L
    private lateinit var app: Context

    @Synchronized
    fun init(ctx: Context): Long {
        if (handle != 0L) return handle
        app = ctx.applicationContext
        val prefs = app.getSharedPreferences("census", Context.MODE_PRIVATE)
        // pc_id: 32 hex, generado una vez. No se usa ANDROID_ID (cambia al reinstalar).
        val pcId = prefs.getString("pc_id", null)
            ?: UUID.randomUUID().toString().replace("-", "").also { prefs.edit().putString("pc_id", it).apply() }
        handle = Native.nativeOpen(app.filesDir.path, pcId, Build.MODEL)
        return handle
    }

    fun token(ctx: Context): String {
        val prefs = ctx.getSharedPreferences("census", Context.MODE_PRIVATE)
        return prefs.getString("token", null)
            ?: UUID.randomUUID().toString().replace("-", "").also { prefs.edit().putString("token", it).apply() }
    }

    fun handle(ctx: Context): Long = init(ctx)

    /** Indexado completo: primera vez o a pedido. */
    @Synchronized
    fun full(ctx: Context): JSONObject {
        val h = init(ctx)
        Native.nativeSetVolumes(h, Volumes.json(app))
        val r = JSONObject(Native.nativeFull(h))
        if (!r.has("error")) MediaDelta(app).markSynced()
        return r
    }

    /** Llamado por MediaChangeJob cuando MediaStore avisa un cambio. */
    @Synchronized
    fun onMediaChange(ctx: Context): JSONObject {
        val h = init(ctx)
        val delta = MediaDelta(app)
        if (!delta.hasCursor()) return full(ctx)
        Native.nativeSetVolumes(h, Volumes.json(app))
        val res = delta.poll()
        val out = JSONObject()
        if (res.dirtyDirs.isNotEmpty()) {
            out.put("dirty", JSONObject(Native.nativeDirty(h, MediaDelta.toJson(res.dirtyDirs))))
        }
        if (res.deletions) {
            out.put("deletions", true)
            sweepSoon(app)
        }
        return out
    }

    @Synchronized
    fun sweep(ctx: Context): JSONObject {
        val h = init(ctx)
        Native.nativeSetVolumes(h, Volumes.json(app))
        return JSONObject(Native.nativeSweep(h))
    }

    fun search(ctx: Context, q: String, limit: Int = 200): JSONObject =
        JSONObject(Native.nativeSearch(init(ctx), q, limit))

    fun stats(ctx: Context): JSONObject = JSONObject(Native.nativeStats(init(ctx)))

    /** Barrido periodico: 6 h, solo con bateria no baja. */
    fun schedulePeriodic(ctx: Context) {
        val req = PeriodicWorkRequestBuilder<SweepWorker>(6, TimeUnit.HOURS)
            .setConstraints(Constraints.Builder().setRequiresBatteryNotLow(true).build())
            .build()
        WorkManager.getInstance(ctx).enqueueUniquePeriodicWork("census-sweep", ExistingPeriodicWorkPolicy.KEEP, req)
    }

    /** Barrido unico ~2 min despues; KEEP agrupa una rafaga de borrados en uno. */
    fun sweepSoon(ctx: Context) {
        val req = OneTimeWorkRequestBuilder<SweepWorker>().setInitialDelay(2, TimeUnit.MINUTES).build()
        WorkManager.getInstance(ctx).enqueueUniqueWork("census-sweep-soon", ExistingWorkPolicy.KEEP, req)
    }
}
