package com.censusindex.android

import android.content.Context
import android.os.Build
import android.provider.Settings
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
        val prefs = app.getSharedPreferences("censusindex", Context.MODE_PRIVATE)
        // pc_id: 32 hex, generado una vez. No se usa ANDROID_ID (cambia al reinstalar).
        val pcId = prefs.getString("pc_id", null)
            ?: UUID.randomUUID().toString().replace("-", "").also { prefs.edit().putString("pc_id", it).apply() }
        // Nombre por defecto: el que el usuario le puso al telefono en Ajustes.
        val osName = Settings.Global.getString(app.contentResolver, "device_name")?.takeIf { it.isNotBlank() }
            ?: Build.MODEL
        handle = Native.nativeOpen(app.filesDir.path, pcId, osName)
        return handle
    }

    fun token(ctx: Context): String {
        val prefs = ctx.getSharedPreferences("censusindex", Context.MODE_PRIVATE)
        return prefs.getString("token", null)
            ?: UUID.randomUUID().toString().replace("-", "").also { prefs.edit().putString("token", it).apply() }
    }

    fun handle(ctx: Context): Long = init(ctx)

    /** Indexado completo: primera vez o a pedido. */
    @Synchronized
    fun full(ctx: Context): JSONObject {
        val h = init(ctx)
        syncVolumes(h)
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
        val out = JSONObject()
        // Un pendrive nuevo dispara MediaStore: se resuelve su politica primero.
        if (syncVolumes(h).optJSONArray("to_index")?.length() ?: 0 > 0) {
            out.put("new_volume", JSONObject(Native.nativeIndexPending(h)))
        }
        val res = delta.poll()
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
        syncVolumes(h)
        return JSONObject(Native.nativeSweep(h))
    }

    /**
     * Declara los volumenes montados y aplica la politica de extraibles. Si hay
     * un volumen nuevo por el que preguntar, publica la notificacion.
     */
    @Synchronized
    fun syncVolumes(h: Long): JSONObject {
        val r = JSONObject(Native.nativeVolumesUpdate(h, Volumes.json(app)))
        val ask = r.optJSONArray("ask")
        val rows = r.optJSONArray("volumes")
        if (ask != null && rows != null) {
            for (i in 0 until ask.length()) {
                for (j in 0 until rows.length()) {
                    val v = rows.getJSONObject(j)
                    if (v.getString("volume_id") == ask.getString(i)) VolumePrompt.show(app, v)
                }
            }
        }
        return r
    }

    /** Llamado al conectar/desconectar un volumen (callback del sistema). */
    fun onVolumesChanged(ctx: Context): JSONObject {
        val h = init(ctx)
        val r = syncVolumes(h)
        if ((r.optJSONArray("to_index")?.length() ?: 0) > 0) {
            return JSONObject(Native.nativeIndexPending(h)).also { MediaDelta(app).markSynced() }
        }
        return r
    }

    fun setPolicy(ctx: Context, volumeId: String, policy: String): JSONObject =
        JSONObject(Native.nativeSetPolicy(init(ctx), volumeId, policy))

    fun policyGet(ctx: Context): JSONObject = JSONObject(Native.nativePolicyGet(init(ctx)))

    fun indexOnce(ctx: Context, volumeId: String): JSONObject =
        JSONObject(Native.nativeIndexOnce(init(ctx), volumeId))

    fun indexPending(ctx: Context): JSONObject = JSONObject(Native.nativeIndexPending(init(ctx)))

    fun search(ctx: Context, q: String, limit: Int = 200): JSONObject =
        JSONObject(Native.nativeSearch(init(ctx), q, limit))

    fun deviceInfo(ctx: Context): JSONObject = JSONObject(Native.nativeDeviceInfo(init(ctx)))
    fun setDeviceName(ctx: Context, name: String): JSONObject = JSONObject(Native.nativeSetDeviceName(init(ctx), name))
    fun aiGet(ctx: Context): JSONObject = JSONObject(Native.nativeAiGet(init(ctx)))
    fun aiSet(ctx: Context, cfg: JSONObject): JSONObject = JSONObject(Native.nativeAiSet(init(ctx), cfg.toString()))
    fun aiModels(ctx: Context, ep: JSONObject): JSONObject = JSONObject(Native.nativeAiModels(init(ctx), ep.toString()))
    fun ask(ctx: Context, q: String): JSONObject = JSONObject(Native.nativeAsk(init(ctx), q, true))

    fun stats(ctx: Context): JSONObject = JSONObject(Native.nativeStats(init(ctx)))

    /** Barrido periodico: 6 h, solo con bateria no baja. */
    fun schedulePeriodic(ctx: Context) {
        val req = PeriodicWorkRequestBuilder<SweepWorker>(6, TimeUnit.HOURS)
            .setConstraints(Constraints.Builder().setRequiresBatteryNotLow(true).build())
            .build()
        WorkManager.getInstance(ctx).enqueueUniquePeriodicWork("censusindex-sweep", ExistingPeriodicWorkPolicy.KEEP, req)
    }

    /** Barrido unico ~2 min despues; KEEP agrupa una rafaga de borrados en uno. */
    fun sweepSoon(ctx: Context) {
        val req = OneTimeWorkRequestBuilder<SweepWorker>().setInitialDelay(2, TimeUnit.MINUTES).build()
        WorkManager.getInstance(ctx).enqueueUniqueWork("censusindex-sweep-soon", ExistingWorkPolicy.KEEP, req)
    }
}
