package com.census.android

import android.content.Context
import android.provider.MediaStore
import org.json.JSONArray
import java.io.File

/**
 * Deteccion de cambios por generacion de MediaStore (API 30+).
 *
 * `GENERATION_MODIFIED` sube cada vez que una fila se crea o cambia, asi que
 * "lo que cambio desde la ultima vez" es una consulta indexada de costo
 * proporcional a los cambios, no al tamano del telefono.
 *
 * MediaStore no deja lapida cuando se borra una fila. Los borrados se detectan
 * por aritmetica: `filas_antes + filas_nuevas - filas_ahora > 0`. Si hay
 * borrados no sabemos *cual* carpeta cambio; se pide un barrido podado.
 */
class MediaDelta(private val ctx: Context) {
    private val prefs = ctx.getSharedPreferences("media_delta", Context.MODE_PRIVATE)

    data class Result(val dirtyDirs: Set<String>, val deletions: Boolean)

    /** Fija el cursor al estado actual (tras un indexado completo). */
    fun markSynced() {
        for (vol in MediaStore.getExternalVolumeNames(ctx)) {
            prefs.edit()
                .putLong("gen:$vol", MediaStore.getGeneration(ctx, vol))
                .putLong("cnt:$vol", count(vol, null, null))
                .apply()
        }
    }

    fun hasCursor(): Boolean = prefs.contains("gen:external_primary")

    fun poll(): Result {
        val dirs = HashSet<String>()
        var deletions = false
        for (vol in MediaStore.getExternalVolumeNames(ctx)) {
            // La generacion se lee ANTES de consultar: un cambio que llegue
            // durante la consulta queda para el proximo ciclo, nunca se pierde.
            val now = MediaStore.getGeneration(ctx, vol)
            val last = prefs.getLong("gen:$vol", -1L)
            if (last < 0) { continue }
            val uri = MediaStore.Files.getContentUri(vol)
            var added = 0L
            ctx.contentResolver.query(
                uri,
                arrayOf(MediaStore.MediaColumns.DATA, MediaStore.MediaColumns.GENERATION_ADDED),
                "${MediaStore.MediaColumns.GENERATION_MODIFIED} > ?",
                arrayOf(last.toString()),
                null,
            )?.use { c ->
                while (c.moveToNext()) {
                    val path = c.getString(0) ?: continue
                    File(path).parent?.let(dirs::add)
                    if (c.getLong(1) > last) added++
                }
            }
            val cnt = count(vol, null, null)
            val before = prefs.getLong("cnt:$vol", cnt)
            if (before + added - cnt > 0) deletions = true
            prefs.edit().putLong("gen:$vol", now).putLong("cnt:$vol", cnt).apply()
        }
        return Result(dirs, deletions)
    }

    private fun count(vol: String, sel: String?, args: Array<String>?): Long =
        ctx.contentResolver.query(
            MediaStore.Files.getContentUri(vol), arrayOf("count(*)"), sel, args, null,
        )?.use { if (it.moveToFirst()) it.getLong(0) else 0L } ?: 0L

    companion object {
        fun toJson(dirs: Collection<String>): String = JSONArray(dirs.toList()).toString()
    }
}
